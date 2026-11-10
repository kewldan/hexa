//! Interactive Lua console (SCR-02).

use std::sync::Arc;

use mlua::{MultiValue, Table, Value};
use serde::{Deserialize, Serialize};

use crate::engine::{ScriptEngine, is_identifier};
use crate::error::ScriptError;
use crate::host::ScriptHost;
use crate::sandbox::ScriptLimits;

/// Chunk name used for REPL input in error messages (`repl:LINE: ...`).
pub const REPL_CHUNK: &str = "repl";

/// Result of evaluating one REPL line (SCR-02).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplResponse {
    /// Text printed by the line.
    pub output: String,
    /// Values of the expression, rendered as Lua literals.
    pub values: Vec<String>,
    /// Error with line information, if evaluation failed.
    pub error: Option<ScriptError>,
    /// The input is an unfinished statement; more lines are expected.
    pub incomplete: bool,
}

/// A REPL keeping one Lua state across lines (SCR-02).
///
/// * `=expr` evaluates and shows `expr`;
/// * a bare expression (`1 + 2`, `hexa.len()`) is shown as well;
/// * statements run as-is; globals persist, `local`s last for one input;
/// * unfinished statements (`for i = 1, 3 do`) are buffered until complete.
#[derive(Debug)]
pub struct Repl {
    engine: ScriptEngine,
    pending: String,
}

impl Repl {
    /// Creates a REPL over a new sandboxed engine.
    pub fn new(host: Arc<dyn ScriptHost>, limits: ScriptLimits) -> Result<Self, ScriptError> {
        Ok(Self::from_engine(ScriptEngine::new(host, limits)?))
    }

    /// Wraps an existing engine (sharing its globals).
    pub fn from_engine(engine: ScriptEngine) -> Self {
        Self {
            engine,
            pending: String::new(),
        }
    }

    /// The underlying engine (limits, cancellation).
    pub fn engine(&self) -> &ScriptEngine {
        &self.engine
    }

    /// Whether a multi-line statement is being continued.
    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Discards a buffered multi-line statement.
    pub fn clear_pending(&mut self) {
        self.pending.clear();
    }

    /// Evaluates one line of input.
    pub fn eval(&mut self, line: &str) -> ReplResponse {
        let source = if self.pending.is_empty() {
            line.to_owned()
        } else {
            format!("{}\n{line}", self.pending)
        };
        self.pending.clear();
        if source.trim().is_empty() {
            return ReplResponse::default();
        }

        let lua = self.engine.lua();
        let name = format!("={REPL_CHUNK}");
        let expression = source
            .strip_prefix('=')
            .map(|rest| format!("return {rest}"));
        let compiled = match expression {
            Some(expr) => lua.load(expr).set_name(&name).into_function(),
            None => lua
                .load(format!("return {source}"))
                .set_name(&name)
                .into_function()
                .or_else(|_| lua.load(source.as_str()).set_name(&name).into_function()),
        };
        let function = match compiled {
            Ok(f) => f,
            Err(mlua::Error::SyntaxError {
                incomplete_input: true,
                ..
            }) => {
                self.pending = source;
                return ReplResponse {
                    incomplete: true,
                    ..ReplResponse::default()
                };
            }
            Err(e) => {
                let limits = self.engine.limits();
                return ReplResponse {
                    error: Some(crate::sandbox::convert_error(&e, REPL_CHUNK, &limits)),
                    ..ReplResponse::default()
                };
            }
        };
        let out = self
            .engine
            .execute(REPL_CHUNK, move |_| function.call::<MultiValue>(()));
        ReplResponse {
            output: out.output,
            values: out.values,
            error: out.error,
            incomplete: false,
        }
    }

    /// Completes the identifier path at the end of `input`, e.g.
    /// `"x = hexa.re"` → `["hexa.read", "hexa.redo", "hexa.replace"]`.
    /// Candidates are global names or keys of nested tables, sorted.
    pub fn complete(&self, input: &str) -> Vec<String> {
        let token_start = input
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
            .last()
            .map_or(input.len(), |(i, _)| i);
        let token = &input[token_start..];
        let (path, prefix) = match token.rfind('.') {
            Some(dot) => (&token[..dot], &token[dot + 1..]),
            None => ("", token),
        };
        let lua = self.engine.lua();
        let mut table = lua.globals();
        if !path.is_empty() {
            for segment in path.split('.') {
                match table.raw_get::<Value>(segment) {
                    Ok(Value::Table(t)) => table = t,
                    _ => return Vec::new(),
                }
            }
        }
        let mut out = table_keys(&table)
            .into_iter()
            .filter(|k| k.starts_with(prefix))
            .map(|k| {
                if path.is_empty() {
                    k
                } else {
                    format!("{path}.{k}")
                }
            })
            .collect::<Vec<_>>();
        out.sort();
        out.dedup();
        out
    }
}

fn table_keys(table: &Table) -> Vec<String> {
    table
        .pairs::<Value, Value>()
        .filter_map(Result::ok)
        .filter_map(|(k, _)| match k {
            Value::String(s) if is_identifier(&s.as_bytes()) => {
                Some(String::from_utf8_lossy(&s.as_bytes()).into_owned())
            }
            _ => None,
        })
        .collect()
}
