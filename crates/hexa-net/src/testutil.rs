//! Tiny HTTP/1.1 server on 127.0.0.1 for offline tests.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use parking_lot::Mutex;

/// A parsed request.
#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub method: String,
    /// Path and query as sent.
    pub target: String,
    pub headers: Vec<(String, String)>,
}

impl Request {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A response to send.
#[derive(Debug, Clone)]
pub(crate) struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Advertised `Content-Length` (HEAD answers have no body).
    pub len: usize,
}

impl Response {
    pub(crate) fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
            len: 0,
        }
    }

    pub(crate) fn with_header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.to_owned(), v.to_owned()));
        self
    }
}

/// Serves `data` honouring `Range` when `ranges` is set.
pub(crate) fn serve_range(req: &Request, data: &[u8], ranges: bool) -> Response {
    let head = req.method == "HEAD";
    let mut resp = Response::status(200);
    if ranges {
        resp = resp.with_header("Accept-Ranges", "bytes");
        if let Some(range) = req.header("range").and_then(|r| r.strip_prefix("bytes=")) {
            let (a, b) = range.split_once('-').expect("range");
            let a: usize = a.parse().expect("start");
            let b: usize = if b.is_empty() {
                data.len().saturating_sub(1)
            } else {
                b.parse::<usize>()
                    .expect("end")
                    .min(data.len().saturating_sub(1))
            };
            if a >= data.len() {
                return Response::status(416)
                    .with_header("Content-Range", &format!("bytes */{}", data.len()));
            }
            let part = data[a..=b].to_vec();
            let mut resp = Response::status(206)
                .with_header("Accept-Ranges", "bytes")
                .with_header("Content-Range", &format!("bytes {a}-{b}/{}", data.len()));
            resp.len = part.len();
            if !head {
                resp.body = part;
            }
            return resp;
        }
    }
    resp.len = data.len();
    if !head {
        resp.body = data.to_vec();
    }
    resp
}

type Handler = dyn Fn(&Request) -> Response + Send + Sync;

/// Background server answering every connection with `handler`.
pub(crate) struct HttpServer {
    addr: std::net::SocketAddr,
    log: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl HttpServer {
    pub(crate) fn start(handler: impl Fn(&Request) -> Response + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("addr");
        let log = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Arc<Handler> = Arc::new(handler);
        let thread = {
            let log = log.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let handler = handler.clone();
                            let log = log.clone();
                            std::thread::spawn(move || serve(stream, &*handler, &log));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            })
        };
        Self {
            addr,
            log,
            stop,
            thread: Some(thread),
        }
    }

    /// `http://127.0.0.1:<port>`.
    pub(crate) fn base(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub(crate) fn requests(&self) -> Vec<Request> {
        self.log.lock().clone()
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn serve(stream: TcpStream, handler: &Handler, log: &Mutex<Vec<Request>>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut out = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_owned();
        let target = parts.next().unwrap_or_default().to_owned();
        let mut headers = Vec::new();
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                headers.push((k.trim().to_owned(), v.trim().to_owned()));
            }
        }
        let req = Request {
            method,
            target,
            headers,
        };
        if let Some(n) = req
            .header("content-length")
            .and_then(|v| v.parse::<usize>().ok())
        {
            let mut body = vec![0; n];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
        }
        log.lock().push(req.clone());
        let resp = handler(&req);
        let mut head = format!("HTTP/1.1 {} X\r\n", resp.status);
        for (k, v) in &resp.headers {
            let _ = write!(head, "{k}: {v}\r\n");
        }
        let _ = write!(
            head,
            "Content-Length: {}\r\n\r\n",
            resp.len.max(resp.body.len())
        );
        if out.write_all(head.as_bytes()).is_err() || out.write_all(&resp.body).is_err() {
            return;
        }
        let _ = out.flush();
    }
}
