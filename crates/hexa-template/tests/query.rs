//! STRC-14 editing, STRC-15 filter/export, DIFF-10 diff, SCR-08 binaries.

mod common;

use std::io::{BufReader, Write};
use std::process::{Command, Stdio};

use common::{get, run_ok};
use hexa_template::query::{ChangeKind, ancestors};
use hexa_template::{ExportFormat, ExportOptions, Value, diff, encode_value, export, filter};

const SRC: &str = r#"
enum Kind : u8 { A = 1, B = 2, C = 4 } [[flags]];
bitfield Bits { lo : 4; hi : 4; };
struct Item { u16 id; char tag[4] [[comment("tag, \"quoted\"")]]; };
struct File {
    u32 magic [[format("hex")]];
    Kind kind;
    Bits bits;
    s16 delta;
    float ratio;
    Item items[2];
    u8 raw[3];
    string name;
};
File file @ 0;
"#;

fn data() -> Vec<u8> {
    let mut d = 0xCAFE_u32.to_le_bytes().to_vec();
    d.push(5);
    d.push(0x21);
    d.extend_from_slice(&(-3i16).to_le_bytes());
    d.extend_from_slice(&0.5f32.to_le_bytes());
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(b"ab<>");
    d.extend_from_slice(&2u16.to_le_bytes());
    d.extend_from_slice(b"cd\0\0");
    d.extend_from_slice(&[7, 8, 9]);
    d.extend_from_slice(b"name\0");
    d
}

#[test]
fn filter_terms() {
    let r = run_ok(SRC, &data());
    let names = |q: &str| -> Vec<String> { filter(&r, q).iter().map(|id| r.path(*id)).collect() };
    assert_eq!(names("tag"), ["file.items[0].tag", "file.items[1].tag"]);
    assert_eq!(
        names("type:item"),
        ["file.items", "file.items[0]", "file.items[1]"]
    );
    assert_eq!(names("value:\"cd\""), ["file.items[1].tag"]);
    assert_eq!(names("kind:enum"), ["file.kind"]);
    assert_eq!(names("offset:0x4 kind:enum"), ["file.kind"]);
    assert_eq!(
        names("comment:quoted name:tag"),
        ["file.items[0].tag", "file.items[1].tag"]
    );
    assert_eq!(names(""), [] as [std::string::String; 0]);
    assert_eq!(names("nothing-matches"), [] as [std::string::String; 0]);
    let id = filter(&r, "hi")[0];
    let chain: Vec<_> = ancestors(&r, id)
        .iter()
        .map(|i| r.nodes[*i as usize].name.clone())
        .collect();
    assert_eq!(chain, ["file", "bits", "hi"]);
}

#[test]
fn export_formats() {
    let r = run_ok(SRC, &data());
    let json = export(&r, ExportFormat::Json, &ExportOptions::default());
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v[0]["name"], "file");
    assert_eq!(v[0]["children"][0]["value"], "0x0000CAFE");
    assert_eq!(v[0]["children"][6]["children"].as_array().unwrap().len(), 3);
    assert_eq!(v[0]["children"][6]["children"][2]["raw"]["value"], 9);

    let csv = export(&r, ExportFormat::Csv, &ExportOptions::default());
    let lines: Vec<_> = csv.lines().collect();
    assert_eq!(lines[0], "path,type,offset,size,value,comment");
    assert!(lines.contains(&"file.kind,Kind,4,1,A | C (0x5),"));
    assert!(lines.iter().any(|l| l.starts_with("file.items[0].tag,") && l.ends_with(",\"tag, \"\"quoted\"\"\"")));
    assert!(lines.contains(&"file.raw[1],u8,25,1,8 (0x8),"));

    let xml = export(&r, ExportFormat::Xml, &ExportOptions::default());
    assert!(xml.starts_with("<?xml"));
    assert!(xml.contains("<node name=\"tag\" type=\"char[4]\" offset=\"14\" size=\"4\" value=\"&quot;ab&lt;&gt;&quot;\""));
    assert!(xml.trim_end().ends_with("</template>"));

    let limited = export(
        &r,
        ExportFormat::Json,
        &ExportOptions {
            max_array_elements: 1,
            ..ExportOptions::default()
        },
    );
    let v: serde_json::Value = serde_json::from_str(&limited).unwrap();
    assert_eq!(v[0]["children"][6]["omitted"], 2);
}

#[test]
fn structural_diff() {
    let a = run_ok(SRC, &data());
    let mut d2 = data();
    d2[11] = 0x40; // ratio 0.5 -> 2.0
    d2[25] = 0x42; // raw[1]
    let mut d2b = d2.clone();
    d2b.truncate(d2b.len() - 5);
    d2b.extend_from_slice(b"x\0");
    let b = run_ok(SRC, &d2b);
    let changes = diff(&a, &b);
    let paths: Vec<_> = changes.iter().map(|c| (c.path.as_str(), c.kind)).collect();
    assert_eq!(
        paths,
        [
            ("file.ratio", ChangeKind::Changed),
            ("file.raw[1]", ChangeKind::Changed),
            ("file.name", ChangeKind::Changed)
        ]
    );
    assert_eq!(changes[1].old.as_deref(), Some("8 (0x8)"));
    assert_eq!(changes[1].new.as_deref(), Some("66 (0x42)"));
    assert_eq!(diff(&a, &a), [] as [hexa_template::FieldChange; 0]);

    let other = run_ok("u8 only;", &[1]);
    let changes = diff(&a, &other);
    assert!(
        changes
            .iter()
            .any(|c| c.path == "only" && c.kind == ChangeKind::Added)
    );
    assert!(
        changes
            .iter()
            .any(|c| c.path == "file.magic" && c.kind == ChangeKind::Removed)
    );
}

#[test]
fn encode_values() {
    let r = run_ok(SRC, &data());
    let enc = |path: &str, text: &str| encode_value(&r, get(&r, path), text);
    assert_eq!(enc("file.magic", "0xBEEF").unwrap(), [0xEF, 0xBE, 0, 0]);
    assert!(enc("file.magic", "0x1_0000_0000").is_err());
    assert_eq!(enc("file.delta", "-2").unwrap(), [0xFE, 0xFF]);
    assert!(enc("file.delta", "40000").is_err());
    assert_eq!(enc("file.kind", "B | C").unwrap(), [6]);
    assert_eq!(enc("file.kind", "Kind::A").unwrap(), [1]);
    assert!(enc("file.kind", "Nope").is_err());
    assert_eq!(enc("file.ratio", "2.0").unwrap(), 2.0f32.to_le_bytes());
    assert!(enc("file.ratio", "1e300").is_err());
    assert_eq!(enc("file.items[0].tag", "xy").unwrap(), *b"xy\0\0");
    assert!(enc("file.items[0].tag", "toolong").is_err());
    assert_eq!(enc("file.name", "abcd").unwrap(), *b"abcd\0");
    assert!(enc("file.name", "abcde").is_err());
    // Bit-field member: only its bits change (0x21 → hi = 0xF).
    assert_eq!(enc("file.bits.hi", "15").unwrap(), [0xF1]);
    assert!(enc("file.bits.hi", "16").is_err());
    assert!(enc("file.items", "1").is_err());
    // Lazy array element from `children`.
    let raw = get(&r, "file.raw");
    let elem = &hexa_template::children(&r, raw.id, 1..2)[0];
    assert_eq!(elem.raw, Some(Value::Int(8)));
    assert_eq!(encode_value(&r, elem, "255").unwrap(), [255]);
    assert!(encode_value(&r, elem, "256").is_err());
}

fn frame(body: &serde_json::Value) -> Vec<u8> {
    let s = body.to_string();
    format!("Content-Length: {}\r\n\r\n{s}", s.len()).into_bytes()
}

#[test]
fn lsp_binary_round_trip() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_hexa-template-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn lsp");
    let mut stdin = child.stdin.take().unwrap();
    let msgs = [
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"capabilities": {}}}),
        serde_json::json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {"uri": "file:///a.hexpat", "languageId": "hexpat", "version": 1, "text": "Nope n;"}}}),
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"}),
        serde_json::json!({"jsonrpc": "2.0", "method": "exit"}),
    ];
    for m in &msgs {
        stdin.write_all(&frame(m)).unwrap();
    }
    drop(stdin);
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let init = hexa_template::lsp::read_message(&mut out).unwrap().unwrap();
    assert_eq!(init["id"], 1);
    let diags = hexa_template::lsp::read_message(&mut out).unwrap().unwrap();
    assert!(
        diags["params"]["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("Nope")
    );
    let shutdown = hexa_template::lsp::read_message(&mut out).unwrap().unwrap();
    assert_eq!(shutdown["id"], 2);
    assert!(child.wait().unwrap().success());
}

#[test]
fn dap_binary_runs_template() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("hexa-template-dap");
    std::fs::create_dir_all(&dir).unwrap();
    let tpl = dir.join("t.hexpat");
    let bin = dir.join("d.bin");
    std::fs::write(&tpl, "u8 a;\nstd::print(\"a={}\", a);\n").unwrap();
    std::fs::write(&bin, [42]).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_hexa-template-dap"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn dap");
    let mut stdin = child.stdin.take().unwrap();
    let reqs = [
        serde_json::json!({"seq": 1, "type": "request", "command": "initialize", "arguments": {}}),
        serde_json::json!({"seq": 2, "type": "request", "command": "launch", "arguments": {"template": tpl, "data": bin}}),
        serde_json::json!({"seq": 3, "type": "request", "command": "configurationDone", "arguments": {}}),
        serde_json::json!({"seq": 4, "type": "request", "command": "disconnect", "arguments": {}}),
    ];
    for r in &reqs {
        stdin.write_all(&frame(r)).unwrap();
    }
    drop(stdin);
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut seen = Vec::new();
    while let Ok(Some(m)) = hexa_template::lsp::read_message(&mut out) {
        seen.push(m);
    }
    assert!(child.wait().unwrap().success());
    assert!(
        seen.iter()
            .any(|m| m["command"] == "launch" && m["success"] == true),
        "{seen:#?}"
    );
    assert!(
        seen.iter()
            .any(|m| m["event"] == "output" && m["body"]["output"] == "a=42\n")
    );
    assert!(seen.iter().any(|m| m["event"] == "terminated"));
}
