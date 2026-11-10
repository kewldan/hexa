//! HTTP(S) range provider (CORE-02 / FILE-16).
//!
//! The object is probed with `HEAD` (falling back to a one-byte ranged `GET`
//! when `HEAD` is refused or incomplete) to learn its size and whether the
//! server honours `Range`. Reads then fetch only the pages they need, with
//! retries on transient failures. Servers without range support are served by
//! downloading the object once, up to [`TransferOptions::max_full_download`].
//! The source is read-only.

use std::fmt;
use std::io;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use hexa_core::provider::{Capabilities, Provider};
use serde::{Deserialize, Serialize};
use ureq::Agent;

use crate::cache::{CacheOptions, PageCache, check_range};
use crate::error::{Error, Result};

/// Network behaviour shared by the HTTP and S3 sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TransferOptions {
    /// Extra attempts after a transient failure (connection error, timeout,
    /// 408, 429, 5xx).
    pub retries: u32,
    /// Delay before the first retry; doubled on each further retry.
    pub retry_delay_ms: u64,
    /// Timeout of one whole request.
    pub timeout_ms: u64,
    /// Largest object downloaded whole when the server ignores `Range`.
    pub max_full_download: u64,
    pub cache: CacheOptions,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self {
            retries: 3,
            retry_delay_ms: 250,
            timeout_ms: 30_000,
            max_full_download: 64 * 1024 * 1024,
            cache: CacheOptions::default(),
        }
    }
}

/// Credentials sent with every request.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum HttpAuth {
    /// `Authorization: Basic base64(user:password)`.
    Basic { user: String, password: String },
    /// `Authorization: Bearer <token>`.
    Bearer { token: String },
}

impl fmt::Debug for HttpAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Basic { user, .. } => f
                .debug_struct("Basic")
                .field("user", user)
                .finish_non_exhaustive(),
            Self::Bearer { .. } => f.debug_struct("Bearer").finish_non_exhaustive(),
        }
    }
}

impl HttpAuth {
    /// Value of the `Authorization` header.
    pub fn header_value(&self) -> String {
        match self {
            Self::Basic { user, password } => format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
            ),
            Self::Bearer { token } => format!("Bearer {token}"),
        }
    }
}

/// How to open an HTTP(S) source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HttpOptions {
    pub auth: Option<HttpAuth>,
    /// Extra request headers (name, value).
    pub headers: Vec<(String, String)>,
    pub transfer: TransferOptions,
}

/// What the server told about the object when it was opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteInfo {
    pub url: String,
    pub len: u64,
    /// The server honours `Range`; otherwise the object is held in memory.
    pub accepts_ranges: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// Adds per-request headers (credentials, signatures).
pub(crate) trait Authorizer: Send + Sync + fmt::Debug {
    /// Every header to send with a `method` request whose request-specific
    /// headers (such as `Range`) are `extra`; the result must include `extra`.
    fn headers(&self, method: &str, extra: &[(String, String)]) -> Vec<(String, String)>;
}

/// Fixed headers plus an optional `Authorization`.
#[derive(Debug)]
struct StaticAuth {
    headers: Vec<(String, String)>,
}

impl Authorizer for StaticAuth {
    fn headers(&self, _method: &str, extra: &[(String, String)]) -> Vec<(String, String)> {
        let mut all = self.headers.clone();
        all.extend_from_slice(extra);
        all
    }
}

/// A received response.
#[derive(Debug)]
pub(crate) struct Reply {
    pub status: u16,
    headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn header_owned(&self, name: &str) -> Option<String> {
        self.header(name).map(str::to_owned)
    }
}

/// Blocking HTTP client with retries.
#[derive(Debug)]
pub(crate) struct Client {
    agent: Agent,
    transfer: TransferOptions,
}

fn retryable_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

fn retryable_error(e: &ureq::Error) -> bool {
    matches!(
        e,
        ureq::Error::Io(_)
            | ureq::Error::Timeout(_)
            | ureq::Error::ConnectionFailed
            | ureq::Error::Protocol(_)
    )
}

impl Client {
    pub(crate) fn new(transfer: TransferOptions) -> Self {
        let config = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_millis(transfer.timeout_ms.max(1))))
            .user_agent(concat!("hexa/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: Agent::new_with_config(config),
            transfer,
        }
    }

    pub(crate) fn transfer(&self) -> &TransferOptions {
        &self.transfer
    }

    /// Sends `method` (`HEAD` or `GET`) to `url`, retrying transient
    /// failures. The headers are rebuilt for each attempt so signatures stay
    /// fresh. Bodies above `body_limit` fail with [`Error::TooLarge`].
    pub(crate) fn send(
        &self,
        method: &str,
        url: &str,
        headers: &dyn Fn() -> Vec<(String, String)>,
        body_limit: u64,
    ) -> Result<Reply> {
        let mut attempt = 0u32;
        loop {
            let last = attempt >= self.transfer.retries;
            match self.send_once(method, url, &headers(), body_limit) {
                Ok(reply) if retryable_status(reply.status) && !last => {}
                Ok(reply) => return Ok(reply),
                Err(Attempt::Retry(_)) if !last => {}
                Err(Attempt::Retry(e) | Attempt::Fatal(e)) => return Err(e),
            }
            let delay = self
                .transfer
                .retry_delay_ms
                .saturating_mul(1 << attempt.min(16))
                .min(30_000);
            std::thread::sleep(Duration::from_millis(delay));
            attempt += 1;
        }
    }

    fn send_once(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body_limit: u64,
    ) -> std::result::Result<Reply, Attempt> {
        let mut req = match method {
            "HEAD" => self.agent.head(url),
            "GET" => self.agent.get(url),
            other => {
                return Err(Attempt::Fatal(Error::Unsupported(format!(
                    "HTTP method {other}"
                ))));
            }
        };
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let mut resp = req.call().map_err(|e| {
            if retryable_error(&e) {
                Attempt::Retry(Error::Http(e.to_string()))
            } else {
                Attempt::Fatal(Error::Http(e.to_string()))
            }
        })?;
        let status = resp.status().as_u16();
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_owned(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();
        let body = if method == "HEAD" {
            Vec::new()
        } else {
            resp.body_mut()
                .with_config()
                .limit(body_limit)
                .read_to_vec()
                .map_err(|e| match e {
                    ureq::Error::BodyExceedsLimit(_) => Attempt::Fatal(Error::TooLarge {
                        size: content_length(&headers).unwrap_or(body_limit.saturating_add(1)),
                        limit: body_limit,
                    }),
                    e if retryable_error(&e) => Attempt::Retry(Error::Http(e.to_string())),
                    e => Attempt::Fatal(Error::Http(e.to_string())),
                })?
        };
        Ok(Reply {
            status,
            headers,
            body,
        })
    }
}

enum Attempt {
    Retry(Error),
    Fatal(Error),
}

fn content_length(headers: &[(String, String)]) -> Option<u64> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
}

/// Parses `bytes <start>-<end>/<total>` (or `bytes */<total>`).
/// Range (`start`, `end`) and total size of a `Content-Range` value.
type ContentRange = (Option<(u64, u64)>, Option<u64>);

fn parse_content_range(v: &str) -> Option<ContentRange> {
    let rest = v.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let total = if total.trim() == "*" {
        None
    } else {
        Some(total.trim().parse().ok()?)
    };
    let range = if range.trim() == "*" {
        None
    } else {
        let (a, b) = range.trim().split_once('-')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    };
    Some((range, total))
}

/// Validates an `http`/`https` URL and returns it with its authority.
pub(crate) fn parse_url(url: &str) -> Result<ureq::http::Uri> {
    let uri: ureq::http::Uri = url.parse().map_err(|_| Error::InvalidUrl(url.to_owned()))?;
    match uri.scheme_str() {
        Some("http" | "https") if uri.host().is_some_and(|h| !h.is_empty()) => Ok(uri),
        _ => Err(Error::InvalidUrl(url.to_owned())),
    }
}

// ---------------------------------------------------------------------------
// Ranged object (shared by HTTP and S3)
// ---------------------------------------------------------------------------

/// A remote object read through ranged `GET`s.
#[derive(Debug)]
pub(crate) struct Ranged {
    client: Client,
    auth: Box<dyn Authorizer>,
    info: RemoteInfo,
    cache: PageCache,
    /// Whole object, when the server ignores `Range`.
    full: OnceLock<Arc<[u8]>>,
}

impl Ranged {
    pub(crate) fn open(
        url: &str,
        auth: Box<dyn Authorizer>,
        transfer: TransferOptions,
    ) -> Result<Self> {
        let client = Client::new(transfer);
        let mut info = RemoteInfo {
            url: url.to_owned(),
            len: 0,
            accepts_ranges: false,
            etag: None,
            last_modified: None,
            content_type: None,
        };
        let head = client.send("HEAD", url, &|| auth.headers("HEAD", &[]), 0)?;
        let mut known_len = None;
        if (200..300).contains(&head.status) {
            known_len = head
                .header("content-length")
                .and_then(|v| v.trim().parse().ok());
            info.accepts_ranges = head
                .header("accept-ranges")
                .is_some_and(|v| v.to_ascii_lowercase().contains("bytes"));
            info.etag = head.header_owned("etag");
            info.last_modified = head.header_owned("last-modified");
            info.content_type = head.header_owned("content-type");
        } else if matches!(head.status, 401 | 404 | 410) {
            return Err(Error::HttpStatus {
                status: head.status,
                url: url.to_owned(),
            });
        }
        let full = OnceLock::new();
        match known_len {
            Some(len) if info.accepts_ranges || len == 0 => info.len = len,
            _ => {
                // HEAD refused, size unknown or ranges not advertised: probe.
                let extra = [("Range".to_owned(), "bytes=0-0".to_owned())];
                let probe = client
                    .send(
                        "GET",
                        url,
                        &|| auth.headers("GET", &extra),
                        transfer.max_full_download,
                    )
                    .map_err(|e| no_range_error(e, transfer.max_full_download))?;
                info.etag = info.etag.or_else(|| probe.header_owned("etag"));
                info.last_modified = info
                    .last_modified
                    .or_else(|| probe.header_owned("last-modified"));
                info.content_type = info
                    .content_type
                    .or_else(|| probe.header_owned("content-type"));
                match probe.status {
                    206 => {
                        let total = probe
                            .header("content-range")
                            .and_then(parse_content_range)
                            .and_then(|(_, total)| total)
                            .or(known_len)
                            .ok_or_else(|| {
                                Error::Protocol("server does not report the object size".into())
                            })?;
                        info.len = total;
                        info.accepts_ranges = true;
                    }
                    200 => {
                        info.len = probe.body.len() as u64;
                        info.accepts_ranges = false;
                        full.set(Arc::from(probe.body)).expect("fresh cell");
                    }
                    416 => info.len = 0,
                    status => {
                        return Err(Error::HttpStatus {
                            status,
                            url: url.to_owned(),
                        });
                    }
                }
            }
        }
        let cache = PageCache::new(transfer.cache);
        Ok(Self {
            client,
            auth,
            info,
            cache,
            full,
        })
    }

    pub(crate) fn info(&self) -> &RemoteInfo {
        &self.info
    }

    pub(crate) fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if let Some(full) = self.full.get() {
            check_range(offset, buf.len(), full.len() as u64)?;
            let start = offset as usize;
            buf.copy_from_slice(&full[start..start + buf.len()]);
            return Ok(());
        }
        self.cache.read(self.info.len, offset, buf, &|off, out| {
            self.fetch(off, out).map_err(io::Error::from)
        })
    }

    pub(crate) fn invalidate(&self) {
        self.cache.clear();
    }

    fn fetch(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let end = offset + buf.len() as u64 - 1;
        let extra = [("Range".to_owned(), format!("bytes={offset}-{end}"))];
        let limit = (buf.len() as u64).max(self.client.transfer().max_full_download);
        let reply = self
            .client
            .send(
                "GET",
                &self.info.url,
                &|| self.auth.headers("GET", &extra),
                limit,
            )
            .map_err(|e| no_range_error(e, self.client.transfer().max_full_download))?;
        if let (Some(old), Some(new)) = (&self.info.etag, reply.header("etag"))
            && old != new
        {
            return Err(Error::Changed);
        }
        match reply.status {
            206 => {
                if let Some((Some((start, _)), _)) =
                    reply.header("content-range").and_then(parse_content_range)
                    && start != offset
                {
                    return Err(Error::Protocol(format!(
                        "asked for offset {offset}, got {start}"
                    )));
                }
                if reply.body.len() != buf.len() {
                    return Err(Error::Protocol(format!(
                        "asked for {} bytes, got {}",
                        buf.len(),
                        reply.body.len()
                    )));
                }
                buf.copy_from_slice(&reply.body);
                Ok(())
            }
            200 => {
                // The server ignored Range this time: keep the whole object.
                if reply.body.len() as u64 != self.info.len {
                    return Err(Error::Changed);
                }
                let start = offset as usize;
                buf.copy_from_slice(&reply.body[start..start + buf.len()]);
                let _ = self.full.set(Arc::from(reply.body));
                Ok(())
            }
            412 => Err(Error::Changed),
            416 => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "range past end").into()),
            status => Err(Error::HttpStatus {
                status,
                url: self.info.url.clone(),
            }),
        }
    }
}

fn no_range_error(e: Error, limit: u64) -> Error {
    match e {
        Error::TooLarge { .. } => Error::Unsupported(format!(
            "the server ignores Range requests and the object exceeds the \
             {limit} byte full-download limit"
        )),
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Read-only HTTP(S) object (CORE-02 / FILE-16).
#[derive(Debug)]
pub struct HttpProvider {
    inner: Ranged,
    name: String,
}

impl HttpProvider {
    /// Probes `url` and returns a provider reading it on demand.
    pub fn open(url: &str, options: &HttpOptions) -> Result<Self> {
        let uri = parse_url(url)?;
        let mut headers = options.headers.clone();
        if let Some(auth) = &options.auth {
            headers.retain(|(k, _)| !k.eq_ignore_ascii_case("authorization"));
            headers.push(("Authorization".to_owned(), auth.header_value()));
        }
        let inner = Ranged::open(url, Box::new(StaticAuth { headers }), options.transfer)?;
        let name = uri
            .path()
            .rsplit('/')
            .find(|s| !s.is_empty())
            .map_or_else(|| uri.host().unwrap_or(url).to_owned(), str::to_owned);
        Ok(Self { inner, name })
    }

    /// Size, range support and validators reported by the server.
    pub fn info(&self) -> &RemoteInfo {
        self.inner.info()
    }
}

impl Provider for HttpProvider {
    fn len(&self) -> u64 {
        self.inner.info().len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_at(offset, buf)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ_ONLY
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn invalidate(&self) {
        self.inner.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{HttpServer, Request, Response, serve_range};

    fn data() -> Vec<u8> {
        (0..300_000u32)
            .map(|i| (i.wrapping_mul(31) >> 3) as u8)
            .collect()
    }

    fn quick() -> TransferOptions {
        TransferOptions {
            retry_delay_ms: 1,
            cache: CacheOptions {
                page_size: 16 * 1024,
                budget: 1 << 20,
            },
            ..TransferOptions::default()
        }
    }

    #[test]
    fn reads_ranges_and_caches_pages() {
        let body = data();
        let server = HttpServer::start({
            let body = body.clone();
            move |req: &Request| serve_range(req, &body, true)
        });
        let url = format!("{}/dir/blob.bin", server.base());
        let p = HttpProvider::open(
            &url,
            &HttpOptions {
                transfer: quick(),
                ..HttpOptions::default()
            },
        )
        .unwrap();
        assert_eq!(p.len(), body.len() as u64);
        assert!(p.info().accepts_ranges);
        assert_eq!(p.name(), "blob.bin");
        let mut buf = vec![0; 40_000];
        p.read_at(10_000, &mut buf).unwrap();
        assert_eq!(buf, body[10_000..50_000]);
        let after_first = server.requests().len();
        p.read_at(20_000, &mut buf[..1000]).unwrap();
        assert_eq!(server.requests().len(), after_first, "served from cache");
        let mut tail = [0; 7];
        p.read_at(body.len() as u64 - 7, &mut tail).unwrap();
        assert_eq!(tail, body[body.len() - 7..]);
        assert!(p.read_at(body.len() as u64 - 3, &mut tail).is_err());
        let reqs = server.requests();
        assert_eq!(reqs[0].method, "HEAD");
        assert!(reqs[1].header("range").unwrap().starts_with("bytes=0-"));
    }

    #[test]
    fn sends_auth_and_custom_headers() {
        let server = HttpServer::start(|req: &Request| {
            if req.header("authorization") == Some("Basic dXNlcjpwYXNz") {
                serve_range(req, b"secret", true)
            } else {
                Response::status(401)
            }
        });
        let url = format!("{}/s", server.base());
        let err = HttpProvider::open(&url, &HttpOptions::default()).unwrap_err();
        assert!(matches!(err, Error::HttpStatus { status: 401, .. }));
        let p = HttpProvider::open(
            &url,
            &HttpOptions {
                auth: Some(HttpAuth::Basic {
                    user: "user".into(),
                    password: "pass".into(),
                }),
                headers: vec![("X-Trace".into(), "1".into())],
                transfer: quick(),
            },
        )
        .unwrap();
        let mut buf = [0; 6];
        p.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf, b"secret");
        assert!(
            server
                .requests()
                .iter()
                .filter(|r| r.header("authorization").is_some())
                .all(|r| r.header("x-trace") == Some("1"))
        );
        assert_eq!(
            HttpAuth::Bearer { token: "t".into() }.header_value(),
            "Bearer t"
        );
        let auth = HttpAuth::Basic {
            user: "u".into(),
            password: "hunter2".into(),
        };
        assert!(!format!("{auth:?}").contains("hunter2"), "secrets redacted");
    }

    #[test]
    fn retries_transient_errors() {
        let body = data();
        let failures = std::sync::atomic::AtomicUsize::new(0);
        let server = HttpServer::start({
            let body = body.clone();
            move |req: &Request| {
                if req.method == "GET"
                    && failures.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2
                {
                    Response::status(503)
                } else {
                    serve_range(req, &body, true)
                }
            }
        });
        let p = HttpProvider::open(
            &format!("{}/x", server.base()),
            &HttpOptions {
                transfer: quick(),
                ..HttpOptions::default()
            },
        )
        .unwrap();
        let mut buf = [0; 100];
        p.read_at(5, &mut buf).unwrap();
        assert_eq!(buf[..], body[5..105]);

        let fail_all = HttpServer::start(|req: &Request| {
            if req.method == "HEAD" {
                serve_range(req, b"abc", true)
            } else {
                Response::status(500)
            }
        });
        let p = HttpProvider::open(
            &format!("{}/y", fail_all.base()),
            &HttpOptions {
                transfer: TransferOptions {
                    retries: 1,
                    ..quick()
                },
                ..HttpOptions::default()
            },
        )
        .unwrap();
        let err = p.read_at(0, &mut [0; 2]).unwrap_err();
        assert!(err.to_string().contains("500"), "{err}");
        assert_eq!(fail_all.requests().len(), 3, "HEAD + GET + one retry");
    }

    #[test]
    fn falls_back_without_range_support() {
        let body = data();
        let server = HttpServer::start({
            let body = body.clone();
            move |req: &Request| {
                if req.method == "HEAD" {
                    Response::status(405)
                } else {
                    serve_range(req, &body, false)
                }
            }
        });
        let p = HttpProvider::open(
            &format!("{}/plain", server.base()),
            &HttpOptions {
                transfer: quick(),
                ..HttpOptions::default()
            },
        )
        .unwrap();
        assert!(!p.info().accepts_ranges);
        assert_eq!(p.len(), body.len() as u64);
        let mut buf = [0; 50];
        p.read_at(1000, &mut buf).unwrap();
        assert_eq!(buf[..], body[1000..1050]);
        assert_eq!(server.requests().len(), 2, "one HEAD, one full GET");

        let err = HttpProvider::open(
            &format!("{}/plain", server.base()),
            &HttpOptions {
                transfer: TransferOptions {
                    max_full_download: 1000,
                    ..quick()
                },
                ..HttpOptions::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "{err}");
    }

    #[test]
    fn detects_changed_object() {
        let version = std::sync::atomic::AtomicUsize::new(0);
        let server = HttpServer::start(move |req: &Request| {
            let v = version.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            serve_range(req, b"0123456789", true).with_header("ETag", &format!("\"v{v}\""))
        });
        let p = HttpProvider::open(
            &format!("{}/c", server.base()),
            &HttpOptions {
                transfer: quick(),
                ..HttpOptions::default()
            },
        )
        .unwrap();
        let err = p.read_at(0, &mut [0; 4]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn rejects_bad_urls_and_parses_content_range() {
        assert!(matches!(
            HttpProvider::open("ftp://x/y", &HttpOptions::default()),
            Err(Error::InvalidUrl(_))
        ));
        assert_eq!(
            parse_content_range("bytes 0-0/1234"),
            Some((Some((0, 0)), Some(1234)))
        );
        assert_eq!(parse_content_range("bytes */77"), Some((None, Some(77))));
        assert_eq!(parse_content_range("items 1-2/3"), None);
    }
}
