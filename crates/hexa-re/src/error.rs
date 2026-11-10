//! Error type shared by every module of the crate.

use thiserror::Error;

/// Errors produced by reverse-engineering operations.
#[derive(Debug, Error)]
pub enum ReError {
    /// The data is not a PE, ELF or Mach-O image.
    #[error("unrecognised executable format")]
    UnknownFormat,
    /// The executable was recognised but its structures are inconsistent.
    #[error("malformed executable: {0}")]
    Malformed(String),
    /// The address does not map to any byte of the file.
    #[error("address {0:#x} is not backed by file data")]
    Unmapped(u64),
    /// Disassembler failure.
    #[error("disassembler: {0}")]
    Disasm(String),
    /// Assembler failure (syntax error, unknown mnemonic, operand mismatch).
    #[error("assembler: {0}")]
    Asm(String),
    /// The requested operation is not available for the architecture.
    #[error("{what} is not supported for {arch}")]
    Unsupported {
        /// Operation name.
        what: &'static str,
        /// Architecture label.
        arch: String,
    },
    /// Invalid argument supplied by the caller.
    #[error("invalid argument: {0}")]
    Invalid(String),
    /// I/O error (tree reads, temp files).
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// JSON (de)serialisation error.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, ReError>;

impl From<goblin::error::Error> for ReError {
    fn from(e: goblin::error::Error) -> Self {
        ReError::Malformed(e.to_string())
    }
}
