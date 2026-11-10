//! Errors returned to the frontend as `{ kind, message }`.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct CmdError {
    pub kind: &'static str,
    pub message: String,
}

pub type CmdResult<T> = Result<T, CmdError>;

impl CmdError {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid", message)
    }
}

impl From<hexa_core::Error> for CmdError {
    fn from(e: hexa_core::Error) -> Self {
        use hexa_core::Error as E;
        let kind = match &e {
            E::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied => "permission",
            E::Io(io) if io.kind() == std::io::ErrorKind::NotFound => "notFound",
            E::Io(_) => "io",
            E::ReadOnly => "readOnly",
            E::FixedSize => "fixedSize",
            E::OutOfRange { .. } => "outOfRange",
            E::NoPath => "noPath",
            E::ExternallyModified(_) => "externallyModified",
            E::Unsupported => "unsupported",
            E::Cancelled => "cancelled",
            E::Invalid(_) => "invalid",
            E::TooLarge { .. } => "tooLarge",
        };
        Self::new(kind, e.to_string())
    }
}

impl From<std::io::Error> for CmdError {
    fn from(e: std::io::Error) -> Self {
        hexa_core::Error::from(e).into()
    }
}

impl From<tauri::Error> for CmdError {
    fn from(e: tauri::Error) -> Self {
        Self::new("internal", e.to_string())
    }
}
