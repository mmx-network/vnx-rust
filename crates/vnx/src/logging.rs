//! VNX terminal/file logging with the original message layout and visibility rules.
use crate::{
    config::Config,
    error::invalid,
    util::{get_sync_time_micros, get_wall_time_micros},
    Result,
};
use chrono::{DateTime, Local, TimeZone};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const ERROR: i32 = 1;
pub const WARN: i32 = 2;
pub const INFO: i32 = 3;
pub const DEBUG: i32 = 4;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogMsg {
    pub time: i64,
    pub level: i32,
    pub display_level: i32,
    pub process: String,
    pub module: String,
    pub message: String,
}
impl LogMsg {
    pub const ERROR: i32 = ERROR;
    pub const WARN: i32 = WARN;
    pub const INFO: i32 = INFO;
    pub const DEBUG: i32 = DEBUG;
    pub fn new(
        process: impl Into<String>,
        module: impl Into<String>,
        level: i32,
        message: impl Into<String>,
    ) -> Self {
        Self {
            time: get_sync_time_micros(),
            level,
            display_level: INFO,
            process: process.into(),
            module: module.into(),
            message: message.into(),
        }
    }
    /// Numeric levels intentionally have no colon/space, matching LogMsg.cpp.
    pub fn get_output(&self) -> String {
        let level = match self.level {
            ERROR => "ERROR: ".into(),
            WARN => "WARN: ".into(),
            INFO => "INFO: ".into(),
            DEBUG => "DEBUG: ".into(),
            other => format!("L{other}"),
        };
        format!("[{}] {level}{}", self.module, self.message)
    }
    pub fn terminal_output(&self, ansi: bool) -> String {
        match (ansi, self.level) {
            (true, ERROR) => format!("\x1b[31m{}\n\x1b[m", self.get_output()),
            (true, WARN) => format!("\x1b[33m{}\n\x1b[m", self.get_output()),
            _ => format!("{}\n", self.get_output()),
        }
    }
}
impl std::fmt::Display for LogMsg {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(&self.get_output())
    }
}

#[derive(Clone, Debug)]
pub struct LogOptions {
    pub log_level: i32,
    pub use_ansi_escape: bool,
    pub log_file_level: i32,
    pub log_history_size: usize,
    pub error_history_size: usize,
    pub log_file_name: String,
    pub log_file_path: PathBuf,
    pub log_file_name_suffix: String,
    pub log_file_entry_prefix: String,
    pub log_file_auto_restart: bool,
}
impl Default for LogOptions {
    fn default() -> Self {
        Self {
            log_level: INFO,
            use_ansi_escape: true,
            log_file_level: WARN,
            log_history_size: 10_000,
            error_history_size: 100,
            log_file_name: String::new(),
            log_file_path: PathBuf::new(),
            log_file_name_suffix: "_%Y_%m_%d.txt".into(),
            log_file_entry_prefix: "%Y-%m-%d %H:%M:%S ".into(),
            log_file_auto_restart: true,
        }
    }
}
impl LogOptions {
    /// Global log level plus the Process module's file/history settings.
    pub fn from_config(config: &Config, process_module: &str) -> Result<Self> {
        let mut options = Self::default();
        if let Some(value) = config.read("log_level")? {
            options.log_level = value;
        }
        if let Some(value) = config.read("use_ansi_escape")? {
            options.use_ansi_escape = value;
        }
        macro_rules! read {
            ($field:ident) => {
                if let Some(value) =
                    config.read(&format!("{}.{}", process_module, stringify!($field)))?
                {
                    options.$field = value;
                }
            };
        }
        read!(log_file_level);
        read!(log_history_size);
        read!(error_history_size);
        read!(log_file_name);
        read!(log_file_name_suffix);
        read!(log_file_entry_prefix);
        read!(log_file_auto_restart);
        if let Some(value) = config.read::<String>(&format!("{process_module}.log_file_path"))? {
            options.log_file_path = value.into();
        }
        Ok(options)
    }
}

/// Called for every published message, including those hidden in the terminal.
/// The callback runs outside internal locks and may publish to `vnx.log_out`.
pub type LogPublisher = Arc<dyn Fn(&LogMsg) + Send + Sync + 'static>;

struct State {
    options: LogOptions,
    terminal: Box<dyn Write + Send>,
    history: VecDeque<LogMsg>,
    errors: VecDeque<LogMsg>,
    grep: String,
    paused: usize,
    file: Option<File>,
    file_path: Option<PathBuf>,
    started: bool,
}
struct Inner {
    process: String,
    state: Mutex<State>,
    publisher: Mutex<Option<LogPublisher>>,
}
/// Thread-safe process logger. Clone freely for module threads.
#[derive(Clone)]
pub struct Logger {
    inner: Arc<Inner>,
}
impl Logger {
    pub fn new(process: impl Into<String>, options: LogOptions) -> Result<Self> {
        Self::with_writer(process, options, io::stderr())
    }
    pub fn with_writer(
        process: impl Into<String>,
        options: LogOptions,
        terminal: impl Write + Send + 'static,
    ) -> Result<Self> {
        let logger = Self {
            inner: Arc::new(Inner {
                process: process.into(),
                publisher: Mutex::new(None),
                state: Mutex::new(State {
                    options,
                    terminal: Box::new(terminal),
                    history: VecDeque::new(),
                    errors: VecDeque::new(),
                    grep: String::new(),
                    paused: 0,
                    file: None,
                    file_path: None,
                    started: false,
                }),
            }),
        };
        logger.poll_file()?;
        Ok(logger)
    }
    pub fn set_publisher(&self, publisher: LogPublisher) -> Result<()> {
        *self
            .inner
            .publisher
            .lock()
            .map_err(|_| invalid("log publisher lock poisoned"))? = Some(publisher);
        Ok(())
    }
    pub fn clear_publisher(&self) -> Result<()> {
        *self
            .inner
            .publisher
            .lock()
            .map_err(|_| invalid("log publisher lock poisoned"))? = None;
        Ok(())
    }
    pub fn module(&self, module: impl Into<String>) -> ModuleLogger {
        ModuleLogger {
            logger: self.clone(),
            module: module.into(),
            display_level: INFO,
        }
    }
    pub fn module_from_config(
        &self,
        module: impl Into<String>,
        config: &Config,
    ) -> Result<ModuleLogger> {
        let mut result = self.module(module);
        result.display_level = config
            .read(&format!("{}.vnx_log_level", result.module))?
            .unwrap_or(config.read("log_level")?.unwrap_or(INFO));
        Ok(result)
    }
    pub fn log(
        &self,
        module: impl Into<String>,
        level: i32,
        message: impl Into<String>,
    ) -> Result<()> {
        self.handle(LogMsg::new(
            self.inner.process.clone(),
            module,
            level,
            message,
        ))
    }
    /// Handles an externally received message without publishing it again.
    /// This is the Process subscription endpoint for log messages.
    pub fn receive(&self, message: LogMsg) -> Result<()> {
        self.receive_at(message, get_wall_time_micros())
    }
    pub fn handle(&self, message: LogMsg) -> Result<()> {
        let outcome = self.receive(message.clone());
        let publisher = self
            .inner
            .publisher
            .lock()
            .map_err(|_| invalid("log publisher lock poisoned"))?
            .clone();
        if let Some(publisher) = publisher {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| publisher(&message)))
                .is_err()
            {
                return Err(invalid("log publisher panicked"));
            }
        }
        outcome
    }
    pub fn receive_at(&self, message: LogMsg, wall_time_micros: i64) -> Result<()> {
        let now = local_time(wall_time_micros)?;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| invalid("logger lock poisoned"))?;
        if state.options.log_file_auto_restart {
            open_file(&mut state, &self.inner.process, now)?;
        }
        state.history.push_back(message.clone());
        let history_size = state.options.log_history_size;
        while state.history.len() > history_size {
            state.history.pop_front();
        }
        if message.level <= ERROR {
            state.errors.push_back(message.clone());
            let size = state.options.error_history_size;
            while state.errors.len() > size {
                state.errors.pop_front();
            }
        }
        let file_level = state.options.log_file_level;
        let prefix = format_date(now, &state.options.log_file_entry_prefix)?;
        if let Some(file) = &mut state.file {
            if message.level <= file_level {
                writeln!(file, "{prefix}{}", message.get_output())?;
                file.flush()?;
            }
        }
        let visible = if state.grep.is_empty() {
            message.level <= state.options.log_level.max(message.display_level)
        } else {
            message.level <= state.options.log_level && grep_matches(&message, &state.grep)
        };
        if state.paused == 0 && visible {
            output(&mut state, &message)?;
        }
        Ok(())
    }
    pub fn options(&self) -> Result<LogOptions> {
        Ok(self
            .inner
            .state
            .lock()
            .map_err(|_| invalid("logger lock poisoned"))?
            .options
            .clone())
    }
    pub fn set_level(&self, level: i32) -> Result<()> {
        self.inner
            .state
            .lock()
            .map_err(|_| invalid("logger lock poisoned"))?
            .options
            .log_level = level;
        Ok(())
    }
    pub fn pause_log(&self) -> Result<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| invalid("logger lock poisoned"))?;
        state.paused = state.paused.saturating_add(1);
        Ok(())
    }
    pub fn resume_log(&self) -> Result<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| invalid("logger lock poisoned"))?;
        state.paused = state.paused.saturating_sub(1);
        Ok(())
    }
    pub fn grep_log(&self, expr: impl Into<String>) -> Result<()> {
        self.inner
            .state
            .lock()
            .map_err(|_| invalid("logger lock poisoned"))?
            .grep = expr.into();
        Ok(())
    }
    pub fn ungrep_log(&self) -> Result<()> {
        self.grep_log("")
    }
    pub fn journal_log(&self, expr: &str) -> Result<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| invalid("logger lock poisoned"))?;
        for message in state
            .history
            .iter()
            .filter(|message| {
                message.level <= state.options.log_level && grep_matches(message, expr)
            })
            .cloned()
            .collect::<Vec<_>>()
        {
            output(&mut state, &message)?;
        }
        state.grep = expr.into();
        Ok(())
    }
    pub fn show_error_log(&self) -> Result<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| invalid("logger lock poisoned"))?;
        if state.errors.is_empty() {
            writeln!(state.terminal, "No errors!")?;
        }
        for message in state.errors.iter().cloned().collect::<Vec<_>>() {
            output(&mut state, &message)?;
        }
        Ok(())
    }
    pub fn history(&self) -> Result<Vec<LogMsg>> {
        Ok(self
            .inner
            .state
            .lock()
            .map_err(|_| invalid("logger lock poisoned"))?
            .history
            .iter()
            .cloned()
            .collect())
    }
    pub fn error_history(&self) -> Result<Vec<LogMsg>> {
        Ok(self
            .inner
            .state
            .lock()
            .map_err(|_| invalid("logger lock poisoned"))?
            .errors
            .iter()
            .cloned()
            .collect())
    }
    /// Call periodically (VNX uses one second) to rotate idle log files too.
    pub fn poll_file(&self) -> Result<()> {
        self.poll_file_at(get_wall_time_micros())
    }
    pub fn poll_file_at(&self, wall_time_micros: i64) -> Result<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| invalid("logger lock poisoned"))?;
        open_file(
            &mut state,
            &self.inner.process,
            local_time(wall_time_micros)?,
        )
    }
    pub fn shutdown(&self) -> Result<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| invalid("logger lock poisoned"))?;
        let prefix = format_date(Local::now(), &state.options.log_file_entry_prefix)?;
        if let Some(mut file) = state.file.take() {
            writeln!(file, "{prefix}[@] Shutdown {}", self.inner.process)?;
            file.flush()?;
        }
        state.file_path = None;
        Ok(())
    }
}
fn output(state: &mut State, message: &LogMsg) -> Result<()> {
    state.terminal.write_all(
        message
            .terminal_output(state.options.use_ansi_escape)
            .as_bytes(),
    )?;
    state.terminal.flush()?;
    Ok(())
}
fn grep_matches(message: &LogMsg, expression: &str) -> bool {
    expression.is_empty()
        || message.module.contains(expression)
        || message.message.contains(expression)
}
fn local_time(micros: i64) -> Result<DateTime<Local>> {
    Local
        .timestamp_opt(
            micros.div_euclid(1_000_000),
            (micros.rem_euclid(1_000_000) * 1000) as u32,
        )
        .single()
        .ok_or_else(|| invalid("log time outside supported range"))
}
fn format_date(now: DateTime<Local>, format: &str) -> Result<String> {
    // Chrono's Display panics on invalid strftime formats; reject them first.
    let items = chrono::format::StrftimeItems::new(format).collect::<Vec<_>>();
    if items
        .iter()
        .any(|item| matches!(item, chrono::format::Item::Error))
    {
        return Err(invalid("invalid log date format"));
    }
    Ok(now.format_with_items(items.iter()).to_string())
}
fn open_file(state: &mut State, process: &str, now: DateTime<Local>) -> Result<()> {
    if state.options.log_file_name.is_empty() {
        return Ok(());
    }
    let name = format!(
        "{}{}",
        state.options.log_file_name,
        format_date(now, &state.options.log_file_name_suffix)?
    );
    let path = state.options.log_file_path.join(name);
    if state.file_path.as_ref() == Some(&path) {
        return Ok(());
    }
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().append(true).create(true).open(&path)?;
    if !state.started {
        writeln!(
            file,
            "\n{}[@] Started {process}\n",
            format_date(now, &state.options.log_file_entry_prefix)?
        )?;
        file.flush()?;
    }
    state.file = Some(file);
    state.file_path = Some(path);
    state.started = true;
    Ok(())
}

#[derive(Clone)]
pub struct ModuleLogger {
    logger: Logger,
    module: String,
    display_level: i32,
}
impl ModuleLogger {
    pub fn display_level(mut self, level: i32) -> Self {
        self.display_level = level;
        self
    }
    pub fn log(&self, level: i32, message: impl Into<String>) -> Result<()> {
        let mut message = LogMsg::new(
            self.logger.inner.process.clone(),
            self.module.clone(),
            level,
            message,
        );
        message.display_level = self.display_level;
        self.logger.handle(message)
    }
    pub fn error(&self, message: impl Into<String>) -> Result<()> {
        self.log(ERROR, message)
    }
    pub fn warn(&self, message: impl Into<String>) -> Result<()> {
        self.log(WARN, message)
    }
    pub fn info(&self, message: impl Into<String>) -> Result<()> {
        self.log(INFO, message)
    }
    pub fn debug(&self, message: impl Into<String>) -> Result<()> {
        self.log(DEBUG, message)
    }
}
