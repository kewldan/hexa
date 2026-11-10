//! Error type and exit-code mapping of the CLI (see the crate docs).

use std::fmt;
use std::process::ExitCode;

/// Exit code: success, no differences, found, check passed.
pub const EXIT_OK: u8 = 0;
/// Exit code: differences found, nothing found, check failed, conflicts.
pub const EXIT_NEGATIVE: u8 = 1;
/// Exit code: invalid command line or argument value.
pub const EXIT_USAGE: u8 = 2;
/// Exit code: a file or device could not be read or written.
pub const EXIT_IO: u8 = 3;
/// Exit code: the input data or a format is invalid.
pub const EXIT_DATA: u8 = 4;

/// Result of a successful command run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Exit code 0.
    Ok,
    /// Exit code 1: the command ran but the answer is "no" (differences,
    /// not found, mismatch, conflicts).
    Negative,
}

impl Outcome {
    pub fn from_ok(ok: bool) -> Self {
        if ok { Self::Ok } else { Self::Negative }
    }

    pub fn exit_code(self) -> ExitCode {
        ExitCode::from(match self {
            Self::Ok => EXIT_OK,
            Self::Negative => EXIT_NEGATIVE,
        })
    }
}

/// A failed command run.
#[derive(Debug)]
pub enum CliError {
    /// Exit code 2.
    Usage(String),
    /// Exit code 3.
    Io(String),
    /// Exit code 4.
    Data(String),
}

pub type CliResult<T = Outcome> = Result<T, CliError>;

impl CliError {
    pub fn usage(msg: impl Into<String>) -> Self {
        Self::Usage(msg.into())
    }

    pub fn io(msg: impl Into<String>) -> Self {
        Self::Io(msg.into())
    }

    pub fn data(msg: impl Into<String>) -> Self {
        Self::Data(msg.into())
    }

    pub fn code(&self) -> u8 {
        match self {
            Self::Usage(_) => EXIT_USAGE,
            Self::Io(_) => EXIT_IO,
            Self::Data(_) => EXIT_DATA,
        }
    }

    /// Prefixes the message with some context (a file name …).
    #[must_use]
    pub fn context(self, ctx: impl fmt::Display) -> Self {
        match self {
            Self::Usage(m) => Self::Usage(format!("{ctx}: {m}")),
            Self::Io(m) => Self::Io(format!("{ctx}: {m}")),
            Self::Data(m) => Self::Data(format!("{ctx}: {m}")),
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(m) | Self::Io(m) | Self::Data(m) => f.write_str(m),
        }
    }
}

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<hexa_core::Error> for CliError {
    fn from(e: hexa_core::Error) -> Self {
        match e {
            hexa_core::Error::Io(e) => Self::Io(e.to_string()),
            e => Self::Data(e.to_string()),
        }
    }
}

impl From<hexa_re::ReError> for CliError {
    fn from(e: hexa_re::ReError) -> Self {
        match e {
            hexa_re::ReError::Io(e) => Self::Io(e.to_string()),
            e => Self::Data(e.to_string()),
        }
    }
}

impl From<hexa_transform::TransformError> for CliError {
    fn from(e: hexa_transform::TransformError) -> Self {
        Self::Data(e.to_string())
    }
}

impl From<hexa_template::Error> for CliError {
    fn from(e: hexa_template::Error) -> Self {
        Self::Data(e.to_string())
    }
}

impl From<serde_json::Error> for CliError {
    fn from(e: serde_json::Error) -> Self {
        if e.is_io() {
            Self::Io(e.to_string())
        } else {
            Self::Data(e.to_string())
        }
    }
}

/// Maps a core error caused by a bad argument (pattern, regex, block size …)
/// to a usage error; I/O errors stay I/O errors.
pub fn invalid_is_usage(e: hexa_core::Error) -> CliError {
    match e {
        hexa_core::Error::Io(e) => CliError::Io(e.to_string()),
        e => CliError::Usage(e.to_string()),
    }
}
