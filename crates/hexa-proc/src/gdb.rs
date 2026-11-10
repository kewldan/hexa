//! GDB Remote Serial Protocol client over TCP (PROC-09).
//!
//! Talks to `gdbserver`, `lldb-server gdbserver`, QEMU's gdbstub, `OpenOCD`,
//! J-Link and other stubs. Only what a hex editor needs is implemented:
//!
//! * `qSupported` (packet size, `QStartNoAckMode`, `qXfer:memory-map:read`);
//! * memory reads with `m`, writes with `X` (binary) falling back to `M`;
//! * the memory map from `qXfer:memory-map:read` when offered, otherwise
//!   LLDB's `qMemoryRegionInfo`, otherwise one flat region;
//! * `qHostInfo` (LLDB) for the pointer size.
//!
//! Registers and execution control are out of scope. [`GdbSource`] adapts
//! the client to [`MemorySource`], so the provider, scanner, pointer scan and
//! dumps all work on remote targets.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;

use crate::error::{Error, Result};
use crate::region::{MemoryRegion, Protection, RegionKind, classify_images};
use crate::source::MemorySource;

/// Packet size assumed when the stub does not announce one.
const DEFAULT_PACKET_SIZE: usize = 0x400;
/// Default socket timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// What the stub supports, from `qSupported` and probing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GdbFeatures {
    /// Maximum packet payload the stub accepts.
    pub packet_size: usize,
    pub no_ack_mode: bool,
    pub memory_map: bool,
    /// `X` packets work (probed on first write).
    pub binary_write: Option<bool>,
    /// LLDB's `qMemoryRegionInfo` works (probed on first region query).
    pub memory_region_info: Option<bool>,
    /// The raw `qSupported` reply items.
    pub raw: Vec<String>,
}

/// A connected GDB remote client (PROC-09).
#[derive(Debug)]
pub struct GdbClient {
    stream: TcpStream,
    peer: SocketAddr,
    ack: bool,
    features: GdbFeatures,
    /// Bytes received but not consumed yet.
    pending: Vec<u8>,
}

fn checksum(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |a, &b| a.wrapping_add(b))
}

/// Escapes binary data for `X` packets (`#`, `$`, `}`, `*`).
pub fn escape_binary(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for &b in data {
        if matches!(b, b'#' | b'$' | b'}' | b'*') {
            out.push(b'}');
            out.push(b ^ 0x20);
        } else {
            out.push(b);
        }
    }
    out
}

/// Reverses [`escape_binary`].
pub fn unescape_binary(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut it = data.iter();
    while let Some(&b) = it.next() {
        if b == b'}' {
            if let Some(&n) = it.next() {
                out.push(n ^ 0x20);
            }
        } else {
            out.push(b);
        }
    }
    out
}

/// Expands run-length encoding (`c*N` = `c` repeated `N - 29` more times).
pub fn expand_rle(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == b'*' && i + 1 < data.len() {
            if let Some(&prev) = out.last() {
                let n = usize::from(data[i + 1].saturating_sub(29));
                out.extend(std::iter::repeat_n(prev, n));
            }
            i += 2;
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

fn hex_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn hex_decode(text: &[u8]) -> Result<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return Err(Error::protocol("odd-length hex data"));
    }
    text.chunks(2)
        .map(|p| {
            std::str::from_utf8(p)
                .ok()
                .and_then(|s| u8::from_str_radix(s, 16).ok())
                .ok_or_else(|| Error::protocol("invalid hex data"))
        })
        .collect()
}

fn is_error_reply(reply: &[u8]) -> bool {
    reply.len() == 3 && reply[0] == b'E' && reply[1..].iter().all(u8::is_ascii_hexdigit)
}

impl GdbClient {
    /// Connects and negotiates features.
    pub fn connect(address: impl ToSocketAddrs, timeout: Duration) -> Result<Self> {
        let mut last = None;
        for addr in address.to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, timeout) {
                Ok(stream) => return Self::handshake(stream, addr, timeout),
                Err(e) => last = Some(e),
            }
        }
        Err(last.map_or_else(|| Error::invalid("address resolved to nothing"), Error::Io))
    }

    fn handshake(stream: TcpStream, peer: SocketAddr, timeout: Duration) -> Result<Self> {
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        stream.set_nodelay(true)?;
        let mut c = Self {
            stream,
            peer,
            ack: true,
            features: GdbFeatures {
                packet_size: DEFAULT_PACKET_SIZE,
                ..GdbFeatures::default()
            },
            pending: Vec::new(),
        };
        let reply = c.request(b"qSupported:multiprocess+;xmlRegisters=i386")?;
        let text = String::from_utf8_lossy(&reply).into_owned();
        for item in text.split(';').filter(|s| !s.is_empty()) {
            if let Some(size) = item.strip_prefix("PacketSize=") {
                if let Ok(n) = usize::from_str_radix(size, 16) {
                    c.features.packet_size = n.max(64);
                }
            } else if item == "QStartNoAckMode+" {
                c.features.no_ack_mode = true;
            } else if item == "qXfer:memory-map:read+" {
                c.features.memory_map = true;
            }
            c.features.raw.push(item.to_string());
        }
        if c.features.no_ack_mode && c.request(b"QStartNoAckMode")? == b"OK" {
            c.ack = false;
        }
        Ok(c)
    }

    pub fn features(&self) -> &GdbFeatures {
        &self.features
    }

    pub fn peer(&self) -> SocketAddr {
        self.peer
    }

    fn read_byte(&mut self) -> Result<u8> {
        if self.pending.is_empty() {
            let mut buf = [0u8; 4096];
            let n = self.stream.read(&mut buf)?;
            if n == 0 {
                return Err(Error::protocol("connection closed by the stub"));
            }
            self.pending.extend_from_slice(&buf[..n]);
            self.pending.reverse();
        }
        Ok(self.pending.pop().expect("refilled above"))
    }

    fn send_packet(&mut self, payload: &[u8]) -> Result<()> {
        let mut frame = Vec::with_capacity(payload.len() + 4);
        frame.push(b'$');
        frame.extend_from_slice(payload);
        frame.push(b'#');
        frame.extend_from_slice(format!("{:02x}", checksum(payload)).as_bytes());
        for _ in 0..3 {
            self.stream.write_all(&frame)?;
            if !self.ack {
                return Ok(());
            }
            match self.read_byte()? {
                b'+' => return Ok(()),
                b'-' => {}
                other => {
                    // A reply began without an ack; keep it for receive.
                    self.pending.push(other);
                    return Ok(());
                }
            }
        }
        Err(Error::protocol("stub rejected the packet three times"))
    }

    fn receive_packet(&mut self) -> Result<Vec<u8>> {
        loop {
            // Skip acks and noise until the start of a packet.
            while self.read_byte()? != b'$' {}
            let mut data = Vec::new();
            loop {
                let b = self.read_byte()?;
                if b == b'#' {
                    break;
                }
                data.push(b);
            }
            let hi = self.read_byte()?;
            let lo = self.read_byte()?;
            let sum = std::str::from_utf8(&[hi, lo])
                .ok()
                .and_then(|s| u8::from_str_radix(s, 16).ok());
            if sum == Some(checksum(&data)) || !self.ack {
                if self.ack {
                    self.stream.write_all(b"+")?;
                }
                return Ok(expand_rle(&data));
            }
            self.stream.write_all(b"-")?;
        }
    }

    /// Sends one packet and returns the (RLE-expanded) reply payload.
    /// Console output (`O…`) and notifications are skipped.
    pub fn request(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        self.send_packet(payload)?;
        loop {
            let reply = self.receive_packet()?;
            if reply.first() == Some(&b'O') && reply.len() > 1 && reply != b"OK" {
                continue;
            }
            return Ok(reply);
        }
    }

    /// Largest memory block per `m` request.
    fn read_chunk(&self) -> usize {
        (self.features.packet_size.saturating_sub(8) / 2).max(16)
    }

    /// Reads target memory with `m` packets.
    pub fn read_memory(&mut self, address: u64, buf: &mut [u8]) -> Result<()> {
        let chunk = self.read_chunk();
        let mut done = 0usize;
        while done < buf.len() {
            let n = chunk.min(buf.len() - done);
            let addr = address + done as u64;
            let reply = self.request(format!("m{addr:x},{n:x}").as_bytes())?;
            if reply.is_empty() || is_error_reply(&reply) {
                return Err(Error::Unmapped { address: addr });
            }
            let bytes = hex_decode(&reply)?;
            if bytes.is_empty() {
                return Err(Error::Unmapped { address: addr });
            }
            let got = bytes.len().min(n);
            buf[done..done + got].copy_from_slice(&bytes[..got]);
            done += got;
        }
        Ok(())
    }

    /// Writes target memory with `X` (binary) or `M` (hex) packets.
    pub fn write_memory(&mut self, address: u64, data: &[u8]) -> Result<()> {
        if self.features.binary_write.is_none() {
            // Probe as GDB does: a zero-length X write.
            let reply = self.request(format!("X{address:x},0:").as_bytes())?;
            self.features.binary_write = Some(reply == b"OK");
        }
        let binary = self.features.binary_write == Some(true);
        // Worst case both encodings double the data (hex, or escaping).
        let chunk = (self.features.packet_size.saturating_sub(32) / 2).max(16);
        for (i, part) in data.chunks(chunk).enumerate() {
            let addr = address + (i * chunk) as u64;
            let mut packet = if binary {
                format!("X{addr:x},{:x}:", part.len()).into_bytes()
            } else {
                format!("M{addr:x},{:x}:", part.len()).into_bytes()
            };
            if binary {
                packet.extend_from_slice(&escape_binary(part));
            } else {
                packet.extend_from_slice(hex_encode(part).as_bytes());
            }
            let reply = self.request(&packet)?;
            if reply != b"OK" {
                return Err(if is_error_reply(&reply) {
                    Error::Unmapped { address: addr }
                } else {
                    Error::protocol(format!(
                        "unexpected reply to memory write: {}",
                        String::from_utf8_lossy(&reply)
                    ))
                });
            }
        }
        Ok(())
    }

    /// Reads a `qXfer` object completely.
    pub fn read_xfer(&mut self, object: &str, annex: &str) -> Result<Option<Vec<u8>>> {
        let mut out = Vec::new();
        let len = self.features.packet_size.saturating_sub(16).max(64);
        loop {
            let reply = self.request(
                format!("qXfer:{object}:read:{annex}:{:x},{len:x}", out.len()).as_bytes(),
            )?;
            match reply.first() {
                None => return Ok(None),
                Some(b'm') => {
                    let data = unescape_binary(&reply[1..]);
                    if data.is_empty() {
                        return Ok(Some(out));
                    }
                    out.extend_from_slice(&data);
                }
                Some(b'l') => {
                    out.extend_from_slice(&unescape_binary(&reply[1..]));
                    return Ok(Some(out));
                }
                Some(_) if is_error_reply(&reply) => {
                    return Err(Error::protocol(format!(
                        "qXfer:{object} failed: {}",
                        String::from_utf8_lossy(&reply)
                    )));
                }
                Some(_) => return Err(Error::protocol("malformed qXfer reply")),
            }
        }
    }

    /// The target memory map, if the stub provides one.
    pub fn memory_map(&mut self) -> Result<Option<Vec<MemoryRegion>>> {
        if !self.features.memory_map {
            return Ok(None);
        }
        let Some(xml) = self.read_xfer("memory-map", "")? else {
            return Ok(None);
        };
        Ok(Some(parse_memory_map(&String::from_utf8_lossy(&xml))))
    }

    /// LLDB `qMemoryRegionInfo` for `address`: the region or gap containing
    /// it. `None` when unsupported.
    pub fn memory_region_info(&mut self, address: u64) -> Result<Option<RegionInfo>> {
        if self.features.memory_region_info == Some(false) {
            return Ok(None);
        }
        let reply = self.request(format!("qMemoryRegionInfo:{address:x}").as_bytes())?;
        if reply.is_empty() || is_error_reply(&reply) {
            if self.features.memory_region_info.is_none() {
                self.features.memory_region_info = Some(false);
            }
            return Ok(None);
        }
        self.features.memory_region_info = Some(true);
        Ok(parse_region_info(&String::from_utf8_lossy(&reply)))
    }

    /// LLDB `qHostInfo` key/value pairs (empty when unsupported).
    pub fn host_info(&mut self) -> Result<HashMap<String, String>> {
        let reply = self.request(b"qHostInfo")?;
        Ok(parse_key_values(&String::from_utf8_lossy(&reply)))
    }

    /// Regions from the memory map or `qMemoryRegionInfo`; `None` if the stub
    /// offers neither.
    pub fn regions(&mut self, limit: u64) -> Result<Option<Vec<MemoryRegion>>> {
        if let Some(map) = self.memory_map()? {
            return Ok(Some(map));
        }
        let mut out = Vec::new();
        let mut address = 0u64;
        while address < limit {
            let Some(info) = self.memory_region_info(address)? else {
                if address == 0 && out.is_empty() {
                    return Ok(None);
                }
                break;
            };
            let end = info.start.saturating_add(info.size);
            if end <= address {
                break;
            }
            if let Some(region) = info.region {
                out.push(region);
            }
            address = end;
        }
        classify_images(&mut out);
        Ok(Some(out))
    }
}

/// Parsed `qMemoryRegionInfo` reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionInfo {
    pub start: u64,
    pub size: u64,
    /// `None` for an unmapped gap.
    pub region: Option<MemoryRegion>,
}

fn parse_key_values(text: &str) -> HashMap<String, String> {
    text.split(';')
        .filter_map(|kv| kv.split_once(':'))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Parses an LLDB `qMemoryRegionInfo` reply.
pub fn parse_region_info(text: &str) -> Option<RegionInfo> {
    let kv = parse_key_values(text);
    let start = u64::from_str_radix(kv.get("start")?, 16).ok()?;
    let size = u64::from_str_radix(kv.get("size")?, 16).ok()?;
    let perms = kv.get("permissions").map_or("", String::as_str);
    if perms.is_empty() {
        return Some(RegionInfo {
            start,
            size,
            region: None,
        });
    }
    let protection = Protection {
        read: perms.contains('r'),
        write: perms.contains('w'),
        execute: perms.contains('x'),
    };
    let name = kv
        .get("name")
        .and_then(|h| hex_decode(h.as_bytes()).ok())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .filter(|n| !n.is_empty());
    let mut region = MemoryRegion::new(start, size, protection);
    match name {
        Some(n) if n.starts_with('[') => {
            region.kind = if n == "[heap]" {
                RegionKind::Heap
            } else if n.starts_with("[stack") {
                RegionKind::Stack
            } else {
                RegionKind::Private
            };
            region.label = Some(n);
        }
        Some(n) => {
            region.kind = RegionKind::Mapped;
            region.path = Some(n);
        }
        None => {}
    }
    if kv.get("type").is_some_and(|t| t.contains("stack")) {
        region.kind = RegionKind::Stack;
    }
    Some(RegionInfo {
        start,
        size,
        region: Some(region),
    })
}

fn xml_attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=");
    let pos = tag.find(&key)? + key.len();
    let rest = &tag[pos..];
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let end = rest[1..].find(quote)?;
    Some(&rest[1..=end])
}

fn parse_u64(text: &str) -> Option<u64> {
    let t = text.trim();
    match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => t.parse().ok(),
    }
}

/// Parses a GDB `<memory-map>` document.
pub fn parse_memory_map(xml: &str) -> Vec<MemoryRegion> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(pos) = rest.find("<memory ") {
        rest = &rest[pos + 8..];
        let tag_end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..tag_end];
        let (Some(start), Some(length)) = (
            xml_attr(tag, "start").and_then(parse_u64),
            xml_attr(tag, "length").and_then(parse_u64),
        ) else {
            continue;
        };
        let ty = xml_attr(tag, "type").unwrap_or("ram");
        let protection = if ty == "ram" {
            Protection::RW
        } else {
            Protection::RX
        };
        let mut region = MemoryRegion::new(start, length, protection);
        region.kind = if ty == "ram" {
            RegionKind::Private
        } else {
            RegionKind::Mapped
        };
        region.label = Some(ty.to_string());
        out.push(region);
    }
    out.sort_by_key(|r| r.start);
    out
}

/// A GDB remote target as a [`MemorySource`] (PROC-09).
#[derive(Debug)]
pub struct GdbSource {
    client: Mutex<GdbClient>,
    name: String,
    pointer_size: u8,
    max_address: u64,
}

impl GdbSource {
    /// Connects to `address` (e.g. `"127.0.0.1:1234"`).
    pub fn connect(address: impl ToSocketAddrs) -> Result<Self> {
        Self::new(GdbClient::connect(address, DEFAULT_TIMEOUT)?)
    }

    /// Wraps a connected client; asks `qHostInfo` for the pointer size.
    pub fn new(mut client: GdbClient) -> Result<Self> {
        let host = client.host_info().unwrap_or_default();
        let pointer_size = host
            .get("ptrsize")
            .and_then(|v| v.parse::<u8>().ok())
            .filter(|&p| p == 4 || p == 8)
            .unwrap_or(8);
        let name = format!("gdb {}", client.peer());
        let mut source = Self {
            client: Mutex::new(client),
            name,
            pointer_size,
            max_address: if pointer_size == 4 { 1 << 32 } else { 1 << 48 },
        };
        if let Ok(Some(map)) = source.client.get_mut().memory_map()
            && let Some(last) = map.last()
        {
            source.max_address = last.end();
        }
        Ok(source)
    }

    /// Overrides the address-space size the provider exposes.
    pub fn with_address_limit(mut self, limit: u64) -> Self {
        self.max_address = limit;
        self
    }

    /// Negotiated features.
    pub fn features(&self) -> GdbFeatures {
        self.client.lock().features().clone()
    }
}

impl MemorySource for GdbSource {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn read(&self, address: u64, buf: &mut [u8]) -> Result<()> {
        self.client.lock().read_memory(address, buf)
    }

    fn write(&self, address: u64, data: &[u8]) -> Result<()> {
        self.client.lock().write_memory(address, data)
    }

    fn writable(&self) -> bool {
        true
    }

    fn regions(&self) -> Result<Vec<MemoryRegion>> {
        let limit = self.max_address;
        Ok(self.client.lock().regions(limit)?.unwrap_or_else(|| {
            // No map: one flat region; unreadable addresses fail per read.
            let mut r = MemoryRegion::new(0, limit, Protection::RW);
            r.kind = RegionKind::Unknown;
            vec![r]
        }))
    }

    fn max_address(&self) -> u64 {
        self.max_address
    }

    fn pointer_size(&self) -> u8 {
        self.pointer_size
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use hexa_core::provider::Provider;
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    /// Behaviour switches of the fake stub.
    #[derive(Clone, Copy)]
    struct StubConfig {
        no_ack: bool,
        memory_map: bool,
        binary_write: bool,
        region_info: bool,
    }

    /// RLE-encodes like real stubs do (runs of 4+ characters).
    fn rle(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < data.len() {
            let c = data[i];
            let mut run = 1;
            while i + run < data.len() && data[i + run] == c && run < 90 {
                run += 1;
            }
            let mut extra = run - 1;
            // '#' (35) and '$' (36) may not be used as counts.
            if extra == 6 || extra == 7 {
                extra = 5;
            }
            out.push(c);
            if extra >= 3 {
                out.push(b'*');
                out.push(extra as u8 + 29);
                i += extra + 1;
            } else {
                i += 1;
            }
        }
        out
    }

    struct Stub {
        cfg: StubConfig,
        /// `(start, bytes)`.
        memory: Vec<(u64, Vec<u8>)>,
        ack: bool,
    }

    impl Stub {
        fn locate(&mut self, addr: u64, len: usize) -> Option<&mut [u8]> {
            self.memory.iter_mut().find_map(|(s, m)| {
                let end = *s + m.len() as u64;
                (addr >= *s && addr + len as u64 <= end)
                    .then(|| &mut m[(addr - *s) as usize..(addr - *s) as usize + len])
            })
        }

        fn handle(&mut self, p: &[u8]) -> Vec<u8> {
            let text = String::from_utf8_lossy(p).into_owned();
            let parse_addr_len = |t: &str| -> (u64, usize) {
                let (a, l) = t.split_once(',').unwrap();
                (
                    u64::from_str_radix(a, 16).unwrap(),
                    usize::from_str_radix(l, 16).unwrap(),
                )
            };
            if text.starts_with("qSupported") {
                let mut s = String::from("PacketSize=100");
                if self.cfg.no_ack {
                    s.push_str(";QStartNoAckMode+");
                }
                if self.cfg.memory_map {
                    s.push_str(";qXfer:memory-map:read+");
                }
                return s.into_bytes();
            }
            if text == "QStartNoAckMode" {
                return b"OK".to_vec();
            }
            if text == "qHostInfo" {
                return if self.cfg.region_info {
                    b"ptrsize:8;endian:little;".to_vec()
                } else {
                    Vec::new()
                };
            }
            if let Some(rest) = text.strip_prefix("qXfer:memory-map:read::") {
                let (off, len) = parse_addr_len(rest);
                let xml = self.memory_map_xml().into_bytes();
                let off = off as usize;
                let end = (off + len).min(xml.len());
                let mut out = vec![if end == xml.len() { b'l' } else { b'm' }];
                out.extend_from_slice(&escape_binary(&xml[off.min(end)..end]));
                return out;
            }
            if let Some(rest) = text.strip_prefix("qMemoryRegionInfo:") {
                if !self.cfg.region_info {
                    return Vec::new();
                }
                let addr = u64::from_str_radix(rest, 16).unwrap();
                return self.region_info(addr).into_bytes();
            }
            if let Some(rest) = text.strip_prefix('m') {
                let (addr, len) = parse_addr_len(rest);
                return match self.locate(addr, len) {
                    Some(m) => rle(hex_encode(m).as_bytes()),
                    None => b"E14".to_vec(),
                };
            }
            if let Some(rest) = p.strip_prefix(b"X") {
                if !self.cfg.binary_write {
                    return Vec::new();
                }
                let colon = rest.iter().position(|&b| b == b':').unwrap();
                let (addr, len) = parse_addr_len(std::str::from_utf8(&rest[..colon]).unwrap());
                let data = unescape_binary(&rest[colon + 1..]);
                assert_eq!(data.len(), len);
                return match self.locate(addr, len) {
                    Some(m) => {
                        m.copy_from_slice(&data);
                        b"OK".to_vec()
                    }
                    None => b"E14".to_vec(),
                };
            }
            if let Some(rest) = text.strip_prefix('M') {
                let (head, hex) = rest.split_once(':').unwrap();
                let (addr, len) = parse_addr_len(head);
                let data = hex_decode(hex.as_bytes()).unwrap();
                assert_eq!(data.len(), len);
                return match self.locate(addr, len) {
                    Some(m) => {
                        m.copy_from_slice(&data);
                        b"OK".to_vec()
                    }
                    None => b"E14".to_vec(),
                };
            }
            Vec::new()
        }

        fn memory_map_xml(&self) -> String {
            let mut s = String::from("<?xml version=\"1.0\"?>\n<memory-map>\n");
            for (i, (start, m)) in self.memory.iter().enumerate() {
                let ty = if i == 0 { "flash" } else { "ram" };
                let _ = writeln!(
                    s,
                    "  <memory type=\"{ty}\" start=\"{start:#x}\" length=\"{:#x}\"/>",
                    m.len()
                );
            }
            s.push_str("</memory-map>\n");
            s
        }

        fn region_info(&self, addr: u64) -> String {
            let mut next = u64::MAX;
            for (start, m) in &self.memory {
                let end = start + m.len() as u64;
                if addr >= *start && addr < end {
                    let name = hex_encode(b"/system/lib/libfake.so");
                    return format!(
                        "start:{start:x};size:{:x};permissions:rw;name:{name};",
                        m.len()
                    );
                }
                if *start > addr {
                    next = next.min(*start);
                }
            }
            if next == u64::MAX {
                return "E01".into();
            }
            format!("start:{addr:x};size:{:x};", next - addr)
        }

        fn serve(mut self, mut stream: TcpStream) {
            let mut buf = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                // Read one packet.
                loop {
                    if stream.read(&mut byte).unwrap_or(0) == 0 {
                        return;
                    }
                    if byte[0] == b'$' {
                        break;
                    }
                }
                buf.clear();
                loop {
                    if stream.read(&mut byte).unwrap_or(0) == 0 {
                        return;
                    }
                    if byte[0] == b'#' {
                        break;
                    }
                    buf.push(byte[0]);
                }
                let mut sum = [0u8; 2];
                if stream.read_exact(&mut sum).is_err() {
                    return;
                }
                let expected = format!("{:02x}", checksum(&buf));
                assert_eq!(sum, expected.as_bytes());
                if self.ack {
                    stream.write_all(b"+").unwrap();
                }
                let reply = self.handle(&buf);
                if buf == b"QStartNoAckMode" {
                    // The client acks this reply, then acks stop.
                    send(&mut stream, &reply);
                    let mut plus = [0u8; 1];
                    stream.read_exact(&mut plus).unwrap();
                    assert_eq!(&plus, b"+");
                    self.ack = false;
                    continue;
                }
                send(&mut stream, &reply);
                if self.ack {
                    let mut plus = [0u8; 1];
                    if stream.read_exact(&mut plus).is_err() {
                        return;
                    }
                    assert_eq!(&plus, b"+");
                }
            }
        }
    }

    fn send(stream: &mut TcpStream, payload: &[u8]) {
        let mut frame = vec![b'$'];
        frame.extend_from_slice(payload);
        frame.push(b'#');
        frame.extend_from_slice(format!("{:02x}", checksum(payload)).as_bytes());
        stream.write_all(&frame).unwrap();
    }

    fn start_stub(cfg: StubConfig) -> (SocketAddr, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut ram = vec![0u8; 0x300];
        for (i, b) in ram.iter_mut().enumerate().skip(0x100) {
            *b = (i * 7) as u8;
        }
        let stub = Stub {
            cfg,
            memory: vec![(0x1000, vec![0x11; 0x80]), (0x8000, ram)],
            ack: true,
        };
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stub.serve(stream);
        });
        (addr, handle)
    }

    #[test]
    fn rle_escape_helpers() {
        assert_eq!(expand_rle(b"0* "), b"0000".to_vec());
        assert_eq!(expand_rle(&rle(&[b'a'; 50])), vec![b'a'; 50]);
        let data = b"a#b$c}d*e".to_vec();
        assert_eq!(unescape_binary(&escape_binary(&data)), data);
        assert!(!escape_binary(&data).contains(&b'#'));
    }

    #[test]
    fn memory_map_and_region_info_parsing() {
        let xml = r#"<memory-map><memory type="ram" start="0x20000000" length="0x1000"/>
            <memory type='flash' start="0x0" length="0x4000"><property name="blocksize">0x400</property></memory></memory-map>"#;
        let m = parse_memory_map(xml);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].start, 0);
        assert_eq!(m[0].label.as_deref(), Some("flash"));
        assert!(!m[0].protection.write);
        assert!(m[1].protection.write);
        let r =
            parse_region_info("start:1000;size:2000;permissions:rx;name:5b737461636b5d;").unwrap();
        let region = r.region.unwrap();
        assert_eq!(region.kind, RegionKind::Stack);
        assert_eq!(region.protection, Protection::RX);
        let gap = parse_region_info("start:0;size:1000;").unwrap();
        assert!(gap.region.is_none());
    }

    #[test]
    fn gdb_stub_with_memory_map_and_no_ack() {
        let (addr, stub) = start_stub(StubConfig {
            no_ack: true,
            memory_map: true,
            binary_write: true,
            region_info: false,
        });
        let source = GdbSource::connect(addr).unwrap();
        let f = source.features();
        assert!(f.no_ack_mode && f.memory_map);
        assert_eq!(f.packet_size, 0x100);
        let regions = source.regions().unwrap();
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[1].start, 0x8000);
        assert_eq!(source.max_address(), 0x8300);

        // Larger than one packet, with RLE-compressed zero runs.
        let mut buf = vec![0u8; 0x300];
        source.read(0x8000, &mut buf).unwrap();
        assert!(buf[..0x100].iter().all(|&b| b == 0));
        assert_eq!(buf[0x123], (0x123 * 7) as u8);
        // Binary write including bytes that need escaping.
        source.write(0x8010, b"#$}*ok").unwrap();
        let mut back = [0u8; 6];
        source.read(0x8010, &mut back).unwrap();
        assert_eq!(&back, b"#$}*ok");
        assert_eq!(source.features().binary_write, Some(true));
        assert!(source.read(0x4000, &mut back).unwrap_err().is_unmapped());

        // The provider works on top of it.
        let provider =
            crate::provider::AddressSpaceProvider::new(std::sync::Arc::new(source)).unwrap();
        let mut b = [0u8; 2];
        provider.read_at(0x1000, &mut b).unwrap();
        assert_eq!(b, [0x11, 0x11]);
        assert_eq!(provider.gaps()[0], 0..0x1000);
        drop(provider);
        stub.join().unwrap();
    }

    #[test]
    fn gdb_stub_lldb_style_with_acks() {
        let (addr, stub) = start_stub(StubConfig {
            no_ack: false,
            memory_map: false,
            binary_write: false,
            region_info: true,
        });
        let source = GdbSource::connect(addr)
            .unwrap()
            .with_address_limit(0x10000);
        assert!(!source.features().no_ack_mode);
        let regions = source.regions().unwrap();
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].path.as_deref(), Some("/system/lib/libfake.so"));
        assert_eq!(source.pointer_size(), 8);
        source.write(0x1004, &[1, 2, 3]).unwrap();
        assert_eq!(source.features().binary_write, Some(false));
        let mut b = [0u8; 5];
        source.read(0x1003, &mut b).unwrap();
        assert_eq!(b, [0x11, 1, 2, 3, 0x11]);
        drop(source);
        stub.join().unwrap();
    }
}
