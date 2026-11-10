//! Byte reordering (XFRM-04): byte swap, reverse, interleave/deinterleave.

use crate::error::{Result, TransformError};
use crate::types::Width;

/// Reverses the byte order of every whole element of `width`
/// (16/32/64-bit swap; 8 is a no-op). A trailing partial element is kept.
pub fn byte_swap(data: &[u8], width: Width) -> Vec<u8> {
    let mut out = data.to_vec();
    for chunk in out.chunks_exact_mut(width.bytes()) {
        chunk.reverse();
    }
    out
}

/// Reverses the whole buffer.
pub fn reverse(data: &[u8]) -> Vec<u8> {
    data.iter().rev().copied().collect()
}

fn check(streams: usize, width: usize) -> Result<()> {
    if streams == 0 {
        return Err(TransformError::param(
            "number of streams must be at least 1",
        ));
    }
    if width == 0 {
        return Err(TransformError::param(
            "element width must be at least 1 byte",
        ));
    }
    Ok(())
}

/// Number of elements that land in stream `k` when `elems` elements are
/// dealt round-robin into `streams` streams.
fn stream_len(elems: usize, streams: usize, k: usize) -> usize {
    if elems > k {
        (elems - k).div_ceil(streams)
    } else {
        0
    }
}

/// Splits round-robin interleaved elements into `streams` consecutive
/// streams: element `i` goes to stream `i % streams`. Output is the streams
/// concatenated (stream 0 first), followed by any trailing partial element.
pub fn deinterleave(data: &[u8], streams: usize, width: usize) -> Result<Vec<u8>> {
    check(streams, width)?;
    let elems = data.len() / width;
    let body = elems * width;
    let mut out = Vec::with_capacity(data.len());
    for k in 0..streams {
        let mut i = k;
        while i < elems {
            out.extend_from_slice(&data[i * width..(i + 1) * width]);
            i += streams;
        }
    }
    out.extend_from_slice(&data[body..]);
    Ok(out)
}

/// Inverse of [`deinterleave`]: `data` holds `streams` consecutive streams
/// (sizes as produced by `deinterleave` for the same total length, i.e.
/// equal sizes when the element count divides evenly) that are merged
/// round-robin, element by element.
pub fn interleave(data: &[u8], streams: usize, width: usize) -> Result<Vec<u8>> {
    check(streams, width)?;
    let elems = data.len() / width;
    let body = elems * width;
    let mut starts = Vec::with_capacity(streams);
    let mut pos = 0;
    for k in 0..streams {
        starts.push(pos);
        pos += stream_len(elems, streams, k) * width;
    }
    let mut out = Vec::with_capacity(data.len());
    for i in 0..elems {
        let k = i % streams;
        let s = starts[k] + (i / streams) * width;
        out.extend_from_slice(&data[s..s + width]);
    }
    out.extend_from_slice(&data[body..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swaps() {
        let d = [1u8, 2, 3, 4, 5, 6, 7, 8, 9];
        assert_eq!(byte_swap(&d, Width::W16), [2, 1, 4, 3, 6, 5, 8, 7, 9]);
        assert_eq!(byte_swap(&d, Width::W32), [4, 3, 2, 1, 8, 7, 6, 5, 9]);
        assert_eq!(byte_swap(&d, Width::W64), [8, 7, 6, 5, 4, 3, 2, 1, 9]);
        assert_eq!(byte_swap(&d, Width::W8), d);
        assert_eq!(reverse(&d), [9, 8, 7, 6, 5, 4, 3, 2, 1]);
    }

    #[test]
    fn deinterleave_stereo_samples() {
        // L0 R0 L1 R1 L2 R2 with 16-bit samples
        let d = [
            0xA0, 0xA0, 0xB0, 0xB0, 0xA1, 0xA1, 0xB1, 0xB1, 0xA2, 0xA2, 0xB2, 0xB2,
        ];
        let s = deinterleave(&d, 2, 2).unwrap();
        assert_eq!(
            s,
            [
                0xA0, 0xA0, 0xA1, 0xA1, 0xA2, 0xA2, 0xB0, 0xB0, 0xB1, 0xB1, 0xB2, 0xB2
            ]
        );
        assert_eq!(interleave(&s, 2, 2).unwrap(), d);
    }

    #[test]
    fn uneven_streams_and_tail() {
        let d = [0u8, 1, 2, 3, 4, 5, 6, 7];
        let s = deinterleave(&d, 3, 1).unwrap();
        assert_eq!(s, [0, 3, 6, 1, 4, 7, 2, 5]);
        assert_eq!(interleave(&s, 3, 1).unwrap(), d);
        let s = deinterleave(&d, 2, 3).unwrap();
        assert_eq!(s, [0, 1, 2, 3, 4, 5, 6, 7]);
        let d = [0u8, 1, 2, 3, 4, 5, 6];
        let s = deinterleave(&d, 2, 2).unwrap();
        assert_eq!(s, [0, 1, 4, 5, 2, 3, 6]);
        assert_eq!(interleave(&s, 2, 2).unwrap(), d);
        assert!(deinterleave(&d, 0, 1).is_err());
        assert!(interleave(&d, 1, 0).is_err());
    }
}
