//! The Lua 5.4 script engine with the `hexa` API table (SCR-01).

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use mlua::{Function, Lua, MultiValue, Table, Value};
use serde::{Deserialize, Serialize};

use crate::error::ScriptError;
use crate::host::ScriptHost;
use crate::lua_api;
use crate::sandbox::{self, CancelToken, ScriptLimits, Watchdog, lock};

/// Maximum captured `print` output per run; the rest is dropped with a marker.
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

/// Result of running a chunk (SCR-01, SCR-02).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptOutput {
    /// Everything printed during the run, one line per `print` call.
    pub output: String,
    /// Returned values rendered as Lua-like literals.
    pub values: Vec<String>,
    /// Why the run failed, if it did.
    pub error: Option<ScriptError>,
}

impl ScriptOutput {
    /// Whether the run completed without error.
    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }

    /// Converts into a `Result`, dropping printed output on error.
    pub fn into_result(self) -> Result<Vec<String>, ScriptError> {
        match self.error {
            Some(e) => Err(e),
            None => Ok(self.values),
        }
    }
}

/// Captured `print` output with a size cap.
#[derive(Debug, Default)]
pub(crate) struct OutputBuffer {
    text: String,
    truncated: bool,
}

impl OutputBuffer {
    pub(crate) fn push_line(&mut self, line: &str) {
        if self.truncated {
            return;
        }
        if self.text.len() + line.len() + 1 > MAX_OUTPUT_BYTES {
            self.text.push_str("[output truncated]\n");
            self.truncated = true;
            return;
        }
        self.text.push_str(line);
        self.text.push('\n');
    }

    fn take(&mut self) -> String {
        self.truncated = false;
        std::mem::take(&mut self.text)
    }
}

/// A sandboxed Lua state bound to a [`ScriptHost`] (SCR-01).
///
/// Globals persist between runs, so the same engine backs the REPL (SCR-02).
/// Limits apply per run; the [`CancelToken`] aborts the current run from any
/// thread.
pub struct ScriptEngine {
    lua: Lua,
    watchdog: Arc<Watchdog>,
    output: Arc<Mutex<OutputBuffer>>,
}

impl std::fmt::Debug for ScriptEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptEngine")
            .field("limits", &self.watchdog.limits())
            .finish_non_exhaustive()
    }
}

impl ScriptEngine {
    /// Creates a sandboxed state exposing the `hexa` API backed by `host`.
    pub fn new(host: Arc<dyn ScriptHost>, limits: ScriptLimits) -> Result<Self, ScriptError> {
        let (lua, watchdog) = sandbox::create_sandbox(limits, CancelToken::new())?;
        let output = Arc::new(Mutex::new(OutputBuffer::default()));
        lua_api::install(&lua, host, Arc::clone(&output)).map_err(ScriptError::internal)?;
        Ok(Self {
            lua,
            watchdog,
            output,
        })
    }

    /// Current limits.
    pub fn limits(&self) -> ScriptLimits {
        self.watchdog.limits()
    }

    /// Replaces the limits for subsequent runs.
    pub fn set_limits(&self, limits: ScriptLimits) -> Result<(), ScriptError> {
        self.lua
            .set_memory_limit(limits.memory_bytes)
            .map_err(ScriptError::internal)?;
        self.watchdog.set_limits(limits);
        Ok(())
    }

    /// The cancellation flag of this engine. It is *not* reset automatically;
    /// call [`CancelToken::reset`] before the next run.
    pub fn cancel_token(&self) -> CancelToken {
        self.watchdog.cancel_token()
    }

    /// Lua heap usage in bytes.
    pub fn used_memory(&self) -> usize {
        self.lua.used_memory()
    }

    /// Compiles and runs `source`; `name` is used in error messages
    /// (`name:LINE: message`).
    pub fn run(&self, source: &str, name: &str) -> ScriptOutput {
        let chunk = chunk_name(name);
        self.execute(chunk, |lua| {
            lua.load(source)
                .set_name(format!("={chunk}"))
                .eval::<MultiValue>()
        })
    }

    /// Calls a global Lua function by name with string arguments, e.g. an
    /// entry point defined by a previously run script.
    pub fn call_global(&self, function: &str, args: &[&str]) -> ScriptOutput {
        self.execute("call", |lua| {
            let f: Function = lua.globals().get(function)?;
            let args = args
                .iter()
                .map(|a| lua.create_string(a).map(Value::String))
                .collect::<mlua::Result<MultiValue>>()?;
            f.call::<MultiValue>(args)
        })
    }

    pub(crate) fn lua(&self) -> &Lua {
        &self.lua
    }

    /// Runs `f` under the watchdog, capturing output and converting errors.
    pub(crate) fn execute(
        &self,
        chunk: &str,
        f: impl FnOnce(&Lua) -> mlua::Result<MultiValue>,
    ) -> ScriptOutput {
        lock(&self.output).take();
        self.watchdog.begin();
        let result = f(&self.lua).map(|values| {
            values
                .iter()
                .map(|v| format_value(v, 0))
                .collect::<Vec<_>>()
        });
        self.watchdog.end();
        let abort = self.watchdog.take_abort();
        let limits = self.watchdog.limits();
        let (values, error) = match (abort, result) {
            (Some(abort), _) => (Vec::new(), Some(abort.into_error())),
            (None, Err(e)) => (Vec::new(), Some(sandbox::convert_error(&e, chunk, &limits))),
            (None, Ok(values)) => (values, None),
        };
        ScriptOutput {
            output: lock(&self.output).take(),
            values,
            error,
        }
    }
}

/// Chunk names must not contain characters that confuse location parsing.
fn chunk_name(name: &str) -> &str {
    if name.is_empty() || name.contains(':') || name.contains('\n') {
        "script"
    } else {
        name
    }
}

const MAX_FORMAT_DEPTH: usize = 2;
const MAX_FORMAT_ENTRIES: usize = 64;

/// Renders a value as a Lua-like literal for REPL display (SCR-02).
pub fn format_value(value: &Value, depth: usize) -> String {
    match value {
        Value::Nil => "nil".to_owned(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => format_number(*n),
        Value::String(s) => quote_bytes(&s.as_bytes()),
        Value::Table(t) => format_table(t, depth),
        Value::LightUserData(_) if value.is_null() => "hexa.null".to_owned(),
        Value::Error(e) => format!("error: {e}"),
        other => format!("{}: {:p}", other.type_name(), other.to_pointer()),
    }
}

fn format_number(n: f64) -> String {
    if n.is_nan() {
        "nan".to_owned()
    } else if n.is_infinite() {
        if n > 0.0 { "inf" } else { "-inf" }.to_owned()
    } else if n != 0.0 && (n.abs() >= 1e16 || n.abs() < 1e-5) {
        format!("{n:e}")
    } else if n.fract() == 0.0 {
        format!("{n:.1}")
    } else {
        format!("{n}")
    }
}

fn format_table(table: &Table, depth: usize) -> String {
    if depth >= MAX_FORMAT_DEPTH {
        return "{...}".to_owned();
    }
    let seq_len = table.raw_len();
    let mut parts = Vec::new();
    let mut truncated = false;
    for i in 1..=seq_len {
        if parts.len() >= MAX_FORMAT_ENTRIES {
            truncated = true;
            break;
        }
        let v: Value = table.raw_get(i).unwrap_or(Value::Nil);
        parts.push(format_value(&v, depth + 1));
    }
    if !truncated {
        let mut keyed = Vec::new();
        for pair in table.pairs::<Value, Value>() {
            let Ok((k, v)) = pair else { continue };
            if let Value::Integer(i) = k
                && i >= 1
                && usize::try_from(i).is_ok_and(|i| i <= seq_len)
            {
                continue;
            }
            if parts.len() + keyed.len() >= MAX_FORMAT_ENTRIES {
                truncated = true;
                break;
            }
            let key = match &k {
                Value::String(s) if is_identifier(&s.as_bytes()) => {
                    String::from_utf8_lossy(&s.as_bytes()).into_owned()
                }
                other => format!("[{}]", format_value(other, MAX_FORMAT_DEPTH)),
            };
            keyed.push(format!("{key} = {}", format_value(&v, depth + 1)));
        }
        keyed.sort();
        parts.extend(keyed);
    }
    if truncated {
        parts.push("...".to_owned());
    }
    if parts.is_empty() {
        "{}".to_owned()
    } else {
        format!("{{{}}}", parts.join(", "))
    }
}

const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

/// Whether `name` is a Lua identifier (and not a keyword).
pub(crate) fn is_identifier(name: &[u8]) -> bool {
    let Some((first, rest)) = name.split_first() else {
        return false;
    };
    (first.is_ascii_alphabetic() || *first == b'_')
        && rest.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
        && !LUA_KEYWORDS.iter().any(|k| k.as_bytes() == name)
}

/// Quotes bytes as a Lua string literal; non-printable bytes use `\xNN`.
pub(crate) fn quote_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 2);
    out.push('"');
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7e => out.push(char::from(b)),
            _ => {
                let _ = write!(out, "\\x{b:02x}");
            }
        }
    }
    out.push('"');
    out
}
