//! In-process VNX services, bounded pipes, topics and module event loops.
//!
//! The runtime owns no global process state. Dropping a subscription unregisters
//! it, cancellation propagates into request contexts, and shutdown closes queues
//! before joining workers. Network callers receive an explicitly configured session.
use crate::{error::invalid, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverflowPolicy {
    Reject,
    DropOldest,
    Block,
}
struct Queue<T> {
    values: VecDeque<T>,
    closed: bool,
    dropped: u64,
}
struct PipeInner<T> {
    state: Mutex<Queue<T>>,
    readable: Condvar,
    writable: Condvar,
    capacity: usize,
    overflow: OverflowPolicy,
}
/// A bounded multi-producer, multi-consumer queue. A close drains queued values.
pub struct Pipe<T> {
    inner: Arc<PipeInner<T>>,
}
impl<T> Clone for Pipe<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
impl<T> Pipe<T> {
    pub fn new(capacity: usize, overflow: OverflowPolicy) -> Result<Self> {
        if capacity == 0 {
            return Err(invalid("pipe capacity must be positive"));
        }
        Ok(Self {
            inner: Arc::new(PipeInner {
                state: Mutex::new(Queue {
                    values: VecDeque::new(),
                    closed: false,
                    dropped: 0,
                }),
                readable: Condvar::new(),
                writable: Condvar::new(),
                capacity,
                overflow,
            }),
        })
    }
    pub fn send(&self, value: T) -> Result<()> {
        self.send_until(value, None, false)
    }
    pub fn try_send(&self, value: T) -> Result<()> {
        self.send_until(value, None, true)
    }
    pub fn send_timeout(&self, value: T, timeout: Duration) -> Result<()> {
        self.send_until(value, Some(Instant::now() + timeout), false)
    }
    fn send_until(&self, value: T, deadline: Option<Instant>, nonblocking: bool) -> Result<()> {
        let mut state = self.inner.state.lock().unwrap();
        loop {
            if state.closed {
                return Err(Error::Closed);
            }
            if state.values.len() < self.inner.capacity {
                break;
            }
            match self.inner.overflow {
                OverflowPolicy::Reject => return Err(Error::Overflow),
                OverflowPolicy::DropOldest => {
                    state.values.pop_front();
                    state.dropped += 1;
                    break;
                }
                OverflowPolicy::Block if nonblocking => return Err(Error::Overflow),
                OverflowPolicy::Block => {
                    if let Some(deadline) = deadline {
                        let now = Instant::now();
                        if now >= deadline {
                            return Err(Error::Timeout);
                        }
                        state = self
                            .inner
                            .writable
                            .wait_timeout(state, deadline - now)
                            .unwrap()
                            .0;
                    } else {
                        state = self.inner.writable.wait(state).unwrap();
                    }
                }
            }
        }
        state.values.push_back(value);
        self.inner.readable.notify_one();
        Ok(())
    }
    pub fn recv(&self) -> Result<T> {
        self.recv_until(None)
    }
    pub fn try_recv(&self) -> Result<Option<T>> {
        let mut state = self.inner.state.lock().unwrap();
        if let Some(value) = state.values.pop_front() {
            self.inner.writable.notify_one();
            Ok(Some(value))
        } else if state.closed {
            Err(Error::Closed)
        } else {
            Ok(None)
        }
    }
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T> {
        self.recv_until(Some(Instant::now() + timeout))
    }
    fn recv_until(&self, deadline: Option<Instant>) -> Result<T> {
        let mut state = self.inner.state.lock().unwrap();
        loop {
            if let Some(value) = state.values.pop_front() {
                self.inner.writable.notify_one();
                return Ok(value);
            }
            if state.closed {
                return Err(Error::Closed);
            }
            if let Some(deadline) = deadline {
                let now = Instant::now();
                if now >= deadline {
                    return Err(Error::Timeout);
                }
                state = self
                    .inner
                    .readable
                    .wait_timeout(state, deadline - now)
                    .unwrap()
                    .0;
            } else {
                state = self.inner.readable.wait(state).unwrap();
            }
        }
    }
    pub fn close(&self) {
        self.inner.state.lock().unwrap().closed = true;
        self.inner.readable.notify_all();
        self.inner.writable.notify_all();
    }
    pub fn is_closed(&self) -> bool {
        self.inner.state.lock().unwrap().closed
    }
    pub fn len(&self) -> usize {
        self.inner.state.lock().unwrap().values.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn dropped(&self) -> u64 {
        self.inner.state.lock().unwrap().dropped
    }
}

type Job = Box<dyn FnOnce() + Send + 'static>;
struct PoolProgress {
    pending: Mutex<BTreeSet<u64>>,
    completed: Condvar,
    next: AtomicU64,
    running: AtomicUsize,
}
struct PoolInner {
    queue: Pipe<Job>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    progress: Arc<PoolProgress>,
    inline: bool,
}
impl PoolInner {
    fn stop(&self) {
        self.queue.close();
        let workers = std::mem::take(&mut *self.workers.lock().unwrap());
        for worker in workers {
            if worker.thread().id() != thread::current().id() {
                let _ = worker.join();
            }
        }
    }
}
impl Drop for PoolInner {
    fn drop(&mut self) {
        self.stop();
    }
}
struct TaskState {
    result: Mutex<Option<std::result::Result<(), String>>>,
    ready: Condvar,
}
/// A reusable completion ticket. Panicking jobs report an error and still signal
/// all barriers; the worker remains available for subsequent jobs.
#[derive(Clone)]
pub struct TaskHandle {
    id: u64,
    state: Arc<TaskState>,
}
impl TaskHandle {
    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn is_finished(&self) -> bool {
        self.state.result.lock().unwrap().is_some()
    }
    pub fn wait(&self) -> Result<()> {
        let mut result = self.state.result.lock().unwrap();
        while result.is_none() {
            result = self.state.ready.wait(result).unwrap();
        }
        result.as_ref().unwrap().clone().map_err(Error::Remote)
    }
    pub fn wait_timeout(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut result = self.state.result.lock().unwrap();
        loop {
            if let Some(result) = result.as_ref() {
                return result.clone().map_err(Error::Remote);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout);
            }
            result = self.state.ready.wait_timeout(result, remaining).unwrap().0;
        }
    }
}
#[derive(Clone)]
pub struct WorkerPool {
    inner: Arc<PoolInner>,
}
pub type ThreadPool = WorkerPool;
impl WorkerPool {
    /// A fixed number of workers; zero executes jobs synchronously. Queues remain
    /// bounded. execute() rejects overflow, while add_task() waits for a free slot.
    pub fn new(threads: usize, capacity: usize) -> Result<Self> {
        let queue: Pipe<Job> = Pipe::new(capacity, OverflowPolicy::Block)?;
        let mut workers = Vec::new();
        for i in 0..threads {
            let input = queue.clone();
            workers.push(
                thread::Builder::new()
                    .name(format!("vnx-worker-{i}"))
                    .spawn(move || {
                        while let Ok(job) = input.recv() {
                            let _ = catch_unwind(AssertUnwindSafe(job));
                        }
                    })?,
            );
        }
        Ok(Self {
            inner: Arc::new(PoolInner {
                queue,
                workers: Mutex::new(workers),
                inline: threads == 0,
                progress: Arc::new(PoolProgress {
                    pending: Mutex::new(BTreeSet::new()),
                    completed: Condvar::new(),
                    next: AtomicU64::new(1),
                    running: AtomicUsize::new(0),
                }),
            }),
        })
    }
    pub fn execute(&self, job: impl FnOnce() + Send + 'static) -> Result<()> {
        self.try_add_task(job).map(|_| ())
    }
    pub fn add_task(&self, job: impl FnOnce() + Send + 'static) -> Result<TaskHandle> {
        self.submit(job, false)
    }
    pub fn try_add_task(&self, job: impl FnOnce() + Send + 'static) -> Result<TaskHandle> {
        self.submit(job, true)
    }
    fn submit(&self, job: impl FnOnce() + Send + 'static, nonblocking: bool) -> Result<TaskHandle> {
        if self.inner.queue.is_closed() {
            return Err(Error::Closed);
        }
        let progress = self.inner.progress.clone();
        let id = progress.next.fetch_add(1, Ordering::Relaxed);
        let state = Arc::new(TaskState {
            result: Mutex::new(None),
            ready: Condvar::new(),
        });
        let ticket = TaskHandle {
            id,
            state: state.clone(),
        };
        progress.pending.lock().unwrap().insert(id);
        let completion = progress.clone();
        let task: Job = Box::new(move || {
            completion.running.fetch_add(1, Ordering::Relaxed);
            let result = match catch_unwind(AssertUnwindSafe(job)) {
                Ok(()) => Ok(()),
                Err(panic) => Err(panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).into()))
                    .unwrap_or_else(|| "worker task panicked".into())),
            };
            *state.result.lock().unwrap() = Some(result);
            state.ready.notify_all();
            completion.running.fetch_sub(1, Ordering::Relaxed);
            completion.pending.lock().unwrap().remove(&id);
            completion.completed.notify_all();
        });
        let submitted = if self.inner.inline {
            task();
            Ok(())
        } else if nonblocking {
            self.inner.queue.try_send(task)
        } else {
            self.inner.queue.send(task)
        };
        if let Err(error) = submitted {
            progress.pending.lock().unwrap().remove(&id);
            progress.completed.notify_all();
            return Err(error);
        }
        Ok(ticket)
    }
    pub fn sync(&self) {
        let mut pending = self.inner.progress.pending.lock().unwrap();
        while !pending.is_empty() {
            pending = self.inner.progress.completed.wait(pending).unwrap();
        }
    }
    pub fn sync_until(&self, barrier: u64) {
        let mut pending = self.inner.progress.pending.lock().unwrap();
        while pending.first().is_some_and(|id| *id <= barrier) {
            pending = self.inner.progress.completed.wait(pending).unwrap();
        }
    }
    pub fn sync_jobs(&self, jobs: &[u64]) {
        let mut pending = self.inner.progress.pending.lock().unwrap();
        while jobs.iter().any(|id| pending.contains(id)) {
            pending = self.inner.progress.completed.wait(pending).unwrap();
        }
    }
    pub fn get_num_pending(&self) -> usize {
        self.inner.queue.len()
    }
    pub fn get_num_running(&self) -> usize {
        self.inner.progress.running.load(Ordering::Relaxed)
    }
    pub fn get_num_pending_total(&self) -> usize {
        self.inner.progress.pending.lock().unwrap().len()
    }
    pub fn shutdown(&self) {
        self.inner.stop();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: u64,
    pub user: String,
    pub permissions: BTreeSet<String>,
    pub login_time: i64,
}
impl Session {
    pub fn anonymous() -> Self {
        Self {
            id: 0,
            user: String::new(),
            permissions: BTreeSet::new(),
            login_time: crate::time::now_micros(),
        }
    }
    pub fn new(user: impl Into<String>, permissions: impl IntoIterator<Item = String>) -> Self {
        Self {
            id: next_identity(),
            user: user.into(),
            permissions: permissions.into_iter().collect(),
            login_time: crate::time::now_micros(),
        }
    }
    pub fn internal() -> Self {
        Self::new("internal", ["*".into()])
    }
    pub fn has_permission(&self, permission: &str) -> bool {
        permission.is_empty()
            || self
                .permissions
                .contains(&format!("vnx.permission_e.{permission}"))
            || self.permissions.contains("*")
            || self.permissions.contains(permission)
    }
    pub fn is_anonymous(&self) -> bool {
        self.user.is_empty()
    }
}
fn next_identity() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
#[derive(Clone)]
pub struct RequestContext {
    pub request_id: u64,
    pub session: Session,
    pub deadline: Instant,
    cancelled: Arc<AtomicBool>,
}
impl RequestContext {
    pub(crate) fn new(request_id: u64, session: Session, timeout: Duration) -> Self {
        Self {
            request_id,
            session,
            deadline: Instant::now() + timeout,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) || Instant::now() >= self.deadline
    }
    pub fn check_cancelled(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Timeout)
        } else {
            Ok(())
        }
    }
}
/// Implemented by generated VNI dispatchers and handwritten services. Methods with
/// no required permission are public only to callers able to reach this service.
pub trait Service: Send + Sync + 'static {
    fn call(&self, method: &str, args: Value, context: &RequestContext) -> Result<Value>;
    fn permission(&self, _method: &str) -> Option<String> {
        None
    }
}
/// Closure-based service with method ACLs, useful when no generated dispatcher is needed.
#[derive(Default)]
pub struct Methods {
    methods: BTreeMap<String, Method>,
}
type Handler = dyn Fn(Value, &RequestContext) -> Result<Value> + Send + Sync;
pub struct Method {
    permission: Option<String>,
    handler: Arc<Handler>,
}
impl Methods {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn add(
        &mut self,
        name: impl Into<String>,
        permission: Option<String>,
        handler: impl Fn(Value, &RequestContext) -> Result<Value> + Send + Sync + 'static,
    ) {
        self.methods.insert(
            name.into(),
            Method {
                permission,
                handler: Arc::new(handler),
            },
        );
    }
}
impl Service for Methods {
    fn call(&self, method: &str, args: Value, context: &RequestContext) -> Result<Value> {
        let method = self
            .methods
            .get(method)
            .ok_or_else(|| Error::Remote(format!("no such method: {method}")))?;
        (method.handler)(args, context)
    }
    fn permission(&self, method: &str) -> Option<String> {
        self.methods.get(method).and_then(|m| m.permission.clone())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sample {
    pub topic: String,
    pub value: Value,
    pub src_mac: u64,
    pub seq_num: u64,
    pub recv_time: i64,
    pub replay: bool,
}
struct TopicState {
    next_seq: u64,
    retained: Option<Sample>,
}
struct Subscriber {
    topic: String,
    pipe: Pipe<Sample>,
}
struct Bus {
    topics: BTreeMap<String, TopicState>,
    subscriptions: BTreeMap<u64, Subscriber>,
}
struct RuntimeInner {
    services: RwLock<BTreeMap<String, Arc<dyn Service>>>,
    bus: Mutex<Bus>,
    workers: WorkerPool,
    closed: AtomicBool,
    publish_lock: Mutex<()>,
    id: u64,
}
impl Drop for RuntimeInner {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        for subscription in self.bus.get_mut().unwrap().subscriptions.values() {
            subscription.pipe.close();
        }
        self.workers.shutdown();
    }
}
/// A handle owns its subscription. Drop closes it and unregisters it atomically.
pub struct Subscription {
    id: u64,
    runtime: Weak<RuntimeInner>,
    pipe: Pipe<Sample>,
}
impl Subscription {
    pub fn recv(&self) -> Result<Sample> {
        self.pipe.recv()
    }
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Sample> {
        self.pipe.recv_timeout(timeout)
    }
    pub fn try_recv(&self) -> Result<Option<Sample>> {
        self.pipe.try_recv()
    }
    pub fn dropped(&self) -> u64 {
        self.pipe.dropped()
    }
    pub fn close(&self) {
        if let Some(runtime) = self.runtime.upgrade() {
            runtime.bus.lock().unwrap().subscriptions.remove(&self.id);
        }
        self.pipe.close();
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.close();
    }
}
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<RuntimeInner>,
}
fn topic_matches(parent: &str, child: &str) -> bool {
    parent.is_empty()
        || child == parent
        || child
            .strip_prefix(parent)
            .is_some_and(|s| s.starts_with('.'))
}
fn validate_topic(topic: &str) -> Result<()> {
    if !topic.is_empty()
        && (topic.split('.').any(str::is_empty) || topic.chars().any(char::is_whitespace))
    {
        return Err(invalid("invalid topic name"));
    }
    Ok(())
}
impl Runtime {
    pub fn new() -> Result<Self> {
        Self::with_workers(2, 1024)
    }
    pub fn with_workers(threads: usize, capacity: usize) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(RuntimeInner {
                services: RwLock::new(BTreeMap::new()),
                bus: Mutex::new(Bus {
                    topics: BTreeMap::new(),
                    subscriptions: BTreeMap::new(),
                }),
                workers: WorkerPool::new(threads, capacity)?,
                closed: AtomicBool::new(false),
                publish_lock: Mutex::new(()),
                id: next_identity(),
            }),
        })
    }
    pub fn register(&self, name: impl Into<String>, service: impl Service) -> Result<()> {
        self.register_shared(name, Arc::new(service))
    }
    pub fn register_shared(
        &self,
        name: impl Into<String>,
        service: Arc<dyn Service>,
    ) -> Result<()> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        let name = name.into();
        if name.is_empty() {
            return Err(invalid("empty service name"));
        }
        let mut services = self.inner.services.write().unwrap();
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        if services.contains_key(&name) {
            return Err(invalid(format!("service already registered: {name}")));
        }
        services.insert(name, service);
        Ok(())
    }
    pub fn unregister(&self, name: &str) -> bool {
        self.inner.services.write().unwrap().remove(name).is_some()
    }
    pub fn topic_names(&self) -> Vec<String> {
        self.inner
            .bus
            .lock()
            .unwrap()
            .topics
            .keys()
            .cloned()
            .collect()
    }
    pub fn service_names(&self) -> Vec<String> {
        self.inner
            .services
            .read()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }
    pub fn client(&self, name: impl Into<String>) -> Client {
        Client::new(self.clone(), name)
    }
    pub fn subscribe(
        &self,
        topic: &str,
        capacity: usize,
        overflow: OverflowPolicy,
        replay: bool,
    ) -> Result<Subscription> {
        validate_topic(topic)?;
        let pipe = Pipe::new(capacity, overflow)?;
        let mut bus = self.inner.bus.lock().unwrap();
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        if replay {
            for (name, state) in &bus.topics {
                if topic_matches(topic, name) {
                    if let Some(sample) = &state.retained {
                        let mut sample = sample.clone();
                        sample.replay = true;
                        pipe.try_send(sample)?;
                    }
                }
            }
        }
        let id = next_identity();
        bus.subscriptions.insert(
            id,
            Subscriber {
                topic: topic.into(),
                pipe: pipe.clone(),
            },
        );
        Ok(Subscription {
            id,
            runtime: Arc::downgrade(&self.inner),
            pipe,
        })
    }
    /// Publishing never holds the topic lock while waiting for a blocking pipe.
    /// The error reports overflow after other subscribers have received the sample.
    pub fn publish(&self, topic: &str, value: Value, retain: bool) -> Result<()> {
        validate_topic(topic)?;
        let _publish = self.inner.publish_lock.lock().unwrap();
        let (sample, pipes) = {
            let mut bus = self.inner.bus.lock().unwrap();
            if self.inner.closed.load(Ordering::Acquire) {
                return Err(Error::Closed);
            }
            let state = bus.topics.entry(topic.into()).or_insert(TopicState {
                next_seq: 1,
                retained: None,
            });
            let sample = Sample {
                topic: topic.into(),
                value,
                src_mac: self.inner.id,
                seq_num: state.next_seq,
                recv_time: crate::time::now_micros(),
                replay: false,
            };
            state.next_seq = state
                .next_seq
                .checked_add(1)
                .ok_or_else(|| invalid("topic sequence exhausted"))?;
            if retain {
                state.retained = Some(sample.clone());
            }
            let pipes: Vec<_> = bus
                .subscriptions
                .values()
                .filter(|s| topic_matches(&s.topic, topic))
                .map(|s| s.pipe.clone())
                .collect();
            (sample, pipes)
        };
        let mut result = Ok(());
        for pipe in pipes {
            if let Err(error) = pipe.send(sample.clone()) {
                result = Err(error);
            }
        }
        result
    }
    pub fn shutdown(&self) {
        if !self.inner.closed.swap(true, Ordering::AcqRel) {
            let mut bus = self.inner.bus.lock().unwrap();
            for sub in bus.subscriptions.values() {
                sub.pipe.close();
            }
            bus.subscriptions.clear();
            drop(bus);
            self.inner.services.write().unwrap().clear();
            self.inner.workers.shutdown();
        }
    }
    pub fn is_shutdown(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }
    pub(crate) fn dispatch(
        &self,
        service: &str,
        method: &str,
        args: Value,
        context: &RequestContext,
    ) -> Result<Value> {
        if self.is_shutdown() {
            return Err(Error::Closed);
        }
        context.check_cancelled()?;
        let service = self
            .inner
            .services
            .read()
            .unwrap()
            .get(service)
            .cloned()
            .ok_or_else(|| Error::Remote(format!("no such service: {service}")))?;
        if let Some(permission) = service.permission(method) {
            if !context.session.has_permission(&permission) {
                return Err(Error::PermissionDenied(permission));
            }
        }
        match catch_unwind(AssertUnwindSafe(|| service.call(method, args, context))) {
            Ok(result) => result,
            Err(_) => Err(Error::Remote("service handler panicked".into())),
        }
    }
}

/// A pending response. Cancel closes its return pipe, waking blocked waiters.
pub struct RpcCall {
    id: u64,
    result: Pipe<Result<Value>>,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
    cleanup: Option<Box<dyn Fn() + Send + Sync>>,
}
impl RpcCall {
    pub(crate) fn pending(id: u64, timeout: Duration) -> Result<(Self, Pipe<Result<Value>>)> {
        let result = Pipe::new(1, OverflowPolicy::Reject)?;
        Ok((
            Self {
                id,
                result: result.clone(),
                cancelled: Arc::new(AtomicBool::new(false)),
                deadline: Instant::now() + timeout,
                cleanup: None,
            },
            result,
        ))
    }
    pub(crate) fn on_cancel(&mut self, cleanup: impl Fn() + Send + Sync + 'static) {
        self.cleanup = Some(Box::new(cleanup));
    }
    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn wait(&self) -> Result<Value> {
        self.wait_timeout(self.deadline.saturating_duration_since(Instant::now()))
    }
    pub fn wait_timeout(&self, timeout: Duration) -> Result<Value> {
        match self
            .result
            .recv_timeout(timeout.min(self.deadline.saturating_duration_since(Instant::now())))
        {
            Ok(result) => result,
            Err(Error::Timeout) => {
                self.cancel();
                Err(Error::Timeout)
            }
            Err(Error::Closed) if Instant::now() >= self.deadline => Err(Error::Timeout),
            Err(error) => Err(error),
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.result.close();
        if let Some(cleanup) = &self.cleanup {
            cleanup();
        }
    }
}
impl Drop for RpcCall {
    fn drop(&mut self) {
        self.cancel();
    }
}
#[derive(Clone)]
pub struct Client {
    runtime: Runtime,
    service: String,
    timeout: Duration,
    session: Session,
}
impl Client {
    pub fn new(runtime: Runtime, service: impl Into<String>) -> Self {
        Self {
            runtime,
            service: service.into(),
            timeout: Duration::from_secs(30),
            session: Session::internal(),
        }
    }
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }
    pub fn set_session(&mut self, session: Session) {
        self.session = session;
    }
    pub fn call_value(&self, method: &str, args: Value) -> Result<Value> {
        self.call_async_value(method, args)?.wait()
    }
    pub fn call_async_value(&self, method: &str, mut args: Value) -> Result<RpcCall> {
        if !args.is_object() {
            return Err(invalid("method arguments must be an object"));
        }
        args.as_object_mut()
            .unwrap()
            .insert("__type".into(), Value::String(method.into()));
        let result = Pipe::new(1, OverflowPolicy::Reject)?;
        let context = RequestContext::new(next_identity(), self.session.clone(), self.timeout);
        let call = RpcCall {
            id: context.request_id,
            result: result.clone(),
            cancelled: context.cancelled.clone(),
            deadline: context.deadline,
            cleanup: None,
        };
        let runtime = self.runtime.clone();
        let service = self.service.clone();
        let method = method.to_owned();
        self.runtime.inner.workers.execute(move || {
            let answer = runtime.dispatch(&service, &method, args, &context);
            if !context.is_cancelled() {
                let _ = result.try_send(answer);
            }
            result.close();
        })?;
        Ok(call)
    }
}

pub type TimerId = u64;
struct Timer {
    next: Instant,
    interval: Option<Duration>,
    callback: Box<dyn FnMut() + Send>,
}
enum Event {
    Run(Job),
    Timer(TimerId, Timer),
    Cancel(TimerId),
}
/// Single-threaded callback loop. Timer callbacks never execute concurrently.
/// Shutdown drains queued callbacks, then drops timers and joins the loop.
pub struct Module {
    events: Pipe<Event>,
    worker: Option<JoinHandle<()>>,
}
impl Module {
    pub fn new(name: impl Into<String>, capacity: usize) -> Result<Self> {
        let events = Pipe::new(capacity, OverflowPolicy::Reject)?;
        let input = events.clone();
        let worker = thread::Builder::new().name(name.into()).spawn(move || {
            let mut timers: BTreeMap<TimerId, Timer> = BTreeMap::new();
            loop {
                let wait = timers
                    .values()
                    .map(|t| t.next.saturating_duration_since(Instant::now()))
                    .min()
                    .unwrap_or(Duration::from_secs(3600));
                match input.recv_timeout(wait) {
                    Ok(Event::Run(callback)) => {
                        let _ = catch_unwind(AssertUnwindSafe(callback));
                    }
                    Ok(Event::Timer(id, timer)) => {
                        timers.insert(id, timer);
                    }
                    Ok(Event::Cancel(id)) => {
                        timers.remove(&id);
                    }
                    Err(Error::Timeout) => {}
                    Err(_) => break,
                }
                let now = Instant::now();
                let due: Vec<_> = timers
                    .iter()
                    .filter(|(_, t)| t.next <= now)
                    .map(|(id, _)| *id)
                    .collect();
                for id in due {
                    let mut timer = timers.remove(&id).unwrap();
                    let _ = catch_unwind(AssertUnwindSafe(&mut timer.callback));
                    if let Some(interval) = timer.interval {
                        timer.next = Instant::now() + interval;
                        timers.insert(id, timer);
                    }
                }
            }
        })?;
        Ok(Self {
            events,
            worker: Some(worker),
        })
    }
    pub fn post(&self, callback: impl FnOnce() + Send + 'static) -> Result<()> {
        self.events.try_send(Event::Run(Box::new(callback)))
    }
    pub fn timer(
        &self,
        delay: Duration,
        interval: Option<Duration>,
        callback: impl FnMut() + Send + 'static,
    ) -> Result<TimerId> {
        if interval == Some(Duration::ZERO) {
            return Err(invalid("timer interval must be positive"));
        }
        let id = next_identity();
        self.events.try_send(Event::Timer(
            id,
            Timer {
                next: Instant::now() + delay,
                interval,
                callback: Box::new(callback),
            },
        ))?;
        Ok(id)
    }
    pub fn cancel_timer(&self, id: TimerId) -> Result<()> {
        self.events.try_send(Event::Cancel(id))
    }
    pub fn shutdown(&mut self) {
        self.events.close();
        if let Some(worker) = self.worker.take() {
            if worker.thread().id() != thread::current().id() {
                let _ = worker.join();
            }
        }
    }
}
impl Drop for Module {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// A clonable mailbox for one module's serialized callback loop.
#[derive(Clone)]
pub struct ModuleHandle {
    events: Pipe<Event>,
}
impl ModuleHandle {
    pub fn post(&self, callback: impl FnOnce() + Send + 'static) -> Result<()> {
        self.events.try_send(Event::Run(Box::new(callback)))
    }
    /// Register a service whose calls execute in this module's event loop.
    /// The runtime worker waits on a bounded return pipe until the caller deadline.
    pub fn register_service(
        &self,
        runtime: &Runtime,
        name: impl Into<String>,
        service: impl Service,
    ) -> Result<()> {
        runtime.register(
            name,
            ModuleService {
                events: self.events.clone(),
                service: Arc::new(service),
            },
        )
    }
}
struct ModuleService {
    events: Pipe<Event>,
    service: Arc<dyn Service>,
}
impl Service for ModuleService {
    fn call(&self, method: &str, args: Value, context: &RequestContext) -> Result<Value> {
        let response = Pipe::new(1, OverflowPolicy::Reject)?;
        let output = response.clone();
        let service = self.service.clone();
        let method = method.to_owned();
        let context = context.clone();
        let deadline = context.deadline;
        self.events.try_send(Event::Run(Box::new(move || {
            let result = if context.is_cancelled() {
                Err(Error::Timeout)
            } else {
                match catch_unwind(AssertUnwindSafe(|| service.call(&method, args, &context))) {
                    Ok(result) => result,
                    Err(_) => Err(Error::Remote("module handler panicked".into())),
                }
            };
            let _ = output.try_send(result);
            output.close();
        })))?;
        response.recv_timeout(deadline.saturating_duration_since(Instant::now()))?
    }
    fn permission(&self, method: &str) -> Option<String> {
        self.service.permission(method)
    }
}
impl Module {
    pub fn handle(&self) -> ModuleHandle {
        ModuleHandle {
            events: self.events.clone(),
        }
    }
    pub fn register_service(
        &self,
        runtime: &Runtime,
        name: impl Into<String>,
        service: impl Service,
    ) -> Result<()> {
        self.handle().register_service(runtime, name, service)
    }
}
