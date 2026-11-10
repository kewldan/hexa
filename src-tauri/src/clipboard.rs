//! System clipboard with Hexa's own binary format (EDIT-17).
//!
//! A copy puts two things on the clipboard: text for other applications and
//! raw bytes under [`FORMAT`] with a small header naming the Hexa instance and
//! copy. Pasting in the same instance uses the internal piece clipboard (no
//! bytes copied); another instance gets the raw bytes.

use clipboard_rs::{Clipboard, ClipboardContent, ClipboardContext};

/// Custom clipboard format name, used on every platform.
pub const FORMAT: &str = "dev.kewldan.hexa.bytes";
const MAGIC: &[u8; 8] = b"HEXACLIP";
/// Largest selection carried as raw bytes between instances.
pub const MAX_RAW: u64 = 64 << 20;

/// What another Hexa put on the clipboard.
#[derive(Debug, PartialEq, Eq)]
pub struct Payload {
    pub pid: u32,
    pub seq: u64,
    pub len: u64,
    /// `None` when the selection was too large to carry.
    pub bytes: Option<Vec<u8>>,
}

pub fn encode(pid: u32, seq: u64, len: u64, bytes: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(29 + bytes.map_or(0, <[u8]>::len));
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&pid.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out.push(u8::from(bytes.is_some()));
    if let Some(b) = bytes {
        out.extend_from_slice(b);
    }
    out
}

pub fn decode(data: &[u8]) -> Option<Payload> {
    if data.len() < 29 || &data[..8] != MAGIC {
        return None;
    }
    let pid = u32::from_le_bytes(data[8..12].try_into().ok()?);
    let seq = u64::from_le_bytes(data[12..20].try_into().ok()?);
    let len = u64::from_le_bytes(data[20..28].try_into().ok()?);
    let bytes = (data[28] == 1).then(|| data[29..].to_vec());
    if bytes.as_ref().is_some_and(|b| b.len() as u64 != len) {
        return None;
    }
    Some(Payload {
        pid,
        seq,
        len,
        bytes,
    })
}

/// Writes text plus Hexa's binary payload. Returns false when the system
/// clipboard is unavailable (the caller then writes text another way).
pub fn write(text: &str, payload: Vec<u8>) -> bool {
    let Ok(ctx) = ClipboardContext::new() else {
        return false;
    };
    ctx.set(vec![
        ClipboardContent::Text(text.to_owned()),
        ClipboardContent::Other(FORMAT.to_owned(), payload),
    ])
    .is_ok()
}

/// Hexa's payload currently on the clipboard, if any.
pub fn read() -> Option<Payload> {
    let ctx = ClipboardContext::new().ok()?;
    let data = ctx.get_buffer(FORMAT).ok()?;
    decode(&data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_roundtrip() {
        let p = encode(7, 42, 3, Some(&[1, 2, 3]));
        assert_eq!(
            decode(&p),
            Some(Payload {
                pid: 7,
                seq: 42,
                len: 3,
                bytes: Some(vec![1, 2, 3])
            })
        );
        let big = encode(7, 43, 1 << 40, None);
        assert_eq!(decode(&big).unwrap().bytes, None);
        assert!(decode(b"garbage").is_none());
        let mut bad = p.clone();
        bad.pop();
        assert!(decode(&bad).is_none());
    }
}
