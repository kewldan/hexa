//! Language server for template sources (SCR-08): stdio JSON-RPC with
//! `Content-Length` framing. Supports `initialize`, `shutdown`/`exit`,
//! `textDocument/didOpen|didChange|didClose` (→ `publishDiagnostics`),
//! `textDocument/completion` and `textDocument/hover`. The dialect is chosen
//! by file extension (`.bt` 010 Editor, `.h`/`.hpp` C headers, `.ksy`
//! Kaitai, anything else the native / `ImHex` language).

use std::collections::HashMap;
use std::io::{self, BufRead, Write};

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic as LspDiagnostic, DiagnosticSeverity, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, Hover as LspHover, HoverContents,
    HoverParams, HoverProviderCapability, InitializeResult, MarkupContent, MarkupKind, Position,
    PublishDiagnosticsParams, Range, ServerCapabilities, ServerInfo, TextDocumentSyncCapability,
    TextDocumentSyncKind, Uri,
};
use serde_json::{Value, json};

use crate::diag::{Diagnostic, Severity};
use crate::editor::{CompletionKind, completions, diagnostics_with, hover};
use crate::parser::{Dialect, ParseOptions};

/// In-memory language server state.
#[derive(Debug, Default)]
pub struct Server {
    docs: HashMap<String, String>,
    shutdown: bool,
    exit: Option<i32>,
}

/// Dialect for a document URI.
pub fn dialect_for(uri: &str) -> Dialect {
    let ext = std::path::Path::new(uri)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("bt") => Dialect::Bt,
        Some("h" | "hpp" | "hh") => Dialect::C,
        Some("ksy") => Dialect::Ksy,
        _ => Dialect::Hexpat,
    }
}

fn to_lsp(d: &Diagnostic) -> LspDiagnostic {
    let pos = |line: u32, col: u32| Position::new(line.saturating_sub(1), col.saturating_sub(1));
    LspDiagnostic {
        range: Range::new(
            pos(d.line, d.col),
            pos(d.end_line.max(d.line), d.end_col.max(d.col)),
        ),
        severity: Some(match d.severity {
            Severity::Error => DiagnosticSeverity::ERROR,
            Severity::Warning => DiagnosticSeverity::WARNING,
            Severity::Info => DiagnosticSeverity::INFORMATION,
        }),
        source: Some("hexa-template".into()),
        message: d.message.clone(),
        ..LspDiagnostic::default()
    }
}

/// Diagnostics for a document of any supported dialect.
pub fn document_diagnostics(uri: &str, text: &str) -> Vec<Diagnostic> {
    match dialect_for(uri) {
        Dialect::Ksy => match crate::import::ksy::from_ksy(text) {
            Ok(_) => Vec::new(),
            Err(e) => vec![Diagnostic::error(
                crate::ast::Span::new(1, 1, 1, 1),
                e.to_string(),
            )],
        },
        dialect => diagnostics_with(
            text,
            &ParseOptions {
                dialect,
                ..ParseOptions::default()
            },
        ),
    }
}

impl Server {
    pub fn new() -> Self {
        Self::default()
    }

    /// Exit code once `exit` was received.
    pub fn exit_code(&self) -> Option<i32> {
        self.exit
    }

    /// Handles one incoming message; returns the messages to send.
    pub fn handle(&mut self, msg: &Value) -> Vec<Value> {
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let respond = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
        let error = |code: i64, message: &str| json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}});
        if self.shutdown && method != "exit" && id.is_some() {
            return vec![error(-32600, "server is shutting down")];
        }
        match method {
            "initialize" => {
                let result = InitializeResult {
                    capabilities: ServerCapabilities {
                        text_document_sync: Some(TextDocumentSyncCapability::Kind(
                            TextDocumentSyncKind::FULL,
                        )),
                        completion_provider: Some(CompletionOptions {
                            trigger_characters: Some(vec![".".into(), ":".into(), "[".into()]),
                            ..CompletionOptions::default()
                        }),
                        hover_provider: Some(HoverProviderCapability::Simple(true)),
                        ..ServerCapabilities::default()
                    },
                    server_info: Some(ServerInfo {
                        name: "hexa-template-lsp".into(),
                        version: Some(env!("CARGO_PKG_VERSION").into()),
                    }),
                };
                vec![respond(serde_json::to_value(result).unwrap_or(Value::Null))]
            }
            "initialized"
            | "$/cancelRequest"
            | "$/setTrace"
            | "workspace/didChangeConfiguration" => vec![],
            "shutdown" => {
                self.shutdown = true;
                vec![respond(Value::Null)]
            }
            "exit" => {
                self.exit = Some(i32::from(!self.shutdown));
                vec![]
            }
            "textDocument/didOpen" => {
                match serde_json::from_value::<DidOpenTextDocumentParams>(params) {
                    Ok(p) => {
                        let uri = p.text_document.uri.as_str().to_string();
                        self.docs.insert(uri.clone(), p.text_document.text);
                        vec![self.publish(&p.text_document.uri, &uri)]
                    }
                    Err(_) => vec![],
                }
            }
            "textDocument/didChange" => {
                match serde_json::from_value::<DidChangeTextDocumentParams>(params) {
                    Ok(p) => {
                        let uri = p.text_document.uri.as_str().to_string();
                        if let Some(change) = p.content_changes.into_iter().last() {
                            self.docs.insert(uri.clone(), change.text);
                        }
                        vec![self.publish(&p.text_document.uri, &uri)]
                    }
                    Err(_) => vec![],
                }
            }
            "textDocument/didClose" => {
                match serde_json::from_value::<DidCloseTextDocumentParams>(params) {
                    Ok(p) => {
                        self.docs.remove(p.text_document.uri.as_str());
                        let clear =
                            PublishDiagnosticsParams::new(p.text_document.uri, vec![], None);
                        vec![notification("textDocument/publishDiagnostics", &clear)]
                    }
                    Err(_) => vec![],
                }
            }
            "textDocument/completion" => match serde_json::from_value::<CompletionParams>(params) {
                Ok(p) => {
                    let uri = p.text_document_position.text_document.uri.as_str();
                    let pos = p.text_document_position.position;
                    let items = self
                        .docs
                        .get(uri)
                        .map(|text| completions(text, pos.line + 1, pos.character + 1))
                        .unwrap_or_default()
                        .into_iter()
                        .map(|c| CompletionItem {
                            label: c.label,
                            kind: Some(completion_kind(c.kind)),
                            detail: c.detail,
                            ..CompletionItem::default()
                        })
                        .collect();
                    let resp = CompletionResponse::Array(items);
                    vec![respond(serde_json::to_value(resp).unwrap_or(Value::Null))]
                }
                Err(e) => vec![error(-32602, &e.to_string())],
            },
            "textDocument/hover" => match serde_json::from_value::<HoverParams>(params) {
                Ok(p) => {
                    let uri = p.text_document_position_params.text_document.uri.as_str();
                    let pos = p.text_document_position_params.position;
                    let h = self
                        .docs
                        .get(uri)
                        .and_then(|t| hover(t, pos.line + 1, pos.character + 1));
                    let value = match h {
                        Some(h) => serde_json::to_value(LspHover {
                            contents: HoverContents::Markup(MarkupContent {
                                kind: MarkupKind::Markdown,
                                value: h.contents,
                            }),
                            range: Some(Range::new(
                                Position::new(h.line - 1, h.col - 1),
                                Position::new(h.line - 1, h.end_col - 1),
                            )),
                        })
                        .unwrap_or(Value::Null),
                        None => Value::Null,
                    };
                    vec![respond(value)]
                }
                Err(e) => vec![error(-32602, &e.to_string())],
            },
            _ if id.is_some() => vec![error(-32601, &format!("method not found: {method}"))],
            _ => vec![],
        }
    }

    fn publish(&self, uri: &Uri, key: &str) -> Value {
        let text = self.docs.get(key).map_or("", String::as_str);
        let diags = document_diagnostics(key, text).iter().map(to_lsp).collect();
        let params = PublishDiagnosticsParams::new(uri.clone(), diags, None);
        notification("textDocument/publishDiagnostics", &params)
    }
}

fn notification(method: &str, params: &impl serde::Serialize) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

fn completion_kind(k: CompletionKind) -> CompletionItemKind {
    match k {
        CompletionKind::Keyword => CompletionItemKind::KEYWORD,
        CompletionKind::Type => CompletionItemKind::TYPE_PARAMETER,
        CompletionKind::Struct => CompletionItemKind::STRUCT,
        CompletionKind::Enum => CompletionItemKind::ENUM,
        CompletionKind::EnumMember => CompletionItemKind::ENUM_MEMBER,
        CompletionKind::Function => CompletionItemKind::FUNCTION,
        CompletionKind::Field => CompletionItemKind::FIELD,
        CompletionKind::Variable => CompletionItemKind::VARIABLE,
        CompletionKind::Attribute => CompletionItemKind::PROPERTY,
    }
}

/// Reads one `Content-Length` framed JSON message; `Ok(None)` at EOF.
pub fn read_message(r: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut len: Option<usize> = None;
    let mut headers = false;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if headers {
                break;
            }
            continue;
        }
        headers = true;
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().ok();
        }
    }
    let len =
        len.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    if len > 64 << 20 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut buf = vec![0; len];
    r.read_exact(&mut buf)?;
    serde_json::from_slice(&buf)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Writes one `Content-Length` framed JSON message.
pub fn write_message(w: &mut impl Write, v: &Value) -> io::Result<()> {
    let body = serde_json::to_string(v).map_err(io::Error::other)?;
    write!(w, "Content-Length: {}\r\n\r\n{body}", body.len())?;
    w.flush()
}

/// Serves on stdin/stdout until `exit`; returns the exit code.
pub fn run_stdio() -> i32 {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut server = Server::new();
    loop {
        match read_message(&mut input) {
            Ok(Some(msg)) => {
                for reply in server.handle(&msg) {
                    if write_message(&mut out, &reply).is_err() {
                        return 1;
                    }
                }
                if let Some(code) = server.exit_code() {
                    return code;
                }
            }
            Ok(None) => return 1,
            Err(e) => {
                let _ = writeln!(io::stderr(), "hexa-template-lsp: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::needless_pass_by_value)]
    fn req(id: i64, method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    #[allow(clippy::needless_pass_by_value)]
    fn note(method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "method": method, "params": params})
    }

    #[test]
    fn session() {
        let mut s = Server::new();
        let r = s.handle(&req(1, "initialize", json!({"capabilities": {}})));
        assert_eq!(r[0]["result"]["capabilities"]["hoverProvider"], true);
        assert_eq!(r[0]["result"]["capabilities"]["textDocumentSync"], 1);
        assert_eq!(s.handle(&note("initialized", json!({}))).len(), 0);

        let uri = "file:///t/a.hexpat";
        let r = s.handle(&note(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": "hexpat", "version": 1,
                "text": "struct S { u8 a; Missing m; };\nS s @ 0;\n"}}),
        ));
        assert_eq!(r[0]["method"], "textDocument/publishDiagnostics");
        let d = &r[0]["params"]["diagnostics"];
        assert_eq!(d.as_array().unwrap().len(), 1);
        assert_eq!(d[0]["range"]["start"]["line"], 0);
        assert_eq!(d[0]["severity"], 1);
        assert!(d[0]["message"].as_str().unwrap().contains("Missing"));

        let r = s.handle(&note(
            "textDocument/didChange",
            json!({"textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{"text": "struct S { u8 a; u16 b; };\nS s @ 0;\nauto x = s.\n"}]}),
        ));
        let d = r[0]["params"]["diagnostics"].as_array().unwrap();
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0]["range"]["start"]["line"], 3);

        let r = s.handle(&req(
            2,
            "textDocument/completion",
            json!({"textDocument": {"uri": uri}, "position": {"line": 2, "character": 11}}),
        ));
        let labels: Vec<_> = r[0]["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["label"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(labels, ["a", "b"]);

        let r = s.handle(&req(
            3,
            "textDocument/hover",
            json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 18}}),
        ));
        assert!(
            r[0]["result"]["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("u16")
        );

        let r = s.handle(&req(4, "workspace/symbol", json!({})));
        assert_eq!(r[0]["error"]["code"], -32601);
        let r = s.handle(&req(5, "shutdown", Value::Null));
        assert!(r[0]["result"].is_null());
        s.handle(&note("exit", Value::Null));
        assert_eq!(s.exit_code(), Some(0));
    }

    #[test]
    fn framing() {
        let mut buf = Vec::new();
        write_message(&mut buf, &json!({"a": 1})).unwrap();
        write_message(&mut buf, &json!({"b": "é"})).unwrap();
        let mut r = io::Cursor::new(buf);
        assert_eq!(read_message(&mut r).unwrap().unwrap()["a"], 1);
        assert_eq!(read_message(&mut r).unwrap().unwrap()["b"], "é");
        assert!(read_message(&mut r).unwrap().is_none());
        let mut bad = io::Cursor::new(b"X: 1\r\n\r\n".to_vec());
        assert!(read_message(&mut bad).is_err());
    }

    #[test]
    fn dialects_by_extension() {
        assert_eq!(dialect_for("file:///a/B.BT"), Dialect::Bt);
        assert_eq!(dialect_for("file:///a/x.h"), Dialect::C);
        assert_eq!(dialect_for("file:///a/x.ksy"), Dialect::Ksy);
        assert_eq!(dialect_for("file:///a/x.pat"), Dialect::Hexpat);
        assert_eq!(
            document_diagnostics("x.ksy", "meta: {id: a}\nseq: [{id: b, type: nope}]").len(),
            1
        );
        assert_eq!(
            document_diagnostics("x.bt", "typedef struct { int a; } X;"),
            []
        );
    }
}
