use std::fmt;
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Json(serde_json::Error),
    Invalid(String),
    Limit(String),
    UnknownType(u64),
    Timeout,
    Closed,
    Overflow,
    PermissionDenied(String),
    Remote(String),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::Json(e) => e.fmt(f),
            Self::Invalid(s) => write!(f, "invalid VNX data: {s}"),
            Self::Limit(s) => write!(f, "VNX limit exceeded: {s}"),
            Self::UnknownType(h) => write!(f, "unknown VNX type code {h:016x}"),
            Self::Timeout => f.write_str("request timed out"),
            Self::Closed => f.write_str("closed"),
            Self::Overflow => f.write_str("queue overflow"),
            Self::PermissionDenied(s) => write!(f, "permission denied: {s}"),
            Self::Remote(s) => f.write_str(s),
        }
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
pub type Result<T> = std::result::Result<T, Error>;
pub fn invalid(s: impl Into<String>) -> Error {
    Error::Invalid(s.into())
}
