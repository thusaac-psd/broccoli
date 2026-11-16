use std::fmt;

#[derive(Debug)]
pub enum StorageError {
    NotFound(String),
    Io(std::io::Error),
    InvalidHash(String),
    SizeLimitExceeded {
        actual: u64,
        limit: u64,
    },
    Backend(String),
    /// The store did not answer, or failed, through every retry: an outage,
    /// not a refusal. Worth retrying later.
    Unavailable(String),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(hash) => write!(f, "blob not found: {hash}"),
            Self::Io(err) => write!(f, "storage IO error: {err}"),
            Self::InvalidHash(msg) => write!(f, "invalid content hash: {msg}"),
            Self::SizeLimitExceeded { actual, limit } => {
                write!(f, "blob exceeds size limit ({actual} > {limit} bytes)")
            }
            Self::Backend(msg) => write!(f, "storage backend error: {msg}"),
            Self::Unavailable(msg) => write!(f, "storage unavailable: {msg}"),
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for StorageError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}
