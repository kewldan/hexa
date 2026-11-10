//! Network stream provider (CORE-02).
//!
//! A live, growing buffer filled from the network: datagrams received by a
//! UDP listener, or bytes received by a TCP client or a TCP server (clients
//! served one after another). Capture can be stopped and restarted; it stops
//! by itself once `max_size` bytes are buffered. Each datagram / connection is
//! recorded as a [`Segment`] so the UI can show packet boundaries.

use std::fmt;
use std::io::{self, Read};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use hexa_core::provider::{Capabilities, Provider};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

use crate::cache::check_range;

/// How often the worker checks for a stop request.
const POLL: Duration = Duration::from_millis(50);

/// Where the bytes come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum StreamSource {
    /// Bind a UDP socket (`0.0.0.0:9000`) and append every datagram.
    UdpListen { bind: String },
    /// Connect to a TCP server (`host:port`) and append what it sends.
    TcpConnect { addr: String },
    /// Listen for TCP clients and append what they send.
    TcpListen { bind: String },
}

/// How to open a stream provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamOptions {
    pub source: StreamSource,
    /// Capture stops once the buffer holds this many bytes.
    pub max_size: u64,
}

/// Capture state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamState {
    /// Never started.
    Idle,
    Running,
    /// Stopped by [`StreamProvider::stop`].
    Stopped,
    /// `max_size` reached.
    Full,
    /// The TCP peer closed the connection (client mode).
    Closed,
    /// A socket error ended the capture; see [`StreamStatus::error`].
    Failed,
}

/// One datagram or one TCP connection's bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub offset: u64,
    pub len: u64,
    pub peer: String,
}

/// Snapshot for the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamStatus {
    pub state: StreamState,
    pub len: u64,
    pub max_size: u64,
    pub segments: usize,
    /// Bound address (listeners) or local end (client).
    pub local_addr: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug)]
struct Shared {
    data: RwLock<Vec<u8>>,
    segments: Mutex<Vec<Segment>>,
    state: Mutex<StreamState>,
    error: Mutex<Option<String>>,
    max_size: u64,
}

impl Shared {
    /// Appends what fits; returns false once full.
    fn append(&self, bytes: &[u8], peer: &str, new_segment: bool) -> bool {
        let mut data = self.data.write();
        let room = self.max_size.saturating_sub(data.len() as u64);
        let take = (bytes.len() as u64).min(room) as usize;
        let offset = data.len() as u64;
        data.extend_from_slice(&bytes[..take]);
        drop(data);
        if take > 0 {
            let mut segs = self.segments.lock();
            match segs.last_mut() {
                Some(last) if !new_segment && last.offset + last.len == offset => {
                    last.len += take as u64;
                }
                _ => segs.push(Segment {
                    offset,
                    len: take as u64,
                    peer: peer.to_owned(),
                }),
            }
        }
        if offset + take as u64 >= self.max_size {
            *self.state.lock() = StreamState::Full;
            return false;
        }
        true
    }

    fn finish(&self, state: StreamState, error: Option<String>) {
        let mut s = self.state.lock();
        if *s == StreamState::Running {
            *s = state;
            *self.error.lock() = error;
        }
    }
}

struct Worker {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

/// A live buffer filled from the network (CORE-02).
pub struct StreamProvider {
    source: StreamSource,
    shared: Arc<Shared>,
    worker: Mutex<Option<Worker>>,
    local_addr: Mutex<Option<SocketAddr>>,
}

impl fmt::Debug for StreamProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamProvider")
            .field("source", &self.source)
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

fn would_block(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

impl StreamProvider {
    /// Creates the provider; capture begins with [`Self::start`].
    pub fn new(options: StreamOptions) -> Self {
        Self {
            source: options.source,
            shared: Arc::new(Shared {
                data: RwLock::new(Vec::new()),
                segments: Mutex::new(Vec::new()),
                state: Mutex::new(StreamState::Idle),
                error: Mutex::new(None),
                max_size: options.max_size,
            }),
            worker: Mutex::new(None),
            local_addr: Mutex::new(None),
        }
    }

    /// Binds / connects and starts capturing in a background thread. Errors
    /// (address in use, connection refused) are returned right away. Calling
    /// it while running does nothing.
    pub fn start(&self) -> io::Result<()> {
        let mut worker = self.worker.lock();
        if let Some(w) = worker.as_ref()
            && !w.thread.is_finished()
        {
            return Ok(());
        }
        if let Some(w) = worker.take() {
            let _ = w.thread.join();
        }
        if self.shared.data.read().len() as u64 >= self.shared.max_size {
            *self.shared.state.lock() = StreamState::Full;
            return Ok(());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let shared = self.shared.clone();
        let flag = stop.clone();
        let (local, run): (SocketAddr, Box<dyn FnOnce() + Send>) = match &self.source {
            StreamSource::UdpListen { bind } => {
                let socket = UdpSocket::bind(bind.as_str())?;
                socket.set_read_timeout(Some(POLL))?;
                (
                    socket.local_addr()?,
                    Box::new(move || udp_loop(&socket, &shared, &flag)),
                )
            }
            StreamSource::TcpConnect { addr } => {
                let stream = TcpStream::connect(addr.as_str())?;
                stream.set_read_timeout(Some(POLL))?;
                (
                    stream.local_addr()?,
                    Box::new(move || {
                        let peer = stream
                            .peer_addr()
                            .map_or_else(|_| String::new(), |a| a.to_string());
                        match tcp_read(stream, &peer, &shared, &flag) {
                            Ok(true) => shared.finish(StreamState::Closed, None),
                            Ok(false) => {}
                            Err(e) => shared.finish(StreamState::Failed, Some(e.to_string())),
                        }
                    }),
                )
            }
            StreamSource::TcpListen { bind } => {
                let listener = TcpListener::bind(bind.as_str())?;
                listener.set_nonblocking(true)?;
                (
                    listener.local_addr()?,
                    Box::new(move || tcp_accept_loop(&listener, &shared, &flag)),
                )
            }
        };
        *self.local_addr.lock() = Some(local);
        *self.shared.state.lock() = StreamState::Running;
        *self.shared.error.lock() = None;
        let thread = std::thread::Builder::new()
            .name("hexa-stream".into())
            .spawn(run)?;
        *worker = Some(Worker { stop, thread });
        Ok(())
    }

    /// Stops capturing and closes the sockets; the buffer is kept.
    pub fn stop(&self) {
        if let Some(w) = self.worker.lock().take() {
            w.stop.store(true, Ordering::SeqCst);
            let _ = w.thread.join();
        }
        let mut state = self.shared.state.lock();
        if *state == StreamState::Running {
            *state = StreamState::Stopped;
        }
    }

    /// Drops the buffered bytes and segments (capture keeps running).
    pub fn clear(&self) {
        let mut data = self.shared.data.write();
        data.clear();
        self.shared.segments.lock().clear();
        let mut state = self.shared.state.lock();
        if *state == StreamState::Full {
            *state = StreamState::Stopped;
        }
    }

    /// Datagram / connection boundaries.
    pub fn segments(&self) -> Vec<Segment> {
        self.shared.segments.lock().clone()
    }

    /// Address the socket is bound to (useful with port 0).
    pub fn local_addr(&self) -> Option<SocketAddr> {
        *self.local_addr.lock()
    }

    pub fn status(&self) -> StreamStatus {
        StreamStatus {
            state: *self.shared.state.lock(),
            len: self.shared.data.read().len() as u64,
            max_size: self.shared.max_size,
            segments: self.shared.segments.lock().len(),
            local_addr: self.local_addr().map(|a| a.to_string()),
            error: self.shared.error.lock().clone(),
        }
    }
}

impl Drop for StreamProvider {
    fn drop(&mut self) {
        self.stop();
    }
}

fn udp_loop(socket: &UdpSocket, shared: &Shared, stop: &AtomicBool) {
    let mut buf = vec![0u8; 65_536];
    while !stop.load(Ordering::SeqCst) {
        match socket.recv_from(&mut buf) {
            Ok((n, peer)) => {
                if !shared.append(&buf[..n], &peer.to_string(), true) {
                    return;
                }
            }
            Err(e) if would_block(&e) => {}
            // Windows reports ICMP port unreachable of earlier sends here.
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {}
            Err(e) => {
                shared.finish(StreamState::Failed, Some(e.to_string()));
                return;
            }
        }
    }
}

/// Reads one connection until EOF (`Ok(true)`), stop or full (`Ok(false)`).
fn tcp_read(
    mut stream: TcpStream,
    peer: &str,
    shared: &Shared,
    stop: &AtomicBool,
) -> io::Result<bool> {
    let mut buf = vec![0u8; 65_536];
    let mut first = true;
    while !stop.load(Ordering::SeqCst) {
        match stream.read(&mut buf) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                if !shared.append(&buf[..n], peer, first) {
                    return Ok(false);
                }
                first = false;
            }
            Err(e) if would_block(&e) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(false)
}

fn tcp_accept_loop(listener: &TcpListener, shared: &Shared, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, peer)) => {
                let setup = stream
                    .set_nonblocking(false)
                    .and_then(|()| stream.set_read_timeout(Some(POLL)));
                if setup.is_err() {
                    continue;
                }
                match tcp_read(stream, &peer.to_string(), shared, stop) {
                    // A client hanging up or resetting is normal for a server.
                    Ok(true) | Err(_) => {}
                    Ok(false) => return,
                }
            }
            Err(e) if would_block(&e) => std::thread::sleep(POLL),
            Err(e) => {
                shared.finish(StreamState::Failed, Some(e.to_string()));
                return;
            }
        }
    }
}

impl Provider for StreamProvider {
    fn len(&self) -> u64 {
        self.shared.data.read().len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let data = self.shared.data.read();
        check_range(offset, buf.len(), data.len() as u64)?;
        let start = offset as usize;
        buf.copy_from_slice(&data[start..start + buf.len()]);
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            live: true,
            ..Capabilities::READ_ONLY
        }
    }

    fn name(&self) -> String {
        match &self.source {
            StreamSource::UdpListen { bind } => format!("UDP {bind}"),
            StreamSource::TcpConnect { addr } => format!("TCP → {addr}"),
            StreamSource::TcpListen { bind } => format!("TCP ← {bind}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Instant;

    fn wait_for(p: &StreamProvider, cond: impl Fn(&StreamStatus) -> bool) -> StreamStatus {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let s = p.status();
            if cond(&s) || Instant::now() > deadline {
                return s;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn contents(p: &StreamProvider) -> Vec<u8> {
        let mut buf = vec![0; p.len() as usize];
        p.read_at(0, &mut buf).unwrap();
        buf
    }

    #[test]
    fn udp_listener_appends_datagrams() {
        let p = StreamProvider::new(StreamOptions {
            source: StreamSource::UdpListen {
                bind: "127.0.0.1:0".into(),
            },
            max_size: 1 << 20,
        });
        assert_eq!(p.status().state, StreamState::Idle);
        p.start().unwrap();
        let addr = p.local_addr().unwrap();
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        tx.send_to(b"hello", addr).unwrap();
        tx.send_to(b" world", addr).unwrap();
        let s = wait_for(&p, |s| s.len == 11);
        assert_eq!(s.state, StreamState::Running);
        assert_eq!(contents(&p), b"hello world");
        let segs = p.segments();
        assert_eq!(segs.len(), 2);
        assert_eq!((segs[1].offset, segs[1].len), (5, 6));
        assert_eq!(segs[0].peer, tx.local_addr().unwrap().to_string());
        assert!(p.capabilities().live);

        p.stop();
        assert_eq!(p.status().state, StreamState::Stopped);
        p.start().unwrap();
        let addr = p.local_addr().unwrap();
        tx.send_to(b"!", addr).unwrap();
        wait_for(&p, |s| s.len == 12);
        assert_eq!(contents(&p), b"hello world!");
    }

    #[test]
    fn tcp_server_stops_at_max_size() {
        let p = StreamProvider::new(StreamOptions {
            source: StreamSource::TcpListen {
                bind: "127.0.0.1:0".into(),
            },
            max_size: 8,
        });
        p.start().unwrap();
        let addr = p.local_addr().unwrap();
        let mut c = TcpStream::connect(addr).unwrap();
        c.write_all(b"abc").unwrap();
        drop(c);
        wait_for(&p, |s| s.len == 3);
        let mut c = TcpStream::connect(addr).unwrap();
        c.write_all(b"defghijk").unwrap();
        let s = wait_for(&p, |s| s.state == StreamState::Full);
        assert_eq!(s.state, StreamState::Full);
        assert_eq!(contents(&p), b"abcdefgh");
        assert_eq!(p.segments().len(), 2, "one segment per connection");
        p.clear();
        assert_eq!(p.len(), 0);
    }

    #[test]
    fn tcp_client_reads_until_close() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (mut s, _) = server.accept().unwrap();
            s.write_all(b"banner\r\n").unwrap();
        });
        let p = StreamProvider::new(StreamOptions {
            source: StreamSource::TcpConnect {
                addr: addr.to_string(),
            },
            max_size: 1024,
        });
        p.start().unwrap();
        t.join().unwrap();
        let s = wait_for(&p, |s| s.state == StreamState::Closed);
        assert_eq!(s.state, StreamState::Closed);
        assert_eq!(contents(&p), b"banner\r\n");
        assert!(p.read_at(5, &mut [0; 10]).is_err());
        assert!(p.name().starts_with("TCP"));

        let refused = StreamProvider::new(StreamOptions {
            source: StreamSource::TcpConnect {
                addr: addr.to_string(),
            },
            max_size: 1024,
        });
        assert!(refused.start().is_err());
    }
}
