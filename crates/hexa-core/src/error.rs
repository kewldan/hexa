//! Error type shared by the whole core.

use std::io;
use std::path::PathBuf;

/// Result alias used across `hexa-core`.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Every failure the core can report.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("document is read-only")]
    ReadOnly,
    #[error("document size is fixed; this edit would change its length")]
    FixedSize,
    #[error("range {start}..{end} is outside of the document (length {len})")]
    OutOfRange { start: u64, end: u64, len: u64 },
    #[error("document has no file path; use Save As")]
    NoPath,
    #[error("file {0} was modified on disk since it was opened")]
    ExternallyModified(PathBuf),
    #[error("operation is not supported by this provider")]
    Unsupported,
    #[error("operation was cancelled")]
    Cancelled,
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("selection is too large for this operation ({len} bytes, limit {limit})")]
    TooLarge { len: u64, limit: u64 },
}

impl Error {
    pub(crate) fn invalid(msg: impl Into<String>) -> Self {
        Self::Invalid(msg.into())
    }
}
