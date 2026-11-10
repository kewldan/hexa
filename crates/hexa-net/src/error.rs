//! Error type shared by the remote and container sources.

use std::io;

/// Result alias of this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong while opening or reading a remote or
/// container source.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),

    /// The URL or endpoint could not be parsed.
    #[error("invalid URL `{0}`")]
    InvalidUrl(String),

    /// The server answered with an unexpected status code.
    #[error("HTTP {status} for {url}")]
    HttpStatus { status: u16, url: String },

    /// The request never produced a response (DNS, TLS, timeout …).
    #[error("HTTP transport error: {0}")]
    Http(String),

    /// The server answered but the answer does not make sense.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// The remote object changed while it was open (`ETag` mismatch).
    #[error("the remote resource changed since it was opened")]
    Changed,

    /// SSH transport or session failure.
    #[error("SSH error: {0}")]
    Ssh(String),

    /// The host key callback refused the server key. The fingerprint is
    /// reported so the UI can ask the user and retry with it pinned.
    #[error("host key {fingerprint} of {host} was rejected")]
    HostKeyRejected { host: String, fingerprint: String },

    /// The server refused the credentials.
    #[error("authentication failed for user `{user}`")]
    AuthFailed { user: String },

    /// The private key could not be decoded.
    #[error("invalid private key: {0}")]
    InvalidKey(String),

    /// An SFTP request failed.
    #[error("SFTP error: {0}")]
    Sftp(String),

    /// The archive is malformed or uses an unsupported feature.
    #[error("archive error: {0}")]
    Archive(String),

    /// No entry of that name exists in the archive.
    #[error("no entry `{0}` in the archive")]
    EntryNotFound(String),

    /// The entry is larger than the configured in-memory limit.
    #[error("entry is {size} bytes, more than the {limit} byte limit")]
    TooLarge { size: u64, limit: u64 },

    /// The operation is not supported by this source.
    #[error("unsupported: {0}")]
    Unsupported(String),
}

impl Error {
    fn kind(&self) -> io::ErrorKind {
        match self {
            Self::Io(e) => e.kind(),
            Self::InvalidUrl(_) | Self::InvalidKey(_) => io::ErrorKind::InvalidInput,
            Self::HttpStatus {
                status: 401 | 403, ..
            }
            | Self::HostKeyRejected { .. }
            | Self::AuthFailed { .. } => io::ErrorKind::PermissionDenied,
            Self::HttpStatus { status: 404, .. } | Self::EntryNotFound(_) => {
                io::ErrorKind::NotFound
            }
            Self::Archive(_) | Self::Protocol(_) | Self::Changed => io::ErrorKind::InvalidData,
            Self::TooLarge { .. } => io::ErrorKind::OutOfMemory,
            Self::Unsupported(_) => io::ErrorKind::Unsupported,
            Self::HttpStatus { .. } | Self::Http(_) | Self::Ssh(_) | Self::Sftp(_) => {
                io::ErrorKind::Other
            }
        }
    }
}

impl From<Error> for io::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(e) => e,
            other => io::Error::new(other.kind(), other),
        }
    }
}

impl From<::zip::result::ZipError> for Error {
    fn from(e: ::zip::result::ZipError) -> Self {
        match e {
            ::zip::result::ZipError::Io(e) => Self::Io(e),
            ::zip::result::ZipError::FileNotFound => Self::EntryNotFound(String::new()),
            ::zip::result::ZipError::UnsupportedArchive(m) => Self::Unsupported(m.to_string()),
            other => Self::Archive(other.to_string()),
        }
    }
}

impl From<sevenz_rust2::Error> for Error {
    fn from(e: sevenz_rust2::Error) -> Self {
        match e {
            sevenz_rust2::Error::Io(e, _) => Self::Io(e),
            other => Self::Archive(other.to_string()),
        }
    }
}
