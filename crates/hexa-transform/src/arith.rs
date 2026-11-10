//! Element-wise wrapping arithmetic (XFRM-02).

use serde::{Deserialize, Serialize};

use crate::error::{Result, TransformError};
use crate::types::{Endian, Width, map_elems};

/// Arithmetic operation applied as `element = element <op> operand`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

fn sign_extend(v: u64, width: Width) -> i64 {
    let shift = 64 - width.bits();
    ((v << shift) as i64) >> shift
}

/// Applies `op` with `operand` to every whole element, wrapping on overflow.
/// `signed` only matters for `Div` and `Mod` (two's complement elements).
/// A trailing partial element is left unchanged.
pub fn arith(
    data: &[u8],
    op: ArithOp,
    operand: u64,
    width: Width,
    endian: Endian,
    signed: bool,
) -> Result<Vec<u8>> {
    let operand = operand & width.mask();
    if matches!(op, ArithOp::Div | ArithOp::Mod) && operand == 0 {
        return Err(TransformError::DivisionByZero);
    }
    Ok(map_elems(data, width, endian, |v| match op {
        ArithOp::Add => v.wrapping_add(operand),
        ArithOp::Sub => v.wrapping_sub(operand),
        ArithOp::Mul => v.wrapping_mul(operand),
        ArithOp::Div if signed => {
            sign_extend(v, width).wrapping_div(sign_extend(operand, width)) as u64
        }
        ArithOp::Mod if signed => {
            sign_extend(v, width).wrapping_rem(sign_extend(operand, width)) as u64
        }
        ArithOp::Div => v / operand,
        ArithOp::Mod => v % operand,
    }))
}

/// Multiplicative inverse of an odd `a` modulo 2^width (used to invert
/// multiplication for write-back, XFRM-09). `None` for even `a`.
pub fn mul_inverse(a: u64, width: Width) -> Option<u64> {
    let a = a & width.mask();
    if a & 1 == 0 {
        return None;
    }
    // Newton iteration doubles the number of correct low bits each round:
    // 3 -> 6 -> 12 -> 24 -> 48 -> 96 bits.
    let mut x = a;
    for _ in 0..5 {
        x = x.wrapping_mul(2u64.wrapping_sub(a.wrapping_mul(x)));
    }
    Some(x & width.mask())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_sub_wrap() {
        let d = [0xFF, 0x01];
        assert_eq!(
            arith(&d, ArithOp::Add, 1, Width::W8, Endian::Little, false).unwrap(),
            [0x00, 0x02]
        );
        assert_eq!(
            arith(&d, ArithOp::Sub, 2, Width::W8, Endian::Little, false).unwrap(),
            [0xFD, 0xFF]
        );
        // 16-bit LE 0x01FF + 1 = 0x0200
        assert_eq!(
            arith(&d, ArithOp::Add, 1, Width::W16, Endian::Little, false).unwrap(),
            [0x00, 0x02]
        );
        // 16-bit BE 0xFF01 + 0x100 = 0x0001 (wrap)
        assert_eq!(
            arith(&d, ArithOp::Add, 0x100, Width::W16, Endian::Big, false).unwrap(),
            [0x00, 0x01]
        );
    }

    #[test]
    fn mul_div_mod() {
        let d = 1000u32.to_le_bytes();
        let m = arith(&d, ArithOp::Mul, 3, Width::W32, Endian::Little, false).unwrap();
        assert_eq!(m, 3000u32.to_le_bytes());
        let q = arith(&d, ArithOp::Div, 7, Width::W32, Endian::Little, false).unwrap();
        assert_eq!(q, 142u32.to_le_bytes());
        let r = arith(&d, ArithOp::Mod, 7, Width::W32, Endian::Little, false).unwrap();
        assert_eq!(r, 6u32.to_le_bytes());
        assert!(matches!(
            arith(&d, ArithOp::Div, 0, Width::W32, Endian::Little, false),
            Err(TransformError::DivisionByZero)
        ));
        // operand masked to the width: 0x100 is zero for bytes
        assert!(arith(&d, ArithOp::Mod, 0x100, Width::W8, Endian::Little, false).is_err());
    }

    #[test]
    fn signed_division() {
        let d = (-7i16).to_be_bytes();
        let q = arith(&d, ArithOp::Div, 2, Width::W16, Endian::Big, true).unwrap();
        assert_eq!(q, (-3i16).to_be_bytes());
        let r = arith(&d, ArithOp::Mod, 2, Width::W16, Endian::Big, true).unwrap();
        assert_eq!(r, (-1i16).to_be_bytes());
        let q = arith(
            &d,
            ArithOp::Div,
            (-1i64) as u64,
            Width::W16,
            Endian::Big,
            true,
        )
        .unwrap();
        assert_eq!(q, 7i16.to_be_bytes());
        let min = i64::MIN.to_le_bytes();
        let q = arith(
            &min,
            ArithOp::Div,
            u64::MAX,
            Width::W64,
            Endian::Little,
            true,
        )
        .unwrap();
        assert_eq!(q, i64::MIN.to_le_bytes());
    }

    #[test]
    fn inverse_of_odd_multiplier() {
        for w in [Width::W8, Width::W16, Width::W32, Width::W64] {
            for a in [1u64, 3, 5, 0xFF, 0x1234_5679, u64::MAX] {
                let inv = mul_inverse(a, w).unwrap();
                assert_eq!(a.wrapping_mul(inv) & w.mask(), 1, "{a} {w:?}");
            }
        }
        assert_eq!(mul_inverse(2, Width::W8), None);
    }
}
