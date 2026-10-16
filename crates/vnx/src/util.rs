//! Portable helpers used by MMX: durable files, crypto randomness, hashes,
//! hexadecimal data and VNX wall/synchronized/virtual clocks.
use crate::{binary, config::parse_json, error::invalid, Error, Registry, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn secure_random_bytes(output: &mut [u8]) -> Result<()> {
    getrandom::getrandom(output)
        .map_err(|error| Error::Io(std::io::Error::other(error.to_string())))
}
pub fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    secure_random_bytes(&mut bytes)?;
    Ok(bytes)
}
pub fn random_u64() -> Result<u64> {
    Ok(u64::from_le_bytes(random_bytes()?))
}
pub fn random_nonzero_u64() -> Result<u64> {
    loop {
        let value = random_u64()?;
        if value != 0 {
            return Ok(value);
        }
    }
}
pub fn sha256(bytes: impl AsRef<[u8]>) -> [u8; 32] {
    Sha256::digest(bytes.as_ref()).into()
}

/// VNX emits uppercase hexadecimal by default; big_endian reverses byte order.
pub fn to_hex_string(bytes: &[u8], big_endian: bool, lower_case: bool) -> String {
    const UPPER: &[u8] = b"0123456789ABCDEF";
    const LOWER: &[u8] = b"0123456789abcdef";
    let table = if lower_case { LOWER } else { UPPER };
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for index in 0..bytes.len() {
        let byte = bytes[if big_endian {
            bytes.len() - 1 - index
        } else {
            index
        }];
        out.push(table[(byte >> 4) as usize] as char);
        out.push(table[(byte & 15) as usize] as char);
    }
    out
}
pub fn from_hex_string(text: &str) -> Result<Vec<u8>> {
    let text = text.strip_prefix("0x").unwrap_or(text);
    if text.len() % 2 != 0 {
        return Err(invalid("hex string length is not a multiple of two"));
    }
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digit = |byte: u8| match byte {
                b'0'..=b'9' => Ok(byte - b'0'),
                b'a'..=b'f' => Ok(byte - b'a' + 10),
                b'A'..=b'F' => Ok(byte - b'A' + 10),
                _ => Err(invalid("invalid hexadecimal character")),
            };
            Ok(digit(pair[0])? * 16 + digit(pair[1])?)
        })
        .collect()
}

pub fn get_wall_time_micros() -> i64 {
    system_micros(SystemTime::now())
}
pub fn get_wall_time_millis() -> i64 {
    get_wall_time_micros() / 1000
}
pub fn get_wall_time_nanos() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(delta) => delta.as_nanos().min(i64::MAX as u128) as i64,
        Err(error) => -(error.duration().as_nanos().min(i64::MAX as u128) as i64),
    }
}
fn system_micros(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(delta) => delta.as_micros().min(i64::MAX as u128) as i64,
        Err(error) => -(error.duration().as_micros().min(i64::MAX as u128) as i64),
    }
}
pub fn get_monotonic_time_micros() -> i64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN
        .get_or_init(Instant::now)
        .elapsed()
        .as_micros()
        .min(i64::MAX as u128) as i64
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSync {
    pub time: i64,
    pub wall_time: i64,
    pub offset: i64,
    pub jitter: i64,
    pub order: i32,
}
impl TimeSync {
    pub fn time_at(&self, wall_time: i64) -> i64 {
        wall_time.saturating_add(self.offset)
    }
    pub fn get_time_micros(&self) -> i64 {
        self.time_at(get_wall_time_micros())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimeControl {
    pub time: i64,
    pub time_start: i64,
    pub time_offset: i64,
    pub is_realtime: bool,
    pub is_paused: bool,
    pub time_speed: f64,
}
impl Default for TimeControl {
    fn default() -> Self {
        Self {
            time: 0,
            time_start: 0,
            time_offset: 0,
            is_realtime: true,
            is_paused: false,
            time_speed: 1.0,
        }
    }
}
impl TimeControl {
    pub fn time_at(&self, sync_time: i64) -> i64 {
        if self.is_realtime {
            return sync_time;
        }
        let base = self.time_start.saturating_add(self.time_offset);
        if self.is_paused {
            return base;
        }
        let elapsed =
            ((sync_time as i128 - self.time_start as i128) as f64 * self.time_speed) as i64;
        base.saturating_add(elapsed)
    }
    pub fn get_time_micros(&self) -> i64 {
        self.time_at(get_sync_time_micros())
    }
}
#[derive(Default)]
struct Clocks {
    sync: TimeSync,
    control: TimeControl,
}
static CLOCKS: OnceLock<RwLock<Clocks>> = OnceLock::new();
fn clocks() -> &'static RwLock<Clocks> {
    CLOCKS.get_or_init(|| RwLock::new(Clocks::default()))
}
pub fn set_time_sync(sync: TimeSync) {
    clocks()
        .write()
        .unwrap_or_else(|error| error.into_inner())
        .sync = sync;
}
pub fn get_time_sync() -> TimeSync {
    clocks()
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .sync
}
pub fn set_time_control(control: TimeControl) -> Result<()> {
    if !control.time_speed.is_finite() {
        return Err(invalid("non-finite virtual clock speed"));
    }
    clocks()
        .write()
        .unwrap_or_else(|error| error.into_inner())
        .control = control;
    Ok(())
}
pub fn get_time_control() -> TimeControl {
    clocks()
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .control
}
pub fn get_sync_time_micros() -> i64 {
    get_time_sync().get_time_micros()
}
pub fn get_time_micros() -> i64 {
    get_time_control().get_time_micros()
}
pub fn get_time_millis() -> i64 {
    get_time_micros() / 1000
}
pub fn get_time_seconds() -> i64 {
    get_time_micros() / 1_000_000
}
pub fn sleep_micros(micros: u64) {
    std::thread::sleep(Duration::from_micros(micros));
}

/// Reads up to `limit` bytes, including pipes/special files whose stat size is 0.
pub fn read_file_bounded(path: impl AsRef<Path>, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(Error::Limit("file size".into()));
    }
    Ok(bytes)
}
/// Writes and fsyncs a private temporary file in the destination directory,
/// atomically replaces the destination, then fsyncs its directory on Unix.
/// Failed writes never truncate the existing file.
pub fn atomic_write(path: impl AsRef<Path>, bytes: &[u8]) -> Result<()> {
    let path = path.as_ref();
    let name = path
        .file_name()
        .ok_or_else(|| invalid("destination has no file name"))?;
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary_name = name.to_os_string();
    temporary_name.push(format!(
        ".tmp-{}",
        to_hex_string(&random_bytes::<16>()?, false, true)
    ));
    let temporary = parent.join(temporary_name);
    let operation = || -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    };
    let outcome = operation();
    if outcome.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    outcome
}
pub fn read_json_file<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<T> {
    let bytes = read_file_bounded(path, 64 * 1024 * 1024)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| invalid("JSON file is not UTF-8"))?;
    Ok(serde_json::from_value(parse_json(text)?)?)
}
pub fn write_json_file(path: impl AsRef<Path>, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}
/// Detects native VNX binary by its file magic, otherwise parses JSON for `.json`
/// files or text beginning with `{`/`[`. Native input restores its type registry.
pub fn read_from_file_with_registry(path: impl AsRef<Path>) -> Result<(Value, Registry)> {
    let path = path.as_ref();
    let bytes = read_file_bounded(path, 256 * 1024 * 1024)?;
    if bytes.starts_with(&[0xff, 0xff, 0x13, 0x37]) || bytes.starts_with(&[0xff, 0xff, 0x37, 0x13])
    {
        return binary::decode(&bytes);
    }
    let first = bytes.iter().find(|byte| !byte.is_ascii_whitespace());
    if path
        .extension()
        .is_some_and(|extension| extension == "json")
        || matches!(first, Some(b'{' | b'['))
    {
        let text = std::str::from_utf8(&bytes).map_err(|_| invalid("JSON file is not UTF-8"))?;
        Ok((parse_json(text)?, Registry::new()))
    } else {
        binary::decode(&bytes)
    }
}
pub fn read_from_file(path: impl AsRef<Path>) -> Result<Value> {
    Ok(read_from_file_with_registry(path)?.0)
}
/// `.json` chooses readable JSON; all other extensions choose native VNX.
pub fn write_to_file(path: impl AsRef<Path>, value: &Value, registry: &Registry) -> Result<()> {
    let path = path.as_ref();
    if path
        .extension()
        .is_some_and(|extension| extension == "json")
    {
        write_json_file(path, value)
    } else {
        atomic_write(path, &binary::encode(value, registry)?)
    }
}

/// VNX local/UTC strftime dates. A negative timestamp selects the current time.
pub fn get_date_string(utc: bool, time_seconds: i64) -> Result<String> {
    get_date_string_ex("%Y-%m-%d_%H:%M:%S", utc, time_seconds)
}
pub fn get_date_string_ex(format: &str, utc: bool, time_seconds: i64) -> Result<String> {
    use chrono::{Local, TimeZone, Utc};
    let seconds = if time_seconds < 0 {
        get_wall_time_micros() / 1_000_000
    } else {
        time_seconds
    };
    let items = chrono::format::StrftimeItems::new(format).collect::<Vec<_>>();
    if items
        .iter()
        .any(|item| matches!(item, chrono::format::Item::Error))
    {
        return Err(invalid("invalid date format"));
    }
    let date = Utc
        .timestamp_opt(seconds, 0)
        .single()
        .ok_or_else(|| invalid("date outside supported range"))?;
    Ok(if utc {
        date.format_with_items(items.iter()).to_string()
    } else {
        date.with_timezone(&Local)
            .format_with_items(items.iter())
            .to_string()
    })
}

pub trait ByteSwap: Sized {
    fn flip_bytes(self) -> Self;
}
macro_rules! swap_integers { ($($ty:ty),*) => { $(impl ByteSwap for $ty { fn flip_bytes(self)->Self { self.swap_bytes() } })* }; }
swap_integers!(u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize);
impl ByteSwap for f32 {
    fn flip_bytes(self) -> Self {
        Self::from_bits(self.to_bits().swap_bytes())
    }
}
impl ByteSwap for f64 {
    fn flip_bytes(self) -> Self {
        Self::from_bits(self.to_bits().swap_bytes())
    }
}
pub fn flip_bytes<T: ByteSwap>(value: T) -> T {
    value.flip_bytes()
}
pub const fn is_little_endian() -> bool {
    cfg!(target_endian = "little")
}
