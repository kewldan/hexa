//! Macro recording, storage, replay and conversion to Lua (SCR-03).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::engine::{is_identifier, quote_bytes};
use crate::host::ScriptHost;

/// Commands whose id starts with this prefix (macro start/stop/play) are never
/// recorded, so recording a replay does not nest.
pub const MACRO_COMMAND_PREFIX: &str = "macro.";

/// One recorded application command (SCR-03).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MacroStep {
    /// Application command id, e.g. `"edit.fill"`.
    pub command: String,
    /// Command arguments as JSON (`null` when the command takes none).
    #[serde(default)]
    pub args: serde_json::Value,
}

/// A named sequence of commands (SCR-03).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Macro {
    /// Display name.
    pub name: String,
    /// Optional description.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Steps in execution order.
    #[serde(default)]
    pub steps: Vec<MacroStep>,
}

/// Errors of macro storage and replay (SCR-03).
#[derive(Debug, thiserror::Error)]
pub enum MacroError {
    /// A step failed; earlier steps have already run.
    #[error("macro step {index} ({command}) failed: {message}")]
    Step {
        /// 0-based step index.
        index: usize,
        /// Command id of the failing step.
        command: String,
        /// Host error message.
        message: String,
    },
    /// Reading or writing the macro file failed.
    #[error("macro file {path}: {source}")]
    Io {
        /// File path.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// The macro JSON is malformed.
    #[error("invalid macro JSON: {0}")]
    Json(#[from] serde_json::Error),
}

impl Macro {
    /// An empty macro.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// Appends a step.
    pub fn push(&mut self, command: impl Into<String>, args: serde_json::Value) {
        self.steps.push(MacroStep {
            command: command.into(),
            args,
        });
    }

    /// Pretty-printed JSON.
    pub fn to_json(&self) -> Result<String, MacroError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Parses JSON.
    pub fn from_json(json: &str) -> Result<Self, MacroError> {
        Ok(serde_json::from_str(json)?)
    }

    /// Writes the macro as JSON to `path`.
    pub fn save(&self, path: &Path) -> Result<(), MacroError> {
        std::fs::write(path, self.to_json()?).map_err(|source| MacroError::Io {
            path: path.to_owned(),
            source,
        })
    }

    /// Reads a macro JSON file.
    pub fn load(path: &Path) -> Result<Self, MacroError> {
        let text = std::fs::read_to_string(path).map_err(|source| MacroError::Io {
            path: path.to_owned(),
            source,
        })?;
        Self::from_json(&text)
    }

    /// Replays all steps through `run_command`, stopping at the first failure.
    /// Returns the number of steps executed.
    pub fn replay<F>(&self, mut run_command: F) -> Result<usize, MacroError>
    where
        F: FnMut(&str, &serde_json::Value) -> Result<(), String>,
    {
        for (index, step) in self.steps.iter().enumerate() {
            run_command(&step.command, &step.args).map_err(|message| MacroError::Step {
                index,
                command: step.command.clone(),
                message,
            })?;
        }
        Ok(self.steps.len())
    }

    /// Replays through [`ScriptHost::run_command`].
    pub fn replay_on(&self, host: &dyn ScriptHost) -> Result<usize, MacroError> {
        self.replay(|command, args| host.run_command(command, args))
    }

    /// Converts the macro into an equivalent Lua script of
    /// `hexa.command(id, args)` calls.
    pub fn to_lua(&self) -> String {
        let mut out = String::new();
        for line in self.name.lines() {
            let _ = writeln!(out, "-- Macro: {line}");
        }
        for line in self.description.lines() {
            let _ = writeln!(out, "-- {line}");
        }
        for step in &self.steps {
            out.push_str("hexa.command(");
            out.push_str(&quote_bytes(step.command.as_bytes()));
            if !step.args.is_null() {
                out.push_str(", ");
                json_to_lua(&step.args, &mut out);
            }
            out.push_str(")\n");
        }
        out
    }
}

/// Writes a JSON value as a Lua expression that `hexa.command` converts back
/// to the same JSON (`null` → `hexa.null`, `[]` → `hexa.array()`).
fn json_to_lua(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Null => out.push_str("hexa.null"),
        serde_json::Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        serde_json::Value::Number(n) => {
            let _ = write!(out, "{n}");
        }
        serde_json::Value::String(s) => out.push_str(&quote_bytes(s.as_bytes())),
        serde_json::Value::Array(items) if items.is_empty() => out.push_str("hexa.array()"),
        serde_json::Value::Array(items) => {
            out.push('{');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                json_to_lua(item, out);
            }
            out.push('}');
        }
        serde_json::Value::Object(map) => {
            out.push('{');
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                if is_identifier(key.as_bytes()) {
                    out.push_str(key);
                } else {
                    out.push('[');
                    out.push_str(&quote_bytes(key.as_bytes()));
                    out.push(']');
                }
                out.push_str(" = ");
                json_to_lua(item, out);
            }
            out.push('}');
        }
    }
}

/// Records commands into a [`Macro`] while active (SCR-03).
#[derive(Debug, Default)]
pub struct MacroRecorder {
    current: Option<Macro>,
}

impl MacroRecorder {
    /// An idle recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts recording a new macro, discarding any unfinished one.
    pub fn start(&mut self, name: impl Into<String>) {
        self.current = Some(Macro::new(name));
    }

    /// Whether recording is active.
    pub fn is_recording(&self) -> bool {
        self.current.is_some()
    }

    /// Records a command if recording; returns whether it was recorded.
    /// Commands starting with [`MACRO_COMMAND_PREFIX`] are skipped.
    pub fn record(&mut self, command: &str, args: &serde_json::Value) -> bool {
        match &mut self.current {
            Some(m) if !command.starts_with(MACRO_COMMAND_PREFIX) => {
                m.push(command, args.clone());
                true
            }
            _ => false,
        }
    }

    /// Number of steps recorded so far.
    pub fn step_count(&self) -> usize {
        self.current.as_ref().map_or(0, |m| m.steps.len())
    }

    /// Stops recording and returns the macro.
    pub fn stop(&mut self) -> Option<Macro> {
        self.current.take()
    }

    /// Stops recording and discards the steps.
    pub fn cancel(&mut self) {
        self.current = None;
    }
}
