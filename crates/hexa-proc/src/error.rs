//! Error type of the process-memory crate.

use std::io;

use crate::diagnostics::Diagnostic;

/// Result alias used across `hexa-proc`.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Every failure `hexa-proc` can report.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("process {pid} does not exist")]
    NoSuchProcess { pid: u32 },
    /// The OS refused access; `diagnostic` explains how to fix it (PROC-10).
    #[error("access to process {pid} was denied: {}", .diagnostic.summary)]
    PermissionDenied {
        pid: u32,
        diagnostic: Box<Diagnostic>,
    },
    /// The address is not mapped (or not readable) in the target. The UI
    /// shows such bytes as `??` (PROC-03).
    #[error("address {address:#x} is not mapped")]
    Unmapped { address: u64 },
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("{operation} failed with OS error {code}: {message}")]
    Os {
        operation: &'static str,
        code: i64,
        message: String,
    },
    #[error("GDB remote protocol error: {0}")]
    Protocol(String),
    #[error("not supported: {0}")]
    Unsupported(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("operation was cancelled")]
    Cancelled,
}

impl Error {
    pub(crate) fn invalid(msg: impl Into<String>) -> Self {
        Self::Invalid(msg.into())
    }

    pub(crate) fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }

    pub(crate) fn protocol(msg: impl Into<String>) -> Self {
        Self::Protocol(msg.into())
    }

    /// Whether this error means "the bytes at this address do not exist".
    pub fn is_unmapped(&self) -> bool {
        matches!(self, Self::Unmapped { .. })
    }

    /// The permission guidance attached to this error, if any (PROC-10).
    pub fn diagnostic(&self) -> Option<&Diagnostic> {
        match self {
            Self::PermissionDenied { diagnostic, .. } => Some(diagnostic),
            _ => None,
        }
    }
}

impl From<hexa_core::Error> for Error {
    fn from(e: hexa_core::Error) -> Self {
        match e {
            hexa_core::Error::Cancelled => Self::Cancelled,
            hexa_core::Error::Io(e) => Self::Io(e),
            other => Self::Invalid(other.to_string()),
        }
    }
}

impl From<Error> for io::Error {
    fn from(e: Error) -> Self {
        let kind = match &e {
            Error::Io(inner) => return io::Error::new(inner.kind(), e),
            Error::NoSuchProcess { .. } => io::ErrorKind::NotFound,
            Error::PermissionDenied { .. } => io::ErrorKind::PermissionDenied,
            Error::Unmapped { .. } | Error::Invalid(_) => io::ErrorKind::InvalidInput,
            Error::Unsupported(_) => io::ErrorKind::Unsupported,
            Error::Cancelled => io::ErrorKind::Interrupted,
            Error::Os { .. } | Error::Protocol(_) => io::ErrorKind::Other,
        };
        io::Error::new(kind, e)
    }
}

/// Whether an I/O error returned by a process-memory [`Provider`] means the
/// address is unmapped, i.e. the UI should render `??` (PROC-03).
///
/// [`Provider`]: hexa_core::provider::Provider
pub fn is_unmapped(err: &io::Error) -> bool {
    err.get_ref()
        .and_then(|inner| inner.downcast_ref::<Error>())
        .is_some_and(Error::is_unmapped)
}

/// Extracts the `hexa-proc` error carried by an I/O error, if any.
pub fn proc_error(err: &io::Error) -> Option<&Error> {
    err.get_ref()
        .and_then(|inner| inner.downcast_ref::<Error>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmapped_survives_io_conversion() {
        let io: io::Error = Error::Unmapped { address: 0x10 }.into();
        assert!(is_unmapped(&io));
        assert_eq!(io.kind(), io::ErrorKind::InvalidInput);
        let other: io::Error = Error::Cancelled.into();
        assert!(!is_unmapped(&other));
        assert!(matches!(proc_error(&other), Some(Error::Cancelled)));
    }
}
