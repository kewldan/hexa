//! STRC-04 (`ImHex` patterns), STRC-05 (010 Editor templates) and STRC-06
//! (C headers).

mod common;

use common::{all_paths, get, int, text, tree};
use hexa_core::Cancel;
use hexa_template::import::{CHeaderOptions, from_bt, from_c_header, from_hexpat};
use hexa_template::{EvalOptions, Program, TemplateResult, Value, evaluate};

fn eval(p: &Program, data: &[u8], root: Option<&str>) -> TemplateResult {
    let opts = EvalOptions {
        root_type: root.map(str::to_string),
        ..EvalOptions::default()
    };
    let r = evaluate(p, &tree(data), &opts, &Cancel::new()).unwrap();
    assert!(r.is_ok(), "{:#?}\n{:?}", r.errors, all_paths(&r));
    r
}

// ----- ImHex ---------------------------------------------------------------

#[test]
fn imhex_pattern_snippet() {
    let src = r#"
#pragma description Simple chunked container
#pragma endian little
#include <std/mem.pat>
import std.string;
import type.magic;

namespace fmt {
    enum ChunkType : u32 {
        Data = 0x41544144,
        End  = 0x20444E45
    };

    bitfield ChunkFlags {
        compressed : 1;
        encrypted  : 1;
        padding    : 6;
    };

    struct Chunk {
        ChunkType type;
        ChunkFlags flags;
        u8 reserved[3] [[hidden]];
        u32 length;
        u8 data[length] [[sealed]];
        if (type == ChunkType::End)
            break;
    } [[format("fmt::chunk_name")]];

    fn chunk_name(ref Chunk c) {
        return std::format("{} ({} bytes)", c.type, c.length);
    };
}

struct Header {
    char magic[4];
    u16 version;
    u16 chunk_count [[comment("informational")]];
};

Header header @ 0x00;
fmt::Chunk chunks[while(!std::mem::eof())] @ 0x08;
std::print("chunks: {}", chunks.length);
"#;
    let p = from_hexpat(src, None).unwrap();
    assert_eq!(p.pragma("description"), Some("Simple chunked container"));
    let mut d = b"CHNK".to_vec();
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&2u16.to_le_bytes());
    d.extend_from_slice(b"DATA");
    d.extend_from_slice(&[0b01, 0, 0, 0]);
    d.extend_from_slice(&3u32.to_le_bytes());
    d.extend_from_slice(b"xyz");
    d.extend_from_slice(b"END ");
    d.extend_from_slice(&[0, 0, 0, 0]);
    d.extend_from_slice(&0u32.to_le_bytes());
    let r = eval(&p, &d, None);
    assert_eq!(text(&r, "header.magic"), "CHNK");
    assert_eq!(get(&r, "chunks").len, 2);
    assert_eq!(get(&r, "chunks[0].type").enum_name.as_deref(), Some("Data"));
    assert_eq!(
        get(&r, "chunks[0].flags.compressed").raw,
        Some(Value::Int(1))
    );
    assert_eq!(get(&r, "chunks[1].type").enum_name.as_deref(), Some("End"));
    assert!(get(&r, "chunks[0].reserved").hidden);
    assert!(
        get(&r, "chunks[0]")
            .value
            .as_deref()
            .unwrap()
            .contains("3 bytes")
    );
    assert_eq!(r.log, ["chunks: 2"]);
}

#[test]
fn imhex_templates_pointers_and_type_wrappers() {
    let src = r"
struct Array<T, auto N> { T items[N]; };
struct Node { u8 value; u8 next_off; };
type::Hex<u32> magic @ 0;
Array<be u16, 2> words @ 4;
Node *head : u8 @ 8;
";
    let p = from_hexpat(src, None).unwrap();
    let r = eval(&p, &[0xEF, 0xBE, 0xAD, 0xDE, 0, 1, 0, 2, 10, 0, 7, 0], None);
    assert_eq!(get(&r, "magic").value.as_deref(), Some("0xDEADBEEF"));
    assert_eq!(get(&r, "words.items").len, 2);
    assert_eq!(
        hexa_template::children(&r, get(&r, "words.items").id, 1..2)[0].raw,
        Some(Value::Int(2))
    );
    assert_eq!(int(&r, "head.*head.value"), 7);
}

// ----- 010 Editor --------------------------------------------------------------

const BT: &str = r#"
//------------------------------------------------
//--- 010 Editor Binary Template
//   File: SIMPLE.bt
//------------------------------------------------
LittleEndian();

typedef enum <ushort> {
    REC_TEXT = 1,
    REC_INT  = 2
} REC_TYPE;

typedef struct {
    char   magic[4] <comment="Signature", bgcolor=cLtBlue>;
    DWORD  version <format=hex>;
    uint   count;
    local int i;
} HEADER <read=ReadHeader>;

string ReadHeader(HEADER &h) {
    local string s;
    SPrintf(s, "v%d", h.version);
    return Str("v%d (%d records)", h.version, h.count);
}

typedef struct (int index) {
    REC_TYPE type;
    ushort   len;
    local int idx = index;
    switch (type) {
        case REC_TEXT:
            char text[len];
            break;
        case REC_INT:
            int value;
            break;
        default:
            uchar raw[len];
    }
    int  flag : 1;
    int  kind : 7;
    int  rest : 24;
} RECORD;

HEADER hdr;
local int n;
for (n = 0; n < hdr.count; n++) {
    RECORD rec(n);
}
if (!FEof()) {
    FSeek(FileSize() - 2);
    WORD trailer <format=hex>;
}
Printf("records=%d first=%s\n", hdr.count, rec[0].text);
"#;

#[test]
fn bt_template() {
    let p = from_bt(BT, None).unwrap();
    let mut d = b"SMPL".to_vec();
    d.extend_from_slice(&0x0102u32.to_le_bytes());
    d.extend_from_slice(&2u32.to_le_bytes());
    // record 0: text
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&3u16.to_le_bytes());
    d.extend_from_slice(b"abc");
    d.extend_from_slice(&0x0000_0005u32.to_le_bytes()); // flag=1 kind=2
    // record 1: int
    d.extend_from_slice(&2u16.to_le_bytes());
    d.extend_from_slice(&4u16.to_le_bytes());
    d.extend_from_slice(&(-7i32).to_le_bytes());
    d.extend_from_slice(&0u32.to_le_bytes());
    d.extend_from_slice(&[0xAA, 0xBB, 0xCD, 0xAB]);
    let r = eval(&p, &d, None);
    assert_eq!(text(&r, "hdr.magic"), "SMPL");
    assert_eq!(get(&r, "hdr.magic").comment.as_deref(), Some("Signature"));
    assert_eq!(get(&r, "hdr.version").value.as_deref(), Some("0x00000102"));
    assert_eq!(get(&r, "hdr").value.as_deref(), Some("v258 (2 records)"));
    let recs: Vec<_> = r
        .roots
        .iter()
        .filter(|&&id| r.nodes[id as usize].name == "rec")
        .collect();
    assert_eq!(recs.len(), 2);
    let rec0 = &r.nodes[*recs[0] as usize];
    let child = |rec: &hexa_template::Node, name: &str| {
        rec.children
            .iter()
            .map(|c| &r.nodes[*c as usize])
            .find(|n| n.name == name)
            .unwrap()
            .clone()
    };
    assert_eq!(child(rec0, "type").enum_name.as_deref(), Some("REC_TEXT"));
    assert_eq!(child(rec0, "text").raw, Some(Value::Str("abc".into())));
    assert_eq!(child(rec0, "flag").raw, Some(Value::Int(-1)));
    assert_eq!(child(rec0, "kind").raw, Some(Value::Int(2)));
    let rec1 = &r.nodes[*recs[1] as usize];
    assert_eq!(child(rec1, "value").raw, Some(Value::Int(-7)));
    assert_eq!(get(&r, "trailer").value.as_deref(), Some("0xABCD"));
    assert_eq!(r.log, ["records=2 first=abc"]);
}

// ----- C headers ---------------------------------------------------------------

const HEADER: &str = r"
#include <stdint.h>
#define NAME_LEN 6

typedef enum { MODE_A, MODE_B = 4 } mode_t;

struct inner {
    uint16_t a;
    uint8_t b;
};

typedef struct record {
    uint8_t  tag;          /* 0 */
    uint32_t value;        /* 4 */
    uint16_t short_val;    /* 8 */
    struct inner in;       /* 10, size 4 */
    char     name[NAME_LEN]; /* 14 */
    uint64_t big;          /* 24 */
    mode_t   mode;         /* 32 */
    unsigned flags : 3;    /* 36, bits 0..2 */
    unsigned level : 5;    /*      bits 3..7 */
    uint8_t  after;        /* 37 */
    double   ratio;        /* 40 */
    char    *ptr;          /* 48 */
} record_t;                /* size 56 */

#pragma pack(push, 1)
struct packed {
    uint8_t  a;
    uint32_t b;
    uint16_t c;
};
#pragma pack(pop)

struct __attribute__((packed)) attr_packed { uint8_t a; uint16_t b; };

struct shared_bits {
    uint8_t x;
    uint16_t f : 4;
    uint16_t g : 3;
};

union u { uint32_t whole; uint8_t bytes[4]; };
";

fn record_bytes() -> Vec<u8> {
    let mut d = vec![0u8; 56];
    d[0] = 0x11;
    d[4..8].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    d[8..10].copy_from_slice(&0x1234u16.to_le_bytes());
    d[10..12].copy_from_slice(&7u16.to_le_bytes());
    d[12] = 9;
    d[14..20].copy_from_slice(b"hello\0");
    d[24..32].copy_from_slice(&u64::MAX.to_le_bytes());
    d[32..36].copy_from_slice(&4i32.to_le_bytes());
    d[36] = 0b10110_101; // flags=5, level=22
    d[37] = 0x42;
    d[40..48].copy_from_slice(&0.25f64.to_le_bytes());
    d[48..56].copy_from_slice(&0x1000u64.to_le_bytes());
    d
}

#[test]
fn c_header_natural_layout() {
    let p = from_c_header(HEADER, CHeaderOptions::default(), None).unwrap();
    let r = eval(&p, &record_bytes(), Some("record_t"));
    let off = |path: &str| get(&r, &format!("record_t.{path}")).offset;
    assert_eq!(get(&r, "record_t").size, 56);
    assert_eq!(off("value"), 4);
    assert_eq!(off("short_val"), 8);
    assert_eq!(off("in"), 10);
    assert_eq!(get(&r, "record_t.in").size, 4);
    assert_eq!(off("name"), 14);
    assert_eq!(off("big"), 24);
    assert_eq!(off("mode"), 32);
    assert_eq!(off("flags"), 36);
    assert_eq!(off("after"), 37);
    assert_eq!(off("ratio"), 40);
    assert_eq!(off("ptr"), 48);
    assert_eq!(int(&r, "record_t.value"), 0xDEAD_BEEF);
    assert_eq!(int(&r, "record_t.in.b"), 9);
    assert_eq!(text(&r, "record_t.name"), "hello");
    assert_eq!(
        get(&r, "record_t.mode").enum_name.as_deref(),
        Some("MODE_B")
    );
    assert_eq!(int(&r, "record_t.flags"), 5);
    assert_eq!(int(&r, "record_t.level"), 22);
    assert_eq!(get(&r, "record_t.ptr").type_name, "char*");
    assert_eq!(int(&r, "record_t.ptr"), 0x1000);
}

#[test]
fn c_header_packing_and_shared_bitfields() {
    let p = from_c_header(HEADER, CHeaderOptions::default(), None).unwrap();
    let r = eval(&p, &[1, 2, 0, 0, 0, 3, 0], Some("packed"));
    assert_eq!(get(&r, "packed").size, 7);
    assert_eq!(int(&r, "packed.c"), 3);
    let r = eval(&p, &[1, 2, 0], Some("attr_packed"));
    assert_eq!(get(&r, "attr_packed").size, 3);
    // GCC/Clang: f and g share byte 1 with... the storage after `x`.
    let r = eval(&p, &[0xAA, 0x53, 0, 0], Some("shared_bits"));
    assert_eq!(get(&r, "shared_bits").size, 2);
    assert_eq!(int(&r, "shared_bits.f"), 3);
    assert_eq!(int(&r, "shared_bits.g"), 5);
    let r = eval(&p, &[1, 2, 3, 4], Some("u"));
    assert_eq!(get(&r, "u").size, 4);
    assert_eq!(int(&r, "u.whole"), 0x0403_0201);
}

#[test]
fn c_header_long_and_pointer_sizes() {
    let src = "struct s { long a; void *p; };";
    let lp64 = from_c_header(src, CHeaderOptions::default(), None).unwrap();
    let r = eval(&lp64, &[0; 16], Some("s"));
    assert_eq!(get(&r, "s").size, 16);
    let llp64 = from_c_header(
        src,
        CHeaderOptions {
            long_size: 4,
            pointer_size: 4,
        },
        None,
    )
    .unwrap();
    let r = eval(&llp64, &[0; 8], Some("s"));
    assert_eq!(get(&r, "s").size, 8);
}

#[test]
fn big_endian_bitfields_follow_compilers() {
    let src = "#pragma endian big\nstruct T { u8 a : 3; u8 b : 5; };";
    let p = from_hexpat(src, None).unwrap();
    let r = eval(&p, &[0b1010_1010], Some("T"));
    assert_eq!(int(&r, "t.a"), 5);
    assert_eq!(int(&r, "t.b"), 10);
}

#[test]
fn dialect_errors_are_reported() {
    assert!(from_bt("typedef struct { int a } X;", None).is_err());
    assert!(from_c_header("struct { int a; ", CHeaderOptions::default(), None).is_err());
    assert!(from_hexpat("#include \"missing.hexpat\"", None).is_err());
}
