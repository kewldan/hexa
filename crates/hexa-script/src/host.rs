//! The host interface through which scripts, macros and the local API reach
//! documents owned by the application (SCR-01, SCR-03, SCR-09).
//!
//! The application implements [`ScriptHost`]; this crate never owns document
//! data. Every fallible method reports errors as human-readable strings, which
//! are surfaced verbatim to Lua scripts, macro replays and API clients.

use serde::{Deserialize, Serialize};

/// Identifier of an open document, assigned by the host.
pub type DocId = u64;

/// Result type of fallible host operations; the error is a user-facing message.
pub type HostResult<T> = Result<T, String>;

/// Summary of an open document (SCR-01 `hexa.documents()`, SCR-09 `listDocuments`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentInfo {
    /// Host-assigned identifier.
    pub id: DocId,
    /// Display name (usually the file name).
    pub name: String,
    /// Backing file path, if any.
    pub path: Option<String>,
    /// Current length in bytes.
    pub len: u64,
    /// Whether edits are rejected.
    pub read_only: bool,
    /// Whether there are unsaved changes.
    pub modified: bool,
}

/// A half-open byte range `[offset, offset + len)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ByteRange {
    /// First byte of the range.
    pub offset: u64,
    /// Number of bytes.
    pub len: u64,
}

impl ByteRange {
    /// Creates a range starting at `offset` spanning `len` bytes.
    pub const fn new(offset: u64, len: u64) -> Self {
        Self { offset, len }
    }

    /// Exclusive end offset (saturating).
    pub const fn end(&self) -> u64 {
        self.offset.saturating_add(self.len)
    }

    /// Whether the range covers no bytes.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// How the pattern of a [`FindQuery`] is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QueryKind {
    /// The pattern is UTF-8 text.
    #[default]
    Text,
    /// The pattern is hex digits, optionally separated by whitespace.
    Hex,
}

/// A search query passed to [`ScriptHost::find_next`] / [`ScriptHost::find_all`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindQuery {
    /// Pattern interpretation.
    #[serde(default)]
    pub kind: QueryKind,
    /// The pattern itself.
    pub pattern: String,
    /// ASCII case-insensitive matching (text queries only).
    #[serde(default)]
    pub ignore_case: bool,
}

impl FindQuery {
    /// A case-sensitive text query.
    pub fn text(pattern: impl Into<String>) -> Self {
        Self {
            kind: QueryKind::Text,
            pattern: pattern.into(),
            ignore_case: false,
        }
    }

    /// A hex query such as `"DE AD BE EF"`.
    pub fn hex(pattern: impl Into<String>) -> Self {
        Self {
            kind: QueryKind::Hex,
            pattern: pattern.into(),
            ignore_case: false,
        }
    }

    /// The literal bytes this query searches for.
    pub fn to_bytes(&self) -> HostResult<Vec<u8>> {
        match self.kind {
            QueryKind::Text => Ok(self.pattern.as_bytes().to_vec()),
            QueryKind::Hex => parse_hex(&self.pattern),
        }
    }
}

/// Parses hex digits, ignoring ASCII whitespace and an optional `0x` prefix
/// per group (`"0xDE ad"` → `[0xDE, 0xAD]`).
pub fn parse_hex(text: &str) -> HostResult<Vec<u8>> {
    let mut digits = Vec::with_capacity(text.len());
    for group in text.split_ascii_whitespace() {
        let group = group
            .strip_prefix("0x")
            .or_else(|| group.strip_prefix("0X"))
            .unwrap_or(group);
        digits.extend_from_slice(group.as_bytes());
    }
    if digits.len() % 2 != 0 {
        return Err(format!("hex string has an odd number of digits: {text:?}"));
    }
    hex::decode(&digits).map_err(|e| format!("invalid hex string {text:?}: {e}"))
}

/// Severity of a [`ScriptHost::log`] message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LogLevel {
    /// Diagnostic detail.
    Debug,
    /// Normal information.
    #[default]
    Info,
    /// Something unexpected but recoverable.
    Warn,
    /// A failure.
    Error,
}

impl LogLevel {
    /// Parses `debug`, `info`, `warn`/`warning` or `error` (case-insensitive).
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" | "warning" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// Everything scripts, macros and the local API may do with the application
/// (SCR-01, SCR-03, SCR-09).
///
/// All offsets are 0-based byte offsets. Methods take `&self`; implementations
/// use interior mutability and must be callable from the script thread.
pub trait ScriptHost: Send + Sync {
    /// All open documents.
    fn documents(&self) -> Vec<DocumentInfo>;
    /// The document that has focus, if any.
    fn active_document(&self) -> Option<DocId>;
    /// Opens a file and returns its new document id.
    fn open_file(&self, path: &str, read_only: bool) -> HostResult<DocId>;
    /// Current length of the document in bytes.
    fn len(&self, doc: DocId) -> HostResult<u64>;
    /// Reads up to `len` bytes at `offset` (short reads at the end of data).
    fn read(&self, doc: DocId, offset: u64, len: u64) -> HostResult<Vec<u8>>;
    /// Overwrites bytes at `offset`, extending the document if needed.
    fn write(&self, doc: DocId, offset: u64, data: &[u8]) -> HostResult<()>;
    /// Inserts bytes before `offset`.
    fn insert(&self, doc: DocId, offset: u64, data: &[u8]) -> HostResult<()>;
    /// Deletes `len` bytes at `offset`.
    fn delete(&self, doc: DocId, offset: u64, len: u64) -> HostResult<()>;
    /// Replaces `len` bytes at `offset` with `data` (lengths may differ).
    fn replace(&self, doc: DocId, offset: u64, len: u64, data: &[u8]) -> HostResult<()>;
    /// The current selection, if any.
    fn selection(&self, doc: DocId) -> HostResult<Option<ByteRange>>;
    /// Sets (`Some`) or clears (`None`) the selection.
    fn set_selection(&self, doc: DocId, selection: Option<ByteRange>) -> HostResult<()>;
    /// The caret offset.
    fn cursor(&self, doc: DocId) -> HostResult<u64>;
    /// Moves the caret without scrolling.
    fn set_cursor(&self, doc: DocId, offset: u64) -> HostResult<()>;
    /// Moves the caret and scrolls it into view.
    fn goto(&self, doc: DocId, offset: u64) -> HostResult<()>;
    /// First match at or after `from`.
    fn find_next(&self, doc: DocId, query: &FindQuery, from: u64) -> HostResult<Option<ByteRange>>;
    /// Up to `limit` matches in document order.
    fn find_all(&self, doc: DocId, query: &FindQuery, limit: usize) -> HostResult<Vec<ByteRange>>;
    /// Digest of `range` as lowercase hex; `algorithm` is a host algorithm id
    /// such as `"sha256"` or `"crc32"`.
    fn hash(&self, doc: DocId, range: ByteRange, algorithm: &str) -> HostResult<String>;
    /// Value at `offset` decoded as `value_type` (e.g. `"u32le"`, `"f64be"`),
    /// formatted as text.
    fn inspect(&self, doc: DocId, offset: u64, value_type: &str) -> HostResult<String>;
    /// Applies the structure template `name` at `offset` and returns the parsed
    /// tree as a JSON string.
    fn apply_template(&self, doc: DocId, name: &str, offset: u64) -> HostResult<String>;
    /// Runs a transform recipe (JSON) over `range` and returns the output bytes
    /// without modifying the document.
    fn transform(&self, doc: DocId, recipe_json: &str, range: ByteRange) -> HostResult<Vec<u8>>;
    /// Undoes the last edit; returns whether anything was undone.
    fn undo(&self, doc: DocId) -> HostResult<bool>;
    /// Redoes the last undone edit; returns whether anything was redone.
    fn redo(&self, doc: DocId) -> HostResult<bool>;
    /// Saves the document to its backing file.
    fn save(&self, doc: DocId) -> HostResult<()>;
    /// Runs an application command by id (macro steps, `hexa.command`).
    fn run_command(&self, command: &str, args: &serde_json::Value) -> HostResult<()>;
    /// Writes to the script log.
    fn log(&self, level: LogLevel, message: &str);
    /// Receives one line of `print` output.
    fn print(&self, text: &str);
    /// Asks for a line of text; `None` when cancelled.
    fn prompt(&self, title: &str, message: &str, default: &str) -> Option<String>;
    /// Asks a yes/no question.
    fn confirm(&self, title: &str, message: &str) -> bool;
    /// Shows an informational message.
    fn message(&self, title: &str, message: &str);
}
