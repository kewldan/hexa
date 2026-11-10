//! STRC-07: types from DWARF and PDB.
//!
//! DWARF is generated in-test with `gimli::write` and wrapped into ELF
//! objects with `object::write`; each imported type is then applied to
//! hand-built bytes and checked against the layout encoded in the DWARF.

use gimli::write::{
    AttributeValue as A, Dwarf, EndianVec, LineProgram, Sections, Unit, UnitEntryId,
};
use gimli::{DwAt, DwAte, DwTag, Encoding, Format, RunTimeEndian};
use hexa_core::piece::Piece;
use hexa_core::{Cancel, Tree};
use hexa_template::ast::{Endian, StructKind, TypeDef};
use hexa_template::import::debuginfo::{
    EnumType, Member, ModelType, Record, Ty, TypeModel, emit, from_dwarf, from_pdb,
};
use hexa_template::{Error, EvalOptions, Node, Program, TemplateResult, Value, evaluate};
use object::write::Object;
use object::{Architecture, BinaryFormat, Endianness, SectionKind};

// ----- helpers ---------------------------------------------------------------

fn run(p: &Program, root: &str, bytes: &[u8]) -> TemplateResult {
    let tree = Tree::from_piece(Piece::bytes(bytes.to_vec()));
    let opts = EvalOptions {
        root_type: Some(root.to_string()),
        ..EvalOptions::default()
    };
    let r = evaluate(p, &tree, &opts, &Cancel::new()).expect("evaluate");
    assert!(r.is_ok(), "evaluation errors: {:#?}", r.errors);
    r
}

fn node<'a>(r: &'a TemplateResult, path: &str) -> &'a Node {
    r.find(path).unwrap_or_else(|| {
        let paths: Vec<String> = r.nodes.iter().map(|n| r.path(n.id)).collect();
        panic!("no node `{path}`; have {paths:?}")
    })
}

fn int(r: &TemplateResult, path: &str) -> i128 {
    node(r, path)
        .raw
        .as_ref()
        .and_then(Value::as_int)
        .unwrap_or_else(|| panic!("`{path}` has no integer value"))
}

fn at(r: &TemplateResult, path: &str) -> (u64, u64) {
    let n = node(r, path);
    (n.offset, n.size)
}

fn field_names(p: &Program, ty: &str) -> Vec<String> {
    match p.type_def(ty) {
        Some(TypeDef::Struct(s)) => s
            .body
            .iter()
            .filter_map(|st| match &st.kind {
                hexa_template::ast::StmtKind::Field(f) => Some(f.name.clone()),
                _ => None,
            })
            .collect(),
        other => panic!("`{ty}` is not a struct: {other:?}"),
    }
}

struct Cu<'u> {
    u: &'u mut Unit,
}

impl Cu<'_> {
    fn die(
        &mut self,
        parent: Option<UnitEntryId>,
        tag: DwTag,
        attrs: Vec<(DwAt, A)>,
    ) -> UnitEntryId {
        let parent = parent.unwrap_or_else(|| self.u.root());
        let id = self.u.add(parent, tag);
        let e = self.u.get_mut(id);
        for (a, v) in attrs {
            e.set(a, v);
        }
        id
    }

    fn base(&mut self, name: &str, enc: DwAte, size: u64) -> UnitEntryId {
        self.die(
            None,
            gimli::DW_TAG_base_type,
            vec![n(name), (gimli::DW_AT_encoding, A::Encoding(enc)), sz(size)],
        )
    }

    fn typedef(&mut self, name: &str, target: UnitEntryId) -> UnitEntryId {
        self.die(None, gimli::DW_TAG_typedef, vec![n(name), ty(target)])
    }

    fn record(&mut self, tag: DwTag, name: Option<&str>, size: u64) -> UnitEntryId {
        let mut attrs = vec![sz(size)];
        if let Some(name) = name {
            attrs.push(n(name));
        }
        self.die(None, tag, attrs)
    }

    fn member(&mut self, parent: UnitEntryId, name: Option<&str>, t: UnitEntryId, off: u64) {
        let mut attrs = vec![ty(t), (gimli::DW_AT_data_member_location, A::Udata(off))];
        if let Some(name) = name {
            attrs.push(n(name));
        }
        self.die(Some(parent), gimli::DW_TAG_member, attrs);
    }

    /// DWARF 4+ bit-field (`DW_AT_data_bit_offset`).
    fn bits(&mut self, parent: UnitEntryId, name: &str, t: UnitEntryId, bit_off: u64, width: u64) {
        self.die(
            Some(parent),
            gimli::DW_TAG_member,
            vec![
                n(name),
                ty(t),
                (gimli::DW_AT_data_bit_offset, A::Udata(bit_off)),
                (gimli::DW_AT_bit_size, A::Udata(width)),
            ],
        );
    }

    /// DWARF 2/3 bit-field (`DW_AT_bit_offset` from the storage unit's MSB).
    fn legacy_bits(
        &mut self,
        parent: UnitEntryId,
        name: &str,
        t: UnitEntryId,
        unit: (u64, u64),
        bit_off: u64,
        width: u64,
    ) {
        self.die(
            Some(parent),
            gimli::DW_TAG_member,
            vec![
                n(name),
                ty(t),
                (gimli::DW_AT_data_member_location, A::Udata(unit.0)),
                sz(unit.1),
                (gimli::DW_AT_bit_offset, A::Udata(bit_off)),
                (gimli::DW_AT_bit_size, A::Udata(width)),
            ],
        );
    }
}

fn n(name: &str) -> (DwAt, A) {
    (gimli::DW_AT_name, A::String(name.as_bytes().to_vec()))
}

fn sz(size: u64) -> (DwAt, A) {
    (gimli::DW_AT_byte_size, A::Udata(size))
}

fn ty(t: UnitEntryId) -> (DwAt, A) {
    (gimli::DW_AT_type, A::UnitRef(t))
}

fn add_unit(dwarf: &mut Dwarf, version: u16) -> gimli::write::UnitId {
    let enc = Encoding {
        format: Format::Dwarf32,
        version,
        address_size: 8,
    };
    dwarf.units.add(Unit::new(enc, LineProgram::none()))
}

fn elf(dwarf: &mut Dwarf, big: bool) -> Vec<u8> {
    let endian = if big {
        RunTimeEndian::Big
    } else {
        RunTimeEndian::Little
    };
    let mut sections = Sections::new(EndianVec::new(endian));
    dwarf.write(&mut sections).expect("write DWARF");
    let (arch, oe) = if big {
        (Architecture::PowerPc64, Endianness::Big)
    } else {
        (Architecture::X86_64, Endianness::Little)
    };
    let mut obj = Object::new(BinaryFormat::Elf, arch, oe);
    sections
        .for_each(|id, data| -> Result<(), ()> {
            let bytes = data.slice();
            if !bytes.is_empty() {
                let s = obj.add_section(
                    Vec::new(),
                    id.name().as_bytes().to_vec(),
                    SectionKind::Debug,
                );
                obj.append_section_data(s, bytes, 1);
            }
            Ok(())
        })
        .unwrap();
    obj.write().expect("write ELF")
}

// ----- little-endian object --------------------------------------------------

/// Two compilation units, as a C compiler for x86-64 would describe:
///
/// ```c
/// enum Color { RED, GREEN, BLUE, NEG = -1 };
/// struct Inner { uint16_t x; uint8_t y; };                       // 4 bytes
/// struct S {
///     uint8_t a; uint32_t b;                                      // 0, 4
///     uint16_t c : 3; uint16_t d : 5; int16_t e : 6;              // bits 64..78
///     struct Inner in;                                            // 10
///     int arr[2][3];                                              // 16
///     char *p;                                                    // 40
///     enum Color col;                                             // 48
/// };                                                              // 56 bytes
/// union U { uint32_t i; float f; uint8_t bytes[4]; uint16_t lo : 12; };
/// typedef struct S S_t;
/// typedef struct { uint16_t v; } Anon;
/// struct W { uint8_t tag; union { uint16_t h; uint8_t b; }; };   // 4 bytes
/// struct Derived : Inner { const uint8_t z; };                    // 6 bytes
/// struct Opaque;                                                  // declaration only
/// struct Point { uint32_t x; };
/// // second unit (DWARF 3, legacy bit offsets):
/// struct Inner { uint16_t x; uint8_t y; };                        // identical
/// struct Point { uint8_t x; };                                    // differs
/// struct Legacy { uint8_t x; uint16_t f : 4; uint16_t g : 3; };   // 2 bytes
/// ```
fn little_endian_object() -> Vec<u8> {
    let mut dwarf = Dwarf::new();
    let u1 = add_unit(&mut dwarf, 5);
    {
        let mut c = Cu {
            u: dwarf.units.get_mut(u1),
        };
        let uchar = c.base("unsigned char", gimli::DW_ATE_unsigned_char, 1);
        let uint = c.base("unsigned int", gimli::DW_ATE_unsigned, 4);
        let ushort = c.base("unsigned short", gimli::DW_ATE_unsigned, 2);
        let short = c.base("short", gimli::DW_ATE_signed, 2);
        let int = c.base("int", gimli::DW_ATE_signed, 4);
        let ch = c.base("char", gimli::DW_ATE_signed_char, 1);
        let float = c.base("float", gimli::DW_ATE_float, 4);
        let u8t = c.typedef("uint8_t", uchar);
        let u16t = c.typedef("uint16_t", ushort);
        let u32t = c.typedef("uint32_t", uint);
        let i16t = c.typedef("int16_t", short);

        let color = c.die(
            None,
            gimli::DW_TAG_enumeration_type,
            vec![n("Color"), sz(4), ty(int)],
        );
        for (name, v) in [("RED", 0), ("GREEN", 1), ("BLUE", 2), ("NEG", -1)] {
            c.die(
                Some(color),
                gimli::DW_TAG_enumerator,
                vec![n(name), (gimli::DW_AT_const_value, A::Sdata(v))],
            );
        }

        let inner = c.record(gimli::DW_TAG_structure_type, Some("Inner"), 4);
        c.member(inner, Some("x"), u16t, 0);
        c.member(inner, Some("y"), u8t, 2);

        let arr = c.die(None, gimli::DW_TAG_array_type, vec![ty(int)]);
        c.die(
            Some(arr),
            gimli::DW_TAG_subrange_type,
            vec![(gimli::DW_AT_upper_bound, A::Udata(1))],
        );
        c.die(
            Some(arr),
            gimli::DW_TAG_subrange_type,
            vec![(gimli::DW_AT_count, A::Udata(3))],
        );
        let pchar = c.die(None, gimli::DW_TAG_pointer_type, vec![sz(8), ty(ch)]);

        let s = c.record(gimli::DW_TAG_structure_type, Some("S"), 56);
        c.member(s, Some("a"), u8t, 0);
        c.member(s, Some("b"), u32t, 4);
        c.bits(s, "c", u16t, 64, 3);
        c.bits(s, "d", u16t, 67, 5);
        c.bits(s, "e", i16t, 72, 6);
        c.member(s, Some("in"), inner, 10);
        c.member(s, Some("arr"), arr, 16);
        c.member(s, Some("p"), pchar, 40);
        c.member(s, Some("col"), color, 48);

        let bytes4 = c.die(None, gimli::DW_TAG_array_type, vec![ty(u8t)]);
        c.die(
            Some(bytes4),
            gimli::DW_TAG_subrange_type,
            vec![(gimli::DW_AT_count, A::Udata(4))],
        );
        let un = c.record(gimli::DW_TAG_union_type, Some("U"), 4);
        c.member(un, Some("i"), u32t, 0);
        c.member(un, Some("f"), float, 0);
        c.member(un, Some("bytes"), bytes4, 0);
        c.bits(un, "lo", u16t, 0, 12);

        c.typedef("S_t", s);
        let anon = c.record(gimli::DW_TAG_structure_type, None, 2);
        c.member(anon, Some("v"), u16t, 0);
        c.typedef("Anon", anon);

        let w = c.record(gimli::DW_TAG_structure_type, Some("W"), 4);
        let wu = c.die(Some(w), gimli::DW_TAG_union_type, vec![sz(2)]);
        c.member(wu, Some("h"), u16t, 0);
        c.member(wu, Some("b"), u8t, 0);
        c.member(w, Some("tag"), u8t, 0);
        c.member(w, None, wu, 2);

        let cu8 = c.die(None, gimli::DW_TAG_const_type, vec![ty(u8t)]);
        let derived = c.record(gimli::DW_TAG_structure_type, Some("Derived"), 6);
        c.die(
            Some(derived),
            gimli::DW_TAG_inheritance,
            vec![ty(inner), (gimli::DW_AT_data_member_location, A::Udata(0))],
        );
        c.member(derived, Some("z"), cu8, 4);

        c.die(
            None,
            gimli::DW_TAG_structure_type,
            vec![n("Opaque"), (gimli::DW_AT_declaration, A::FlagPresent)],
        );

        let point = c.record(gimli::DW_TAG_structure_type, Some("Point"), 4);
        c.member(point, Some("x"), u32t, 0);
    }

    let u2 = add_unit(&mut dwarf, 3);
    {
        let mut c = Cu {
            u: dwarf.units.get_mut(u2),
        };
        let uchar = c.base("unsigned char", gimli::DW_ATE_unsigned_char, 1);
        let ushort = c.base("unsigned short", gimli::DW_ATE_unsigned, 2);
        let u8t = c.typedef("uint8_t", uchar);
        let u16t = c.typedef("uint16_t", ushort);

        let inner = c.record(gimli::DW_TAG_structure_type, Some("Inner"), 4);
        c.member(inner, Some("x"), u16t, 0);
        c.member(inner, Some("y"), u8t, 2);

        let point = c.record(gimli::DW_TAG_structure_type, Some("Point"), 1);
        c.member(point, Some("x"), u8t, 0);

        let legacy = c.record(gimli::DW_TAG_structure_type, Some("Legacy"), 2);
        c.member(legacy, Some("x"), u8t, 0);
        // f: bits 8..12, g: bits 12..15 of a 2-byte unit at offset 0.
        c.legacy_bits(legacy, "f", u16t, (0, 2), 16 - 8 - 4, 4);
        c.legacy_bits(legacy, "g", u16t, (0, 2), 16 - 12 - 3, 3);
    }
    elf(&mut dwarf, false)
}

#[test]
fn struct_layout_matches_dwarf_offsets() {
    let p = from_dwarf(&little_endian_object()).expect("import");
    assert!(p.pragma("endian").is_none());

    let mut data = vec![0u8; 56];
    data[0] = 0x11;
    data[4..8].copy_from_slice(&0x4433_2211u32.to_le_bytes());
    data[8] = (22 << 3) | 5; // d = 22, c = 5
    data[9] = 0b11_1101; // e = -3 (6 bits)
    data[10..12].copy_from_slice(&0x0102u16.to_le_bytes());
    data[12] = 7;
    for (i, v) in (1..=6i32).enumerate() {
        data[16 + i * 4..20 + i * 4].copy_from_slice(&v.to_le_bytes());
    }
    data[40..48].copy_from_slice(&0x1000u64.to_le_bytes());
    data[48..52].copy_from_slice(&2u32.to_le_bytes());
    data.extend_from_slice(&[0xFF; 8]); // must not be consumed

    let r = run(&p, "S", &data);
    assert_eq!(at(&r, "s"), (0, 56));
    assert_eq!(int(&r, "s.a"), 0x11);
    assert_eq!(at(&r, "s.b"), (4, 4));
    assert_eq!(int(&r, "s.b"), 0x4433_2211);
    assert_eq!(node(&r, "s.b").type_name, "uint32_t");
    assert_eq!(at(&r, "s.__anon_bits0"), (8, 2));
    assert_eq!(int(&r, "s.__anon_bits0.c"), 5);
    assert_eq!(int(&r, "s.__anon_bits0.d"), 22);
    assert_eq!(int(&r, "s.__anon_bits0.e"), -3);
    assert_eq!(at(&r, "s.in"), (10, 4));
    assert_eq!(int(&r, "s.in.x"), 0x0102);
    assert_eq!(int(&r, "s.in.y"), 7);
    let arr = node(&r, "s.arr");
    assert_eq!((arr.offset, arr.size, arr.len), (16, 24, 6));
    assert_eq!(at(&r, "s.p"), (40, 8));
    assert_eq!(int(&r, "s.p"), 0x1000);
    assert_eq!(node(&r, "s.p").type_name, "char*");
    let col = node(&r, "s.col");
    assert_eq!((col.offset, col.size), (48, 4));
    assert_eq!(col.enum_name.as_deref(), Some("BLUE"));

    // Explicit layout: no C alignment rules, gaps are padding.
    assert_eq!(
        field_names(&p, "S"),
        [
            "a",
            "padding",
            "b",
            "__anon_bits0",
            "in",
            "padding",
            "arr",
            "p",
            "col",
            "padding"
        ]
    );
}

#[test]
fn enums_unions_typedefs_and_anonymous_types() {
    let p = from_dwarf(&little_endian_object()).expect("import");

    match p.type_def("Color") {
        Some(TypeDef::Enum(e)) => {
            assert_eq!(e.base.name, "s32");
            let names: Vec<&str> = e.variants.iter().map(|v| v.name.as_str()).collect();
            assert_eq!(names, ["RED", "GREEN", "BLUE", "NEG"]);
        }
        other => panic!("Color: {other:?}"),
    }
    let r = run(&p, "Color", &(-1i32).to_le_bytes());
    assert_eq!(node(&r, "color").enum_name.as_deref(), Some("NEG"));

    match p.type_def("U") {
        Some(TypeDef::Struct(s)) => assert_eq!(s.kind, StructKind::Union),
        other => panic!("U: {other:?}"),
    }
    let r = run(&p, "U", &[0x78, 0x56, 0x34, 0x12, 0xFF]);
    assert_eq!(at(&r, "u"), (0, 4));
    assert_eq!(int(&r, "u.i"), 0x1234_5678);
    assert_eq!(int(&r, "u.__anon_bits0.lo"), 0x678);
    assert_eq!(at(&r, "u.bytes"), (0, 4));
    assert!(matches!(node(&r, "u.f").raw, Some(Value::Float(_))));

    // `typedef struct S S_t;` stays an alias; `typedef struct {..} Anon;`
    // names the anonymous struct itself.
    assert!(matches!(p.type_def("S_t"), Some(TypeDef::Alias(a)) if a.target.name == "S"));
    assert!(matches!(p.type_def("Anon"), Some(TypeDef::Struct(_))));
    let r = run(&p, "S_t", &[0u8; 56]);
    assert_eq!(at(&r, "s_t"), (0, 56));

    // Anonymous union member.
    let r = run(&p, "W", &[1, 0xEE, 0x34, 0x12]);
    assert_eq!(at(&r, "w"), (0, 4));
    assert_eq!(int(&r, "w.tag"), 1);
    assert_eq!(at(&r, "w.__anon1"), (2, 2));
    assert_eq!(int(&r, "w.__anon1.h"), 0x1234);
    assert_eq!(int(&r, "w.__anon1.b"), 0x34);

    // Base class and a const member.
    let r = run(&p, "Derived", &[0x02, 0x01, 9, 0, 0x42, 0]);
    assert_eq!(at(&r, "derived"), (0, 6));
    assert_eq!(int(&r, "derived.__anon_base0.x"), 0x0102);
    assert_eq!(int(&r, "derived.z"), 0x42);

    // Declarations without definitions are skipped.
    assert!(p.type_def("Opaque").is_none());
}

#[test]
fn units_are_merged_and_collisions_suffixed() {
    let p = from_dwarf(&little_endian_object()).expect("import");
    let count = |name: &str| p.type_defs().filter(|t| t.name() == name).count();
    assert_eq!(count("Inner"), 1, "identical definitions merge");
    assert_eq!(count("uint8_t"), 1);
    assert_eq!(count("Point"), 1);
    assert_eq!(count("Point_2"), 1, "differing definitions are suffixed");
    assert_eq!(run(&p, "Point", &[1, 0, 0, 0]).nodes[0].size, 4);
    assert_eq!(run(&p, "Point_2", &[1]).nodes[0].size, 1);
}

#[test]
fn legacy_bit_offsets() {
    let p = from_dwarf(&little_endian_object()).expect("import");
    let r = run(&p, "Legacy", &[0xAA, (5 << 4) | 3]);
    assert_eq!(at(&r, "legacy"), (0, 2));
    assert_eq!(int(&r, "legacy.x"), 0xAA);
    assert_eq!(at(&r, "legacy.__anon_bits0"), (1, 1));
    assert_eq!(int(&r, "legacy.__anon_bits0.f"), 3);
    assert_eq!(int(&r, "legacy.__anon_bits0.g"), 5);
}

// ----- big-endian object -----------------------------------------------------

/// `PowerPC64` (big-endian) object:
///
/// ```c
/// struct Be  { uint16_t a; uint8_t c : 3; uint8_t d : 5; uint32_t b; };  // DWARF 3
/// struct Be4 { uint16_t a; uint16_t c : 3; uint16_t d : 9; };            // DWARF 4
/// ```
fn big_endian_object() -> Vec<u8> {
    let mut dwarf = Dwarf::new();
    let u1 = add_unit(&mut dwarf, 3);
    {
        let mut c = Cu {
            u: dwarf.units.get_mut(u1),
        };
        let u8t = c.base("uint8_t", gimli::DW_ATE_unsigned, 1);
        let u16t = c.base("uint16_t", gimli::DW_ATE_unsigned, 2);
        let u32t = c.base("uint32_t", gimli::DW_ATE_unsigned, 4);
        let be = c.record(gimli::DW_TAG_structure_type, Some("Be"), 8);
        c.member(be, Some("a"), u16t, 0);
        c.legacy_bits(be, "c", u8t, (2, 1), 0, 3);
        c.legacy_bits(be, "d", u8t, (2, 1), 3, 5);
        c.member(be, Some("b"), u32t, 4);
    }
    let u2 = add_unit(&mut dwarf, 4);
    {
        let mut c = Cu {
            u: dwarf.units.get_mut(u2),
        };
        let u16t = c.base("uint16_t", gimli::DW_ATE_unsigned, 2);
        let be4 = c.record(gimli::DW_TAG_structure_type, Some("Be4"), 4);
        c.member(be4, Some("a"), u16t, 0);
        c.bits(be4, "c", u16t, 16, 3);
        c.bits(be4, "d", u16t, 19, 9);
    }
    elf(&mut dwarf, true)
}

#[test]
fn big_endian_object_and_msb_first_bit_fields() {
    let p = from_dwarf(&big_endian_object()).expect("import");
    assert_eq!(p.pragma("endian"), Some("big"));

    let r = run(
        &p,
        "Be",
        &[0x12, 0x34, 0b101_01010, 0, 0xDE, 0xAD, 0xBE, 0xEF],
    );
    assert_eq!(at(&r, "be"), (0, 8));
    assert_eq!(int(&r, "be.a"), 0x1234);
    assert_eq!(int(&r, "be.__anon_bits0.c"), 5);
    assert_eq!(int(&r, "be.__anon_bits0.d"), 10);
    assert_eq!(int(&r, "be.b"), 0xDEAD_BEEF);

    // c = 5, d = 0x1AB: 101 110101011 0000 = 0xBAB0
    let r = run(&p, "Be4", &[0x12, 0x34, 0xBA, 0xB0]);
    assert_eq!(at(&r, "be4"), (0, 4));
    assert_eq!(at(&r, "be4.__anon_bits0"), (2, 2));
    assert_eq!(int(&r, "be4.__anon_bits0.c"), 5);
    assert_eq!(int(&r, "be4.__anon_bits0.d"), 0x1AB);
}

// ----- errors ----------------------------------------------------------------

#[test]
fn clean_errors_for_bad_input() {
    assert!(matches!(
        from_dwarf(b"not an object"),
        Err(Error::Invalid(_))
    ));
    let obj = Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little)
        .write()
        .unwrap();
    match from_dwarf(&obj) {
        Err(Error::Invalid(msg)) => assert!(msg.contains("no DWARF"), "{msg}"),
        other => panic!("expected an error, got {other:?}"),
    }
    assert!(matches!(
        from_pdb(b"Microsoft C/C++ MSF 7.00\r\n\x1aDS\0\0\0"),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(from_pdb(&[0u8; 4096]), Err(Error::Invalid(_))));
}

// ----- emitter on a PDB-shaped model ----------------------------------------

/// What the PDB front-end produces for (MSVC, x64):
///
/// ```c
/// enum Mode : uint32_t { Off, On };
/// struct Base { uint32_t id; };
/// struct Packet : Base {
///     uint8_t kind;                              // 4
///     union {                                    // 8 (flattened in the field list)
///         uint32_t value; float fvalue; uint8_t raw[4];
///         struct { uint16_t lo, hi; };
///     };
///     uint16_t flags : 3; Mode mode : 2; int16_t level : 7;   // 12, LF_BITFIELD
/// };                                             // 16 bytes
/// ```
fn pdb_like_model() -> TypeModel {
    let mut m = TypeModel::new(Endian::Little);
    let mode = m.add(ModelType::Enum(EnumType {
        name: Some("Mode".into()),
        size: 4,
        signed: false,
        variants: vec![("Off".into(), 0), ("On".into(), 1)],
    }));
    let base = m.add(ModelType::Record(Record {
        name: Some("Base".into()),
        union: false,
        size: 4,
        members: vec![Member::new(Some("id"), Ty::prim("u32"), 0)],
    }));
    m.add(ModelType::Record(Record {
        name: Some("Packet".into()),
        union: false,
        size: 16,
        members: vec![
            Member::new(Some("__anon_base0"), Ty::Ref(base), 0),
            Member::new(Some("kind"), Ty::prim("u8"), 4),
            Member::new(Some("value"), Ty::prim("u32"), 8),
            Member::new(Some("fvalue"), Ty::prim("float"), 8),
            Member::new(Some("raw"), Ty::array(Ty::prim("u8"), 4), 8),
            Member::new(Some("lo"), Ty::prim("u16"), 8),
            Member::new(Some("hi"), Ty::prim("u16"), 10),
            // `LF_MEMBER` offset 12 + `LF_BITFIELD` position
            Member::bits(Some("flags"), Ty::prim("u16"), 12 * 8, 3),
            Member::bits(Some("mode"), Ty::Ref(mode), 12 * 8 + 3, 2),
            Member::bits(Some("level"), Ty::prim("s16"), 12 * 8 + 5, 7),
        ],
    }));
    m
}

#[test]
fn emitter_reproduces_pdb_layout() {
    let p = emit(&pdb_like_model()).expect("emit");
    let mut data = vec![0u8; 16];
    data[0..4].copy_from_slice(&7u32.to_le_bytes());
    data[4] = 3;
    data[8..12].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    let bits: u16 = 6 | (1 << 3) | (0b111_1011 << 5); // flags 6, mode On, level -5
    data[12..14].copy_from_slice(&bits.to_le_bytes());

    let r = run(&p, "Packet", &data);
    assert_eq!(at(&r, "packet"), (0, 16));
    assert_eq!(int(&r, "packet.__anon_base0.id"), 7);
    assert_eq!(at(&r, "packet.kind"), (4, 1));
    assert_eq!(int(&r, "packet.kind"), 3);
    assert_eq!(at(&r, "packet.__anon_u0"), (8, 4));
    assert_eq!(int(&r, "packet.__anon_u0.value"), 0x1234_5678);
    assert!(matches!(
        node(&r, "packet.__anon_u0.fvalue").raw,
        Some(Value::Float(_))
    ));
    assert_eq!(at(&r, "packet.__anon_u0.raw"), (8, 4));
    assert_eq!(int(&r, "packet.__anon_u0.__anon_s0.lo"), 0x5678);
    assert_eq!(at(&r, "packet.__anon_u0.__anon_s0.hi"), (10, 2));
    assert_eq!(int(&r, "packet.__anon_u0.__anon_s0.hi"), 0x1234);
    assert_eq!(at(&r, "packet.__anon_bits0"), (12, 2));
    assert_eq!(int(&r, "packet.__anon_bits0.flags"), 6);
    let mode = node(&r, "packet.__anon_bits0.mode");
    assert_eq!(mode.enum_name.as_deref(), Some("On"));
    assert_eq!(int(&r, "packet.__anon_bits0.level"), -5);
}

#[test]
fn emitter_output_is_deterministic() {
    let a = emit(&pdb_like_model()).unwrap();
    let b = emit(&pdb_like_model()).unwrap();
    assert_eq!(a, b);
    let names: Vec<&str> = a.type_defs().map(TypeDef::name).collect();
    assert_eq!(
        names,
        [
            "Mode",
            "Base",
            "Packet__bits0",
            "Packet__u0_s0",
            "Packet__u0",
            "Packet"
        ]
    );
}
