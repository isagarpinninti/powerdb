use std::fmt;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Store(String),
    Json(serde_json::Error),
    NotFound(String),
    /// The request is malformed or breaks a collection's rules (400).
    Invalid(String),
    /// Stored data failed a check: a hash that does not match, a torn entry (500).
    Corrupt(String),
    /// The caller is not allowed (403).
    Denied(String),
    /// The thing already exists (409).
    Conflict(String),
    /// A message for the person running a command-line client, shown as it is.
    Cli(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "i/o error: {e}"),
            Error::Store(e) => write!(f, "storage error: {e}"),
            Error::Json(e) => write!(f, "json error: {e}"),
            Error::NotFound(m) => write!(f, "not found: {m}"),
            Error::Invalid(m) => write!(f, "invalid: {m}"),
            Error::Corrupt(m) => write!(f, "corrupt data: {m}"),
            Error::Denied(m) => write!(f, "denied: {m}"),
            Error::Conflict(m) => write!(f, "conflict: {m}"),
            Error::Cli(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
impl From<fjall::Error> for Error {
    fn from(e: fjall::Error) -> Self {
        Error::Store(e.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}
