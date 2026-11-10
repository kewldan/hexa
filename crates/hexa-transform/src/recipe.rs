//! Steps, recipes, write-back and previews (XFRM-09, XFRM-10, XFRM-12).
//!
//! A [`Step`] is one operation with its parameters; a [`Recipe`] is an
//! ordered list of steps saved as JSON. [`apply`] is pure: it only reads the
//! input slice and returns a new buffer.

use serde::{Deserialize, Serialize};

use crate::arith::{self, ArithOp};
use crate::bitwise::{self, BitOp, Direction};
use crate::codec::{self, Codec};
use crate::compress::{self, Compression};
use crate::crypto::{self, AesMode, Cipher};
use crate::error::{Result, TransformError};
use crate::recode;
use crate::reorder;
use crate::types::{Endian, HexBytes, Width, operand};
use crate::xor;

const fn one() -> usize {
    1
}

/// One transformation with its parameters (XFRM-01..08). JSON form:
/// `{"kind": "bitwise", "op": "xor", "operand": 255, "width": 8}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Step {
    /// XOR/AND/OR/NAND/NOR with an operand per element (XFRM-01).
    Bitwise {
        op: BitOp,
        /// Number, or string in decimal / `0x` hex / `0b` binary; negative
        /// values are two's complement.
        #[serde(with = "operand")]
        operand: u64,
        #[serde(default)]
        width: Width,
        #[serde(default)]
        endian: Endian,
    },
    /// Bitwise NOT (XFRM-01).
    Not,
    /// Logical shift per element (XFRM-01).
    Shift {
        direction: Direction,
        amount: u32,
        #[serde(default)]
        width: Width,
        #[serde(default)]
        endian: Endian,
    },
    /// Rotate per element (XFRM-01).
    Rotate {
        direction: Direction,
        amount: u32,
        #[serde(default)]
        width: Width,
        #[serde(default)]
        endian: Endian,
    },
    /// Bit-order reversal per byte (`width` 8) or element (XFRM-01).
    ReverseBits {
        #[serde(default)]
        width: Width,
    },
    /// Wrapping arithmetic per element (XFRM-02).
    Arithmetic {
        op: ArithOp,
        #[serde(with = "operand")]
        operand: u64,
        #[serde(default)]
        width: Width,
        #[serde(default)]
        endian: Endian,
        /// Signed division / remainder.
        #[serde(default)]
        signed: bool,
    },
    /// XOR with a repeating key (XFRM-03).
    XorKey { key: HexBytes },
    /// Byte swap of 16/32/64-bit elements (XFRM-04).
    ByteSwap { width: Width },
    /// Reverse the whole buffer (XFRM-04).
    Reverse,
    /// Merge `streams` consecutive streams element by element (XFRM-04).
    Interleave {
        streams: usize,
        /// Element width in bytes.
        #[serde(default = "one")]
        width: usize,
    },
    /// Split round-robin elements into `streams` streams (XFRM-04).
    Deinterleave {
        streams: usize,
        #[serde(default = "one")]
        width: usize,
    },
    /// Binary-to-text encode (XFRM-05).
    Encode { codec: Codec },
    /// Text-to-binary decode (XFRM-05).
    Decode { codec: Codec },
    /// Compress (XFRM-06).
    Compress { codec: Compression },
    /// Decompress (XFRM-06).
    Decompress { codec: Compression },
    /// Encrypt (XFRM-07).
    Encrypt { cipher: Cipher },
    /// Decrypt (XFRM-07).
    Decrypt { cipher: Cipher },
    /// Recode text from one encoding to another (XFRM-08).
    Recode { from: String, to: String },
}

impl Step {
    /// The `kind` tag of the step.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Bitwise { .. } => "bitwise",
            Self::Not => "not",
            Self::Shift { .. } => "shift",
            Self::Rotate { .. } => "rotate",
            Self::ReverseBits { .. } => "reverseBits",
            Self::Arithmetic { .. } => "arithmetic",
            Self::XorKey { .. } => "xorKey",
            Self::ByteSwap { .. } => "byteSwap",
            Self::Reverse => "reverse",
            Self::Interleave { .. } => "interleave",
            Self::Deinterleave { .. } => "deinterleave",
            Self::Encode { .. } => "encode",
            Self::Decode { .. } => "decode",
            Self::Compress { .. } => "compress",
            Self::Decompress { .. } => "decompress",
            Self::Encrypt { .. } => "encrypt",
            Self::Decrypt { .. } => "decrypt",
            Self::Recode { .. } => "recode",
        }
    }

    /// Checks parameters without touching data, so the UI can flag a bad
    /// step before running it.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Arithmetic {
                op: ArithOp::Div | ArithOp::Mod,
                operand,
                width,
                ..
            } if operand & width.mask() == 0 => Err(TransformError::DivisionByZero),
            Self::XorKey { key } if key.0.is_empty() => {
                Err(TransformError::param("XOR key must not be empty"))
            }
            Self::Interleave { streams, width } | Self::Deinterleave { streams, width }
                if *streams == 0 || *width == 0 =>
            {
                Err(TransformError::param(
                    "streams and element width must be at least 1",
                ))
            }
            Self::Compress { codec } | Self::Decompress { codec } => codec.validate(),
            Self::Encrypt { cipher } | Self::Decrypt { cipher } => cipher.validate(),
            Self::Recode { from, to } => {
                recode::check_encoding(from)?;
                recode::check_encoding(to)
            }
            _ => Ok(()),
        }
    }

    /// Applies the step to `data` (pure).
    pub fn apply(&self, data: &[u8]) -> Result<Vec<u8>> {
        match self {
            Self::Bitwise {
                op,
                operand,
                width,
                endian,
            } => Ok(bitwise::bit_op(data, *op, *operand, *width, *endian)),
            Self::Not => Ok(bitwise::not(data)),
            Self::Shift {
                direction,
                amount,
                width,
                endian,
            } => Ok(bitwise::shift(data, *direction, *amount, *width, *endian)),
            Self::Rotate {
                direction,
                amount,
                width,
                endian,
            } => Ok(bitwise::rotate(data, *direction, *amount, *width, *endian)),
            Self::ReverseBits { width } => Ok(bitwise::reverse_bits(data, *width)),
            Self::Arithmetic {
                op,
                operand,
                width,
                endian,
                signed,
            } => arith::arith(data, *op, *operand, *width, *endian, *signed),
            Self::XorKey { key } => xor::xor_key(data, &key.0),
            Self::ByteSwap { width } => Ok(reorder::byte_swap(data, *width)),
            Self::Reverse => Ok(reorder::reverse(data)),
            Self::Interleave { streams, width } => reorder::interleave(data, *streams, *width),
            Self::Deinterleave { streams, width } => reorder::deinterleave(data, *streams, *width),
            Self::Encode { codec } => codec::encode(codec, data),
            Self::Decode { codec } => codec::decode(codec, data),
            Self::Compress { codec } => compress::compress(codec, data),
            Self::Decompress { codec } => compress::decompress(codec, data),
            Self::Encrypt { cipher } => crypto::encrypt(cipher, data),
            Self::Decrypt { cipher } => crypto::decrypt(cipher, data),
            Self::Recode { from, to } => recode::recode(data, from, to),
        }
    }

    /// The step that undoes this one, if any (XFRM-09).
    ///
    /// Encoders, compressors and ciphers are inverted semantically: the
    /// re-encoded bytes decode to the same data but may differ from the
    /// original bytes (padding, line wrapping, compression choices).
    /// Inverting LZ4 block decompression yields a block whose decompressed
    /// size is the length of the edited data; update `size` accordingly
    /// before re-applying.
    pub fn invert(&self) -> Option<Step> {
        Some(match self {
            Self::Bitwise { op: BitOp::Xor, .. }
            | Self::Not
            | Self::ReverseBits { .. }
            | Self::XorKey { .. }
            | Self::ByteSwap { .. }
            | Self::Reverse => self.clone(),
            Self::Bitwise { .. } | Self::Shift { .. } => return None,
            Self::Rotate {
                direction,
                amount,
                width,
                endian,
            } => Self::Rotate {
                direction: direction.opposite(),
                amount: *amount,
                width: *width,
                endian: *endian,
            },
            Self::Arithmetic {
                op,
                operand,
                width,
                endian,
                signed,
            } => {
                let (op, operand) = match op {
                    ArithOp::Add => (ArithOp::Sub, *operand),
                    ArithOp::Sub => (ArithOp::Add, *operand),
                    ArithOp::Mul => (ArithOp::Mul, arith::mul_inverse(*operand, *width)?),
                    ArithOp::Div | ArithOp::Mod => return None,
                };
                Self::Arithmetic {
                    op,
                    operand,
                    width: *width,
                    endian: *endian,
                    signed: *signed,
                }
            }
            Self::Interleave { streams, width } => Self::Deinterleave {
                streams: *streams,
                width: *width,
            },
            Self::Deinterleave { streams, width } => Self::Interleave {
                streams: *streams,
                width: *width,
            },
            Self::Encode { codec } => Self::Decode {
                codec: codec.clone(),
            },
            Self::Decode { codec } => Self::Encode {
                codec: codec.clone(),
            },
            Self::Compress { codec } => Self::Decompress {
                codec: codec.clone(),
            },
            Self::Decompress { codec } => Self::Compress {
                codec: codec.clone(),
            },
            Self::Encrypt { cipher } => Self::Decrypt {
                cipher: cipher.is_reversible().then(|| cipher.clone())?,
            },
            Self::Decrypt { cipher } => Self::Encrypt {
                cipher: cipher.is_reversible().then(|| cipher.clone())?,
            },
            Self::Recode { from, to } => Self::Recode {
                from: to.clone(),
                to: from.clone(),
            },
        })
    }

    /// Whether [`Step::invert`] returns a step.
    pub fn is_reversible(&self) -> bool {
        self.invert().is_some()
    }

    /// True when running the step on a prefix of the input gives a prefix of
    /// the full output, except for a short tail (used by [`preview`]).
    fn prefix_safe(&self) -> bool {
        match self {
            Self::Reverse
            | Self::Interleave { .. }
            | Self::Deinterleave { .. }
            | Self::Compress { .. }
            | Self::Decompress { .. }
            | Self::Encode {
                codec: Codec::Base58,
            }
            | Self::Decode {
                codec: Codec::Base58,
            } => false,
            Self::Encrypt { cipher } | Self::Decrypt { cipher } => !matches!(
                cipher,
                Cipher::Aes {
                    mode: AesMode::Gcm,
                    ..
                }
            ),
            _ => true,
        }
    }
}

/// An ordered list of steps (XFRM-10).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recipe {
    /// Optional user-facing name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub steps: Vec<Step>,
}

impl Recipe {
    /// Recipe from steps.
    pub fn new(steps: Vec<Step>) -> Self {
        Self { name: None, steps }
    }

    /// Serialises to pretty JSON.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Parses JSON produced by [`Recipe::to_json`] or the UI.
    pub fn from_json(json: &str) -> Result<Self> {
        Ok(serde_json::from_str(json)?)
    }

    /// Validates every step; the error names the failing step.
    pub fn validate(&self) -> Result<()> {
        for (i, s) in self.steps.iter().enumerate() {
            s.validate().map_err(|e| wrap(i, s, e))?;
        }
        Ok(())
    }

    /// Whether every step can be inverted (XFRM-09).
    pub fn is_reversible(&self) -> bool {
        self.steps.iter().all(Step::is_reversible)
    }

    /// The recipe that maps outputs back to inputs: inverted steps in
    /// reverse order.
    pub fn invert(&self) -> Result<Recipe> {
        let steps = self
            .steps
            .iter()
            .enumerate()
            .rev()
            .map(|(i, s)| {
                s.invert().ok_or_else(|| {
                    wrap(
                        i,
                        s,
                        TransformError::NotReversible(format!("'{}' cannot be undone", s.name())),
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Recipe {
            name: self.name.clone(),
            steps,
        })
    }
}

fn wrap(index: usize, step: &Step, e: TransformError) -> TransformError {
    TransformError::Step {
        index,
        name: step.name(),
        source: Box::new(e),
    }
}

/// Runs `steps` in order; the error names the failing step.
pub fn apply_steps(steps: &[Step], data: &[u8]) -> Result<Vec<u8>> {
    let mut cur: Option<Vec<u8>> = None;
    for (i, s) in steps.iter().enumerate() {
        let input = cur.as_deref().unwrap_or(data);
        cur = Some(s.apply(input).map_err(|e| wrap(i, s, e))?);
    }
    Ok(cur.unwrap_or_else(|| data.to_vec()))
}

/// Applies a recipe to `data` (pure, XFRM-10).
pub fn apply(recipe: &Recipe, data: &[u8]) -> Result<Vec<u8>> {
    apply_steps(&recipe.steps, data)
}

/// Write-back (XFRM-09): given the edited output of `steps`, computes the
/// bytes to store in the source so that applying `steps` yields
/// `new_output` (semantically, see [`Step::invert`]).
pub fn reverse_apply(steps: &[Step], new_output: &[u8]) -> Result<Vec<u8>> {
    let inverse = Recipe::new(steps.to_vec()).invert()?;
    apply_steps(&inverse.steps, new_output).map_err(|e| match e {
        TransformError::Step {
            index,
            name,
            source,
        } => TransformError::Step {
            index: steps.len() - 1 - index,
            name,
            source,
        },
        other => other,
    })
}

/// Result of [`preview`] (XFRM-12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    /// At most `max_out` leading bytes of the output.
    pub output: Vec<u8>,
    /// Length of the full output when it was computed completely.
    pub total_len: Option<usize>,
    /// The output was cut (it is longer than `output`).
    pub truncated: bool,
    /// Only a prefix of the input was processed.
    pub input_truncated: bool,
}

/// Bytes past `max_out` that must be produced before a prefix-computed
/// result is trusted (covers padding/trailer differences at the tail).
const PREVIEW_MARGIN: usize = 1024;
const PREVIEW_MIN_BUDGET: usize = 64 * 1024;
/// Prefix lengths are multiples of this (lcm of 3, 5 and 256) so element,
/// block and Base32/64/85 groups stay aligned.
const PREVIEW_ALIGN: usize = 3840;

/// Computes the first `max_out` output bytes cheaply for live UI previews.
///
/// When every step works on a prefix (element-wise ops, XOR, encodings,
/// stream/ECB/CBC ciphers…) only a prefix of the input is processed;
/// decompression stops once enough output exists. Otherwise the whole input
/// is processed. The returned bytes always equal the start of
/// [`apply`]'s output.
pub fn preview(recipe: &Recipe, data: &[u8], max_out: usize) -> Result<Preview> {
    let steps = &recipe.steps;
    recipe.validate()?;
    let budget = max_out
        .saturating_mul(4)
        .saturating_add(PREVIEW_MARGIN)
        .max(PREVIEW_MIN_BUDGET)
        .div_ceil(PREVIEW_ALIGN)
        .saturating_mul(PREVIEW_ALIGN);
    let need = max_out.saturating_add(PREVIEW_MARGIN);
    if data.len() > budget
        && steps.iter().all(Step::prefix_safe)
        && let Ok(out) = apply_steps(steps, &data[..budget])
        && out.len() >= need
    {
        return Ok(Preview {
            output: out[..max_out].to_vec(),
            total_len: None,
            truncated: true,
            input_truncated: true,
        });
    }
    let (mut out, mut capped) = apply_capped(steps, data, budget)?;
    if capped && out.len() < need {
        out = apply_steps(steps, data)?;
        capped = false;
    }
    let total_len = (!capped).then_some(out.len());
    let truncated = capped || out.len() > max_out;
    out.truncate(max_out);
    Ok(Preview {
        output: out,
        total_len,
        truncated,
        input_truncated: false,
    })
}

/// Like [`apply_steps`], but a decompression followed only by prefix-safe
/// steps stops after `cap` bytes. Returns whether anything was cut.
fn apply_capped(steps: &[Step], data: &[u8], cap: usize) -> Result<(Vec<u8>, bool)> {
    let mut cur: Option<Vec<u8>> = None;
    let mut capped = false;
    for (i, s) in steps.iter().enumerate() {
        let input = cur.as_deref().unwrap_or(data);
        let r = match s {
            Step::Decompress { codec } if steps[i + 1..].iter().all(Step::prefix_safe) => {
                compress::decompress_capped(codec, input, cap, true).map(|(v, cut)| {
                    capped |= cut;
                    v
                })
            }
            _ => s.apply(input),
        };
        cur = Some(r.map_err(|e| wrap(i, s, e))?);
    }
    Ok((cur.unwrap_or_else(|| data.to_vec()), capped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{Base32Alphabet, Base64Alphabet};
    use crate::crypto::{BlockMode, Padding};

    fn b64() -> Codec {
        Codec::Base64 {
            alphabet: Base64Alphabet::Standard,
            padding: true,
        }
    }

    fn sample(n: usize) -> Vec<u8> {
        (0..n)
            .map(|i| (i * 31 % 251) as u8 ^ (i >> 9) as u8)
            .collect()
    }

    fn reversible_steps() -> Vec<Step> {
        vec![
            Step::Bitwise {
                op: BitOp::Xor,
                operand: 0xDEAD_BEEF,
                width: Width::W32,
                endian: Endian::Big,
            },
            Step::Not,
            Step::Rotate {
                direction: Direction::Left,
                amount: 3,
                width: Width::W16,
                endian: Endian::Little,
            },
            Step::ReverseBits { width: Width::W8 },
            Step::Arithmetic {
                op: ArithOp::Add,
                operand: 7,
                width: Width::W64,
                endian: Endian::Little,
                signed: false,
            },
            Step::Arithmetic {
                op: ArithOp::Mul,
                operand: 0x1234_5679,
                width: Width::W32,
                endian: Endian::Little,
                signed: false,
            },
            Step::XorKey {
                key: HexBytes(b"key".to_vec()),
            },
            Step::ByteSwap { width: Width::W32 },
            Step::Reverse,
            Step::Deinterleave {
                streams: 3,
                width: 2,
            },
            Step::Compress {
                codec: Compression::Zlib { level: 6 },
            },
            Step::Encrypt {
                cipher: Cipher::Aes {
                    mode: AesMode::Cbc,
                    key: HexBytes(vec![1; 16]),
                    iv: HexBytes(vec![2; 16]),
                    padding: Padding::Pkcs7,
                    aad: HexBytes::default(),
                    tag: None,
                },
            },
            Step::Encode { codec: b64() },
        ]
    }

    #[test]
    fn apply_chain_and_reverse_apply() {
        let r = Recipe::new(reversible_steps());
        assert!(r.is_reversible());
        let data = sample(10_007);
        let out = apply(&r, &data).unwrap();
        assert!(out.iter().all(u8::is_ascii));
        assert_eq!(reverse_apply(&r.steps, &out).unwrap(), data);
        // the inverted recipe is a decoded view of `out`: edit it, write back
        let view = r.invert().unwrap();
        assert!(view.is_reversible());
        assert_eq!(apply(&view, &out).unwrap(), data);
        let mut edited = data.clone();
        edited[100] ^= 0xFF;
        edited.extend_from_slice(b"appended");
        let source = reverse_apply(&view.steps, &edited).unwrap();
        assert_eq!(apply(&view, &source).unwrap(), edited);
    }

    #[test]
    fn decoded_view_write_back() {
        // the source holds base64(zlib(text)); the view shows text
        let text = b"hello hex editor".repeat(20);
        let z = compress::compress(&Compression::Zlib { level: 9 }, &text).unwrap();
        let source = codec::encode(&b64(), &z).unwrap();
        let view = Recipe::new(vec![
            Step::Decode { codec: b64() },
            Step::Decompress {
                codec: Compression::Zlib { level: 9 },
            },
        ]);
        assert_eq!(apply(&view, &source).unwrap(), text);
        let new_source = reverse_apply(&view.steps, b"edited").unwrap();
        assert_eq!(apply(&view, &new_source).unwrap(), b"edited");
    }

    #[test]
    fn irreversible_steps() {
        let s = [
            Step::Bitwise {
                op: BitOp::And,
                operand: 1,
                width: Width::W8,
                endian: Endian::Little,
            },
            Step::Shift {
                direction: Direction::Left,
                amount: 1,
                width: Width::W8,
                endian: Endian::Little,
            },
            Step::Arithmetic {
                op: ArithOp::Mul,
                operand: 2,
                width: Width::W8,
                endian: Endian::Little,
                signed: false,
            },
            Step::Arithmetic {
                op: ArithOp::Div,
                operand: 3,
                width: Width::W8,
                endian: Endian::Little,
                signed: false,
            },
        ];
        for step in &s {
            assert!(!step.is_reversible(), "{step:?}");
        }
        let steps = vec![Step::Not, s[1].clone()];
        let e = reverse_apply(&steps, b"x").unwrap_err();
        assert!(matches!(e, TransformError::Step { index: 1, .. }), "{e}");
    }

    #[test]
    fn inverses_are_exact_for_bijective_steps() {
        let data = sample(1000);
        for s in reversible_steps() {
            let inv = s.invert().unwrap();
            assert_eq!(inv.apply(&s.apply(&data).unwrap()).unwrap(), data, "{s:?}");
        }
        let rec = Step::Recode {
            from: "utf-8".into(),
            to: "utf-16be".into(),
        };
        let inv = rec.invert().unwrap();
        assert_eq!(inv.apply(&rec.apply(b"abc").unwrap()).unwrap(), b"abc");
    }

    #[test]
    fn errors_name_the_step() {
        let r = Recipe::new(vec![
            Step::Not,
            Step::Decode {
                codec: Codec::Base16 { uppercase: false },
            },
        ]);
        let e = apply(&r, b"zz").unwrap_err();
        assert!(matches!(
            e,
            TransformError::Step {
                index: 1,
                name: "decode",
                ..
            }
        ));
        assert!(
            e.to_string()
                .starts_with("step 2 (decode): invalid Base16 input"),
            "{e}"
        );
        let bad = Recipe::new(vec![Step::XorKey {
            key: HexBytes::default(),
        }]);
        assert!(bad.validate().is_err());
    }

    #[test]
    fn json_roundtrip_and_shape() {
        let mut steps = reversible_steps();
        steps.extend([
            Step::Arithmetic {
                op: ArithOp::Sub,
                operand: u64::MAX,
                width: Width::W64,
                endian: Endian::Big,
                signed: true,
            },
            Step::Decrypt {
                cipher: Cipher::Blowfish {
                    mode: BlockMode::Ctr,
                    key: HexBytes(vec![9; 8]),
                    iv: HexBytes(vec![0; 8]),
                    padding: Padding::None,
                },
            },
            Step::Encode {
                codec: Codec::Base32 {
                    alphabet: Base32Alphabet::Hex,
                    padding: false,
                },
            },
            Step::Recode {
                from: "cp437".into(),
                to: "utf-8".into(),
            },
            Step::Interleave {
                streams: 2,
                width: 4,
            },
        ]);
        let r = Recipe {
            name: Some("test".into()),
            steps,
        };
        let json = r.to_json().unwrap();
        assert_eq!(Recipe::from_json(&json).unwrap(), r);
        assert!(json.contains("\"operand\": \"18446744073709551615\""));
        let ui = r#"{"steps":[
            {"kind":"bitwise","op":"xor","operand":"0xff"},
            {"kind":"arithmetic","op":"sub","operand":-1,"width":16,"endian":"big"},
            {"kind":"decode","codec":{"kind":"base64"}},
            {"kind":"decompress","codec":{"kind":"gzip"}}
        ]}"#;
        let r = Recipe::from_json(ui).unwrap();
        assert_eq!(
            r.steps[0],
            Step::Bitwise {
                op: BitOp::Xor,
                operand: 0xFF,
                width: Width::W8,
                endian: Endian::Little
            }
        );
        assert!(matches!(
            r.steps[1],
            Step::Arithmetic {
                operand: u64::MAX,
                width: Width::W16,
                ..
            }
        ));
        assert!(Recipe::from_json(r#"{"steps":[{"kind":"nope"}]}"#).is_err());
        assert!(
            Recipe::from_json(
                r#"{"steps":[{"kind":"shift","direction":"left","amount":1,"width":12}]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn empty_recipe_is_identity() {
        assert_eq!(apply(&Recipe::default(), b"abc").unwrap(), b"abc");
        assert_eq!(reverse_apply(&[], b"abc").unwrap(), b"abc");
    }

    #[test]
    fn preview_matches_apply() {
        let data = sample(1_000_000);
        let recipes = [
            Recipe::new(vec![Step::Not, Step::Encode { codec: b64() }]),
            Recipe::new(vec![
                Step::Encode { codec: b64() },
                Step::Decode { codec: b64() },
                Step::Encode {
                    codec: Codec::Base16 { uppercase: true },
                },
            ]),
            Recipe::new(vec![Step::Reverse]),
            Recipe::new(vec![Step::Encrypt {
                cipher: Cipher::Aes {
                    mode: AesMode::Cbc,
                    key: HexBytes(vec![1; 16]),
                    iv: HexBytes(vec![2; 16]),
                    padding: Padding::Pkcs7,
                    aad: HexBytes::default(),
                    tag: None,
                },
            }]),
            Recipe::new(vec![Step::Encode {
                codec: Codec::Uuencode {
                    file_name: "x".into(),
                },
            }]),
        ];
        for r in &recipes {
            let full = apply(r, &data).unwrap();
            for max in [0, 1, 100, 5000] {
                let p = preview(r, &data, max).unwrap();
                assert_eq!(p.output, full[..max.min(full.len())], "{r:?} {max}");
                assert!(p.truncated);
            }
        }
        let p = preview(&recipes[0], &data, 10).unwrap();
        assert!(p.input_truncated);
        assert_eq!(p.total_len, None);
        let p = preview(&recipes[2], &data, 10).unwrap();
        assert!(!p.input_truncated);
        assert_eq!(p.total_len, Some(data.len()));
        let p = preview(&recipes[0], b"ab", 100).unwrap();
        assert_eq!(
            (p.output.as_slice(), p.truncated, p.total_len),
            (&b"np0="[..], false, Some(4))
        );
    }

    #[test]
    fn preview_stops_decompression_early() {
        let big = vec![b'A'; 50 * 1024 * 1024];
        let z = compress::compress(&Compression::Gzip { level: 1 }, &big).unwrap();
        let r = Recipe::new(vec![
            Step::Decompress {
                codec: Compression::Gzip { level: 1 },
            },
            Step::Encode {
                codec: Codec::Base16 { uppercase: false },
            },
        ]);
        let p = preview(&r, &z, 64).unwrap();
        assert_eq!(p.output, b"41".repeat(32));
        assert!(p.truncated);
        assert_eq!(p.total_len, None);
        // small stream: computed fully
        let z = compress::compress(&Compression::Gzip { level: 1 }, b"hi").unwrap();
        let p = preview(&r, &z, 64).unwrap();
        assert_eq!((p.output.as_slice(), p.total_len), (&b"6869"[..], Some(4)));
        // errors are reported
        assert!(preview(&r, b"junk", 64).is_err());
    }
}
