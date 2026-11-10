//! STRC-03 Kaitai Struct import.

mod common;

use common::{all_paths, get, int, text, tree};
use hexa_core::Cancel;
use hexa_template::import::ksy::{from_ksy, from_ksy_with};
use hexa_template::{EvalOptions, TemplateResult, Value, evaluate};

fn eval(ksy: &str, data: &[u8]) -> TemplateResult {
    let p = from_ksy(ksy).unwrap_or_else(|e| panic!("{e}"));
    let r = evaluate(&p, &tree(data), &EvalOptions::default(), &Cancel::new()).unwrap();
    assert!(r.is_ok(), "{:#?}\n{:?}", r.errors, all_paths(&r));
    r
}

const SPEC: &str = r#"
meta:
  id: archive
  endian: be
  encoding: ASCII
doc: A toy archive format.
seq:
  - id: magic
    contents: ["AR", 0x01]
  - id: version
    type: u1
    valid:
      min: 1
      max: 3
  - id: flags
    type: b1
  - id: level
    type: b3
  - id: reserved
    type: b4
  - id: num_entries
    type: u2
  - id: title
    type: strz
  - id: entries
    type: entry
    repeat: expr
    repeat-expr: num_entries
  - id: trailer_len
    type: u2le
  - id: trailer
    size: trailer_len
    type: trailer
  - id: tail
    type: u1
    repeat: until
    repeat-until: _ == 0xff
types:
  entry:
    seq:
      - id: kind
        type: u1
        enum: entry_kind
      - id: len
        type: u1
      - id: body
        size: len
        type:
          switch-on: kind
          cases:
            'entry_kind::text': text_body
            'entry_kind::number': number_body(len)
      - id: extra
        type: u1
        if: kind == entry_kind::number and len > 1
    instances:
      is_text:
        value: kind == entry_kind::text
    types:
      text_body:
        seq:
          - id: value
            type: str
            size-eos: true
      number_body:
        params:
          - id: width
            type: u1
        seq:
          - id: value
            type: u2
          - id: rest
            size: width - 2
  trailer:
    seq:
      - id: words
        type: u2le
        repeat: eos
    instances:
      first_byte:
        pos: 0
        type: u1
      word_count:
        value: words.size
enums:
  entry_kind:
    1: text
    2:
      id: number
      doc: big-endian number
"#;

fn sample() -> Vec<u8> {
    let mut d = b"AR\x01".to_vec();
    d.push(2); // version
    d.push(0b1_101_0110); // flags=1 level=5 reserved=6
    d.extend_from_slice(&2u16.to_be_bytes());
    d.extend_from_slice(b"hello\0");
    d.extend_from_slice(&[1, 3, b'a', b'b', b'c']); // text entry
    d.extend_from_slice(&[2, 3, 0x12, 0x34, 0x56, 0x77]); // number entry + extra
    d.extend_from_slice(&4u16.to_le_bytes());
    d.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]);
    d.extend_from_slice(&[5, 6, 0xff]);
    d
}

#[test]
fn full_spec_evaluates() {
    let r = eval(SPEC, &sample());
    assert_eq!(int(&r, "archive.version"), 2);
    assert_eq!(
        get(&r, "archive.__anon_bits_1.flags").raw,
        Some(Value::Bool(true))
    );
    assert_eq!(int(&r, "archive.__anon_bits_1.level"), 5);
    assert_eq!(int(&r, "archive.__anon_bits_1.reserved"), 6);
    assert_eq!(int(&r, "archive.num_entries"), 2);
    assert_eq!(text(&r, "archive.title"), "hello");
    assert_eq!(
        get(&r, "archive.entries[0].kind").enum_name.as_deref(),
        Some("text")
    );
    assert_eq!(text(&r, "archive.entries[0].body.value"), "abc");
    assert_eq!(
        get(&r, "archive.entries[0].is_text").raw,
        Some(Value::Bool(true))
    );
    assert!(r.find("archive.entries[0].extra").is_none());
    assert_eq!(int(&r, "archive.entries[1].body.value"), 0x1234);
    assert_eq!(get(&r, "archive.entries[1].body").size, 3);
    assert_eq!(int(&r, "archive.entries[1].extra"), 0x77);
    assert_eq!(get(&r, "archive.trailer").size, 4);
    assert_eq!(get(&r, "archive.trailer.words").len, 2);
    assert_eq!(int(&r, "archive.trailer.word_count"), 2);
    assert_eq!(int(&r, "archive.trailer.first_byte"), b'A'.into());
    assert_eq!(get(&r, "archive.tail").len, 3);
    assert_eq!(get(&r, "archive.magic").size, 3);
    assert_eq!(
        get(&r, "archive").comment.as_deref(),
        Some("A toy archive format.")
    );
}

#[test]
fn contents_and_valid_failures_are_reported() {
    let mut bad = sample();
    bad[0] = b'X';
    let p = from_ksy(SPEC).unwrap();
    let r = evaluate(&p, &tree(&bad), &EvalOptions::default(), &Cancel::new()).unwrap();
    assert!(
        r.errors[0].message.contains("expected contents"),
        "{:?}",
        r.errors
    );

    let mut bad = sample();
    bad[3] = 9; // version out of range
    let r = evaluate(&p, &tree(&bad), &EvalOptions::default(), &Cancel::new()).unwrap();
    assert!(
        r.errors[0].message.contains("failed validation"),
        "{:?}",
        r.errors
    );
}

#[test]
fn strings_encodings_and_terminators() {
    let spec = r"
meta:
  id: s
  endian: le
seq:
  - id: fixed
    type: str
    size: 4
    encoding: UTF-8
  - id: wide
    type: str
    size: 4
    encoding: UTF-16LE
  - id: names
    type: strz
    repeat: expr
    repeat-expr: 2
  - id: until_comma
    type: str
    terminator: 0x2c
  - id: tags
    type: str
    size: 2
    repeat: expr
    repeat-expr: 2
  - id: raw
    size-eos: true
";
    let mut d = b"ab\0\0".to_vec();
    d.extend_from_slice(&[b'h', 0, b'i', 0]);
    d.extend_from_slice(b"x\0yz\0q,");
    d.extend_from_slice(b"t1t2");
    d.extend_from_slice(&[9, 9, 9]);
    let r = eval(spec, &d);
    assert_eq!(text(&r, "s.fixed"), "ab");
    assert_eq!(text(&r, "s.wide"), "hi");
    assert_eq!(text(&r, "s.names[1]"), "yz");
    assert_eq!(text(&r, "s.until_comma"), "q,");
    assert_eq!(text(&r, "s.tags[1].value"), "t2");
    assert_eq!(get(&r, "s.raw").len, 3);
}

#[test]
fn imports_through_resolver() {
    let main =
        "meta:\n  id: main\n  imports:\n    - common/point\nseq:\n  - id: p\n    type: point\n";
    let point = "meta:\n  id: point\nseq:\n  - id: x\n    type: u1\n  - id: y\n    type: u1\n";
    let resolver = |path: &str| (path == "common/point.ksy").then(|| point.to_string());
    let p = from_ksy_with(main, Some(&resolver)).unwrap();
    let r = evaluate(&p, &tree(&[3, 4]), &EvalOptions::default(), &Cancel::new()).unwrap();
    assert!(r.is_ok(), "{:?}", r.errors);
    assert_eq!(int(&r, "main.p.y"), 4);
    assert!(from_ksy(main).is_err());
}

#[test]
fn expressions_translate() {
    let spec = r#"
meta:
  id: e
seq:
  - id: a
    type: u1
  - id: items
    type: item
    repeat: expr
    repeat-expr: 2
instances:
  pos_after:
    value: _io.pos
  size:
    value: _io.size
  check:
    value: 'a == 3 and not (a > 5) ? "yes" : ''no'''
  idx_sum:
    value: items[0].idx + items[1].idx
types:
  item:
    seq:
      - id: v
        type: u1
    instances:
      idx:
        value: _index
      root_a:
        value: _root.a
      parent_a:
        value: _parent.a
"#;
    let r = eval(spec, &[3, 7, 8]);
    assert_eq!(int(&r, "e.pos_after"), 3);
    assert_eq!(int(&r, "e.size"), 3);
    assert_eq!(text(&r, "e.check"), "yes");
    assert_eq!(int(&r, "e.idx_sum"), 1);
    assert_eq!(int(&r, "e.items[1].root_a"), 3);
    assert_eq!(int(&r, "e.items[1].parent_a"), 3);
}
