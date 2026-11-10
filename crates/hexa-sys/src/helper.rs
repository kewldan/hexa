//! Privileged device helper (NFR-10).
//!
//! Raw devices need root/administrator rights, but the GUI must not run
//! elevated. The app starts a tiny elevated helper (`hexa-cli helper`) that
//! runs [`helper_serve`] over its stdin/stdout, and talks to it through
//! [`HelperConnection`] / [`HelperClient`].
//!
//! # Wire format
//!
//! Every message is a frame: `u32` little-endian payload length, then the
//! payload. A request payload is `u8 op` followed by op-specific fields; a
//! response payload is `u8 status` (0 = ok, 1 = error) followed by the result
//! or, on error, `u8 kind` + UTF-8 message. All integers are little-endian.
//!
//! | op | request                                   | response                       |
//! |----|-------------------------------------------|--------------------------------|
//! | 0  | hello                                     | `u32 version`, `u8 allow_write`|
//! | 1  | open: `u8 writable`, UTF-8 path           | `u64 handle`, `u64 size`, `u32 sector` |
//! | 2  | read: `u64 handle`, `u64 offset`, `u32 n` | `n` bytes                      |
//! | 3  | write: `u64 handle`, `u64 offset`, data   | –                              |
//! | 4  | size: `u64 handle`                        | `u64 size`, `u32 sector`       |
//! | 5  | flush: `u64 handle`                       | –                              |
//! | 6  | close: `u64 handle`                       | –                              |

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use hexa_core::provider::{Capabilities, Provider};
use parking_lot::Mutex;

use crate::device::{DeviceOpenOptions, DeviceProvider};
use crate::util::{check_range, le32, le64};

/// Protocol version reported by `hello`.
pub const PROTOCOL_VERSION: u32 = 1;
/// Largest frame either side accepts (guards against garbage lengths).
pub const MAX_FRAME: usize = 64 * 1024 * 1024 + 64;
/// Largest single read/write chunk the client sends.
pub const MAX_IO: usize = 8 * 1024 * 1024;

const OP_HELLO: u8 = 0;
const OP_OPEN: u8 = 1;
const OP_READ: u8 = 2;
const OP_WRITE: u8 = 3;
const OP_SIZE: u8 = 4;
const OP_FLUSH: u8 = 5;
const OP_CLOSE: u8 = 6;

fn read_frame(r: &mut dyn Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("helper frame of {len} bytes is too large"),
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

fn write_frame(w: &mut dyn Write, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

fn kind_code(kind: io::ErrorKind) -> u8 {
    match kind {
        io::ErrorKind::NotFound => 1,
        io::ErrorKind::PermissionDenied => 2,
        io::ErrorKind::UnexpectedEof => 3,
        io::ErrorKind::InvalidInput => 4,
        io::ErrorKind::Unsupported => 5,
        io::ErrorKind::InvalidData => 6,
        _ => 0,
    }
}

fn code_kind(code: u8) -> io::ErrorKind {
    match code {
        1 => io::ErrorKind::NotFound,
        2 => io::ErrorKind::PermissionDenied,
        3 => io::ErrorKind::UnexpectedEof,
        4 => io::ErrorKind::InvalidInput,
        5 => io::ErrorKind::Unsupported,
        6 => io::ErrorKind::InvalidData,
        _ => io::ErrorKind::Other,
    }
}

fn short() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "truncated helper message")
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Serves helper requests from `input` to `output` until `input` reaches EOF.
///
/// The app runs this elevated as `hexa-cli helper`; writes are refused unless
/// `allow_write` is set, independently of what the client asks for.
pub fn helper_serve(
    mut input: impl Read,
    mut output: impl Write,
    allow_write: bool,
) -> io::Result<()> {
    let mut devices: HashMap<u64, DeviceProvider> = HashMap::new();
    let mut next_handle = 1u64;
    while let Some(req) = read_frame(&mut input)? {
        let result = handle_request(&req, &mut devices, &mut next_handle, allow_write);
        let mut resp = Vec::new();
        match result {
            Ok(body) => {
                resp.push(0);
                resp.extend_from_slice(&body);
            }
            Err(e) => {
                resp.push(1);
                resp.push(kind_code(e.kind()));
                resp.extend_from_slice(e.to_string().as_bytes());
            }
        }
        write_frame(&mut output, &resp)?;
    }
    Ok(())
}

fn handle_request(
    req: &[u8],
    devices: &mut HashMap<u64, DeviceProvider>,
    next_handle: &mut u64,
    allow_write: bool,
) -> io::Result<Vec<u8>> {
    let (&op, body) = req.split_first().ok_or_else(short)?;
    let device = |devices: &HashMap<u64, DeviceProvider>| -> io::Result<u64> {
        let h = le64(body, 0);
        if body.len() < 8 || !devices.contains_key(&h) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "unknown handle"));
        }
        Ok(h)
    };
    match op {
        OP_HELLO => {
            let mut out = PROTOCOL_VERSION.to_le_bytes().to_vec();
            out.push(u8::from(allow_write));
            Ok(out)
        }
        OP_OPEN => {
            let (&writable, path) = body.split_first().ok_or_else(short)?;
            let writable = writable != 0;
            if writable && !allow_write {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "helper was started read-only",
                ));
            }
            let path = std::str::from_utf8(path)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path is not UTF-8"))?;
            let dev = DeviceProvider::open(
                path,
                DeviceOpenOptions {
                    writable,
                    sector_size: None,
                },
            )?;
            let handle = *next_handle;
            *next_handle += 1;
            let mut out = handle.to_le_bytes().to_vec();
            out.extend_from_slice(&dev.len().to_le_bytes());
            out.extend_from_slice(&dev.sector_size().to_le_bytes());
            devices.insert(handle, dev);
            Ok(out)
        }
        OP_READ => {
            let h = device(devices)?;
            if body.len() < 20 {
                return Err(short());
            }
            let offset = le64(body, 8);
            let n = le32(body, 16) as usize;
            if n > MAX_IO {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "read too large",
                ));
            }
            let mut buf = vec![0u8; n];
            devices[&h].read_at(offset, &mut buf)?;
            Ok(buf)
        }
        OP_WRITE => {
            let h = device(devices)?;
            if body.len() < 16 {
                return Err(short());
            }
            if !allow_write {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "helper was started read-only",
                ));
            }
            devices[&h].write_at(le64(body, 8), &body[16..])?;
            Ok(Vec::new())
        }
        OP_SIZE => {
            let h = device(devices)?;
            let dev = &devices[&h];
            let mut out = dev.len().to_le_bytes().to_vec();
            out.extend_from_slice(&dev.sector_size().to_le_bytes());
            Ok(out)
        }
        OP_FLUSH => {
            let h = device(devices)?;
            devices[&h].flush()?;
            Ok(Vec::new())
        }
        OP_CLOSE => {
            let h = device(devices)?;
            devices.remove(&h);
            Ok(Vec::new())
        }
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("unknown helper op {op}"),
        )),
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

struct Transport {
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
}

/// A connection to a running helper; one connection can serve many devices.
pub struct HelperConnection {
    io: Mutex<Option<Transport>>,
    child: Mutex<Option<Child>>,
    allow_write: bool,
}

impl fmt::Debug for HelperConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HelperConnection")
            .field("allow_write", &self.allow_write)
            .field("child", &self.child.lock().as_ref().map(Child::id))
            .finish_non_exhaustive()
    }
}

impl HelperConnection {
    /// Starts `command` (typically an elevation wrapper around
    /// `hexa-cli helper`) with piped stdin/stdout and says hello.
    pub fn spawn(mut command: Command) -> io::Result<Arc<Self>> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let writer = child.stdin.take().ok_or_else(short)?;
        let reader = child.stdout.take().ok_or_else(short)?;
        let conn = Self::handshake(Box::new(reader), Box::new(writer), Some(child))?;
        Ok(Arc::new(conn))
    }

    /// Talks to a helper over existing streams (pipes, sockets, tests).
    pub fn from_streams(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> io::Result<Arc<Self>> {
        Ok(Arc::new(Self::handshake(
            Box::new(reader),
            Box::new(writer),
            None,
        )?))
    }

    fn handshake(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        child: Option<Child>,
    ) -> io::Result<Self> {
        let mut conn = Self {
            io: Mutex::new(Some(Transport { reader, writer })),
            child: Mutex::new(child),
            allow_write: false,
        };
        let hello = conn.call(&[OP_HELLO])?;
        let version = le32(&hello, 0);
        if hello.len() < 5 || version != PROTOCOL_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("helper speaks protocol {version}, expected {PROTOCOL_VERSION}"),
            ));
        }
        conn.allow_write = hello[4] != 0;
        Ok(conn)
    }

    /// Whether the helper accepts writes.
    pub fn allows_write(&self) -> bool {
        self.allow_write
    }

    /// Opens a device through the helper.
    pub fn open(self: &Arc<Self>, path: &str, writable: bool) -> io::Result<HelperClient> {
        let mut req = vec![OP_OPEN, u8::from(writable)];
        req.extend_from_slice(path.as_bytes());
        let resp = self.call(&req)?;
        if resp.len() < 20 {
            return Err(short());
        }
        Ok(HelperClient {
            conn: Arc::clone(self),
            handle: le64(&resp, 0),
            path: path.to_string(),
            len: le64(&resp, 8),
            sector_size: le32(&resp, 16),
            writable,
        })
    }

    fn call(&self, req: &[u8]) -> io::Result<Vec<u8>> {
        let mut guard = self.io.lock();
        let t = guard
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "helper is closed"))?;
        write_frame(&mut t.writer, req)?;
        let resp = read_frame(&mut t.reader)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "helper closed the connection")
        })?;
        match resp.split_first() {
            Some((0, body)) => Ok(body.to_vec()),
            Some((_, body)) => {
                let kind = code_kind(body.first().copied().unwrap_or(0));
                let msg = String::from_utf8_lossy(body.get(1..).unwrap_or_default());
                Err(io::Error::new(kind, format!("helper: {msg}")))
            }
            None => Err(short()),
        }
    }
}

impl Drop for HelperConnection {
    fn drop(&mut self) {
        // Closing stdin makes the helper's serve loop end.
        self.io.lock().take();
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.wait();
        }
    }
}

/// A device opened through the privileged helper; usable as any provider.
pub struct HelperClient {
    conn: Arc<HelperConnection>,
    handle: u64,
    path: String,
    len: u64,
    sector_size: u32,
    writable: bool,
}

impl fmt::Debug for HelperClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HelperClient")
            .field("path", &self.path)
            .field("handle", &self.handle)
            .field("len", &self.len)
            .field("sector_size", &self.sector_size)
            .field("writable", &self.writable)
            .finish_non_exhaustive()
    }
}

impl HelperClient {
    /// Spawns a helper and opens one device through it.
    pub fn spawn(command: Command, path: &str, writable: bool) -> io::Result<Self> {
        HelperConnection::spawn(command)?.open(path, writable)
    }

    pub fn connection(&self) -> &Arc<HelperConnection> {
        &self.conn
    }

    pub fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn handle_req(&self, op: u8) -> Vec<u8> {
        let mut req = vec![op];
        req.extend_from_slice(&self.handle.to_le_bytes());
        req
    }
}

impl Provider for HelperClient {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        check_range(offset, buf.len(), self.len)?;
        for (i, chunk) in buf.chunks_mut(MAX_IO).enumerate() {
            let mut req = self.handle_req(OP_READ);
            req.extend_from_slice(&(offset + (i * MAX_IO) as u64).to_le_bytes());
            req.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            let data = self.conn.call(&req)?;
            if data.len() != chunk.len() {
                return Err(short());
            }
            chunk.copy_from_slice(&data);
        }
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            readable: true,
            writable: self.writable,
            resizable: false,
            sector_size: Some(self.sector_size),
            live: false,
        }
    }

    fn name(&self) -> String {
        self.path.clone()
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        if !self.writable {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "device is open read-only",
            ));
        }
        check_range(offset, data.len(), self.len)?;
        for (i, chunk) in data.chunks(MAX_IO).enumerate() {
            let mut req = self.handle_req(OP_WRITE);
            req.extend_from_slice(&(offset + (i * MAX_IO) as u64).to_le_bytes());
            req.extend_from_slice(chunk);
            self.conn.call(&req)?;
        }
        Ok(())
    }

    fn flush(&self) -> io::Result<()> {
        self.conn.call(&self.handle_req(OP_FLUSH)).map(drop)
    }
}

impl Drop for HelperClient {
    fn drop(&mut self) {
        let _ = self.conn.call(&self.handle_req(OP_CLOSE));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn start(allow_write: bool) -> (Arc<HelperConnection>, thread::JoinHandle<io::Result<()>>) {
        let (req_r, req_w) = io::pipe().unwrap();
        let (resp_r, resp_w) = io::pipe().unwrap();
        let server = thread::spawn(move || helper_serve(req_r, resp_w, allow_write));
        let conn = HelperConnection::from_streams(resp_r, req_w).unwrap();
        (conn, server)
    }

    #[test]
    fn protocol_round_trip_over_pipes() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        let data: Vec<u8> = (0..8192u32).map(|i| (i * 13) as u8).collect();
        tmp.write_all(&data).unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let (conn, server) = start(true);
        assert!(conn.allows_write());
        {
            let dev = conn.open(&path, true).unwrap();
            assert_eq!(dev.len(), 8192);
            assert_eq!(dev.capabilities().sector_size, Some(512));
            let mut buf = [0u8; 100];
            dev.read_at(1000, &mut buf).unwrap();
            assert_eq!(&buf[..], &data[1000..1100]);
            dev.write_at(511, b"xy").unwrap();
            dev.flush().unwrap();
            assert!(dev.read_at(8190, &mut buf).is_err());
            let err = conn.open("/definitely/not/here", false).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::NotFound);
        }
        drop(conn);
        server.join().unwrap().unwrap();
        let back = std::fs::read(tmp.path()).unwrap();
        assert_eq!(&back[511..513], b"xy");
        assert_eq!(back[510], data[510]);
    }

    #[test]
    fn read_only_helper_refuses_writes() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), [0u8; 1024]).unwrap();
        let (conn, server) = start(false);
        let path = tmp.path().to_str().unwrap();
        let err = conn.open(path, true).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        let dev = conn.open(path, false).unwrap();
        assert!(dev.write_at(0, b"a").is_err());
        drop(dev);
        drop(conn);
        server.join().unwrap().unwrap();
    }
}
