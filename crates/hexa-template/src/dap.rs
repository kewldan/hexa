//! Minimal Debug Adapter Protocol server for templates (STRC-18): launch
//! with a template and a data file, line breakpoints, step in/over/out,
//! continue, stack trace, scopes and variables. Uses the same
//! `Content-Length` framing as the language server.

use std::io::{self, Write};
use std::path::Path;

use hexa_core::piece::Piece;
use hexa_core::{Cancel, Tree};
use serde_json::{Value, json};

use crate::debugger::{DebugEvent, Debugger, PauseReason};
use crate::eval::EvalOptions;
use crate::lsp::{read_message, write_message};
use crate::parser::{Dialect, ParseOptions, parse_strict};

const THREAD: i64 = 1;
const REF_LOCALS: i64 = 1;
const REF_FIELDS: i64 = 2;
const REF_POSITION: i64 = 3;

/// Debug adapter state.
#[derive(Debug, Default)]
pub struct Adapter {
    seq: i64,
    debugger: Option<Debugger>,
    source: String,
    stop_on_entry: bool,
    started: bool,
    /// Breakpoint lines; clients may send them before `launch`.
    breakpoints: Vec<u32>,
    done: bool,
    cancel: Cancel,
}

impl Adapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The client asked to disconnect.
    pub fn is_done(&self) -> bool {
        self.done
    }

    fn next_seq(&mut self) -> i64 {
        self.seq += 1;
        self.seq
    }

    #[allow(clippy::needless_pass_by_value)]
    fn response(
        &mut self,
        req: &Value,
        success: bool,
        body: Value,
        message: Option<String>,
    ) -> Value {
        let mut v = json!({
            "seq": self.next_seq(),
            "type": "response",
            "request_seq": req.get("seq").cloned().unwrap_or(Value::Null),
            "command": req.get("command").cloned().unwrap_or(Value::Null),
            "success": success,
            "body": body,
        });
        if let Some(m) = message {
            v["message"] = Value::String(m);
        }
        v
    }

    #[allow(clippy::needless_pass_by_value)]
    fn event(&mut self, event: &str, body: Value) -> Value {
        json!({"seq": self.next_seq(), "type": "event", "event": event, "body": body})
    }

    /// Launches from in-memory template source and data (used by `launch`).
    pub fn load(
        &mut self,
        source: &str,
        dialect: Dialect,
        data: Vec<u8>,
        opts: EvalOptions,
    ) -> crate::Result<()> {
        let program = parse_strict(
            source,
            &ParseOptions {
                dialect,
                ..ParseOptions::default()
            },
        )?;
        let tree = if data.is_empty() {
            Tree::new()
        } else {
            Tree::from_piece(Piece::bytes(data))
        };
        let mut debugger = Debugger::new(program, tree, opts);
        debugger.set_breakpoints(self.breakpoints.iter().copied());
        self.debugger = Some(debugger);
        Ok(())
    }

    /// Handles one request; returns responses and events to send.
    pub fn handle(&mut self, req: &Value) -> Vec<Value> {
        let command = req
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let args = req.get("arguments").cloned().unwrap_or(Value::Null);
        match command.as_str() {
            "initialize" => {
                let body = json!({
                    "supportsConfigurationDoneRequest": true,
                    "supportsStepBack": false,
                    "supportsEvaluateForHovers": false,
                });
                let r = self.response(req, true, body, None);
                let e = self.event("initialized", json!({}));
                vec![r, e]
            }
            "launch" => {
                let template = args
                    .get("template")
                    .or_else(|| args.get("program"))
                    .and_then(Value::as_str);
                let data = args.get("data").and_then(Value::as_str);
                let (Some(template), Some(data)) = (template, data) else {
                    return vec![self.response(
                        req,
                        false,
                        json!({}),
                        Some("launch needs `template` and `data` paths".into()),
                    )];
                };
                let loaded = (|| -> Result<(), String> {
                    let src = std::fs::read_to_string(template)
                        .map_err(|e| format!("{template}: {e}"))?;
                    let bytes = std::fs::read(data).map_err(|e| format!("{data}: {e}"))?;
                    let opts = EvalOptions {
                        root_type: args
                            .get("rootType")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        base_offset: args.get("offset").and_then(Value::as_u64).unwrap_or(0),
                        ..EvalOptions::default()
                    };
                    let dialect = crate::lsp::dialect_for(template);
                    if dialect == Dialect::Ksy {
                        return Err("Kaitai specifications cannot be debugged".into());
                    }
                    self.load(&src, dialect, bytes, opts)
                        .map_err(|e| e.to_string())?;
                    self.source = Path::new(template).to_string_lossy().into_owned();
                    Ok(())
                })();
                self.stop_on_entry = args
                    .get("stopOnEntry")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                match loaded {
                    Ok(()) => vec![self.response(req, true, json!({}), None)],
                    Err(m) => vec![self.response(req, false, json!({}), Some(m))],
                }
            }
            "setBreakpoints" => {
                let lines: Vec<u32> = args
                    .get("breakpoints")
                    .and_then(Value::as_array)
                    .map(|b| {
                        b.iter()
                            .filter_map(|x| x.get("line")?.as_u64())
                            .map(|l| l as u32)
                            .collect()
                    })
                    .or_else(|| {
                        args.get("lines").and_then(Value::as_array).map(|l| {
                            l.iter()
                                .filter_map(Value::as_u64)
                                .map(|l| l as u32)
                                .collect()
                        })
                    })
                    .unwrap_or_default();
                if let Some(d) = &mut self.debugger {
                    d.set_breakpoints(lines.iter().copied());
                }
                self.breakpoints.clone_from(&lines);
                let bps: Vec<Value> = lines
                    .iter()
                    .map(|l| json!({"verified": true, "line": l}))
                    .collect();
                vec![self.response(req, true, json!({"breakpoints": bps}), None)]
            }
            "configurationDone" => {
                let r = self.response(req, true, json!({}), None);
                let mut out = vec![r];
                if !self.started {
                    self.started = true;
                    let entry = self.stop_on_entry;
                    out.extend(self.drive(|d, c| {
                        if entry {
                            d.start(c)
                        } else {
                            d.start_running(c)
                        }
                    }));
                }
                out
            }
            "threads" => vec![self.response(
                req,
                true,
                json!({"threads": [{"id": THREAD, "name": "template"}]}),
                None,
            )],
            "stackTrace" => {
                let source = json!({"name": Path::new(&self.source).file_name().map(|n| n.to_string_lossy().into_owned()), "path": self.source});
                let frames: Vec<Value> = self
                    .debugger
                    .as_ref()
                    .and_then(Debugger::state)
                    .map(|s| {
                        s.call_stack
                            .iter()
                            .enumerate()
                            .map(|(i, f)| json!({"id": i, "name": f.name, "line": f.line, "column": 1, "source": source}))
                            .collect()
                    })
                    .unwrap_or_default();
                let total = frames.len();
                vec![self.response(
                    req,
                    true,
                    json!({"stackFrames": frames, "totalFrames": total}),
                    None,
                )]
            }
            "scopes" => {
                let scopes = json!([
                    {"name": "Locals", "variablesReference": REF_LOCALS, "expensive": false},
                    {"name": "Fields", "variablesReference": REF_FIELDS, "expensive": false},
                    {"name": "Position", "variablesReference": REF_POSITION, "expensive": false},
                ]);
                vec![self.response(req, true, json!({"scopes": scopes}), None)]
            }
            "variables" => {
                let reference = args
                    .get("variablesReference")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let vars: Vec<Value> = match self.debugger.as_ref().and_then(Debugger::state) {
                    Some(s) => match reference {
                        REF_LOCALS => s.locals.iter().map(|v| json!({"name": v.name, "value": v.value, "variablesReference": 0})).collect(),
                        REF_FIELDS => s.fields.iter().map(|v| json!({"name": v.name, "value": v.value, "variablesReference": 0})).collect(),
                        REF_POSITION => vec![
                            json!({"name": "$", "value": format!("0x{:X} ({})", s.offset, s.offset), "variablesReference": 0}),
                            json!({"name": "step", "value": s.step.to_string(), "variablesReference": 0}),
                        ],
                        _ => vec![],
                    },
                    None => vec![],
                };
                vec![self.response(req, true, json!({"variables": vars}), None)]
            }
            "next" | "stepIn" | "stepOut" | "continue" => {
                let mut out =
                    vec![self.response(req, true, json!({"allThreadsContinued": true}), None)];
                out.extend(self.drive(|d, c| match command.as_str() {
                    "next" => d.step_over(c),
                    "stepIn" => d.step_into(c),
                    "stepOut" => d.step_out(c),
                    _ => d.resume(c),
                }));
                out
            }
            "pause" => vec![self.response(
                req,
                false,
                json!({}),
                Some("evaluation pauses only at breakpoints and steps".into()),
            )],
            "disconnect" | "terminate" => {
                self.done = true;
                self.cancel.cancel();
                vec![self.response(req, true, json!({}), None)]
            }
            _ => vec![self.response(
                req,
                false,
                json!({}),
                Some(format!("unsupported request `{command}`")),
            )],
        }
    }

    fn drive(
        &mut self,
        f: impl FnOnce(&mut Debugger, &Cancel) -> crate::Result<DebugEvent>,
    ) -> Vec<Value> {
        let cancel = self.cancel.clone();
        let Some(d) = self.debugger.as_mut() else {
            return vec![self.event(
                "output",
                json!({"category": "stderr", "output": "no program launched\n"}),
            )];
        };
        let outcome = f(d, &cancel);
        let log: Vec<String> = d.result().map(|r| r.log.clone()).unwrap_or_default();
        let errors: Vec<String> = d
            .result()
            .map(|r| {
                r.errors
                    .iter()
                    .map(|e| format!("{}:{}: {}", e.line, e.col, e.message))
                    .collect()
            })
            .unwrap_or_default();
        match outcome {
            Ok(DebugEvent::Paused { reason, .. }) => {
                let reason = match reason {
                    PauseReason::Entry => "entry",
                    PauseReason::Step => "step",
                    PauseReason::Breakpoint => "breakpoint",
                };
                vec![self.event(
                    "stopped",
                    json!({"reason": reason, "threadId": THREAD, "allThreadsStopped": true}),
                )]
            }
            Ok(DebugEvent::Finished) => {
                let mut out = Vec::new();
                for l in log {
                    out.push(self.event(
                        "output",
                        json!({"category": "stdout", "output": format!("{l}\n")}),
                    ));
                }
                for e in &errors {
                    out.push(self.event(
                        "output",
                        json!({"category": "stderr", "output": format!("{e}\n")}),
                    ));
                }
                out.push(self.event("terminated", json!({})));
                out.push(self.event("exited", json!({"exitCode": i32::from(!errors.is_empty())})));
                out
            }
            Err(e) => vec![
                self.event(
                    "output",
                    json!({"category": "stderr", "output": format!("{e}\n")}),
                ),
                self.event("terminated", json!({})),
            ],
        }
    }
}

/// Serves DAP on stdin/stdout until `disconnect`; returns the exit code.
pub fn run_stdio() -> i32 {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut adapter = Adapter::new();
    loop {
        match read_message(&mut input) {
            Ok(Some(msg)) => {
                for reply in adapter.handle(&msg) {
                    if write_message(&mut out, &reply).is_err() {
                        return 1;
                    }
                }
                if adapter.is_done() {
                    return 0;
                }
            }
            Ok(None) => return 0,
            Err(e) => {
                let _ = writeln!(io::stderr(), "hexa-template-dap: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::needless_pass_by_value)]
    fn req(seq: i64, command: &str, arguments: Value) -> Value {
        json!({"seq": seq, "type": "request", "command": command, "arguments": arguments})
    }

    fn events(out: &[Value]) -> Vec<String> {
        out.iter()
            .filter(|v| v["type"] == "event")
            .map(|v| v["event"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn session() {
        let mut a = Adapter::new();
        let out = a.handle(&req(1, "initialize", json!({"adapterID": "hexa"})));
        assert_eq!(out[0]["success"], true);
        assert_eq!(events(&out), ["initialized"]);
        let src = "struct P {\n    u8 a;\n    u8 b;\n};\nP p;\nstd::print(\"done {}\", p.b);\n";
        a.load(src, Dialect::Hexpat, vec![1, 2], EvalOptions::default())
            .unwrap();
        a.stop_on_entry = true;
        let out = a.handle(&req(
            2,
            "setBreakpoints",
            json!({"source": {"path": "t.hexpat"}, "breakpoints": [{"line": 3}]}),
        ));
        assert_eq!(out[0]["body"]["breakpoints"][0]["verified"], true);
        let out = a.handle(&req(3, "configurationDone", json!({})));
        assert_eq!(out[1]["body"]["reason"], "entry");
        let out = a.handle(&req(4, "continue", json!({"threadId": 1})));
        assert_eq!(out[1]["body"]["reason"], "breakpoint");
        let out = a.handle(&req(5, "stackTrace", json!({"threadId": 1})));
        assert_eq!(out[0]["body"]["stackFrames"][0]["line"], 3);
        assert_eq!(out[0]["body"]["stackFrames"][0]["name"], "P p");
        let out = a.handle(&req(
            6,
            "variables",
            json!({"variablesReference": REF_FIELDS}),
        ));
        assert_eq!(out[0]["body"]["variables"][0]["name"], "a");
        let out = a.handle(&req(
            7,
            "variables",
            json!({"variablesReference": REF_POSITION}),
        ));
        assert_eq!(out[0]["body"]["variables"][0]["value"], "0x1 (1)");
        let out = a.handle(&req(8, "stepOut", json!({"threadId": 1})));
        assert_eq!(out[1]["body"]["reason"], "step");
        let out = a.handle(&req(9, "next", json!({"threadId": 1})));
        assert_eq!(events(&out), ["output", "terminated", "exited"]);
        assert_eq!(out[1]["body"]["output"], "done 2\n");
        let out = a.handle(&req(10, "disconnect", json!({})));
        assert_eq!(out[0]["success"], true);
        assert!(a.is_done());
    }

    #[test]
    fn breakpoints_set_before_launch_are_kept() {
        let mut a = Adapter::new();
        a.handle(&req(1, "initialize", json!({})));
        let out = a.handle(&req(
            2,
            "setBreakpoints",
            json!({"source": {"path": "t.hexpat"}, "breakpoints": [{"line": 2}]}),
        ));
        assert_eq!(out[0]["body"]["breakpoints"][0]["verified"], true);
        a.load(
            "u8 a;\nu8 b;\n",
            Dialect::Hexpat,
            vec![1, 2],
            EvalOptions::default(),
        )
        .unwrap();
        let out = a.handle(&req(3, "configurationDone", json!({})));
        assert_eq!(out[1]["body"]["reason"], "breakpoint");
    }

    #[test]
    fn launch_errors() {
        let mut a = Adapter::new();
        let out = a.handle(&req(1, "launch", json!({})));
        assert_eq!(out[0]["success"], false);
        let out = a.handle(&req(
            2,
            "launch",
            json!({"template": "/nonexistent/x.hexpat", "data": "/nonexistent/d"}),
        ));
        assert_eq!(out[0]["success"], false);
        let out = a.handle(&req(3, "bogus", json!({})));
        assert_eq!(out[0]["success"], false);
        assert!(
            a.load("struct {", Dialect::Hexpat, vec![], EvalOptions::default())
                .is_err()
        );
    }
}
