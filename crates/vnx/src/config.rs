//! VNX configuration trees, dotted namespaces, command-line overrides and access protection.
//!
//! Directory trees are layered in load order. A `parent` file is resolved relative
//! to its directory and loaded first. Objects flatten into dotted keys; a trailing
//! `+` appends to an array. Files use the same namespaces and extensions as VNX.
use crate::{error::invalid, Error, Result};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DEPTH: usize = 128;

#[derive(Default)]
struct State {
    values: BTreeMap<String, Value>,
    protection: BTreeSet<String>,
}

/// Clones share one configuration. Protection is enforced by the public/RPC
/// accessors; trusted local module accessors can read protected namespaces.
#[derive(Clone, Default)]
pub struct Config {
    state: Arc<RwLock<State>>,
}
impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Result<Option<Value>> {
        self.get_impl(key, false)
    }
    pub fn get_public(&self, key: &str) -> Result<Option<Value>> {
        self.get_impl(key, true)
    }
    fn get_impl(&self, key: &str, public: bool) -> Result<Option<Value>> {
        let state = self
            .state
            .read()
            .map_err(|_| invalid("configuration lock poisoned"))?;
        if public && is_protected(&state, key) {
            return Err(Error::PermissionDenied(format!(
                "config is protected: '{key}'"
            )));
        }
        if let Some(value) = state.values.get(key) {
            return Ok(Some(value.clone()));
        }
        let object = object_at(&state, key, public);
        Ok((!object.is_empty()).then_some(Value::Object(object)))
    }
    pub fn read<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        self.get(key)?
            .map(serde_json::from_value)
            .transpose()
            .map_err(Into::into)
    }
    pub fn get_object(&self, namespace: &str) -> Result<Map<String, Value>> {
        let state = self
            .state
            .read()
            .map_err(|_| invalid("configuration lock poisoned"))?;
        Ok(object_at(&state, namespace, false))
    }
    pub fn get_object_public(&self, namespace: &str) -> Result<Map<String, Value>> {
        let state = self
            .state
            .read()
            .map_err(|_| invalid("configuration lock poisoned"))?;
        if is_protected(&state, namespace) {
            return Err(Error::PermissionDenied(format!(
                "config is protected: '{namespace}'"
            )));
        }
        Ok(object_at(&state, namespace, true))
    }
    pub fn set(&self, key: &str, value: Value) -> Result<()> {
        self.set_impl(key, value, false)
    }
    pub fn set_public(&self, key: &str, value: Value) -> Result<()> {
        self.set_impl(key, value, true)
    }
    fn set_impl(&self, key: &str, value: Value, public: bool) -> Result<()> {
        let mut state = self
            .state
            .write()
            .map_err(|_| invalid("configuration lock poisoned"))?;
        let mut pending = Vec::new();
        flatten(key, value, &mut pending, 0)?;
        // Check the whole operation before changing anything; nested protected
        // keys cannot cause a partially applied public write.
        for (key, _) in &pending {
            if public && is_protected(&state, key.trim_end_matches('+')) {
                return Err(Error::PermissionDenied(format!(
                    "config is protected: '{key}'"
                )));
            }
        }
        for (key, value) in pending {
            if let Some(key) = key.strip_suffix('+') {
                let existing = state
                    .values
                    .entry(key.to_owned())
                    .or_insert_with(|| Value::Array(vec![]));
                let array = existing
                    .as_array_mut()
                    .ok_or_else(|| invalid(format!("cannot append to non-array config '{key}'")))?;
                match value {
                    Value::Array(more) => array.extend(more),
                    value => array.push(value),
                }
            } else {
                state.values.insert(key, value);
            }
        }
        Ok(())
    }
    pub fn set_text(&self, key: &str, text: &str) -> Result<()> {
        self.set(key, parse_json(text)?)
    }
    pub fn protect(&self, key: impl Into<String>) -> Result<()> {
        self.state
            .write()
            .map_err(|_| invalid("configuration lock poisoned"))?
            .protection
            .insert(key.into());
        Ok(())
    }
    pub fn is_protected(&self, key: &str) -> Result<bool> {
        let state = self
            .state
            .read()
            .map_err(|_| invalid("configuration lock poisoned"))?;
        Ok(is_protected(&state, key))
    }
    pub fn protection(&self) -> Result<BTreeSet<String>> {
        Ok(self
            .state
            .read()
            .map_err(|_| invalid("configuration lock poisoned"))?
            .protection
            .clone())
    }
    /// Returns only explicitly stored leaf keys, in lexical order.
    pub fn entries(&self, public: bool) -> Result<BTreeMap<String, Value>> {
        let state = self
            .state
            .read()
            .map_err(|_| invalid("configuration lock poisoned"))?;
        Ok(state
            .values
            .iter()
            .filter(|(key, _)| !public || !is_protected(&state, key))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }
    pub fn load_tree(&self, root: impl AsRef<Path>) -> Result<()> {
        if root.as_ref().as_os_str().is_empty() {
            return Ok(());
        }
        self.load_root(root.as_ref(), &mut BTreeSet::new(), 0)
    }
    fn load_root(&self, root: &Path, active: &mut BTreeSet<PathBuf>, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(Error::Limit("config parent recursion".into()));
        }
        let canonical = fs::canonicalize(root)?;
        if !active.insert(canonical.clone()) {
            return Err(invalid(format!(
                "config parent cycle at {}",
                root.display()
            )));
        }
        let parent_file = root.join("parent");
        match read_text_bounded(&parent_file) {
            Ok(text) => {
                let value = if text.trim().is_empty() {
                    Value::String(String::new())
                } else {
                    parse_json(&text)?
                };
                let parent = value
                    .as_str()
                    .ok_or_else(|| invalid("config parent must be a path string"))?;
                if !parent.is_empty() && parent != "." {
                    let path = Path::new(parent);
                    self.load_root(
                        &if path.is_absolute() {
                            path.to_path_buf()
                        } else {
                            root.join(path)
                        },
                        active,
                        depth + 1,
                    )?;
                }
            }
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.load_directory(root, "", &mut BTreeSet::new(), 0)?;
        active.remove(&canonical);
        Ok(())
    }
    fn load_directory(
        &self,
        root: &Path,
        prefix: &str,
        active: &mut BTreeSet<PathBuf>,
        depth: usize,
    ) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(Error::Limit("config directory recursion".into()));
        }
        let canonical = fs::canonicalize(root)?;
        if !active.insert(canonical.clone()) {
            return Err(invalid("config directory symlink cycle"));
        }
        let mut entries = fs::read_dir(root)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in &entries {
            if entry.path().is_dir() {
                let name = entry.file_name().to_string_lossy().into_owned();
                self.load_directory(
                    &entry.path(),
                    &format!("{prefix}{name}."),
                    active,
                    depth + 1,
                )?;
            }
        }
        for entry in entries {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name.ends_with('.') {
                continue;
            }
            let extension = path.extension().and_then(|x| x.to_str()).unwrap_or("");
            if !["", "json", "jary", "lst", "txt"].contains(&extension) {
                continue;
            }
            let stem = if extension.is_empty() {
                name
            } else {
                path.file_stem()
                    .ok_or_else(|| invalid("missing config filename"))?
                    .to_string_lossy()
                    .into_owned()
            };
            let text = read_text_bounded(&path)?;
            let value = match extension {
                "txt" => Value::String(text),
                "lst" => Value::Array(
                    text.split_terminator('\n')
                        .map(|line| Value::String(line.to_owned()))
                        .collect(),
                ),
                "jary" => Value::Array(
                    text.lines()
                        .filter(|line| !line.trim().is_empty())
                        .map(parse_json)
                        .collect::<Result<Vec<_>>>()?,
                ),
                _ => {
                    if text.trim().is_empty() {
                        continue;
                    }
                    parse_json(&text)?
                }
            };
            self.set(&format!("{prefix}{stem}"), value)?;
        }
        active.remove(&canonical);
        Ok(())
    }
    /// Loads configuration directories and applies options in argument order,
    /// as VNX init does. Arguments exclude the executable name.
    pub fn apply_args(
        &self,
        args: &[String],
        aliases: &BTreeMap<String, String>,
    ) -> Result<Vec<String>> {
        let entries = parse_args(args, aliases)?;
        let mut positional = Vec::new();
        let mut paths = Vec::new();
        for (key, values) in entries {
            if key == "config" {
                for value in values {
                    let path = value
                        .as_str()
                        .ok_or_else(|| invalid("config argument must be a directory"))?;
                    self.load_tree(path)?;
                    paths.push(Value::String(path.to_owned()));
                }
            } else if key.is_empty() {
                for value in values {
                    let text = value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string());
                    self.set(&format!("${}", positional.len() + 1), value)?;
                    positional.push(text);
                }
            } else {
                self.set(
                    &key,
                    match values.len() {
                        0 => Value::Bool(true),
                        1 => values.into_iter().next().unwrap(),
                        _ => Value::Array(values),
                    },
                )?;
            }
        }
        if !paths.is_empty() {
            self.set("config", Value::Array(paths))?;
        }
        if self.get("debug")?.is_some_and(|value| {
            value == Value::Bool(true) || value.as_i64().is_some_and(|v| v != 0)
        }) {
            self.set("log_level", Value::from(4))?;
        }
        if let Some(Value::Array(keys)) = self.get("vnx.protected_config")? {
            for key in keys {
                self.protect(
                    key.as_str()
                        .ok_or_else(|| invalid("protected config keys must be strings"))?,
                )?;
            }
        }
        self.protect("vnx")?;
        Ok(positional)
    }
    pub fn write_json(&self, path: impl AsRef<Path>, namespace: &str) -> Result<()> {
        crate::util::write_json_file(path, &Value::Object(self.get_object(namespace)?))
    }
}

fn flatten(key: &str, value: Value, out: &mut Vec<(String, Value)>, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Limit("config object recursion".into()));
    }
    if let Value::Object(object) = value {
        for (field, value) in object {
            let separator = if key.is_empty() || key.ends_with('.') {
                ""
            } else {
                "."
            };
            flatten(&format!("{key}{separator}{field}"), value, out, depth + 1)?;
        }
    } else {
        out.push((key.to_owned(), value));
    }
    Ok(())
}
fn is_protected(state: &State, key: &str) -> bool {
    let mut current = key;
    loop {
        if state.protection.contains(current) {
            return true;
        }
        let Some((parent, _)) = current.rsplit_once('.') else {
            return false;
        };
        current = parent;
    }
}
fn object_at(state: &State, namespace: &str, public: bool) -> Map<String, Value> {
    let prefix = if namespace.is_empty() {
        String::new()
    } else {
        format!("{namespace}.")
    };
    let mut result = Map::new();
    for (key, value) in &state.values {
        if public && is_protected(state, key) {
            continue;
        }
        if let Some(relative) = key.strip_prefix(&prefix) {
            if !relative.is_empty() {
                insert_object(&mut result, relative, value.clone());
            }
        }
    }
    result
}
fn insert_object(object: &mut Map<String, Value>, path: &str, value: Value) {
    if let Some((head, tail)) = path.split_once('.') {
        let child = object
            .entry(head)
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(child) = child.as_object_mut() {
            insert_object(child, tail, value);
        }
    } else {
        object.insert(path.to_owned(), value);
    }
}
fn read_text_bounded(path: &Path) -> Result<String> {
    let file = fs::File::open(path)?;
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(Error::Limit(format!("config file {}", path.display())));
    }
    String::from_utf8(bytes)
        .map_err(|_| invalid(format!("config is not UTF-8: {}", path.display())))
}

pub type ParsedArgs = Vec<(String, Vec<Value>)>;
/// Parses VNX options, including `-c`, `-d`, `-h`, dotted long options,
/// negative numbers and JSON values. `--key=value` is accepted as well.
pub fn parse_args(args: &[String], aliases: &BTreeMap<String, String>) -> Result<ParsedArgs> {
    let mut options = aliases.clone();
    for (short, long) in [("c", "config"), ("d", "debug"), ("h", "help")] {
        options.insert(short.into(), long.into());
    }
    let mut result = Vec::new();
    let mut key = String::new();
    let mut values = Vec::new();
    for arg in args {
        if arg == "--" {
            break;
        }
        let option = if let Some(long) = arg.strip_prefix("--") {
            Some(long)
        } else {
            arg.strip_prefix('-').filter(|short| {
                short
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic())
            })
        };
        if let Some(option) = option {
            if !key.is_empty() || !values.is_empty() {
                result.push((key, std::mem::take(&mut values)));
            }
            let (name, inline) = option
                .split_once('=')
                .map_or((option, None), |(k, v)| (k, Some(v)));
            if name.is_empty() {
                return Err(invalid("empty command-line option"));
            }
            key = options
                .get(name)
                .filter(|_| name.len() == 1)
                .cloned()
                .unwrap_or_else(|| name.to_owned());
            if let Some(value) = inline {
                values.push(parse_json(value)?);
            }
        } else {
            values.push(
                if arg.chars().any(char::is_whitespace) && !arg.starts_with(['{', '[', '"']) {
                    Value::String(arg.clone())
                } else {
                    parse_json(arg)?
                },
            );
        }
    }
    if !key.is_empty() || !values.is_empty() {
        result.push((key, values));
    }
    Ok(result)
}

/// Parses VNX's JSON dialect: unquoted strings/keys, hexadecimal and positive
/// signed numbers, and trailing commas. JSON integers retain full precision.
/// Deliberately rejects comments (the C++ config parser does not support them).
pub fn parse_json(text: &str) -> Result<Value> {
    let mut parser = Parser {
        input: text.as_bytes(),
        offset: 0,
    };
    parser.whitespace();
    if parser.offset == parser.input.len() {
        return Ok(Value::Null);
    }
    let value = parser.value(0, true)?;
    parser.whitespace();
    if parser.offset != parser.input.len() {
        return Err(invalid("trailing JSON data"));
    }
    Ok(value)
}
struct Parser<'a> {
    input: &'a [u8],
    offset: usize,
}
impl Parser<'_> {
    fn whitespace(&mut self) {
        while self
            .input
            .get(self.offset)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.offset += 1;
        }
    }
    fn value(&mut self, depth: usize, top: bool) -> Result<Value> {
        if depth > MAX_DEPTH {
            return Err(Error::Limit("JSON recursion".into()));
        }
        self.whitespace();
        match self.input.get(self.offset).copied() {
            Some(b'"') => {
                let start = self.offset;
                self.offset += 1;
                loop {
                    match self.input.get(self.offset) {
                        Some(b'\\') => self.offset += 2,
                        Some(b'"') => {
                            self.offset += 1;
                            break;
                        }
                        Some(_) => self.offset += 1,
                        None => return Err(invalid("unterminated JSON string")),
                    }
                }
                serde_json::from_slice(&self.input[start..self.offset]).map_err(Into::into)
            }
            Some(b'[') => {
                self.offset += 1;
                let mut values = Vec::new();
                loop {
                    self.whitespace();
                    match self.input.get(self.offset) {
                        Some(b']') => {
                            self.offset += 1;
                            return Ok(Value::Array(values));
                        }
                        Some(b',') => {
                            self.offset += 1;
                        }
                        None => return Err(invalid("unterminated JSON array")),
                        _ => values.push(self.value(depth + 1, false)?),
                    }
                }
            }
            Some(b'{') => {
                self.offset += 1;
                let mut object = Map::new();
                loop {
                    self.whitespace();
                    if self.input.get(self.offset) == Some(&b'}') {
                        self.offset += 1;
                        return Ok(Value::Object(object));
                    }
                    let name = self.value(depth + 1, false)?;
                    let name = name
                        .as_str()
                        .ok_or_else(|| invalid("JSON object key must be a string"))?
                        .to_owned();
                    self.whitespace();
                    if self.input.get(self.offset) != Some(&b':') {
                        return Err(invalid("expected ':' after JSON key"));
                    }
                    self.offset += 1;
                    let value = self.value(depth + 1, false)?;
                    object.insert(name, value);
                    self.whitespace();
                    match self.input.get(self.offset) {
                        Some(b',') => self.offset += 1,
                        Some(b'}') => {
                            self.offset += 1;
                            return Ok(Value::Object(object));
                        }
                        _ => return Err(invalid("expected ',' or '}' after JSON value")),
                    }
                }
            }
            None | Some(b',' | b':' | b']' | b'}') => Err(invalid("expected JSON value")),
            _ => {
                let start = self.offset;
                while let Some(c) = self.input.get(self.offset) {
                    if !top && matches!(c, b',' | b':' | b']' | b'}') {
                        break;
                    }
                    self.offset += 1;
                }
                let raw = std::str::from_utf8(&self.input[start..self.offset])
                    .map_err(|_| invalid("invalid UTF-8 JSON"))?;
                let token: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
                if token.is_empty() {
                    return Err(invalid("empty JSON token"));
                }
                let positive = token.strip_prefix('+').unwrap_or(&token);
                if let Some(hex) = positive.strip_prefix("0x") {
                    let number = u128::from_str_radix(hex, 16)
                        .map_err(|_| invalid("invalid hexadecimal integer"))?;
                    return Ok(serde_json::from_str(&number.to_string())?);
                }
                if let Some(hex) = positive.strip_prefix("-0x") {
                    let number = u128::from_str_radix(hex, 16)
                        .map_err(|_| invalid("invalid hexadecimal integer"))?;
                    return Ok(serde_json::from_str(&format!("-{number}"))?);
                }
                Ok(serde_json::from_str(positive).unwrap_or(Value::String(token)))
            }
        }
    }
}

static GLOBAL: OnceLock<Config> = OnceLock::new();
pub fn global_config() -> &'static Config {
    GLOBAL.get_or_init(Config::new)
}
pub fn get_config(key: &str) -> Result<Option<Value>> {
    global_config().get(key)
}
pub fn set_config(key: &str, value: Value) -> Result<()> {
    global_config().set(key, value)
}
pub fn read_config_tree(path: impl AsRef<Path>) -> Result<()> {
    global_config().load_tree(path)
}
pub fn read_config_file(path: impl AsRef<Path>) -> Result<Map<String, Value>> {
    parse_json(&read_text_bounded(path.as_ref())?)?
        .as_object()
        .cloned()
        .ok_or_else(|| invalid("config file must contain an object"))
}
pub fn write_config_file(path: impl AsRef<Path>, object: &Map<String, Value>) -> Result<()> {
    crate::util::write_json_file(path, object)
}
