//! Sandboxed Lua 5.4 states with memory, instruction and time limits plus a
//! cancellation flag (SCR-01, SRCH-08).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use mlua::{HookTriggers, Lua, LuaOptions, StdLib, VmState};

use crate::error::ScriptError;

/// The VM instruction hook fires after this many instructions; budgets are
/// therefore enforced with this granularity.
pub const HOOK_INTERVAL: u32 = 1000;

/// Resource limits of a Lua state (SCR-01).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptLimits {
    /// Maximum Lua heap size in bytes.
    pub memory_bytes: usize,
    /// Maximum VM instructions per run (`None` = unlimited).
    pub instruction_budget: Option<u64>,
    /// Maximum wall-clock time per run (`None` = unlimited).
    pub timeout: Option<Duration>,
}

impl Default for ScriptLimits {
    /// 64 MiB heap, 500 million instructions, 30 seconds.
    fn default() -> Self {
        Self {
            memory_bytes: 64 * 1024 * 1024,
            instruction_budget: Some(500_000_000),
            timeout: Some(Duration::from_secs(30)),
        }
    }
}

/// A shareable cancellation flag (SCR-01). Cloning shares the flag; the UI
/// keeps a clone and calls [`CancelToken::cancel`] from another thread.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A fresh, non-cancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation of the running script.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Clears the flag so the state can run again.
    pub fn reset(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Reason a run was aborted by the watchdog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Abort {
    Cancelled,
    Budget(u64),
    Timeout(Duration),
}

impl Abort {
    pub(crate) fn into_error(self) -> ScriptError {
        match self {
            Self::Cancelled => ScriptError::Cancelled,
            Self::Budget(budget) => ScriptError::BudgetExceeded { budget },
            Self::Timeout(t) => ScriptError::Timeout {
                millis: u64::try_from(t.as_millis()).unwrap_or(u64::MAX),
            },
        }
    }

    fn to_lua_error(self) -> mlua::Error {
        mlua::Error::RuntimeError(self.into_error().to_string())
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Shared state between a Lua state's instruction hook and its owner.
#[derive(Debug)]
pub(crate) struct Watchdog {
    limits: Mutex<ScriptLimits>,
    cancel: CancelToken,
    steps: AtomicU64,
    started: Mutex<Option<Instant>>,
    abort: Mutex<Option<Abort>>,
}

impl Watchdog {
    fn new(limits: ScriptLimits, cancel: CancelToken) -> Self {
        Self {
            limits: Mutex::new(limits),
            cancel,
            steps: AtomicU64::new(0),
            started: Mutex::new(None),
            abort: Mutex::new(None),
        }
    }

    pub(crate) fn limits(&self) -> ScriptLimits {
        *lock(&self.limits)
    }

    pub(crate) fn set_limits(&self, limits: ScriptLimits) {
        *lock(&self.limits) = limits;
    }

    pub(crate) fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Starts a budgeted run: resets the counters and the abort reason.
    pub(crate) fn begin(&self) {
        self.steps.store(0, Ordering::SeqCst);
        *lock(&self.abort) = None;
        *lock(&self.started) = Some(Instant::now());
    }

    /// Ends the run; the hook stops enforcing limits.
    pub(crate) fn end(&self) {
        *lock(&self.started) = None;
    }

    /// Returns and clears the abort reason of the last run.
    pub(crate) fn take_abort(&self) -> Option<Abort> {
        lock(&self.abort).take()
    }

    /// Checks cancellation and the wall clock without counting instructions;
    /// `None` outside a run.
    pub(crate) fn check(&self) -> Option<Abort> {
        let start = (*lock(&self.started))?;
        let mut abort = lock(&self.abort);
        if abort.is_none() {
            if self.cancel.is_cancelled() {
                *abort = Some(Abort::Cancelled);
            } else if let Some(timeout) = self.limits().timeout
                && start.elapsed() > timeout
            {
                *abort = Some(Abort::Timeout(timeout));
            }
        }
        *abort
    }

    fn tick(&self) -> mlua::Result<VmState> {
        let Some(start) = *lock(&self.started) else {
            return Ok(VmState::Continue);
        };
        let mut abort = lock(&self.abort);
        if let Some(reason) = *abort {
            // Re-raise so `pcall` loops cannot swallow the abort for long.
            return Err(reason.to_lua_error());
        }
        let limits = self.limits();
        let steps = self
            .steps
            .fetch_add(u64::from(HOOK_INTERVAL), Ordering::SeqCst)
            + u64::from(HOOK_INTERVAL);
        let reason = if self.cancel.is_cancelled() {
            Some(Abort::Cancelled)
        } else if let Some(budget) = limits.instruction_budget.filter(|b| steps > *b) {
            Some(Abort::Budget(budget))
        } else {
            limits
                .timeout
                .filter(|t| start.elapsed() > *t)
                .map(Abort::Timeout)
        };
        match reason {
            Some(reason) => {
                *abort = Some(reason);
                Err(reason.to_lua_error())
            }
            None => Ok(VmState::Continue),
        }
    }
}

/// Removes everything that touches the outside world from the base library.
/// `io`, `os`, `package` and `debug` are never loaded.
const SANDBOX_PRELUDE: &str = r#"
dofile = nil
loadfile = nil
string.dump = nil
do
  local raw_load = load
  load = function(chunk, name, _mode, env)
    if env == nil then
      return raw_load(chunk, name, "t")
    end
    return raw_load(chunk, name, "t", env)
  end
  local raw_gc = collectgarbage
  collectgarbage = function(opt, ...)
    if opt == nil or opt == "collect" or opt == "count" or opt == "step" then
      return raw_gc(opt, ...)
    end
    error("collectgarbage option not allowed: " .. tostring(opt), 2)
  end
end
print = function() end
"#;

/// Creates a sandboxed Lua 5.4 state with `string`, `table`, `math`, `utf8`
/// and `coroutine`, the memory limit applied and the watchdog hook installed.
pub(crate) fn create_sandbox(
    limits: ScriptLimits,
    cancel: CancelToken,
) -> Result<(Lua, Arc<Watchdog>), ScriptError> {
    let libs = StdLib::STRING | StdLib::TABLE | StdLib::MATH | StdLib::UTF8 | StdLib::COROUTINE;
    let lua = Lua::new_with(libs, LuaOptions::default()).map_err(ScriptError::internal)?;
    lua.load(SANDBOX_PRELUDE)
        .set_name("=sandbox")
        .exec()
        .map_err(ScriptError::internal)?;
    lua.set_memory_limit(limits.memory_bytes)
        .map_err(ScriptError::internal)?;
    let watchdog = Arc::new(Watchdog::new(limits, cancel));
    install_protected_call_guards(&lua, &watchdog).map_err(ScriptError::internal)?;
    let hook = Arc::clone(&watchdog);
    lua.set_global_hook(
        HookTriggers::new().every_nth_instruction(HOOK_INTERVAL),
        move |_, _| hook.tick(),
    )
    .map_err(ScriptError::internal)?;
    Ok((lua, watchdog))
}

/// Wraps `pcall`, `xpcall` and `coroutine.resume` so that an aborted run
/// re-raises as soon as a protected call returns; otherwise a loop around
/// `pcall` could keep swallowing the hook's error.
const GUARD_WRAPPERS: &str = r"
local check = ...
local raw_pcall, raw_xpcall, raw_resume = pcall, xpcall, coroutine.resume
pcall = function(...) return check(raw_pcall(...)) end
xpcall = function(...) return check(raw_xpcall(...)) end
coroutine.resume = function(...) return check(raw_resume(...)) end
";

fn install_protected_call_guards(lua: &Lua, watchdog: &Arc<Watchdog>) -> mlua::Result<()> {
    let watchdog = Arc::clone(watchdog);
    let check =
        lua.create_function(move |_, results: mlua::MultiValue| match watchdog.check() {
            Some(abort) => Err(abort.to_lua_error()),
            None => Ok(results),
        })?;
    lua.load(GUARD_WRAPPERS)
        .set_name("=sandbox")
        .call::<()>(check)
}

/// Converts an `mlua` error into a [`ScriptError`], extracting the line of
/// the innermost frame belonging to `chunk`.
pub(crate) fn convert_error(err: &mlua::Error, chunk: &str, limits: &ScriptLimits) -> ScriptError {
    match err {
        mlua::Error::SyntaxError {
            message,
            incomplete_input,
        } => {
            let (line, message) = split_location(message, chunk);
            ScriptError::Syntax {
                message,
                line,
                incomplete: *incomplete_input,
            }
        }
        mlua::Error::MemoryError(_) => ScriptError::MemoryLimit {
            limit: limits.memory_bytes,
        },
        mlua::Error::CallbackError { traceback, cause } => {
            match convert_error(cause, chunk, limits) {
                ScriptError::Runtime {
                    message,
                    line: None,
                } => ScriptError::Runtime {
                    message,
                    line: find_line(traceback, chunk),
                },
                other => other,
            }
        }
        mlua::Error::WithContext { cause, .. } => convert_error(cause, chunk, limits),
        mlua::Error::RuntimeError(message) => {
            let (line, message) = split_location(message, chunk);
            ScriptError::Runtime { message, line }
        }
        mlua::Error::ExternalError(e) => ScriptError::Runtime {
            message: e.to_string(),
            line: None,
        },
        other => {
            let text = other.to_string();
            let (line, message) = split_location(&text, chunk);
            ScriptError::Runtime { message, line }
        }
    }
}

/// Strips a leading `chunk:LINE: ` prefix; falls back to searching anywhere.
fn split_location(message: &str, chunk: &str) -> (Option<u32>, String) {
    let (message, traceback) = match message.find("\nstack traceback:") {
        Some(pos) => (&message[..pos], &message[pos..]),
        None => (message, ""),
    };
    let prefix = format!("{chunk}:");
    if let Some(rest) = message.strip_prefix(&prefix) {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(line) = digits.parse::<u32>()
            && let Some(tail) = rest[digits.len()..].strip_prefix(':')
        {
            return (Some(line), tail.trim_start().to_owned());
        }
    }
    let line = find_line(message, chunk).or_else(|| find_line(traceback, chunk));
    (line, message.to_owned())
}

/// Finds the first `chunk:LINE:` occurrence in `text`.
fn find_line(text: &str, chunk: &str) -> Option<u32> {
    let prefix = format!("{chunk}:");
    let mut rest = text;
    while let Some(pos) = rest.find(&prefix) {
        let after = &rest[pos + prefix.len()..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() && after[digits.len()..].starts_with(':') {
            return digits.parse().ok();
        }
        rest = after;
    }
    None
}
