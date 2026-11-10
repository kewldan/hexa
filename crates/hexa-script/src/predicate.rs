//! Lua predicate search over chunked data (SRCH-08).

use mlua::{Function, Lua, Value};

use crate::error::ScriptError;
use crate::sandbox::{self, CancelToken, ScriptLimits, Watchdog};
use std::sync::Arc;

const CHUNK: &str = "predicate";

/// Options of a [`PredicateSearch`] (SRCH-08).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PredicateOptions {
    /// Bytes passed to the predicate per offset (shorter at the end of data).
    pub window: usize,
    /// Only offsets that are multiples of this value are tested (≥ 1).
    pub alignment: u64,
    /// The search stops after this many matches.
    pub max_matches: usize,
    /// Budget for the whole search (instructions/time are counted from
    /// [`PredicateSearch::compile`]).
    pub limits: ScriptLimits,
}

impl Default for PredicateOptions {
    fn default() -> Self {
        Self {
            window: 16,
            alignment: 1,
            max_matches: 10_000,
            limits: ScriptLimits::default(),
        }
    }
}

/// A compiled `function(offset, bytes) return bool end` evaluated at every
/// (aligned) offset of a stream fed in contiguous chunks (SRCH-08).
///
/// Windows spanning chunk boundaries are handled by carrying the last
/// `window - 1` bytes over; [`PredicateSearch::finish`] evaluates the tail with
/// truncated windows. The predicate runs in its own sandbox without the
/// `hexa` API.
pub struct PredicateSearch {
    lua: Lua,
    watchdog: Arc<Watchdog>,
    predicate: Function,
    options: PredicateOptions,
    carry: Vec<u8>,
    carry_offset: Option<u64>,
    matches: usize,
    finished: bool,
}

impl std::fmt::Debug for PredicateSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PredicateSearch")
            .field("options", &self.options)
            .field("carry_offset", &self.carry_offset)
            .field("matches", &self.matches)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl PredicateSearch {
    /// Compiles `source`, which must evaluate to a function — either an
    /// expression (`function(o, b) ... end`) or a chunk returning one.
    pub fn compile(source: &str, options: PredicateOptions) -> Result<Self, ScriptError> {
        if options.window == 0 || options.alignment == 0 {
            return Err(ScriptError::InvalidInput {
                message: "window and alignment must be at least 1".to_owned(),
            });
        }
        let (lua, watchdog) = sandbox::create_sandbox(options.limits, CancelToken::new())?;
        let name = format!("={CHUNK}");
        let convert = |e: &mlua::Error| sandbox::convert_error(e, CHUNK, &options.limits);
        let chunk = lua
            .load(format!("return {source}"))
            .set_name(&name)
            .into_function()
            .or_else(|_| lua.load(source).set_name(&name).into_function())
            .map_err(|e| convert(&e))?;
        watchdog.begin();
        let value = chunk.call::<Value>(());
        if let Some(abort) = watchdog.take_abort() {
            return Err(abort.into_error());
        }
        let predicate = match value.map_err(|e| convert(&e))? {
            Value::Function(f) => f,
            other => {
                return Err(ScriptError::InvalidInput {
                    message: format!("predicate must be a function, got {}", other.type_name()),
                });
            }
        };
        Ok(Self {
            lua,
            watchdog,
            predicate,
            options,
            carry: Vec::new(),
            carry_offset: None,
            matches: 0,
            finished: false,
        })
    }

    /// Flag that aborts the search from another thread.
    pub fn cancel_token(&self) -> CancelToken {
        self.watchdog.cancel_token()
    }

    /// Whether `max_matches` was reached (further input is ignored).
    pub fn is_limit_reached(&self) -> bool {
        self.matches >= self.options.max_matches
    }

    /// Feeds the next chunk starting at absolute `offset`; chunks must be
    /// contiguous. Returns matching offsets whose full window is available.
    pub fn feed(&mut self, offset: u64, chunk: &[u8]) -> Result<Vec<u64>, ScriptError> {
        if self.finished {
            return Err(ScriptError::InvalidInput {
                message: "search already finished".to_owned(),
            });
        }
        let base = match self.carry_offset {
            None => offset,
            Some(carry_offset) => {
                let expected = carry_offset + self.carry.len() as u64;
                if offset != expected {
                    return Err(ScriptError::InvalidInput {
                        message: format!("expected chunk at offset {expected}, got {offset}"),
                    });
                }
                carry_offset
            }
        };
        let mut buffer = std::mem::take(&mut self.carry);
        buffer.extend_from_slice(chunk);
        let window = self.options.window;
        let evaluable = buffer.len().saturating_sub(window - 1);
        let found = self.evaluate(base, &buffer, 0..evaluable, false)?;
        self.carry = buffer[evaluable..].to_vec();
        self.carry_offset = Some(base + evaluable as u64);
        Ok(found)
    }

    /// Evaluates the remaining offsets near the end of data with truncated
    /// windows and ends the search.
    pub fn finish(&mut self) -> Result<Vec<u64>, ScriptError> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;
        let buffer = std::mem::take(&mut self.carry);
        let base = self.carry_offset.unwrap_or(0);
        self.evaluate(base, &buffer, 0..buffer.len(), true)
    }

    /// Drives the whole search: reads `total_len` bytes starting at `start`
    /// through `read(offset, len)` in chunks of `chunk_size`.
    pub fn scan<R>(
        &mut self,
        start: u64,
        total_len: u64,
        chunk_size: usize,
        mut read: R,
    ) -> Result<Vec<u64>, ScriptError>
    where
        R: FnMut(u64, usize) -> Result<Vec<u8>, String>,
    {
        let chunk_size = chunk_size.max(1);
        let end = start.saturating_add(total_len);
        let mut offset = start;
        let mut found = Vec::new();
        while offset < end && !self.is_limit_reached() {
            let len = usize::try_from(end - offset).map_or(chunk_size, |l| l.min(chunk_size));
            let data =
                read(offset, len).map_err(|message| ScriptError::InvalidInput { message })?;
            if data.is_empty() {
                break;
            }
            found.extend(self.feed(offset, &data)?);
            offset += data.len() as u64;
        }
        found.extend(self.finish()?);
        Ok(found)
    }

    fn evaluate(
        &mut self,
        base: u64,
        buffer: &[u8],
        positions: std::ops::Range<usize>,
        truncate: bool,
    ) -> Result<Vec<u64>, ScriptError> {
        let mut found = Vec::new();
        let align = self.options.alignment;
        let window = self.options.window;
        // The watchdog's timer runs from compile(); keep it armed while evaluating.
        for pos in positions {
            if self.is_limit_reached() {
                break;
            }
            let offset = base + pos as u64;
            if !offset.is_multiple_of(align) {
                continue;
            }
            let end = if truncate {
                buffer.len().min(pos + window)
            } else {
                pos + window
            };
            if let Some(abort) = self.watchdog.check() {
                return Err(abort.into_error());
            }
            let result = self
                .lua
                .create_string(&buffer[pos..end])
                .and_then(|bytes| self.predicate.call::<Value>((offset, bytes)));
            if let Some(abort) = self.watchdog.take_abort() {
                return Err(abort.into_error());
            }
            let value =
                result.map_err(|e| sandbox::convert_error(&e, CHUNK, &self.options.limits))?;
            if !matches!(value, Value::Nil | Value::Boolean(false)) {
                found.push(offset);
                self.matches += 1;
            }
        }
        Ok(found)
    }
}
