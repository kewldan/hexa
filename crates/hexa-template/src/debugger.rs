//! Template debugger (STRC-18): statement-level stepping with line
//! breakpoints, exposing the current offset, call stack and variables.
//!
//! Evaluation is deterministic, so the debugger re-runs the template up to
//! the next pause point instead of suspending a thread; every pause point is
//! identified by its step number. Each run is bounded by the usual limits.

use std::collections::BTreeSet;

use hexa_core::{Cancel, Tree};
use serde::{Deserialize, Serialize};

use crate::ast::Program;
use crate::diag::Result;
use crate::eval::{DebugState, EvalOptions, run};
use crate::result::TemplateResult;

/// Why execution stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PauseReason {
    Entry,
    Step,
    Breakpoint,
}

/// Outcome of a debugger command.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "event")]
pub enum DebugEvent {
    Paused {
        reason: PauseReason,
        state: DebugState,
    },
    /// Evaluation finished; the result holds the complete tree.
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Entry,
    Into,
    Over(usize),
    Out(usize),
    Continue,
}

/// Step-by-step evaluator with line breakpoints.
#[derive(Debug)]
pub struct Debugger {
    program: Program,
    tree: Tree,
    opts: EvalOptions,
    breakpoints: BTreeSet<u32>,
    /// Step number of the current pause point (0 before the start).
    position: u64,
    depth: usize,
    state: Option<DebugState>,
    result: Option<TemplateResult>,
    finished: bool,
}

impl Debugger {
    pub fn new(program: Program, tree: Tree, opts: EvalOptions) -> Self {
        Self {
            program,
            tree,
            opts,
            breakpoints: BTreeSet::new(),
            position: 0,
            depth: 0,
            state: None,
            result: None,
            finished: false,
        }
    }

    /// Replaces the breakpoint lines (1-based).
    pub fn set_breakpoints(&mut self, lines: impl IntoIterator<Item = u32>) {
        self.breakpoints = lines.into_iter().collect();
    }

    pub fn breakpoints(&self) -> impl Iterator<Item = u32> + '_ {
        self.breakpoints.iter().copied()
    }

    /// State at the current pause point.
    pub fn state(&self) -> Option<&DebugState> {
        self.state.as_ref()
    }

    /// Tree built so far (at the pause point) or the final tree.
    pub fn result(&self) -> Option<&TemplateResult> {
        self.result.as_ref()
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Restarts and stops before the first statement.
    pub fn start(&mut self, cancel: &Cancel) -> Result<DebugEvent> {
        self.position = 0;
        self.depth = 0;
        self.finished = false;
        self.run(Mode::Entry, cancel)
    }

    /// Restarts and runs to the first breakpoint.
    pub fn start_running(&mut self, cancel: &Cancel) -> Result<DebugEvent> {
        self.position = 0;
        self.depth = 0;
        self.finished = false;
        self.run(Mode::Continue, cancel)
    }

    /// Stops at the next statement, entering structures and functions.
    pub fn step_into(&mut self, cancel: &Cancel) -> Result<DebugEvent> {
        self.run(Mode::Into, cancel)
    }

    /// Stops at the next statement at the same or a shallower depth.
    pub fn step_over(&mut self, cancel: &Cancel) -> Result<DebugEvent> {
        self.run(Mode::Over(self.depth), cancel)
    }

    /// Runs until the current structure or function returns.
    pub fn step_out(&mut self, cancel: &Cancel) -> Result<DebugEvent> {
        self.run(Mode::Out(self.depth), cancel)
    }

    /// Runs to the next breakpoint or the end.
    pub fn resume(&mut self, cancel: &Cancel) -> Result<DebugEvent> {
        self.run(Mode::Continue, cancel)
    }

    fn run(&mut self, mode: Mode, cancel: &Cancel) -> Result<DebugEvent> {
        if self.finished {
            return Ok(DebugEvent::Finished);
        }
        let position = self.position;
        let breakpoints = &self.breakpoints;
        let mut reason = PauseReason::Step;
        let mut depth_at = 0;
        let mut hook = |ev: &crate::eval::Eval<'_, '_>, span: crate::ast::Span| -> bool {
            if ev.steps <= position {
                return false;
            }
            let depth = ev.call_depth();
            let stop = match mode {
                Mode::Entry => {
                    reason = PauseReason::Entry;
                    true
                }
                Mode::Into => true,
                Mode::Over(d) => depth <= d,
                Mode::Out(d) => depth < d,
                Mode::Continue => false,
            };
            let at_breakpoint = breakpoints.contains(&span.line);
            if stop || at_breakpoint {
                if !stop {
                    reason = PauseReason::Breakpoint;
                }
                depth_at = depth;
                return true;
            }
            false
        };
        let (result, paused) = run(
            &self.program,
            &self.tree,
            &self.opts,
            cancel,
            Some(&mut hook),
        )?;
        self.result = Some(result);
        if let Some(state) = paused {
            self.position = state.step;
            self.depth = depth_at;
            self.state = Some(state.clone());
            Ok(DebugEvent::Paused { reason, state })
        } else {
            self.finished = true;
            self.state = None;
            Ok(DebugEvent::Finished)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hexa_core::piece::Piece;

    const SRC: &str = "struct P {\n    u8 a;\n    u8 b;\n};\nfn twice(u8 v) {\n    return v * 2;\n};\nP p;\nauto t = twice(p.a);\nu8 last;\n";

    fn dbg() -> Debugger {
        let program = crate::parse(SRC).unwrap();
        let tree = Tree::from_piece(Piece::bytes(vec![1, 2, 3]));
        Debugger::new(program, tree, EvalOptions::default())
    }

    fn line(e: &DebugEvent) -> u32 {
        match e {
            DebugEvent::Paused { state, .. } => state.line,
            DebugEvent::Finished => 0,
        }
    }

    #[test]
    fn stepping() {
        let c = Cancel::new();
        let mut d = dbg();
        let e = d.start(&c).unwrap();
        assert!(matches!(
            e,
            DebugEvent::Paused {
                reason: PauseReason::Entry,
                ..
            }
        ));
        assert_eq!(line(&e), 8);
        // Into the struct.
        assert_eq!(line(&d.step_into(&c).unwrap()), 2);
        let s = d.state().unwrap();
        assert_eq!(s.offset, 0);
        assert_eq!(s.call_stack[0].name, "P p");
        assert_eq!(line(&d.step_into(&c).unwrap()), 3);
        assert_eq!(d.state().unwrap().offset, 1);
        assert_eq!(d.state().unwrap().fields[0].name, "a");
        // Out of the struct to the next top-level statement.
        assert_eq!(line(&d.step_out(&c).unwrap()), 9);
        // Over the function call.
        assert_eq!(line(&d.step_over(&c).unwrap()), 10);
        assert!(matches!(d.step_over(&c).unwrap(), DebugEvent::Finished));
        assert!(d.is_finished());
        assert!(d.result().unwrap().find("last").is_some());
    }

    #[test]
    fn breakpoints_and_locals() {
        let c = Cancel::new();
        let mut d = dbg();
        d.set_breakpoints([6]);
        let e = d.start_running(&c).unwrap();
        assert!(matches!(
            e,
            DebugEvent::Paused {
                reason: PauseReason::Breakpoint,
                ..
            }
        ));
        let s = d.state().unwrap();
        assert_eq!(s.line, 6);
        assert_eq!(
            s.locals,
            [crate::eval::Variable {
                name: "v".into(),
                value: "1".into()
            }]
        );
        assert_eq!(s.call_stack.len(), 2);
        // Partial tree at the pause point.
        assert!(d.result().unwrap().find("p.b").is_some());
        assert!(d.result().unwrap().find("last").is_none());
        assert!(matches!(d.resume(&c).unwrap(), DebugEvent::Finished));
    }
}
