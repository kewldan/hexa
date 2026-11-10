//! Errors of Lua execution (SCR-01, SCR-02, SRCH-08).

use serde::{Deserialize, Serialize};

/// Why a script did not complete successfully.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ScriptError {
    /// The source failed to compile.
    #[error("syntax error{}: {message}", line_suffix(*.line))]
    Syntax {
        /// Message without the chunk/line prefix.
        message: String,
        /// 1-based line, when known.
        line: Option<u32>,
        /// The input ended in the middle of a statement (REPL continuation).
        incomplete: bool,
    },
    /// The script raised an error while running.
    #[error("runtime error{}: {message}", line_suffix(*.line))]
    Runtime {
        /// Message without the chunk/line prefix.
        message: String,
        /// 1-based line of the innermost frame in the script chunk, when known.
        line: Option<u32>,
    },
    /// The cancellation flag was raised.
    #[error("script cancelled")]
    Cancelled,
    /// The instruction budget ran out.
    #[error("instruction budget of {budget} exceeded")]
    BudgetExceeded {
        /// Configured budget in VM instructions.
        budget: u64,
    },
    /// The wall-clock limit was hit.
    #[error("script timed out after {millis} ms")]
    Timeout {
        /// Configured limit in milliseconds.
        millis: u64,
    },
    /// The Lua heap limit was hit.
    #[error("memory limit of {limit} bytes exceeded")]
    MemoryLimit {
        /// Configured limit in bytes.
        limit: usize,
    },
    /// The caller passed invalid input (e.g. non-contiguous chunks).
    #[error("invalid input: {message}")]
    InvalidInput {
        /// Description of the problem.
        message: String,
    },
    /// The Lua state could not be set up.
    #[error("script engine error: {message}")]
    Internal {
        /// Description of the problem.
        message: String,
    },
}

impl ScriptError {
    /// The 1-based source line, if the error carries one.
    pub fn line(&self) -> Option<u32> {
        match self {
            Self::Syntax { line, .. } | Self::Runtime { line, .. } => *line,
            _ => None,
        }
    }

    pub(crate) fn internal(err: impl std::fmt::Display) -> Self {
        Self::Internal {
            message: err.to_string(),
        }
    }
}

fn line_suffix(line: Option<u32>) -> String {
    line.map(|l| format!(" at line {l}")).unwrap_or_default()
}
