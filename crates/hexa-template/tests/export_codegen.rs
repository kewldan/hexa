//! STRC-21 (export to Kaitai / C) and STRC-22 (parser generation).

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{int, text, tree};
use hexa_core::Cancel;
use hexa_template::codegen::{Language, generate};
use hexa_template::export::{to_c_header, to_ksy};
use hexa_template::{Dialect, EvalOptions, ParseOptions, evaluate, parse, parse_with};
use saphyr::LoadableYamlNode;

const SAMPLE: &str = r#"
#pragma endian little
enum Kind : u8 { A = 1, B, C = 0x10, R = 0x20 ... 0x2F };
struct Item { u16 id; char tag[4]; be u32 value; };
struct Header {
    char magic[4] [[comment("File magic")]];
    u8 version;
    Kind kind;
    u16 count;
    Item items[count];
    u8 raw[count * 2 + 1];
    padding[2];
    string name;
    be s32 delta;
    double ratio;
    bool flag;
    Kind kinds[2];
    u64 big;
};
"#;

fn sample_bytes() -> Vec<u8> {
    let mut b = b"HEXA".to_vec();
    b.push(1); // version
    b.push(2); // kind = B
    b.extend_from_slice(&2u16.to_le_bytes());
    for (id, tag, value) in [(7u16, b"ab\0\0", 0x0102_0304u32), (8, b"wxyz", 5)] {
        b.extend_from_slice(&id.to_le_bytes());
        b.extend_from_slice(tag);
        b.extend_from_slice(&value.to_be_bytes());
    }
    b.extend_from_slice(&[9, 8, 7, 6, 5]); // raw
    b.extend_from_slice(&[0, 0]); // padding
    b.extend_from_slice(b"hi\0");
    b.extend_from_slice(&(-5i32).to_be_bytes());
    b.extend_from_slice(&1.5f64.to_le_bytes());
    b.push(1);
    b.extend_from_slice(&[1, 0x10]);
    b.extend_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
    b
}

fn work_dir(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("hexa-template-codegen")
        .join(name);
    std::fs::create_dir_all(&d).expect("create work dir");
    d
}

fn tool(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
        || Command::new(name)
            .arg("version")
            .output()
            .is_ok_and(|o| o.status.success())
}

fn run(cmd: &mut Command) -> String {
    let out = cmd.output().expect("spawn");
    assert!(
        out.status.success(),
        "{cmd:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The reference decoding by the template evaluator itself.
#[test]
fn template_decodes_sample() {
    let p = parse(SAMPLE).unwrap();
    let opts = EvalOptions {
        root_type: Some("Header".into()),
        ..EvalOptions::default()
    };
    let r = evaluate(&p, &tree(&sample_bytes()), &opts, &Cancel::new()).unwrap();
    assert!(r.is_ok(), "{:?}", r.errors);
    assert_eq!(text(&r, "header.magic"), "HEXA");
    assert_eq!(int(&r, "header.items[0].value"), 0x0102_0304);
    assert_eq!(int(&r, "header.delta"), -5);
    assert_eq!(int(&r, "header.big"), 0x1122_3344_5566_7788);
}

#[test]
fn ksy_export_is_valid_yaml_and_round_trips() {
    let p = parse(SAMPLE).unwrap();
    let ksy = to_ksy(&p, "Header").unwrap();
    let docs = saphyr::Yaml::load_from_str(&ksy).expect("valid YAML");
    let doc = &docs[0];
    assert_eq!(doc["meta"]["id"].as_str(), Some("header"));
    assert_eq!(doc["meta"]["endian"].as_str(), Some("le"));
    let seq = doc["seq"].as_vec().expect("seq");
    assert_eq!(seq[0]["id"].as_str(), Some("magic"));
    assert_eq!(seq[0]["type"].as_str(), Some("str"));
    assert_eq!(seq[0]["doc"].as_str(), Some("File magic"));
    assert_eq!(seq[4]["repeat-expr"].as_str(), Some("count"));
    assert_eq!(seq[8]["type"].as_str(), Some("s4be"));
    assert!(doc["types"]["item"]["seq"].as_vec().is_some());
    assert!(ksy.contains("16: c"));

    // Importing the export again decodes the same values.
    let back = hexa_template::import::ksy::from_ksy(&ksy).expect("re-import");
    let r = evaluate(
        &back,
        &tree(&sample_bytes()),
        &EvalOptions::default(),
        &Cancel::new(),
    )
    .unwrap();
    assert!(r.is_ok(), "{:?}", r.errors);
    assert_eq!(text(&r, "header.magic"), "HEXA");
    assert_eq!(int(&r, "header.items[1].id"), 8);
    assert_eq!(int(&r, "header.delta"), -5);
    assert_eq!(text(&r, "header.name"), "hi");
    assert_eq!(
        r.find("header.kind")
            .and_then(|n| n.enum_name.clone())
            .as_deref(),
        Some("b")
    );
}

#[test]
fn ksy_export_conditionals_switch_and_instances() {
    let src = r"
        struct Body1 { u8 x; };
        struct Body2 { u16 y; };
        struct Root {
            u8 kind;
            if (kind == 1 && !(kind > 5)) u8 extra;
            match (kind) {
                (1): Body1 body;
                (2): Body2 body;
            }
            auto doubled = kind * 2;
            u8 at_end @ $ + 1;
            u8 rest[];
        };
    ";
    let ksy = to_ksy(&parse(src).unwrap(), "Root").unwrap();
    let doc = &saphyr::Yaml::load_from_str(&ksy).unwrap()[0];
    let seq = doc["seq"].as_vec().unwrap();
    assert_eq!(seq[1]["if"].as_str(), Some("kind == 1 and not (kind > 5)"));
    assert!(seq[2]["type"]["switch-on"].as_str().is_some(), "{ksy}");
    assert!(
        doc["instances"]["doubled"]["value"].as_str().is_some(),
        "{ksy}"
    );
    assert!(
        doc["instances"]["at_end"]["pos"].as_str().is_some(),
        "{ksy}"
    );
    assert_eq!(seq[3]["repeat"].as_str(), Some("eos"));
}

#[test]
fn ksy_export_rejects_unsupported() {
    let p = parse("struct R { u8 n; while (n > 0) { u8 x; } };").unwrap();
    let e = to_ksy(&p, "R").unwrap_err().to_string();
    assert!(e.contains("while"), "{e}");
    assert!(to_ksy(&p, "Missing").is_err());
}

#[test]
fn c_header_round_trips_through_c_parser() {
    let src = r#"
        enum Mode : u16 { Off, On = 5 };
        bitfield Flags { a : 3; b : 5; };
        struct Point { s32 x; s32 y; };
        struct Shape {
            u8 tag [[comment("shape tag")]];
            Mode mode;
            Point corners[2];
            Flags flags;
            char name[6];
            double scale;
        };
    "#;
    let p = parse(src).unwrap();
    let h = to_c_header(&p).unwrap();
    assert!(h.contains("#pragma pack(push, 1)"), "{h}");
    assert!(h.contains("#include <stdint.h>"));
    assert!(h.contains("shape tag"));
    let (cp, diags) = parse_with(
        &h,
        &ParseOptions {
            dialect: Dialect::C,
            ..ParseOptions::default()
        },
    );
    assert!(
        diags
            .iter()
            .all(|d| d.severity != hexa_template::Severity::Error),
        "{diags:#?}\n{h}"
    );
    let mut data = vec![0xAA];
    data.extend_from_slice(&5u16.to_le_bytes());
    for v in [1i32, -2, 3, -4] {
        data.extend_from_slice(&v.to_le_bytes());
    }
    data.push(0b1010_1011);
    data.extend_from_slice(b"shape\0");
    data.extend_from_slice(&2.0f64.to_le_bytes());
    let opts = EvalOptions {
        root_type: Some("Shape".into()),
        ..EvalOptions::default()
    };
    let orig = evaluate(&p, &tree(&data), &opts, &Cancel::new()).unwrap();
    let back = evaluate(&cp, &tree(&data), &opts, &Cancel::new()).unwrap();
    assert!(
        orig.is_ok() && back.is_ok(),
        "{:?} {:?}\n{h}",
        orig.errors,
        back.errors
    );
    assert_eq!(
        orig.find("shape").unwrap().size,
        back.find("shape").unwrap().size,
        "{h}"
    );
    assert_eq!(int(&back, "shape.corners[1].y"), -4);
    assert_eq!(int(&back, "shape.mode"), 5);
    assert_eq!(text(&back, "shape.name"), "shape");
    if tool("cc") {
        let dir = work_dir("c");
        let path = dir.join("shape.h");
        std::fs::write(&path, &h).unwrap();
        let c = dir.join("main.c");
        std::fs::write(
            &c,
            "#include \"shape.h\"\n_Static_assert(sizeof(struct Shape) == 34, \"size\");\nint main(void) { return 0; }\n",
        )
        .unwrap();
        run(Command::new("cc").arg("-fsyntax-only").arg(&c));
    }
}

#[test]
fn c_header_rejects_dynamic_middle_fields() {
    let e = to_c_header(&parse(SAMPLE).unwrap())
        .unwrap_err()
        .to_string();
    assert!(e.contains("items") && e.contains("line"), "{e}");
}

#[test]
fn codegen_key_lines() {
    let p = parse(SAMPLE).unwrap();
    let py = generate(&p, "Header", Language::Python).unwrap();
    assert!(py.contains("class Header:"));
    assert!(py.contains("def parse(data: bytes) -> Header:"));
    assert!(py.contains("struct.unpack_from(\">i\""));
    let go = generate(&p, "Header", Language::Go).unwrap();
    assert!(go.contains("package parser"));
    assert!(go.contains("func ParseHeader(data []byte) (*Header, error)"));
    assert!(go.contains("binary.BigEndian"));
    let ts = generate(&p, "Header", Language::TypeScript).unwrap();
    assert!(ts.contains("export function parseHeader(view: DataView, offset = 0)"));
    assert!(ts.contains("getBigUint64"));
    let rs = generate(&p, "Header", Language::Rust).unwrap();
    assert!(rs.contains("pub struct Header {"));
    assert!(rs.contains("pub fn parse(data: &[u8]) -> Result<(Self, usize), ParseError>"));
    // Deterministic output.
    assert_eq!(rs, generate(&p, "Header", Language::Rust).unwrap());
}

#[test]
fn codegen_rejects_unsupported_constructs() {
    for (src, what) in [
        ("struct R { u8 a; if (a) u8 b; };", "if"),
        ("struct R { u8 a; u8 b @ 4; };", "placed"),
        ("union R { u8 a; u16 b; };", "union"),
        ("struct R { u8 v[while($ < 4)]; };", "while"),
        (
            "bitfield B { a : 4; b : 4; }; struct R { B b; };",
            "bitfield",
        ),
    ] {
        let p = parse(src).unwrap();
        for lang in [
            Language::Python,
            Language::Go,
            Language::TypeScript,
            Language::Rust,
        ] {
            let e = generate(&p, "R", lang).unwrap_err().to_string();
            assert!(e.to_lowercase().contains(what), "{lang:?} {src}: {e}");
        }
    }
}

#[test]
fn generated_rust_compiles_and_parses() {
    if !tool("rustc") {
        return;
    }
    let p = parse(SAMPLE).unwrap();
    let mut src = generate(&p, "Header", Language::Rust).unwrap();
    src.push_str(
        r#"
fn main() {
    let data = std::fs::read(std::env::args().nth(1).unwrap()).unwrap();
    let (h, n) = Header::parse(&data).unwrap();
    println!("{} {} {} {} {} {} {}", h.magic, h.items[1].tag, h.items[0].value_, h.delta, h.name, h.big, n);
    assert!(Header::parse(&data[..10]).is_err());
}
"#,
    );
    let dir = work_dir("rust");
    std::fs::write(dir.join("gen.rs"), src).unwrap();
    std::fs::write(dir.join("sample.bin"), sample_bytes()).unwrap();
    run(Command::new("rustc")
        .args(["--edition", "2021", "-O", "-o"])
        .arg(dir.join("gen"))
        .arg(dir.join("gen.rs")));
    let out = run(Command::new(dir.join("gen")).arg(dir.join("sample.bin")));
    assert_eq!(
        out.trim(),
        format!(
            "HEXA wxyz 16909060 -5 hi {} {}",
            0x1122_3344_5566_7788u64,
            sample_bytes().len()
        )
    );
}

#[test]
fn generated_python_runs() {
    if !tool("python3") {
        return;
    }
    let p = parse(SAMPLE).unwrap();
    let dir = work_dir("python");
    std::fs::write(
        dir.join("gen.py"),
        generate(&p, "Header", Language::Python).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("sample.bin"), sample_bytes()).unwrap();
    let script = "import sys; sys.path.insert(0, sys.argv[1]); import gen\n\
                  h = gen.parse(open(sys.argv[2], 'rb').read())\n\
                  print(h.magic, h.items[1].tag, h.items[0].value_, h.delta, h.name, h.kind.name, h.ratio)";
    let out = run(Command::new("python3")
        .args(["-I", "-c", script])
        .arg(&dir)
        .arg(dir.join("sample.bin")));
    assert_eq!(out.trim(), "HEXA wxyz 16909060 -5 hi B 1.5");
}

#[test]
fn generated_typescript_runs() {
    if !tool("node") {
        return;
    }
    let p = parse(SAMPLE).unwrap();
    let dir = work_dir("ts");
    std::fs::write(
        dir.join("gen.ts"),
        generate(&p, "Header", Language::TypeScript).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("sample.bin"), sample_bytes()).unwrap();
    std::fs::write(
        dir.join("main.ts"),
        "import { parseHeader } from './gen.ts';\n\
         import { readFileSync } from 'node:fs';\n\
         const b = readFileSync(process.argv[2]);\n\
         const r = parseHeader(new DataView(b.buffer, b.byteOffset, b.byteLength));\n\
         console.log(r.value.magic, r.value.items[1].tag, r.value.delta, r.value.name, r.value.big, r.size);\n",
    )
    .unwrap();
    let out = Command::new("node")
        .arg("--experimental-transform-types")
        .arg("--no-warnings")
        .arg(dir.join("main.ts"))
        .arg(dir.join("sample.bin"))
        .output()
        .unwrap();
    if !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("bad option") {
        return; // Node without TypeScript support.
    }
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!(
            "HEXA wxyz -5 hi {} {}",
            0x1122_3344_5566_7788u64,
            sample_bytes().len()
        )
    );
}

#[test]
fn generated_go_builds_and_runs() {
    if !tool("go") {
        return;
    }
    let p = parse(SAMPLE).unwrap();
    let dir = work_dir("go");
    let code = generate(&p, "Header", Language::Go).unwrap();
    std::fs::write(
        dir.join("parser.go"),
        code.replacen("package parser", "package main", 1),
    )
    .unwrap();
    std::fs::write(dir.join("go.mod"), "module example.com/gen\n\ngo 1.21\n").unwrap();
    std::fs::write(
        dir.join("main.go"),
        "package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n\nfunc main() {\n\tb, _ := os.ReadFile(os.Args[1])\n\th, err := ParseHeader(b)\n\tif err != nil {\n\t\tpanic(err)\n\t}\n\tfmt.Println(h.Magic, h.Items[1].Tag, h.Delta, h.Name, h.Big)\n\tif _, err := ParseHeader(b[:10]); err == nil {\n\t\tpanic(\"expected error\")\n\t}\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("sample.bin"), sample_bytes()).unwrap();
    let out = run(Command::new("go")
        .args(["run", "."])
        .arg(dir.join("sample.bin"))
        .current_dir(&dir)
        .env("GOFLAGS", "-mod=mod")
        .env("GOTOOLCHAIN", "local"));
    assert_eq!(
        out.trim(),
        format!("HEXA wxyz -5 hi {}", 0x1122_3344_5566_7788u64)
    );
}
