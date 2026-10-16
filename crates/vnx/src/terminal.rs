//! Line-oriented VNX process console. Applications can supply their own line editor.
use crate::{config, error::invalid, process::Process, Registry, Result};
use serde_json::{Map, Value};
use std::io::{BufRead, Write};
pub const HELP:&str="Help: quit | debug [level] | errors | topic [expr] | module [expr] | exec/call <module>[.method [args...]] | config [expr] | grep <expr> | journal [expr]";
pub struct Terminal {
    process: Process,
    registry: Registry,
}
impl Terminal {
    pub fn new(process: Process, registry: Registry) -> Self {
        Self { process, registry }
    }
    /// Execute a console command. Log/history output uses the process logger's
    /// writer so filtering and ANSI policy stay identical to ordinary log output.
    pub fn execute(&self, line: &str) -> Result<String> {
        let (command, tail) = line
            .trim()
            .split_once(char::is_whitespace)
            .unwrap_or((line.trim(), ""));
        let tail = tail.trim();
        let logger = self.process.logger();
        match command {
            "help" | "h" | "?" => Ok(format!("{HELP}\n")),
            "" => {
                logger.ungrep_log()?;
                Ok(String::new())
            }
            "quit" | "q" => {
                self.process.trigger_shutdown();
                Ok(String::new())
            }
            "debug" => {
                let level = if tail.is_empty() {
                    4
                } else {
                    tail.parse()
                        .map_err(|_| invalid("debug level must be an integer"))?
                };
                logger.set_level(level)?;
                Ok(format!("[Terminal] INFO: process.set_debug({level})\n"))
            }
            "grep" | "g" => {
                logger.grep_log(tail)?;
                Ok(String::new())
            }
            "journal" => {
                logger.journal_log(tail)?;
                Ok(String::new())
            }
            "errors" | "e" => {
                logger.show_error_log()?;
                Ok(String::new())
            }
            "pause" => {
                logger.pause_log()?;
                Ok(String::new())
            }
            "resume" => {
                logger.resume_log()?;
                Ok(String::new())
            }
            "module" | "m" => Ok(self
                .process
                .runtime()
                .service_names()
                .into_iter()
                .filter(|name| name.contains(tail))
                .map(|name| format!("{name}\n"))
                .collect()),
            "topic" | "t" => Ok(self
                .process
                .runtime()
                .topic_names()
                .into_iter()
                .filter(|name| name.contains(tail))
                .map(|name| format!("{name}\n"))
                .collect()),
            "config" => {
                let mut out = String::new();
                for (key, value) in self.process.config().entries(true)? {
                    if key.contains(tail) {
                        out.push_str(&format!("{key} = {value}\n"));
                    }
                }
                Ok(out)
            }
            "call" | "exec" => self.call(tail),
            _ => Err(invalid(format!("Invalid command: {command}"))),
        }
    }
    fn call(&self, tail: &str) -> Result<String> {
        let (target, args) = tail.split_once(char::is_whitespace).unwrap_or((tail, ""));
        let services = self.process.runtime().service_names();
        let registered = services
            .iter()
            .filter(|name| {
                target == name.as_str()
                    || target
                        .strip_prefix(name.as_str())
                        .is_some_and(|s| s.starts_with('.'))
            })
            .max_by_key(|name| name.len());
        let (service, method) = if let Some(name) = registered {
            (name.as_str(), target[name.len()..].trim_start_matches('.'))
        } else {
            target.split_once('.').unwrap_or((target, ""))
        };
        if service.is_empty() {
            return Err(invalid("missing module name"));
        }
        let short_name = service.rsplit('.').next().unwrap_or(service);
        let methods: Vec<_> = self
            .registry
            .iter()
            .filter(|t| {
                t.is_method
                    && (t.name.rsplit_once('.').is_some_and(|(owner, _)| {
                        owner
                            .rsplit('.')
                            .next()
                            .is_some_and(|name| name.eq_ignore_ascii_case(short_name))
                    }) || t.name == method)
            })
            .collect();
        if method.is_empty() {
            return Ok(methods.iter().map(|t| format!("{}\n", t.name)).collect());
        }
        let named: Vec<_> = methods
            .iter()
            .filter(|t| t.name == method || t.name.ends_with(&format!(".{method}")))
            .collect();
        if named.len() != 1 {
            return Err(invalid(format!("unknown or ambiguous method {target}")));
        }
        let schema = named[0];
        let mut object = Map::new();
        if args.trim().starts_with('{') {
            object = config::parse_json(args)?
                .as_object()
                .ok_or_else(|| invalid("expected named argument object"))?
                .clone();
        } else {
            let args = split_arguments(args)?;
            if args.len() > schema.fields.len() {
                return Err(invalid("too many method arguments"));
            }
            for (field, arg) in schema.fields.iter().zip(args) {
                object.insert(field.name.clone(), config::parse_json(&arg)?);
            }
        }
        for field in &schema.fields {
            if !object.contains_key(&field.name) && !field.value.is_empty() {
                object.insert(field.name.clone(), config::parse_json(&field.value)?);
            }
        }
        let value = self
            .process
            .runtime()
            .client(service)
            .call_value(&schema.name, Value::Object(object))?;
        Ok(format!(
            "{}\n",
            serde_json::to_string_pretty(value.get("_ret_0").unwrap_or(&value))?
        ))
    }
    /// Reads complete lines; closing stdin returns without shutting down modules.
    /// Terminal ownership remains with the application, including signal handling.
    pub fn run(&self, mut input: impl BufRead, mut output: impl Write) -> Result<()> {
        writeln!(output, "{HELP}")?;
        output.flush()?;
        let mut line = String::new();
        while self.process.do_run() {
            line.clear();
            if input.read_line(&mut line)? == 0 {
                break;
            }
            match self.execute(&line) {
                Ok(text) => output.write_all(text.as_bytes())?,
                Err(e) => writeln!(output, "[Terminal] WARN: {e}")?,
            }
            output.flush()?;
        }
        Ok(())
    }
}
fn split_arguments(text: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut quote = false;
    let mut escaped = false;
    let mut depth = 0usize;
    for (i, c) in text.char_indices() {
        if quote {
            if escaped {
                escaped = false
            } else if c == '\\' {
                escaped = true
            } else if c == '"' {
                quote = false
            }
            continue;
        }
        match c {
            '"' => quote = true,
            '[' | '{' => {
                depth = depth
                    .checked_add(1)
                    .ok_or_else(|| invalid("argument nesting"))?
            }
            ']' | '}' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| invalid("argument delimiter"))?
            }
            _ => {}
        }
        if depth > 100 {
            return Err(invalid("argument nesting"));
        }
        if c.is_whitespace() && depth == 0 {
            if start < i {
                out.push(text[start..i].to_owned());
            }
            start = i + c.len_utf8();
        }
    }
    if quote || depth != 0 {
        return Err(invalid("unterminated argument"));
    }
    if start < text.len() {
        out.push(text[start..].into());
    }
    Ok(out)
}
