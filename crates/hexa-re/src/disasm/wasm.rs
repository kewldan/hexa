//! Built-in WebAssembly bytecode decoder (ANL-12).
//!
//! Covers the MVP opcode space plus sign-extension, non-trapping
//! float-to-int, bulk-memory, reference-types and tail-call opcodes. SIMD
//! (`0xfd`) and threads (`0xfe`) prefixes are shown as data.

use super::{InsnGroup, Instruction, data_insn};

#[derive(Clone, Copy)]
enum Imm {
    None,
    BlockType,
    U32,
    U32x2,
    BrTable,
    MemArg,
    I32,
    I64,
    F32,
    F64,
    Byte,
    SelectT,
}

fn op_info(op: u8) -> Option<(&'static str, Imm)> {
    use Imm::{
        BlockType, BrTable, Byte, F32, F64, I32, I64, MemArg, None as N, SelectT, U32, U32x2,
    };
    Some(match op {
        0x00 => ("unreachable", N),
        0x01 => ("nop", N),
        0x02 => ("block", BlockType),
        0x03 => ("loop", BlockType),
        0x04 => ("if", BlockType),
        0x05 => ("else", N),
        0x0b => ("end", N),
        0x0c => ("br", U32),
        0x0d => ("br_if", U32),
        0x0e => ("br_table", BrTable),
        0x0f => ("return", N),
        0x10 => ("call", U32),
        0x11 => ("call_indirect", U32x2),
        0x12 => ("return_call", U32),
        0x13 => ("return_call_indirect", U32x2),
        0x1a => ("drop", N),
        0x1b => ("select", N),
        0x1c => ("select", SelectT),
        0x20 => ("local.get", U32),
        0x21 => ("local.set", U32),
        0x22 => ("local.tee", U32),
        0x23 => ("global.get", U32),
        0x24 => ("global.set", U32),
        0x25 => ("table.get", U32),
        0x26 => ("table.set", U32),
        0x28 => ("i32.load", MemArg),
        0x29 => ("i64.load", MemArg),
        0x2a => ("f32.load", MemArg),
        0x2b => ("f64.load", MemArg),
        0x2c => ("i32.load8_s", MemArg),
        0x2d => ("i32.load8_u", MemArg),
        0x2e => ("i32.load16_s", MemArg),
        0x2f => ("i32.load16_u", MemArg),
        0x30 => ("i64.load8_s", MemArg),
        0x31 => ("i64.load8_u", MemArg),
        0x32 => ("i64.load16_s", MemArg),
        0x33 => ("i64.load16_u", MemArg),
        0x34 => ("i64.load32_s", MemArg),
        0x35 => ("i64.load32_u", MemArg),
        0x36 => ("i32.store", MemArg),
        0x37 => ("i64.store", MemArg),
        0x38 => ("f32.store", MemArg),
        0x39 => ("f64.store", MemArg),
        0x3a => ("i32.store8", MemArg),
        0x3b => ("i32.store16", MemArg),
        0x3c => ("i64.store8", MemArg),
        0x3d => ("i64.store16", MemArg),
        0x3e => ("i64.store32", MemArg),
        0x3f => ("memory.size", Byte),
        0x40 => ("memory.grow", Byte),
        0x41 => ("i32.const", I32),
        0x42 => ("i64.const", I64),
        0x43 => ("f32.const", F32),
        0x44 => ("f64.const", F64),
        0xd0 => ("ref.null", Byte),
        0xd1 => ("ref.is_null", N),
        0xd2 => ("ref.func", U32),
        0x45..=0xc4 => (NUMERIC[(op - 0x45) as usize], N),
        _ => return None,
    })
}

const NUMERIC: [&str; 128] = [
    "i32.eqz",
    "i32.eq",
    "i32.ne",
    "i32.lt_s",
    "i32.lt_u",
    "i32.gt_s",
    "i32.gt_u",
    "i32.le_s",
    "i32.le_u",
    "i32.ge_s",
    "i32.ge_u",
    "i64.eqz",
    "i64.eq",
    "i64.ne",
    "i64.lt_s",
    "i64.lt_u",
    "i64.gt_s",
    "i64.gt_u",
    "i64.le_s",
    "i64.le_u",
    "i64.ge_s",
    "i64.ge_u",
    "f32.eq",
    "f32.ne",
    "f32.lt",
    "f32.gt",
    "f32.le",
    "f32.ge",
    "f64.eq",
    "f64.ne",
    "f64.lt",
    "f64.gt",
    "f64.le",
    "f64.ge",
    "i32.clz",
    "i32.ctz",
    "i32.popcnt",
    "i32.add",
    "i32.sub",
    "i32.mul",
    "i32.div_s",
    "i32.div_u",
    "i32.rem_s",
    "i32.rem_u",
    "i32.and",
    "i32.or",
    "i32.xor",
    "i32.shl",
    "i32.shr_s",
    "i32.shr_u",
    "i32.rotl",
    "i32.rotr",
    "i64.clz",
    "i64.ctz",
    "i64.popcnt",
    "i64.add",
    "i64.sub",
    "i64.mul",
    "i64.div_s",
    "i64.div_u",
    "i64.rem_s",
    "i64.rem_u",
    "i64.and",
    "i64.or",
    "i64.xor",
    "i64.shl",
    "i64.shr_s",
    "i64.shr_u",
    "i64.rotl",
    "i64.rotr",
    "f32.abs",
    "f32.neg",
    "f32.ceil",
    "f32.floor",
    "f32.trunc",
    "f32.nearest",
    "f32.sqrt",
    "f32.add",
    "f32.sub",
    "f32.mul",
    "f32.div",
    "f32.min",
    "f32.max",
    "f32.copysign",
    "f64.abs",
    "f64.neg",
    "f64.ceil",
    "f64.floor",
    "f64.trunc",
    "f64.nearest",
    "f64.sqrt",
    "f64.add",
    "f64.sub",
    "f64.mul",
    "f64.div",
    "f64.min",
    "f64.max",
    "f64.copysign",
    "i32.wrap_i64",
    "i32.trunc_f32_s",
    "i32.trunc_f32_u",
    "i32.trunc_f64_s",
    "i32.trunc_f64_u",
    "i64.extend_i32_s",
    "i64.extend_i32_u",
    "i64.trunc_f32_s",
    "i64.trunc_f32_u",
    "i64.trunc_f64_s",
    "i64.trunc_f64_u",
    "f32.convert_i32_s",
    "f32.convert_i32_u",
    "f32.convert_i64_s",
    "f32.convert_i64_u",
    "f32.demote_f64",
    "f64.convert_i32_s",
    "f64.convert_i32_u",
    "f64.convert_i64_s",
    "f64.convert_i64_u",
    "f64.promote_f32",
    "i32.reinterpret_f32",
    "i64.reinterpret_f64",
    "f32.reinterpret_i32",
    "f64.reinterpret_i64",
    "i32.extend8_s",
    "i32.extend16_s",
    "i64.extend8_s",
    "i64.extend16_s",
    "i64.extend32_s",
];

fn fc_info(sub: u32) -> Option<(&'static str, Imm)> {
    use Imm::{None as N, U32, U32x2};
    Some(match sub {
        0 => ("i32.trunc_sat_f32_s", N),
        1 => ("i32.trunc_sat_f32_u", N),
        2 => ("i32.trunc_sat_f64_s", N),
        3 => ("i32.trunc_sat_f64_u", N),
        4 => ("i64.trunc_sat_f32_s", N),
        5 => ("i64.trunc_sat_f32_u", N),
        6 => ("i64.trunc_sat_f64_s", N),
        7 => ("i64.trunc_sat_f64_u", N),
        8 => ("memory.init", U32x2),
        9 => ("data.drop", U32),
        10 => ("memory.copy", U32x2),
        11 => ("memory.fill", U32),
        12 => ("table.init", U32x2),
        13 => ("elem.drop", U32),
        14 => ("table.copy", U32x2),
        15 => ("table.grow", U32),
        16 => ("table.size", U32),
        17 => ("table.fill", U32),
        _ => return None,
    })
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Option<u8> {
        let v = *self.b.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }

    fn uleb(&mut self) -> Option<u64> {
        let mut result = 0u64;
        let mut shift = 0u32;
        loop {
            let b = self.byte()?;
            if shift < 64 {
                result |= u64::from(b & 0x7f) << shift;
            }
            shift += 7;
            if b & 0x80 == 0 {
                return Some(result);
            }
            if shift > 70 {
                return None;
            }
        }
    }

    fn sleb(&mut self) -> Option<i64> {
        let mut result = 0i64;
        let mut shift = 0u32;
        loop {
            let b = self.byte()?;
            if shift < 64 {
                result |= i64::from(b & 0x7f) << shift;
            }
            shift += 7;
            if b & 0x80 == 0 {
                if shift < 64 && b & 0x40 != 0 {
                    result |= -1i64 << shift;
                }
                return Some(result);
            }
            if shift > 70 {
                return None;
            }
        }
    }

    fn fixed<const N: usize>(&mut self) -> Option<[u8; N]> {
        let s = self.b.get(self.pos..self.pos + N)?;
        self.pos += N;
        s.try_into().ok()
    }
}

fn valtype(b: u8) -> String {
    match b {
        0x7f => "i32".into(),
        0x7e => "i64".into(),
        0x7d => "f32".into(),
        0x7c => "f64".into(),
        0x7b => "v128".into(),
        0x70 => "funcref".into(),
        0x6f => "externref".into(),
        _ => format!("0x{b:02x}"),
    }
}

fn immediates(r: &mut Reader<'_>, imm: Imm) -> Option<String> {
    Some(match imm {
        Imm::None => String::new(),
        Imm::BlockType => {
            let b = *r.b.get(r.pos)?;
            if b == 0x40 {
                r.pos += 1;
                String::new()
            } else if (0x6f..=0x7f).contains(&b) {
                r.pos += 1;
                format!("(result {})", valtype(b))
            } else {
                format!("(type {})", r.sleb()?)
            }
        }
        Imm::U32 => r.uleb()?.to_string(),
        Imm::U32x2 => {
            let a = r.uleb()?;
            let b = r.uleb()?;
            format!("{a} {b}")
        }
        Imm::BrTable => {
            let n = r.uleb()?;
            if n > 1 << 20 {
                return None;
            }
            let mut v = Vec::new();
            for _ in 0..=n {
                v.push(r.uleb()?.to_string());
            }
            v.join(" ")
        }
        Imm::MemArg => {
            let align = r.uleb()?;
            let offset = r.uleb()?;
            format!("offset={offset} align={}", 1u64.checked_shl(align as u32)?)
        }
        Imm::I32 => (r.sleb()? as i32).to_string(),
        Imm::I64 => r.sleb()?.to_string(),
        Imm::F32 => f32::from_le_bytes(r.fixed::<4>()?).to_string(),
        Imm::F64 => f64::from_le_bytes(r.fixed::<8>()?).to_string(),
        Imm::Byte => {
            let b = r.byte()?;
            if b == 0 { String::new() } else { valtype(b) }
        }
        Imm::SelectT => {
            let n = r.uleb()?;
            if n > 16 {
                return None;
            }
            let mut v = Vec::new();
            for _ in 0..n {
                v.push(valtype(r.byte()?));
            }
            format!("(result {})", v.join(" "))
        }
    })
}

fn decode_at(code: &[u8], pos: usize) -> Option<(usize, &'static str, String)> {
    let mut r = Reader { b: code, pos };
    let op = r.byte()?;
    let (name, imm) = if op == 0xfc {
        fc_info(u32::try_from(r.uleb()?).ok()?)?
    } else {
        op_info(op)?
    };
    let ops = immediates(&mut r, imm)?;
    Some((r.pos - pos, name, ops))
}

pub(super) fn disassemble(code: &[u8], address: u64, max: usize) -> Vec<Instruction> {
    let limit = if max == 0 { usize::MAX } else { max };
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < code.len() && out.len() < limit {
        let addr = address.wrapping_add(pos as u64);
        if let Some((len, name, ops)) = decode_at(code, pos) {
            let groups = match name {
                "br" | "br_if" | "br_table" => vec![InsnGroup::Jump],
                "call" | "call_indirect" => vec![InsnGroup::Call],
                "return_call" | "return_call_indirect" => vec![InsnGroup::Call, InsnGroup::Ret],
                "return" => vec![InsnGroup::Ret],
                "unreachable" => vec![InsnGroup::Interrupt],
                _ => Vec::new(),
            };
            out.push(Instruction {
                address: addr,
                size: len as u32,
                bytes: code[pos..pos + len].to_vec(),
                mnemonic: name.to_string(),
                operands: ops,
                groups,
                branch_target: None,
                memory_reference: None,
                is_data: false,
            });
            pos += len;
        } else {
            out.push(data_insn(addr, vec![code[pos]]));
            pos += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_function_body() {
        // local.get 0; i32.const -1; i32.add; call 3; br_table 0 1 0;
        // i32.load offset=8 align=4; f64.const 1.5; end
        let mut code = vec![
            0x20, 0x00, 0x41, 0x7f, 0x6a, 0x10, 0x03, 0x0e, 0x02, 0, 1, 0,
        ];
        code.extend([0x28, 0x02, 0x08, 0x44]);
        code.extend(1.5f64.to_le_bytes());
        code.extend([0x0b, 0xfc, 0x0a, 0x00, 0x00, 0xfd]);
        let v = disassemble(&code, 0x100, 0);
        let texts: Vec<String> = v.iter().map(Instruction::text).collect();
        assert_eq!(
            texts,
            vec![
                "local.get 0",
                "i32.const -1",
                "i32.add",
                "call 3",
                "br_table 0 1 0",
                "i32.load offset=8 align=4",
                "f64.const 1.5",
                "end",
                "memory.copy 0 0",
                ".byte 0xfd",
            ]
        );
        assert!(v[3].is_call());
        assert!(v[4].is_jump());
        assert_eq!(v[1].address, 0x102);
        assert!(v[9].is_data);
    }

    #[test]
    fn block_types() {
        let v = disassemble(&[0x02, 0x40, 0x03, 0x7f, 0x04, 0x05, 0x0f], 0, 0);
        assert_eq!(v[0].text(), "block");
        assert_eq!(v[1].text(), "loop (result i32)");
        assert_eq!(v[2].text(), "if (type 5)");
        assert!(v[3].is_ret());
        assert_eq!(
            disassemble(&[0x42, 0x80, 0x01], 0, 0)[0].text(),
            "i64.const 128"
        );
    }
}
