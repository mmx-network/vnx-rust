use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc, Arc,
};
use std::time::Duration;
use vnx::runtime::{Client, Methods, Module, OverflowPolicy, Pipe, Runtime, Session, WorkerPool};
use vnx::{Error, Result};

#[test]
fn bounded_pipes_overflow_drop_and_close() -> Result<()> {
    let pipe = Pipe::new(1, OverflowPolicy::Reject)?;
    pipe.send(1)?;
    assert!(matches!(pipe.send(2), Err(Error::Overflow)));
    pipe.close();
    assert_eq!(pipe.recv()?, 1);
    assert!(matches!(pipe.recv(), Err(Error::Closed)));
    assert!(matches!(pipe.send(3), Err(Error::Closed)));
    let pipe = Pipe::new(2, OverflowPolicy::DropOldest)?;
    for value in 1..=3 {
        pipe.send(value)?;
    }
    assert_eq!(pipe.dropped(), 1);
    assert_eq!(pipe.recv()?, 2);
    assert_eq!(pipe.recv()?, 3);
    assert!(pipe.try_recv()?.is_none());
    assert!(matches!(
        pipe.recv_timeout(Duration::ZERO),
        Err(Error::Timeout)
    ));
    Ok(())
}
#[test]
fn blocking_pipes_resume_and_shutdown_wakes_waiters() -> Result<()> {
    let pipe = Pipe::new(1, OverflowPolicy::Block)?;
    pipe.send(1)?;
    assert!(matches!(
        pipe.send_timeout(2, Duration::from_millis(2)),
        Err(Error::Timeout)
    ));
    let sender = pipe.clone();
    let worker = std::thread::spawn(move || sender.send(2));
    assert_eq!(pipe.recv()?, 1);
    worker.join().unwrap()?;
    assert_eq!(pipe.recv()?, 2);
    let reader = pipe.clone();
    let worker = std::thread::spawn(move || reader.recv());
    pipe.close();
    assert!(matches!(worker.join().unwrap(), Err(Error::Closed)));
    Ok(())
}
#[test]
fn hierarchical_topics_replay_overflow_and_cleanup() -> Result<()> {
    let runtime = Runtime::new()?;
    runtime.publish("mmx.node.blocks", json!({"height":1}), true)?;
    runtime.publish("mmx.node2", json!(9), true)?;
    let sub = runtime.subscribe("mmx.node", 2, OverflowPolicy::Reject, true)?;
    let first = sub.recv()?;
    assert!(first.replay);
    assert_eq!(first.seq_num, 1);
    assert_eq!(first.topic, "mmx.node.blocks");
    assert!(sub.try_recv()?.is_none());
    runtime.publish("mmx.node.blocks", json!({"height":2}), true)?;
    assert!(!sub.recv()?.replay);
    let slow = runtime.subscribe("mmx.node", 1, OverflowPolicy::DropOldest, false)?;
    for value in 3..=4 {
        runtime.publish("mmx.node.blocks", json!(value), true)?;
    }
    assert_eq!(slow.dropped(), 1);
    assert_eq!(slow.recv()?.seq_num, 4);
    drop(sub);
    runtime.publish("mmx.node.blocks", json!(5), false)?;
    let retained = runtime.subscribe("mmx.node", 1, OverflowPolicy::Reject, true)?;
    assert_eq!(retained.recv()?.value, json!(4));
    runtime.shutdown();
    assert!(slow.recv_timeout(Duration::ZERO).is_ok());
    assert!(matches!(slow.recv(), Err(Error::Closed)));
    assert!(matches!(
        runtime.publish("x", json!(0), true),
        Err(Error::Closed)
    ));
    Ok(())
}
#[test]
fn rpc_permissions_errors_timeouts_and_cancel() -> Result<()> {
    let runtime = Runtime::new()?;
    let mut service = Methods::new();
    service.add("test.read", None, |args, context| {
        Ok(json!({"value":args["value"],"request_id":context.request_id}))
    });
    service.add("test.secret", Some("admin".into()), |_, _| Ok(json!(42)));
    service.add("test.failure", None, |_, _| {
        Err(Error::Remote("intentional".into()))
    });
    let cancelled = Arc::new(AtomicUsize::new(0));
    let observed = cancelled.clone();
    service.add("test.wait", None, move |_, context| {
        while !context.is_cancelled() {
            std::thread::sleep(Duration::from_millis(1));
        }
        observed.fetch_add(1, Ordering::SeqCst);
        Err(Error::Timeout)
    });
    runtime.register("node", service)?;
    let mut client = Client::new(runtime.clone(), "node");
    client.set_session(Session::anonymous());
    let first = client.call_value("test.read", json!({"value":7}))?;
    let second = client.call_value("test.read", json!({"value":8}))?;
    assert_eq!(first["value"], 7);
    assert_ne!(first["request_id"], second["request_id"]);
    assert!(matches!(
        client.call_value("test.secret", json!({})),
        Err(Error::PermissionDenied(_))
    ));
    client.set_session(Session::new("operator", ["admin".into()]));
    assert_eq!(client.call_value("test.secret", json!({}))?, 42);
    assert!(
        matches!(client.call_value("test.failure",json!({})),Err(Error::Remote(s)) if s=="intentional")
    );
    assert!(matches!(
        client.call_value("missing", json!({})),
        Err(Error::Remote(_))
    ));
    client.set_timeout(Duration::from_millis(10));
    let call = client.call_async_value("test.wait", json!({}))?;
    assert!(matches!(call.wait(), Err(Error::Timeout)));
    let call = client.call_async_value("test.wait", json!({}))?;
    call.cancel();
    assert!(matches!(call.wait(), Err(Error::Closed)));
    runtime.shutdown();
    assert!(cancelled.load(Ordering::SeqCst) >= 1);
    Ok(())
}
#[test]
fn pool_drains_jobs_and_module_timers_are_serial() -> Result<()> {
    let pool = WorkerPool::new(2, 100)?;
    let count = Arc::new(AtomicUsize::new(0));
    for _ in 0..20 {
        let count = count.clone();
        pool.execute(move || {
            count.fetch_add(1, Ordering::SeqCst);
        })?;
    }
    pool.shutdown();
    assert_eq!(count.load(Ordering::SeqCst), 20);
    assert!(matches!(pool.execute(|| {}), Err(Error::Closed)));
    let mut module = Module::new("test-module", 20)?;
    let (send, recv) = mpsc::channel();
    let cancelled = module.timer(Duration::from_secs(60), None, || {
        panic!("cancelled timer fired")
    })?;
    module.cancel_timer(cancelled)?;
    let timer = module.timer(Duration::ZERO, Some(Duration::from_millis(2)), move || {
        send.send(std::thread::current().id()).unwrap();
    })?;
    let first = recv.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(first, recv.recv_timeout(Duration::from_secs(1)).unwrap());
    module.cancel_timer(timer)?;
    module.shutdown();
    assert!(matches!(module.post(|| {}), Err(Error::Closed)));
    Ok(())
}
#[test]
fn replay_clock_pause_rate_and_seek() -> Result<()> {
    let clock = vnx::time::Clock::new();
    clock.set_rate(0.0)?;
    clock.set(123);
    std::thread::sleep(Duration::from_millis(2));
    assert_eq!(clock.now_micros(), 123);
    assert!(clock.set_rate(f64::NAN).is_err());
    assert!(clock.set_rate(-1.0).is_err());
    clock.set_rate(2.0)?;
    std::thread::sleep(Duration::from_millis(2));
    assert!(clock.now_micros() >= 2123);
    Ok(())
}

#[test]
fn module_services_execute_on_the_module_thread() -> Result<()> {
    let runtime = Runtime::with_workers(4, 20)?;
    let mut module = Module::new("serialized-module", 20)?;
    let (send, recv) = mpsc::channel();
    module.post(move || {
        send.send(std::thread::current().id()).unwrap();
    })?;
    let module_thread = recv.recv_timeout(Duration::from_secs(1)).unwrap();
    let mut methods = Methods::new();
    methods.add("test.serial", None, move |_, _| {
        assert_eq!(std::thread::current().id(), module_thread);
        Ok(json!("done"))
    });
    module.register_service(&runtime, "serial", methods)?;
    let client = runtime.client("serial");
    let first = client.call_async_value("test.serial", json!({}))?;
    let second = client.call_async_value("test.serial", json!({}))?;
    assert_eq!(first.wait()?, "done");
    assert_eq!(second.wait()?, "done");
    module.shutdown();
    assert!(matches!(
        client.call_value("test.serial", json!({})),
        Err(Error::Closed)
    ));
    runtime.shutdown();
    Ok(())
}

#[test]
fn worker_tickets_barriers_waitsets_and_panics() -> Result<()> {
    let pool = WorkerPool::new(2, 20)?;
    let (gate, wait) = mpsc::channel();
    let first = pool.add_task(move || {
        wait.recv().unwrap();
    })?;
    let second = pool.add_task(|| {})?;
    second.wait()?;
    pool.sync_jobs(&[second.id()]);
    assert!(!first.is_finished());
    assert!(matches!(
        first.wait_timeout(Duration::ZERO),
        Err(Error::Timeout)
    ));
    gate.send(()).unwrap();
    pool.sync_until(first.id());
    first.wait()?;
    let panic = pool.add_task(|| panic!("intentional worker failure"))?;
    assert!(
        matches!(panic.wait(),Err(Error::Remote(message)) if message=="intentional worker failure")
    );
    pool.add_task(|| {})?.wait()?;
    pool.sync();
    assert_eq!(pool.get_num_pending_total(), 0);
    pool.shutdown();
    let inline = WorkerPool::new(0, 1)?;
    let count = Arc::new(AtomicUsize::new(0));
    let done = count.clone();
    let task = inline.add_task(move || {
        done.fetch_add(1, Ordering::SeqCst);
    })?;
    assert!(task.is_finished());
    assert_eq!(count.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn dropping_the_runtime_closes_remaining_subscribers() -> Result<()> {
    let runtime = Runtime::new()?;
    let subscription = runtime.subscribe("node", 1, OverflowPolicy::Reject, false)?;
    drop(runtime);
    assert!(matches!(subscription.recv(), Err(Error::Closed)));
    Ok(())
}
