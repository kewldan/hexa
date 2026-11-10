//! STRC-01 / STRC-02 language semantics.

mod common;

use common::{get, int, run, run_ok, text, tree};
use hexa_core::Cancel;
use hexa_template::{
    Color, EvalOptions, Limits, NodeKind, Value, children, evaluate, highlights, node_at, parse,
};

#[test]
fn primitives_and_endianness() {
    let data = [
        0x01, 0x02, 0x03, 0x04, // u32 le
        0x01, 0x02, // be u16
        0xFF, // s8
        0x00, 0x00, 0x80, 0x3F, // float 1.0
        0x01, // bool
        0x41, // char
        0x01, 0x02, 0x03, // u24
    ];
    let r = run_ok(
        "u32 a; be u16 b; s8 c; float f; bool t; char ch; u24 x;",
        &data,
    );
    assert_eq!(int(&r, "a"), 0x0403_0201);
    assert_eq!(int(&r, "b"), 0x0102);
    assert_eq!(int(&r, "c"), -1);
    assert_eq!(get(&r, "f").raw, Some(Value::Float(1.0)));
    assert_eq!(get(&r, "t").raw, Some(Value::Bool(true)));
    assert_eq!(get(&r, "ch").value.as_deref(), Some("'A'"));
    assert_eq!(int(&r, "x"), 0x03_0201);
    assert_eq!(get(&r, "a").value.as_deref(), Some("67305985 (0x4030201)"));
    assert_eq!(get(&r, "b").offset, 4);
    assert_eq!(get(&r, "b").size, 2);
}

#[test]
fn pragma_endian_big() {
    let r = run_ok("#pragma endian big\nu16 a; le u16 b;", &[1, 2, 1, 2]);
    assert_eq!(int(&r, "a"), 0x0102);
    assert_eq!(int(&r, "b"), 0x0201);
}

#[test]
fn structs_nested_and_parent_this() {
    let src = r"
        struct Inner { u8 v; u8 twice = v * 2; auto p = parent.kind; };
        struct Outer {
            u8 kind;
            Inner inner;
            auto addr = addressof(inner);
            auto sz = sizeof(inner);
            auto self_size = sizeof(this);
        };
        Outer o @ 0;
    ";
    let r = run_ok(src, &[7, 9]);
    assert_eq!(int(&r, "o.kind"), 7);
    assert_eq!(int(&r, "o.inner.v"), 9);
    assert_eq!(int(&r, "o.inner.p"), 7);
    assert_eq!(int(&r, "o.addr"), 1);
    assert_eq!(int(&r, "o.sz"), 1);
    assert_eq!(int(&r, "o.self_size"), 2);
    let o = get(&r, "o");
    assert_eq!((o.offset, o.size, o.kind), (0, 2, NodeKind::Struct));
    assert_eq!(get(&r, "o.addr").kind, NodeKind::Computed);
}

#[test]
fn unions_take_largest_member() {
    let r = run_ok(
        "union U { u8 a; u32 b; u16 c; }; U u; u8 after;",
        &[1, 2, 3, 4, 5],
    );
    assert_eq!(get(&r, "u").size, 4);
    assert_eq!(int(&r, "u.a"), 1);
    assert_eq!(int(&r, "u.c"), 0x0201);
    assert_eq!(int(&r, "after"), 5);
}

#[test]
fn enums_and_flags() {
    let src = r"
        enum Kind : u8 { A = 1, B, C = 0x10, R = 0x20 ... 0x2F };
        enum Perm : u8 { Read = 1, Write = 2, Exec = 4 } [[flags]];
        Kind k1; Kind k2; Kind k3; Kind k4; Perm p;
        auto is_b = k2 == Kind::B;
    ";
    let r = run_ok(src, &[1, 2, 0x25, 0x99, 5]);
    assert_eq!(get(&r, "k1").enum_name.as_deref(), Some("A"));
    assert_eq!(get(&r, "k2").enum_name.as_deref(), Some("B"));
    assert_eq!(get(&r, "k3").enum_name.as_deref(), Some("R"));
    assert_eq!(get(&r, "k4").enum_name, None);
    assert_eq!(get(&r, "k4").value.as_deref(), Some("??? (0x99)"));
    assert_eq!(get(&r, "p").enum_name.as_deref(), Some("Read | Exec"));
    assert_eq!(get(&r, "is_b").raw, Some(Value::Bool(true)));
    assert_eq!(get(&r, "k1").kind, NodeKind::Enum);
    assert_eq!(r.enum_variants("Kind").len(), 4);
}

#[test]
fn bitfields_little_and_big() {
    let src = r"
        enum Mode : u8 { Off, On };
        bitfield Flags { a : 3; b : 4; Mode m : 1; };
        Flags f;
        be Flags g;
    ";
    // 0b1_0101_011 = 0xAB
    let r = run_ok(src, &[0xAB, 0xAB]);
    assert_eq!(int(&r, "f.a"), 0b011);
    assert_eq!(int(&r, "f.b"), 0b0101);
    assert_eq!(get(&r, "f.m").enum_name.as_deref(), Some("On"));
    assert_eq!(get(&r, "f").size, 1);
    assert_eq!(get(&r, "f.a").bit_size, Some(3));
    // Big endian: fields from the most significant bit.
    assert_eq!(int(&r, "g.a"), 0b101);
    assert_eq!(int(&r, "g.b"), 0b0101);
}

#[test]
fn fixed_lazy_arrays_and_paging() {
    let data: Vec<u8> = (0..=255).collect();
    let r = run_ok("u16 words[100]; u8 rest[while($ < 0x100)];", &data);
    let w = get(&r, "words");
    assert!(w.is_array && w.lazy);
    assert_eq!((w.len, w.size), (100, 200));
    assert_eq!(w.children, [] as [u32; 0]);
    let page = children(&r, w.id, 10..13);
    assert_eq!(page.len(), 3);
    assert_eq!(page[0].elem_index, Some(10));
    assert_eq!(page[0].offset, 20);
    assert_eq!(page[0].raw, Some(Value::Int(0x1514)));
    assert_eq!(get(&r, "rest").len, 56);
    assert_eq!(children(&r, w.id, 99..200).len(), 1);
}

#[test]
fn array_element_access_in_expressions() {
    let src = "u8 n; u8 vals[n]; auto second = vals[1]; auto total = vals[0] + vals[2];";
    let r = run_ok(src, &[3, 10, 20, 30]);
    assert_eq!(int(&r, "second"), 20);
    assert_eq!(int(&r, "total"), 40);
}

#[test]
fn struct_arrays_while_until_and_eof() {
    let src = r"
        struct Rec { u8 tag; u8 len; u8 data[len]; };
        Rec recs[while(std::mem::read_unsigned($, 1) != 0)];
        u8 zero;
        Rec until_recs[until(_.tag == 2)];
        u8 tail[];
    ";
    let data = [1, 1, 0xAA, 1, 0, 0, 1, 0, 2, 1, 0xBB, 9, 9];
    let r = run_ok(src, &data);
    assert_eq!(get(&r, "recs").len, 2);
    assert_eq!(int(&r, "recs[1].tag"), 1);
    assert_eq!(get(&r, "recs[0]").size, 3);
    assert_eq!(get(&r, "until_recs").len, 2);
    assert_eq!(int(&r, "until_recs[1].tag"), 2);
    assert_eq!(get(&r, "tail").len, 2);
}

#[test]
fn strings() {
    let src = r#"
        char magic[4];
        string name;
        char16 wide[2];
        wstring wz;
        auto len = std::string::length(name);
        auto joined = magic + "-" + name;
    "#;
    let mut data = b"PNG\0abc\0".to_vec();
    data.extend_from_slice(&[b'h', 0, b'i', 0, b'x', 0, 0, 0]);
    let r = run_ok(src, &data);
    assert_eq!(text(&r, "magic"), "PNG");
    assert_eq!(text(&r, "name"), "abc");
    assert_eq!(get(&r, "name").size, 4);
    assert_eq!(text(&r, "wide"), "hi");
    assert_eq!(text(&r, "wz"), "x");
    assert_eq!(int(&r, "len"), 3);
    assert_eq!(text(&r, "joined"), "PNG-abc");
    assert_eq!(get(&r, "magic").value.as_deref(), Some("\"PNG\""));
}

#[test]
fn conditionals_match_and_loops() {
    let src = r"
        struct S {
            u8 t;
            if (t == 1) { u16 a; } else if (t == 2) { u32 b; } else { u8 c; }
            match (t, 0) {
                (1 | 3, _): u8 m1;
                (2 ... 5, 0): u8 m2;
                (_, _): u8 m3;
            }
            for (u8 i = 0, i < 2, i += 1) { u8 loop_v; }
            u8 count = 0;
            while (count < 1) { u8 w; count += 1; }
        };
        S s;
    ";
    let r = run_ok(src, &[2, 1, 2, 3, 4, 5, 6, 7, 8]);
    assert!(r.find("s.b").is_some());
    assert!(r.find("s.a").is_none());
    assert!(r.find("s.m2").is_some());
    assert!(r.find("s.m1").is_none());
    let s = get(&r, "s");
    let names: Vec<_> = s
        .children
        .iter()
        .map(|c| r.nodes[*c as usize].name.clone())
        .collect();
    assert_eq!(names, ["t", "b", "m2", "loop_v", "loop_v", "w"]);
    assert_eq!(s.size, 9);
}

#[test]
fn pointers_and_placement() {
    let src = r"
        struct Target { u8 x; u8 y; };
        Target *p : u8;
        u8 placed @ 0x05;
        u8 next;
        Target *rel : u8 [[pointer_base(2)]];
    ";
    let r = run_ok(src, &[3, 0, 0, 0xAA, 0xBB, 0x77, 0xCC]);
    let p = get(&r, "p");
    assert_eq!(p.kind, NodeKind::Pointer);
    assert_eq!(p.size, 1);
    assert_eq!(int(&r, "p.*p.x"), 0xAA);
    assert_eq!(int(&r, "placed"), 0x77);
    assert_eq!(int(&r, "next"), 0);
    assert_eq!(get(&r, "next").offset, 1);
    // rel = 0 + base 2 → 0x00 0xAA
    assert_eq!(int(&r, "rel.*rel.y"), 0xAA);
}

#[test]
fn attributes() {
    let src = r#"
        u32 a [[name("Alpha"), comment("first"), color("FF8800"), format("hex")]];
        u8 hidden_one [[hidden]];
        u16 crc [[checksum("crc32", 0, 4)]];
        fn fmt_b(u8 v) { return "b=" + std::string::to_string(v); };
        u8 b [[format("fmt_b")]];
    "#;
    let r = run_ok(src, &[1, 0, 0, 0, 5, 6, 7, 9]);
    let a = get(&r, "a");
    assert_eq!(a.label(), "Alpha");
    assert_eq!(a.comment.as_deref(), Some("first"));
    assert_eq!(a.color, Some(Color::Rgb("ff8800".into())));
    assert_eq!(a.value.as_deref(), Some("0x00000001"));
    assert!(get(&r, "hidden_one").hidden);
    let cs = hexa_template::checksum_fields(&r);
    assert_eq!(cs.len(), 1);
    assert_eq!((cs[0].start, cs[0].end, cs[0].offset), (0, 4, 5));
    assert_eq!(cs[0].stored, Some(0x0706));
    assert_eq!(get(&r, "b").value.as_deref(), Some("b=9"));
}

#[test]
fn functions_recursion_and_locals() {
    let src = r"
        fn fact(u32 n) { if (n <= 1) return 1; return n * fact(n - 1); };
        fn sum_to(u32 n) { u32 acc = 0; for (u32 i = 1; i <= n; i += 1) acc += i; return acc; };
        auto f = fact(5);
        auto s = sum_to(10);
        auto t = true ? 0x10 : 0b11;
        auto bits = (0xF0 >> 4) | (1 << 8) ^ 3;
        auto fl = 1.5 * 2;
    ";
    let r = run_ok(src, &[]);
    assert_eq!(int(&r, "f"), 120);
    assert_eq!(int(&r, "s"), 55);
    assert_eq!(int(&r, "t"), 16);
    assert_eq!(int(&r, "bits"), 0x10F);
    assert_eq!(get(&r, "fl").raw, Some(Value::Float(3.0)));
}

#[test]
fn templates_and_inheritance() {
    let src = r"
        struct Base { u8 kind; };
        struct Derived : Base { u8 extra; };
        struct Pair<T, auto N> { T items[N]; };
        Derived d;
        Pair<u16, 2> pr;
    ";
    let r = run_ok(src, &[1, 2, 3, 0, 4, 0]);
    assert_eq!(int(&r, "d.kind"), 1);
    assert_eq!(int(&r, "d.extra"), 2);
    let items = get(&r, "pr.items");
    assert_eq!((items.len, items.size), (2, 4));
}

#[test]
fn using_alias_and_padding_and_dollar() {
    let src = r"
        using Word = be u16;
        Word w;
        padding[2];
        $ += 1;
        u8 after_skip;
        $ = 0;
        u8 first_again;
    ";
    let r = run_ok(src, &[1, 2, 0, 0, 0, 9]);
    assert_eq!(int(&r, "w"), 0x0102);
    assert_eq!(get(&r, "w").type_name, "Word");
    assert_eq!(int(&r, "after_skip"), 9);
    assert_eq!(int(&r, "first_again"), 1);
    assert_eq!(get(&r, "padding").kind, NodeKind::Padding);
}

#[test]
fn assert_failure_keeps_partial_tree() {
    let src = r#"
        struct H { u8 magic; assert(magic == 0x7F, "bad magic"); u8 more; };
        H h;
    "#;
    let r = run(src, &[1, 2]);
    assert!(!r.is_ok());
    assert_eq!(r.errors[0].message, "bad magic");
    assert_eq!(r.errors[0].line, 2);
    let h = get(&r, "h");
    assert_eq!(h.error.as_deref(), Some("bad magic"));
    assert!(r.find("h.magic").is_some());
    assert!(r.find("h.more").is_none());
}

#[test]
fn assert_warn_continues() {
    let r = run("std::assert_warn(false, \"just a warning\"); u8 a;", &[1]);
    assert!(r.is_ok());
    assert_eq!(r.errors.len(), 1);
    assert!(r.errors[0].warning);
    assert_eq!(int(&r, "a"), 1);
}

#[test]
fn read_past_end_is_an_error() {
    let r = run("u32 a;", &[1, 2]);
    assert!(!r.is_ok());
    assert!(
        r.errors[0].message.contains("outside of the data"),
        "{:?}",
        r.errors
    );
    let r = run("u8 a[10];", &[1, 2]);
    assert!(r.errors[0].message.contains("extends past the end"));
}

#[test]
fn std_library() {
    let src = r#"
        auto size = std::mem::size();
        auto peek = std::mem::read_unsigned(1, 2);
        auto peek_be = std::mem::read_unsigned(1, 2, "big");
        auto neg = std::mem::read_signed(0, 1);
        auto found = std::mem::find_sequence(0, 0xBB, 0xCC);
        auto f = std::format("{} {:02X} {:#x}", 7, 10, 255);
        std::print("size is {}", size);
        auto sub = std::string::substr("abcdef", 2, 3);
    "#;
    let r = run_ok(src, &[0xFF, 0xAA, 0xBB, 0xCC]);
    assert_eq!(int(&r, "size"), 4);
    assert_eq!(int(&r, "peek"), 0xBBAA);
    assert_eq!(int(&r, "peek_be"), 0xAABB);
    assert_eq!(int(&r, "neg"), -1);
    assert_eq!(int(&r, "found"), 2);
    assert_eq!(text(&r, "f"), "7 0A 0xff");
    assert_eq!(text(&r, "sub"), "cde");
    assert_eq!(r.log, ["size is 4"]);
}

#[test]
fn includes_through_resolver() {
    use hexa_template::{ParseOptions, parse_with};
    let resolver = |p: &str| (p == "common.hexpat").then(|| "struct Common { u8 x; };".to_string());
    let (prog, d) = parse_with(
        "#include \"common.hexpat\"\nimport std.mem;\nCommon c;",
        &ParseOptions {
            resolver: Some(&resolver),
            ..ParseOptions::default()
        },
    );
    assert!(d.is_empty(), "{d:?}");
    let r = evaluate(&prog, &tree(&[5]), &EvalOptions::default(), &Cancel::new()).unwrap();
    assert_eq!(int(&r, "c.x"), 5);
}

#[test]
fn limits_and_cancellation() {
    let prog = parse("u8 a; while (true) { }").unwrap();
    let opts = EvalOptions {
        limits: Limits {
            max_steps: 10_000,
            ..Limits::default()
        },
        ..EvalOptions::default()
    };
    let r = evaluate(&prog, &tree(&[1]), &opts, &Cancel::new()).unwrap();
    assert!(r.truncated);
    assert!(r.errors[0].message.contains("step budget"));
    assert_eq!(int(&r, "a"), 1);

    let cancel = Cancel::new();
    cancel.cancel();
    let prog = parse("while (true) { }").unwrap();
    assert!(evaluate(&prog, &tree(&[]), &EvalOptions::default(), &cancel).is_err());

    let prog = parse("struct R { R r; }; R r;").unwrap();
    let r = evaluate(&prog, &tree(&[]), &EvalOptions::default(), &Cancel::new()).unwrap();
    assert!(r.errors[0].message.contains("depth"));

    let prog = parse("u8 a[4]; u8 b[4];").unwrap();
    let opts = EvalOptions {
        limits: Limits {
            max_nodes: 1,
            ..Limits::default()
        },
        ..EvalOptions::default()
    };
    let r = evaluate(&prog, &tree(&[0; 8]), &opts, &Cancel::new()).unwrap();
    assert!(r.truncated);
    assert_eq!(r.nodes.len(), 1);
}

#[test]
fn eager_limit_stores_struct_offsets() {
    let prog = parse("struct E { u8 a; u8 b; }; E items[6];").unwrap();
    let opts = EvalOptions {
        limits: Limits {
            eager_elements: 2,
            ..Limits::default()
        },
        ..EvalOptions::default()
    };
    let data: Vec<u8> = (0..12).collect();
    let r = evaluate(&prog, &tree(&data), &opts, &Cancel::new()).unwrap();
    assert!(r.is_ok(), "{:?}", r.errors);
    let items = get(&r, "items");
    assert_eq!(items.len, 6);
    assert_eq!(items.children.len(), 2);
    assert_eq!(items.size, 12);
    let page = children(&r, items.id, 0..6);
    assert_eq!(page.len(), 6);
    assert_eq!(page[4].offset, 8);
    assert!(page[4].lazy);
    let sub = hexa_template::expand_element(&r, items.id, 4).unwrap();
    assert_eq!(int(&sub, "items[4].b"), 9);
    let hit = node_at(&r, 9).unwrap();
    assert_eq!(hit.element_index, Some(4));
}

#[test]
fn node_at_and_highlights() {
    let r = run_ok("struct S { u8 a; u16 b; }; S s; u32 arr[2];", &[0; 11]);
    let hit = node_at(&r, 2).unwrap();
    let ids: Vec<_> = hit
        .path
        .iter()
        .map(|i| r.nodes[*i as usize].name.clone())
        .collect();
    assert_eq!(ids, ["s", "b"]);
    let hit = node_at(&r, 8).unwrap();
    assert_eq!(hit.element_index, Some(1));
    assert!(node_at(&r, 100).is_none());
    let hl = highlights(&r);
    assert_eq!(hl.len(), 3);
    assert_eq!((hl[0].0, hl[0].1), (0, 1));
    assert_ne!(hl[0].2, hl[1].2);
}

#[test]
fn selection_range_and_root_type() {
    let prog = parse("struct P { u8 a; u8 b; }; u8 first; u8 rest[];").unwrap();
    let opts = EvalOptions {
        base_offset: 2,
        range: Some(2..5),
        ..EvalOptions::default()
    };
    let r = evaluate(&prog, &tree(&[9, 9, 1, 2, 3, 4]), &opts, &Cancel::new()).unwrap();
    assert!(r.is_ok());
    assert_eq!(int(&r, "first"), 1);
    assert_eq!(get(&r, "rest").len, 2);

    let opts = EvalOptions {
        base_offset: 4,
        root_type: Some("P".into()),
        ..EvalOptions::default()
    };
    let r = evaluate(&prog, &tree(&[0, 0, 0, 0, 7, 8]), &opts, &Cancel::new()).unwrap();
    assert_eq!(int(&r, "p.b"), 8);
    assert_eq!(r.roots.len(), 1);
}

#[test]
fn evaluation_is_deterministic() {
    let src = "struct R { u8 n; u8 d[n]; }; R rs[3];";
    let data = [1, 5, 2, 6, 7, 0];
    let a = serde_json::to_string(&run_ok(src, &data)).unwrap();
    let b = serde_json::to_string(&run_ok(src, &data)).unwrap();
    assert_eq!(a, b);
}

#[test]
fn break_inside_struct_stops_array() {
    let src = r"
        struct Item { u8 v; if (v == 0) break; };
        Item items[8];
        u8 after;
    ";
    let r = run_ok(src, &[3, 4, 0, 9]);
    assert_eq!(get(&r, "items").len, 3);
    assert_eq!(int(&r, "after"), 9);
}

#[test]
fn errors_for_unknown_names() {
    let r = run("Missing m;", &[0]);
    assert!(r.errors[0].message.contains("unknown type `Missing`"));
    let r = run("auto x = nope + 1;", &[0]);
    assert!(r.errors[0].message.contains("unknown identifier `nope`"));
    let r = run("auto x = 1 / 0;", &[]);
    assert!(r.errors[0].message.contains("division by zero"));
}

#[test]
fn serializes_camel_case() {
    let r = run_ok("u8 a [[name(\"A\")]];", &[1]);
    let json = serde_json::to_value(&r).unwrap();
    let n = &json["nodes"][0];
    assert_eq!(n["displayName"], "A");
    assert_eq!(n["typeName"], "u8");
    assert_eq!(n["raw"]["type"], "int");
    assert!(n["color"]["palette"].is_number());
}
