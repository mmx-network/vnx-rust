//! Process bootstrap for VNX applications: configuration, logging, system topics,
//! process RPCs, module ownership and orderly shutdown.
//!
//! `Process` is also usable without global state. The free `init` / `do_run` /
//! `trigger_shutdown` / `wait` functions offer the familiar VNX application shape.
//! OS signal registration is deliberately left to the executable: call
//! `trigger_shutdown()` from its safe signal-handler integration.
use crate::{
    auth::AuthenticationServer,
    config::Config,
    error::invalid,
    logging::{LogOptions, Logger, ModuleLogger},
    runtime::{Methods, Module, OverflowPolicy, RequestContext, Runtime},
    util, Error, Result,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const DIAGNOSTIC_LIMIT: usize = 100;
#[derive(Clone)]
pub struct ProcessOptions {
    pub config: Config,
    pub aliases: BTreeMap<String, String>,
    pub config_paths: Vec<PathBuf>,
    pub workers: usize,
    pub queue_capacity: usize,
    /// VNX startup config listing. Off by default to keep application secrets out
    /// of terminals; when enabled only unprotected leaf values are printed.
    pub print_config: bool,
}
impl Default for ProcessOptions {
    fn default() -> Self {
        Self {
            config: Config::new(),
            aliases: BTreeMap::new(),
            config_paths: Vec::new(),
            workers: 2,
            queue_capacity: 1024,
            print_config: false,
        }
    }
}
struct Control {
    running: AtomicBool,
    mutex: Mutex<()>,
    changed: Condvar,
}
impl Control {
    fn stop(&self) {
        let _guard = self.mutex.lock().unwrap_or_else(|error| error.into_inner());
        self.running.store(false, Ordering::Release);
        self.changed.notify_all();
    }
}
struct Inner {
    name: String,
    id: u64,
    config: Config,
    positional: Vec<String>,
    runtime: Runtime,
    auth: AuthenticationServer,
    logger: Logger,
    control: Arc<Control>,
    modules: Mutex<Vec<Module>>,
    monitor: Mutex<Option<JoinHandle<()>>>,
    shutting_down: AtomicBool,
    stopped: AtomicBool,
    diagnostics: Arc<Mutex<VecDeque<String>>>,
}
impl Inner {
    fn finish(&self) -> Result<()> {
        self.control.stop();
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let mut modules = std::mem::take(
            &mut *self
                .modules
                .lock()
                .map_err(|_| invalid("process module lock poisoned"))?,
        );
        for module in &mut modules {
            module.shutdown();
        }
        self.runtime.shutdown();
        let _ = self.auth.revoke_all();
        if let Some(monitor) = self
            .monitor
            .lock()
            .map_err(|_| invalid("process monitor lock poisoned"))?
            .take()
        {
            if monitor.thread().id() != thread::current().id() {
                let _ = monitor.join();
            }
        }
        let clear = self.logger.clear_publisher();
        let flushed = self.logger.shutdown();
        self.stopped.store(true, Ordering::Release);
        self.control.changed.notify_all();
        clear.and(flushed)
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
#[derive(Clone)]
pub struct Process {
    inner: Arc<Inner>,
}
impl Process {
    pub fn new(name: impl Into<String>, args: &[String], options: ProcessOptions) -> Result<Self> {
        Self::with_writer(name, args, options, io::stderr())
    }
    pub fn with_writer(
        name: impl Into<String>,
        args: &[String],
        options: ProcessOptions,
        mut terminal: impl Write + Send + 'static,
    ) -> Result<Self> {
        let name = name.into();
        if name.is_empty() {
            return Err(invalid("empty process name"));
        }
        let config = options.config;
        for path in &options.config_paths {
            config.load_tree(path)?;
        }
        if !options.config_paths.is_empty() {
            config.set("config", json!(options.config_paths))?;
        }
        let positional = config.apply_args(args, &options.aliases)?;
        let help = config.get("help")?.is_some();
        if help {
            writeln!(terminal,"Usage: {name} [-h | --help] [-c | --config <directory>] [-d | --debug] [--key <value>]")?;
        }
        let log_options = LogOptions::from_config(&config, "Process")?;
        if options.print_config && log_options.log_level >= crate::logging::INFO {
            for (key, value) in config.entries(true)? {
                writeln!(terminal, "{key} = {value}")?;
            }
        }
        let logger = Logger::with_writer(name.clone(), log_options, terminal)?;
        let auth = AuthenticationServer::from_config(&config)?;
        let runtime = Runtime::with_workers(options.workers, options.queue_capacity)?;
        let subscription = runtime.subscribe("vnx", 4096, OverflowPolicy::DropOldest, false)?;
        let control = Arc::new(Control {
            running: AtomicBool::new(!help),
            mutex: Mutex::new(()),
            changed: Condvar::new(),
        });
        let diagnostics = Arc::new(Mutex::new(VecDeque::new()));
        let id = util::random_nonzero_u64()?;
        let process = Self {
            inner: Arc::new(Inner {
                name,
                id,
                config,
                positional,
                runtime: runtime.clone(),
                auth,
                logger: logger.clone(),
                control: control.clone(),
                modules: Mutex::new(Vec::new()),
                monitor: Mutex::new(None),
                shutting_down: AtomicBool::new(false),
                stopped: AtomicBool::new(false),
                diagnostics: diagnostics.clone(),
            }),
        };
        let publish_diagnostics = diagnostics.clone();
        logger.set_publisher(Arc::new(move |message| {
            // LogMsg has no fallible custom serialization. The private origin
            // marker suppresses the local subscription echo without dropping
            // logs from another process that happens to use the same name.
            let outcome = (|| -> Result<()> {
                let mut value = serde_json::to_value(message)?;
                value["__type"] = json!("vnx.LogMsg");
                value["__vnx_origin"] = json!(id);
                runtime.publish("vnx.log_out", value, false)
            })();
            if let Err(error) = outcome {
                // A failed optional subscriber must not abort an MMX operation
                // or recursively log through this same callback.
                if !matches!(error, Error::Closed) {
                    diagnostic(
                        &publish_diagnostics,
                        format!("log publication failed: {error}"),
                    );
                }
            }
        }))?;
        process.register_service()?;
        let monitor = thread::Builder::new()
            .name("vnx-process".into())
            .spawn(move || {
                let mut next_rotation = Instant::now() + Duration::from_secs(1);
                loop {
                    match subscription.recv_timeout(Duration::from_millis(100)) {
                        Ok(sample) => {
                            let outcome = match sample.topic.as_str() {
                                "vnx.log_out"
                                    if sample.value["__vnx_origin"].as_u64() != Some(id) =>
                                {
                                    serde_json::from_value(sample.value)
                                        .map_err(Error::from)
                                        .and_then(|message| logger.receive(message))
                                }
                                "vnx.time_sync" => {
                                    serde_json::from_value::<util::TimeSync>(sample.value)
                                        .map_err(Error::from)
                                        .map(|sync| {
                                            if (util::get_wall_time_micros() as i128
                                                - sync.wall_time as i128)
                                                .abs()
                                                < 1_000_000
                                            {
                                                util::set_time_sync(sync);
                                            }
                                        })
                                }
                                "vnx.time_control" => {
                                    serde_json::from_value::<util::TimeControl>(sample.value)
                                        .map_err(Error::from)
                                        .and_then(|value| {
                                            if (util::get_sync_time_micros() as i128
                                                - value.time as i128)
                                                .abs()
                                                < 1_000_000
                                            {
                                                util::set_time_control(value)?;
                                            }
                                            Ok(())
                                        })
                                }
                                _ => Ok(()),
                            };
                            if let Err(error) = outcome {
                                diagnostic(&diagnostics, format!("system topic rejected: {error}"));
                            }
                        }
                        Err(Error::Timeout) => {
                            if !control.running.load(Ordering::Acquire) {
                                break;
                            }
                        }
                        Err(Error::Closed) => break,
                        Err(error) => {
                            diagnostic(
                                &diagnostics,
                                format!("system subscription failed: {error}"),
                            );
                            break;
                        }
                    }
                    if Instant::now() >= next_rotation {
                        if let Err(error) = logger.poll_file() {
                            diagnostic(&diagnostics, format!("log file rotation failed: {error}"));
                        }
                        next_rotation = Instant::now() + Duration::from_secs(1);
                    }
                }
            })?;
        *process
            .inner
            .monitor
            .lock()
            .map_err(|_| invalid("process monitor lock poisoned"))? = Some(monitor);
        Ok(process)
    }
    pub fn name(&self) -> &str {
        &self.inner.name
    }
    pub fn id(&self) -> u64 {
        self.inner.id
    }
    pub fn config(&self) -> &Config {
        &self.inner.config
    }
    pub fn runtime(&self) -> &Runtime {
        &self.inner.runtime
    }
    pub fn auth(&self) -> &AuthenticationServer {
        &self.inner.auth
    }
    /// Creates a native network listener policy using this process's authority.
    /// Exported services and topics remain explicit allow-lists. Anonymous peers
    /// receive only the VNX transport-control role until they log in.
    pub fn network_options(
        &self,
        services: impl IntoIterator<Item = String>,
    ) -> Result<crate::net::ServerOptions> {
        self.network_options_with_role(services, "UNAUTHENTICATED")
    }
    pub fn network_options_with_role(
        &self,
        services: impl IntoIterator<Item = String>,
        default_access: &str,
    ) -> Result<crate::net::ServerOptions> {
        let authenticate = self.inner.auth.clone();
        let validate = self.inner.auth.clone();
        Ok(crate::net::ServerOptions {
            process_id: self.id(),
            services: services.into_iter().collect(),
            session: self.inner.auth.login_anonymous(default_access)?,
            authenticator: Some(Arc::new(move |name, hash| {
                authenticate.authenticate_hash(name, hash)
            })),
            session_validator: Some(Arc::new(move |id| validate.get_session(id))),
            ..Default::default()
        })
    }
    pub fn logger(&self) -> &Logger {
        &self.inner.logger
    }
    pub fn positional_args(&self) -> &[String] {
        &self.inner.positional
    }
    pub fn module_logger(&self, name: impl Into<String>) -> Result<ModuleLogger> {
        self.inner
            .logger
            .module_from_config(name, &self.inner.config)
    }
    /// Moves an event-loop module into process lifecycle ownership.
    pub fn own_module(&self, module: Module) -> Result<()> {
        let mut modules = self
            .inner
            .modules
            .lock()
            .map_err(|_| invalid("process module lock poisoned"))?;
        if !self.do_run() {
            return Err(Error::Closed);
        }
        modules.push(module);
        Ok(())
    }
    pub fn start_module(&self, name: impl Into<String>, capacity: usize) -> Result<()> {
        self.own_module(Module::new(name, capacity)?)
    }
    pub fn do_run(&self) -> bool {
        self.inner.control.running.load(Ordering::Acquire) && !self.inner.runtime.is_shutdown()
    }
    pub fn trigger_shutdown(&self) {
        self.inner.control.stop();
    }
    /// Waits for a shutdown request, then drains modules and closes services.
    pub fn wait(&self) -> Result<()> {
        let mut guard = self
            .inner
            .control
            .mutex
            .lock()
            .map_err(|_| invalid("process shutdown lock poisoned"))?;
        while self.do_run() {
            guard = self
                .inner
                .control
                .changed
                .wait_timeout(guard, Duration::from_millis(100))
                .map_err(|_| invalid("process shutdown lock poisoned"))?
                .0;
        }
        drop(guard);
        self.shutdown()
    }
    /// Returns false if no shutdown was requested before the timeout.
    pub fn wait_timeout(&self, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| invalid("shutdown timeout overflow"))?;
        let mut guard = self
            .inner
            .control
            .mutex
            .lock()
            .map_err(|_| invalid("process shutdown lock poisoned"))?;
        while self.do_run() {
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            guard = self
                .inner
                .control
                .changed
                .wait_timeout(guard, (deadline - now).min(Duration::from_millis(100)))
                .map_err(|_| invalid("process shutdown lock poisoned"))?
                .0;
        }
        drop(guard);
        self.shutdown()?;
        Ok(true)
    }
    pub fn shutdown(&self) -> Result<()> {
        self.inner.finish()
    }
    pub fn is_stopped(&self) -> bool {
        self.inner.stopped.load(Ordering::Acquire)
    }
    /// Bounded diagnostics from optional log publication, invalid system samples
    /// or file rotation; reporting them never recursively invokes the logger.
    pub fn diagnostics(&self) -> Vec<String> {
        self.inner
            .diagnostics
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .cloned()
            .collect()
    }
    pub fn reload_config(&self) -> Result<()> {
        if let Some(paths) = self.inner.config.read::<Vec<String>>("config")? {
            for path in paths {
                self.inner.config.load_tree(path)?;
            }
        }
        if let Some(level) = self.inner.config.read("log_level")? {
            self.inner.logger.set_level(level)?;
        }
        Ok(())
    }
    fn register_service(&self) -> Result<()> {
        let mut methods = Methods::new();
        for (name, permission) in [
            ("get_name", Some("VIEW")),
            ("get_sync_time", Some("TIME_SYNC")),
            ("get_process_info", Some("VIEW")),
            ("get_global_config", Some("READ_CONFIG")),
            ("set_global_config", Some("WRITE_CONFIG")),
            ("reload_config", Some("WRITE_CONFIG")),
            ("pause_log", Some("LOCAL")),
            ("resume_log", Some("LOCAL")),
            ("set_debug", Some("LOCAL")),
            ("show_error_log", Some("LOCAL")),
            ("grep_log", Some("LOCAL")),
            ("journal_log", Some("LOCAL")),
            ("ungrep_log", Some("LOCAL")),
            ("trigger_shutdown", Some("SHUTDOWN")),
        ] {
            for alias in [name.to_owned(), format!("vnx.Process.{name}")] {
                let weak = Arc::downgrade(&self.inner);
                let qualified = alias.starts_with("vnx.Process.");
                let return_type = format!("{alias}.return");
                methods.add(
                    alias,
                    permission.map(str::to_owned),
                    move |args, context| {
                        let value = process_call(&weak, name, args, context)?;
                        if qualified {
                            let mut result = json!({"__type": return_type});
                            if matches!(
                                name,
                                "get_name"
                                    | "get_sync_time"
                                    | "get_process_info"
                                    | "get_global_config"
                            ) {
                                result["_ret_0"] = value;
                            }
                            Ok(result)
                        } else {
                            Ok(value)
                        }
                    },
                );
            }
        }
        self.inner.runtime.register("vnx.process", methods)
    }
}
fn diagnostic(output: &Mutex<VecDeque<String>>, message: String) {
    let mut output = output.lock().unwrap_or_else(|error| error.into_inner());
    output.push_back(message);
    while output.len() > DIAGNOSTIC_LIMIT {
        output.pop_front();
    }
}
fn arg_string<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .ok_or_else(|| invalid(format!("argument '{key}' must be a string")))
}
fn process_call(
    weak: &Weak<Inner>,
    method: &str,
    args: Value,
    context: &RequestContext,
) -> Result<Value> {
    let process = Process {
        inner: weak.upgrade().ok_or(Error::Closed)?,
    };
    match method {
        "get_name" => Ok(json!(process.name())),
        "get_sync_time" => {
            let mut time = util::get_time_sync();
            time.time = time.get_time_micros();
            time.wall_time = util::get_wall_time_micros();
            time.order = time.order.saturating_add(1);
            Ok(serde_json::to_value(time)?)
        }
        "get_process_info" => Ok(
            json!({"__type":"vnx.ProcessInfo","name":process.name(),"id":process.id(),"time":util::get_time_micros(),"topics":[],"modules":[]}),
        ),
        "get_global_config" => {
            let key = arg_string(&args, "key")?;
            if context.session.has_permission("PROTECTED_CONFIG") {
                Ok(process.config().get(key)?.unwrap_or(Value::Null))
            } else {
                Ok(process.config().get_public(key)?.unwrap_or(Value::Null))
            }
        }
        "set_global_config" => {
            let key = arg_string(&args, "key")?;
            let value = args
                .get("value")
                .ok_or_else(|| invalid("missing config value"))?
                .clone();
            if context.session.has_permission("PROTECTED_CONFIG") {
                process.config().set(key, value.clone())?;
            } else {
                process.config().set_public(key, value.clone())?;
            }
            if key == "log_level" {
                process.logger().set_level(serde_json::from_value(value)?)?;
            }
            Ok(Value::Null)
        }
        "reload_config" => {
            process.reload_config()?;
            Ok(Value::Null)
        }
        "pause_log" => {
            process.logger().pause_log()?;
            Ok(Value::Null)
        }
        "resume_log" => {
            process.logger().resume_log()?;
            Ok(Value::Null)
        }
        "set_debug" => {
            process.logger().set_level(serde_json::from_value(
                args.get("level")
                    .cloned()
                    .ok_or_else(|| invalid("missing debug level"))?,
            )?)?;
            Ok(Value::Null)
        }
        "show_error_log" => {
            process.logger().show_error_log()?;
            Ok(Value::Null)
        }
        "grep_log" => {
            process.logger().grep_log(arg_string(&args, "expr")?)?;
            Ok(Value::Null)
        }
        "journal_log" => {
            process.logger().journal_log(arg_string(&args, "expr")?)?;
            Ok(Value::Null)
        }
        "ungrep_log" => {
            process.logger().ungrep_log()?;
            Ok(Value::Null)
        }
        "trigger_shutdown" => {
            process.trigger_shutdown();
            Ok(Value::Null)
        }
        _ => Err(Error::Remote(format!("no such process method: {method}"))),
    }
}

static GLOBAL: OnceLock<Mutex<Option<Process>>> = OnceLock::new();
fn global() -> &'static Mutex<Option<Process>> {
    GLOBAL.get_or_init(|| Mutex::new(None))
}
pub fn init(name: impl Into<String>, args: &[String]) -> Result<Process> {
    init_with_options(
        name,
        args,
        ProcessOptions {
            config: crate::config::global_config().clone(),
            ..ProcessOptions::default()
        },
    )
}
pub fn init_with_options(
    name: impl Into<String>,
    args: &[String],
    options: ProcessOptions,
) -> Result<Process> {
    let mut current = global()
        .lock()
        .map_err(|_| invalid("global process lock poisoned"))?;
    if current.as_ref().is_some_and(Process::do_run) {
        return Err(invalid("process is already initialized"));
    }
    let process = Process::new(name, args, options)?;
    *current = Some(process.clone());
    Ok(process)
}
pub fn current_process() -> Option<Process> {
    global().lock().ok()?.clone()
}
pub fn do_run() -> bool {
    current_process().is_some_and(|process| process.do_run())
}
pub fn trigger_shutdown() -> Result<()> {
    current_process().ok_or(Error::Closed)?.trigger_shutdown();
    Ok(())
}
pub fn wait() -> Result<()> {
    let process = current_process().ok_or(Error::Closed)?;
    let outcome = process.wait();
    let mut current = global()
        .lock()
        .map_err(|_| invalid("global process lock poisoned"))?;
    if current
        .as_ref()
        .is_some_and(|value| value.id() == process.id())
    {
        *current = None;
    }
    outcome
}

/// Gets the authentication authority created by global init.
pub fn get_auth_server() -> Result<AuthenticationServer> {
    Ok(current_process().ok_or(Error::Closed)?.auth().clone())
}
