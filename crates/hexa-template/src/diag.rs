//! Diagnostics and the crate error type (STRC-17).

use serde::{Deserialize, Serialize};

use crate::ast::Span;

/// Diagnostic severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// A located message about template source (STRC-17).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub severity: Severity,
    pub message: String,
}

impl Diagnostic {
    pub fn new(span: Span, severity: Severity, message: impl Into<String>) -> Self {
        Self {
            line: span.line,
            col: span.col,
            end_line: span.end_line,
            end_col: span.end_col,
            severity,
            message: message.into(),
        }
    }

    pub fn error(span: Span, message: impl Into<String>) -> Self {
        Self::new(span, Severity::Error, message)
    }

    pub fn warning(span: Span, message: impl Into<String>) -> Self {
        Self::new(span, Severity::Warning, message)
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.message)
    }
}

/// Errors returned by the template API.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("template has errors: {}", first(.0))]
    Parse(Vec<Diagnostic>),
    #[error("evaluation was cancelled")]
    Cancelled,
    #[error("{0}")]
    Invalid(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

fn first(d: &[Diagnostic]) -> String {
    d.iter()
        .find(|d| d.severity == Severity::Error)
        .or(d.first())
        .map_or_else(String::new, ToString::to_string)
}

/// Result alias for the crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub(crate) fn invalid(msg: impl Into<String>) -> Self {
        Self::Invalid(msg.into())
    }
}
