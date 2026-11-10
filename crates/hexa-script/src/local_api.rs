//! Localhost JSON API protocol and dispatcher (SCR-09).
//!
//! The application serves this behind an HTTP listener bound to localhost;
//! each request body is one [`ApiRequest`] and the `Authorization: Bearer`
//! header must carry the configured token.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::{ScriptEngine, ScriptOutput};
use crate::host::{ByteRange, DocId, FindQuery, ScriptHost, parse_hex};
use crate::sandbox::ScriptLimits;

/// Protocol version reported by `version`.
pub const API_VERSION: u32 = 1;

/// One API call (SCR-09). JSON: `{"id": 1, "method": "readRange",
/// "params": {"offset": 0, "len": 16}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "method",
    content = "params",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ApiCall {
    /// Protocol/app version.
    Version,
    /// Opens a file; result `{doc}`.
    OpenFile {
        /// File path.
        path: String,
        /// Open read-only.
        #[serde(default)]
        read_only: bool,
    },
    /// Lists open documents; result `[DocumentInfo]`.
    ListDocuments,
    /// Reads bytes; result `{doc, offset, len, data}` with hex `data`.
    ReadRange {
        /// Document (default: active).
        #[serde(default)]
        doc: Option<DocId>,
        /// Start offset.
        offset: u64,
        /// Byte count (≤ `maxReadLen`).
        len: u64,
    },
    /// Writes hex `data`; result `{doc, offset, len}`.
    Write {
        /// Document (default: active).
        #[serde(default)]
        doc: Option<DocId>,
        /// Start offset.
        offset: u64,
        /// Hex-encoded bytes.
        data: String,
        /// Overwrite (default) or insert.
        #[serde(default)]
        mode: WriteMode,
    },
    /// Moves the caret; result `{doc, offset}`.
    Goto {
        /// Document (default: active).
        #[serde(default)]
        doc: Option<DocId>,
        /// Target offset.
        offset: u64,
    },
    /// Searches; result `{doc, matches: [ByteRange]}`.
    Find {
        /// Document (default: active).
        #[serde(default)]
        doc: Option<DocId>,
        /// The query.
        query: FindQuery,
        /// Start offset for a single search.
        #[serde(default)]
        from: u64,
        /// Return all matches instead of the next one.
        #[serde(default)]
        all: bool,
        /// Maximum matches when `all` (default 1000).
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Runs a Lua script in a fresh sandbox; result [`ScriptOutput`].
    RunScript {
        /// Lua source.
        source: String,
        /// Chunk name for error messages.
        #[serde(default)]
        name: Option<String>,
    },
}

/// How `write` applies its data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WriteMode {
    /// Overwrite existing bytes.
    #[default]
    Overwrite,
    /// Insert before `offset`.
    Insert,
}

/// Request envelope (SCR-09).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiRequest {
    /// Client-chosen id echoed in the response.
    #[serde(default)]
    pub id: Value,
    /// The call.
    #[serde(flatten)]
    pub call: ApiCall,
}

/// Machine-readable error code (SCR-09).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApiErrorCode {
    /// Missing or wrong bearer token.
    Unauthorized,
    /// Malformed request.
    BadRequest,
    /// The call is disabled by configuration.
    Forbidden,
    /// Request exceeds a configured size limit.
    TooLarge,
    /// The host rejected the operation.
    HostError,
    /// A script failed (details in the result).
    ScriptError,
}

impl ApiErrorCode {
    /// HTTP status the listener should use.
    pub const fn http_status(self) -> u16 {
        match self {
            Self::Unauthorized => 401,
            Self::BadRequest => 400,
            Self::Forbidden => 403,
            Self::TooLarge => 413,
            Self::HostError | Self::ScriptError => 422,
        }
    }
}

/// Error payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    /// Code.
    pub code: ApiErrorCode,
    /// Human-readable message.
    pub message: String,
}

/// Response envelope (SCR-09).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiResponse {
    /// The request id (`null` if it could not be parsed).
    pub id: Value,
    /// Whether the call succeeded.
    pub ok: bool,
    /// Result on success (and the script output for `scriptError`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Error on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

impl ApiResponse {
    fn success(id: Value, result: Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    fn failure(id: Value, code: ApiErrorCode, message: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(ApiError {
                code,
                message: message.into(),
            }),
        }
    }

    /// HTTP status for this response (200 on success).
    pub fn http_status(&self) -> u16 {
        self.error.as_ref().map_or(200, |e| e.code.http_status())
    }

    /// Serializes to JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| {
            json!({"id": null, "ok": false, "error": {"code": "badRequest", "message": e.to_string()}})
                .to_string()
        })
    }
}

/// Configuration of the dispatcher (SCR-09).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiConfig {
    /// Bearer token; an empty token rejects every request.
    pub token: String,
    /// Maximum `readRange` length.
    pub max_read_len: u64,
    /// Whether `write` is allowed.
    pub allow_write: bool,
    /// Whether `runScript` is allowed.
    pub allow_scripts: bool,
    /// Limits of `runScript`.
    pub script_limits: ScriptLimits,
}

impl ApiConfig {
    /// Default limits (16 MiB reads, writes and scripts enabled) with `token`.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            max_read_len: 16 * 1024 * 1024,
            allow_write: true,
            allow_scripts: true,
            script_limits: ScriptLimits::default(),
        }
    }
}

/// Stateless request dispatcher (SCR-09).
#[derive(Debug, Clone)]
pub struct LocalApi {
    config: ApiConfig,
}

const DEFAULT_FIND_ALL_LIMIT: usize = 1000;

impl LocalApi {
    /// Creates a dispatcher.
    pub fn new(config: ApiConfig) -> Self {
        Self { config }
    }

    /// The configuration.
    pub fn config(&self) -> &ApiConfig {
        &self.config
    }

    /// Checks an `Authorization` header value (`Bearer <token>`) in constant
    /// time with respect to the token contents.
    pub fn is_authorized(&self, authorization: Option<&str>) -> bool {
        let Some(header) = authorization.map(str::trim) else {
            return false;
        };
        let Some((scheme, token)) = header.split_once(' ') else {
            return false;
        };
        if !scheme.eq_ignore_ascii_case("bearer") || self.config.token.is_empty() {
            return false;
        }
        constant_time_eq(token.trim().as_bytes(), self.config.token.as_bytes())
    }

    /// Handles one request body and returns the JSON response body. Use
    /// [`LocalApi::handle_request`] to also obtain the HTTP status.
    ///
    /// The host is passed as an `Arc` because `runScript` binds it into a Lua
    /// state whose callbacks must be `'static`.
    pub fn handle(
        &self,
        authorization: Option<&str>,
        request_json: &str,
        host: &Arc<dyn ScriptHost>,
    ) -> String {
        self.handle_request(authorization, request_json, host)
            .to_json()
    }

    /// Handles one request body and returns the typed response.
    pub fn handle_request(
        &self,
        authorization: Option<&str>,
        request_json: &str,
        host: &Arc<dyn ScriptHost>,
    ) -> ApiResponse {
        if !self.is_authorized(authorization) {
            return ApiResponse::failure(
                Value::Null,
                ApiErrorCode::Unauthorized,
                "missing or invalid bearer token",
            );
        }
        let raw: Value = match serde_json::from_str(request_json) {
            Ok(v) => v,
            Err(e) => {
                return ApiResponse::failure(
                    Value::Null,
                    ApiErrorCode::BadRequest,
                    format!("invalid JSON: {e}"),
                );
            }
        };
        let id = raw.get("id").cloned().unwrap_or(Value::Null);
        let request: ApiRequest = match serde_json::from_value(raw) {
            Ok(r) => r,
            Err(e) => {
                return ApiResponse::failure(id, ApiErrorCode::BadRequest, e.to_string());
            }
        };
        self.dispatch(request, host)
    }

    fn dispatch(&self, request: ApiRequest, host: &Arc<dyn ScriptHost>) -> ApiResponse {
        let id = request.id;
        let h = host.as_ref();
        let host_fail = |id: Value, e: String| ApiResponse::failure(id, ApiErrorCode::HostError, e);
        let resolve = |doc: Option<DocId>| {
            doc.or_else(|| h.active_document())
                .ok_or_else(|| "no active document".to_owned())
        };
        let result: Result<Value, String> = match request.call {
            ApiCall::Version => Ok(json!({
                "apiVersion": API_VERSION,
                "version": env!("CARGO_PKG_VERSION"),
            })),
            ApiCall::OpenFile { path, read_only } => h
                .open_file(&path, read_only)
                .map(|doc| json!({ "doc": doc })),
            ApiCall::ListDocuments => Ok(to_value(&h.documents())),
            ApiCall::ReadRange { doc, offset, len } => {
                if len > self.config.max_read_len {
                    return ApiResponse::failure(
                        id,
                        ApiErrorCode::TooLarge,
                        format!("read length {len} exceeds {}", self.config.max_read_len),
                    );
                }
                resolve(doc).and_then(|doc| {
                    h.read(doc, offset, len).map(|bytes| {
                        json!({
                            "doc": doc,
                            "offset": offset,
                            "len": bytes.len(),
                            "data": hex::encode(bytes),
                        })
                    })
                })
            }
            ApiCall::Write {
                doc,
                offset,
                data,
                mode,
            } => {
                if !self.config.allow_write {
                    return ApiResponse::failure(
                        id,
                        ApiErrorCode::Forbidden,
                        "writes are disabled",
                    );
                }
                let bytes = match parse_hex(&data) {
                    Ok(b) => b,
                    Err(e) => return ApiResponse::failure(id, ApiErrorCode::BadRequest, e),
                };
                resolve(doc).and_then(|doc| {
                    match mode {
                        WriteMode::Overwrite => h.write(doc, offset, &bytes),
                        WriteMode::Insert => h.insert(doc, offset, &bytes),
                    }
                    .map(|()| json!({ "doc": doc, "offset": offset, "len": bytes.len() }))
                })
            }
            ApiCall::Goto { doc, offset } => resolve(doc).and_then(|doc| {
                h.goto(doc, offset)
                    .map(|()| json!({ "doc": doc, "offset": offset }))
            }),
            ApiCall::Find {
                doc,
                query,
                from,
                all,
                limit,
            } => resolve(doc).and_then(|doc| {
                let matches: Result<Vec<ByteRange>, String> = if all {
                    h.find_all(doc, &query, limit.unwrap_or(DEFAULT_FIND_ALL_LIMIT))
                } else {
                    h.find_next(doc, &query, from)
                        .map(|m| m.into_iter().collect())
                };
                matches.map(|m| json!({ "doc": doc, "matches": m }))
            }),
            ApiCall::RunScript { source, name } => {
                if !self.config.allow_scripts {
                    return ApiResponse::failure(
                        id,
                        ApiErrorCode::Forbidden,
                        "scripts are disabled",
                    );
                }
                return self.run_script(id, &source, name.as_deref(), host);
            }
        };
        match result {
            Ok(value) => ApiResponse::success(id, value),
            Err(e) => host_fail(id, e),
        }
    }

    fn run_script(
        &self,
        id: Value,
        source: &str,
        name: Option<&str>,
        host: &Arc<dyn ScriptHost>,
    ) -> ApiResponse {
        let engine = match ScriptEngine::new(Arc::clone(host), self.config.script_limits) {
            Ok(engine) => engine,
            Err(e) => return ApiResponse::failure(id, ApiErrorCode::ScriptError, e.to_string()),
        };
        let output: ScriptOutput = engine.run(source, name.unwrap_or("api"));
        match &output.error {
            None => ApiResponse::success(id, to_value(&output)),
            Some(e) => {
                let mut response =
                    ApiResponse::failure(id, ApiErrorCode::ScriptError, e.to_string());
                response.result = Some(to_value(&output));
                response
            }
        }
    }
}

fn to_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
