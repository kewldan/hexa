//! End-to-end tests running the built `hexa-cli` binary on temporary files.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hexa-cli"))
}

fn run(args: &[&str]) -> Output {
    bin().args(args).output().expect("hexa-cli runs")
}

fn code(o: &Output) -> i32 {
    o.status.code().expect("exit code")
}

fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).expect("UTF-8 stdout")
}

fn json(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid JSON ({e}): {}\nstderr: {}",
            stdout(o),
            String::from_utf8_lossy(&o.stderr)
        )
    })
}

struct Dir(TempDir);

impl Dir {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }

    fn file(&self, name: &str, data: &[u8]) -> PathBuf {
        let p = self.0.path().join(name);
        std::fs::write(&p, data).unwrap();
        p
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.path().join(name)
    }
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

/// Deterministic pseudo-random bytes.
fn noise(len: usize, mut seed: u32) -> Vec<u8> {
    (0..len)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as u8
        })
        .collect()
}

#[test]
fn hash_known_vectors() {
    let d = Dir::new();
    let f = d.file("v.bin", b"123456789");
    let o = run(&["--json", "hash", s(&f), "-a", "crc32,md5,sha256,adler32"]);
    assert_eq!(code(&o), 0);
    let v = json(&o);
    let hashes = v["files"][0]["hashes"].as_array().unwrap();
    let get = |id: &str| {
        hashes.iter().find(|h| h["id"] == id).unwrap()["value"]
            .as_str()
            .unwrap()
            .to_ascii_lowercase()
    };
    assert_eq!(get("crc32"), "cbf43926");
    assert_eq!(get("md5"), "25f9e794323b453885f5181f1b624d0b");
    assert_eq!(
        get("sha256"),
        "15e2b0d3c33891ebb0f1ef609ec419420c20e320ce94c65fbc8c3312448eb225"
    );
    assert_eq!(get("adler32"), "091e01de");

    let abc = d.file("abc", b"abc");
    let o = run(&["hash", s(&abc), "-a", "sha1"]);
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("a9993e364706816aba3e25717850c26c9cd0d89d"));
}

#[test]
fn hash_expect_and_errors() {
    let d = Dir::new();
    let f = d.file("v.bin", b"123456789");
    let ok = run(&["hash", s(&f), "--expect", "crc32=0xCBF43926"]);
    assert_eq!(code(&ok), 0);
    let bad = run(&["--json", "hash", s(&f), "--expect", "sha256=00"]);
    assert_eq!(code(&bad), 1);
    assert_eq!(json(&bad)["expect"]["ok"], false);
    assert_eq!(code(&run(&["hash", s(&f), "-a", "nope"])), 2);
    assert_eq!(code(&run(&["hash", s(&d.path("missing"))])), 3);
    let o = run(&[
        "hash",
        s(&f),
        "-a",
        "crc32",
        "--start",
        "1",
        "--length",
        "2",
    ]);
    assert_eq!(code(&o), 0);
    let o = run(&["hash", s(&f), "--start", "5", "--end", "99"]);
    assert_eq!(code(&o), 2);
}

#[test]
fn usage_errors_exit_2() {
    assert_eq!(code(&run(&[])), 2);
    assert_eq!(code(&run(&["no-such-command"])), 2);
    assert_eq!(code(&run(&["--help"])), 0);
    assert!(stdout(&run(&["--help"])).contains("Exit codes"));
}

#[test]
fn search_hit_and_miss() {
    let d = Dir::new();
    let f = d.file("s.bin", b"\x00\x01ABCD\x00ABCD\x10\x00\x00\x00");
    let hit = run(&["--json", "search", s(&f), "--hex", "41 42 ?? 44"]);
    assert_eq!(code(&hit), 0);
    let v = json(&hit);
    let offs: Vec<u64> = v["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["offset"].as_u64().unwrap())
        .collect();
    assert_eq!(offs, [2, 7]);
    assert_eq!(v["count"], 2);

    let o = run(&["search", s(&f), "--text", "abcd", "-i", "--count"]);
    assert_eq!(code(&o), 0);
    assert_eq!(stdout(&o).trim(), "2");

    let o = run(&["--json", "search", s(&f), "--seq", "u32le=0x10"]);
    assert_eq!(code(&o), 0);
    assert_eq!(json(&o)["matches"][0]["offset"], 11);

    let o = run(&["search", s(&f), "--regex", "AB(C|X)D", "--start", "3"]);
    assert_eq!(code(&o), 0);
    assert_eq!(stdout(&o).lines().count(), 1);

    let miss = run(&["search", s(&f), "--text", "zzz"]);
    assert_eq!(code(&miss), 1);
    assert_eq!(stdout(&miss), "");
    assert_eq!(code(&run(&["search", s(&f), "--hex", "4G"])), 2);
    assert_eq!(code(&run(&["search", s(&f)])), 2);
}

#[test]
fn diff_exit_codes_and_json() {
    let d = Dir::new();
    let base = noise(4096, 7);
    let mut changed = base.clone();
    changed[100] ^= 0xFF;
    changed.extend_from_slice(b"tail");
    let a = d.file("a.bin", &base);
    let a2 = d.file("a2.bin", &base);
    let b = d.file("b.bin", &changed);

    let same = run(&["diff", s(&a), s(&a2)]);
    assert_eq!(code(&same), 0);
    let o = run(&["--json", "diff", s(&a), s(&a2)]);
    assert_eq!(code(&o), 0);
    assert_eq!(json(&o)["identical"], true);

    let o = run(&["--json", "diff", s(&a), s(&b)]);
    assert_eq!(code(&o), 1);
    let v = json(&o);
    assert_eq!(v["identical"], false);
    let regions = v["regions"].as_array().unwrap();
    assert_eq!(regions.len(), 2);
    assert_eq!(regions[0]["kind"], "changed");
    assert_eq!(regions[0]["a"]["start"], 100);
    assert_eq!(regions[0]["a"]["len"], 1);
    assert_eq!(regions[1]["kind"], "inserted");
    assert_eq!(regions[1]["b"]["len"], 4);
    assert_eq!(v["stats"]["differences"], 2);
    assert_eq!(v["stats"]["lenB"], 4100);

    let csv = run(&["diff", "-f", "csv", s(&a), s(&b)]);
    assert_eq!(code(&csv), 1);
    assert!(stdout(&csv).starts_with("index,kind,aStart,aLen,bStart,bLen\n"));
    let stats = run(&["--json", "diff", "--stats", s(&a), s(&b)]);
    assert!(json(&stats)["stats"]["matchPercent"].as_f64().unwrap() > 99.0);
    let quiet = run(&["diff", "-q", s(&a), s(&b)]);
    assert_eq!(code(&quiet), 1);
    assert_eq!(stdout(&quiet), "");
    assert_eq!(code(&run(&["diff", "--bytewise", s(&a), s(&b)])), 1);
    assert_eq!(code(&run(&["diff", s(&a), s(&d.path("nope"))])), 3);
}

#[test]
fn patch_round_trip_all_formats() {
    let d = Dir::new();
    let src = noise(20_000, 3);
    let mut dst = src.clone();
    dst[10..20].copy_from_slice(b"0123456789");
    dst.splice(5000..5000, b"inserted bytes".iter().copied());
    dst.truncate(18_000);
    let a = d.file("src.bin", &src);
    let b = d.file("dst.bin", &dst);
    for f in ["ips", "ups", "bps", "vcdiff", "bsdiff"] {
        let p = d.path(&format!("p.{f}"));
        let o = run(&["patch", "create", s(&a), s(&b), "-o", s(&p), "-f", f]);
        assert_eq!(
            code(&o),
            0,
            "create {f}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        let out = d.path(&format!("out.{f}"));
        let o = run(&["--json", "patch", "apply", s(&a), s(&p), "-o", s(&out)]);
        assert_eq!(
            code(&o),
            0,
            "apply {f}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert_eq!(json(&o)["size"], 18_000);
        assert_eq!(std::fs::read(&out).unwrap(), dst, "{f} round trip");
        let info = run(&["--json", "patch", "info", s(&p)]);
        assert_eq!(code(&info), 0);
        assert!(json(&info)["format"].is_string());
    }
    // Format from the output extension.
    let p = d.path("auto.bps");
    assert_eq!(
        code(&run(&["patch", "create", s(&a), s(&b), "-o", s(&p)])),
        0
    );
    assert!(std::fs::read(&p).unwrap().starts_with(b"BPS1"));
    // A BPS patch refuses the wrong source.
    let wrong = d.file("wrong.bin", &noise(20_000, 4));
    let o = run(&["patch", "apply", s(&wrong), s(&p), "-o", s(&d.path("x"))]);
    assert_eq!(code(&o), 4);
    // Garbage is a data error.
    let junk = d.file("junk.ips", b"not a patch");
    assert_eq!(code(&run(&["patch", "info", s(&junk)])), 4);
}

#[test]
fn convert_round_trips() {
    let d = Dir::new();
    let data = noise(3000, 11);
    let bin = d.file("fw.bin", &data);
    let hex = d.path("fw.hex");
    let o = run(&["convert", s(&bin), "-o", s(&hex), "--base", "0x08000000"]);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    let text = std::fs::read_to_string(&hex).unwrap();
    assert!(text.starts_with(':'));
    assert!(text.trim_end().ends_with(":00000001FF"));
    let back = d.path("back.bin");
    let o = run(&["--json", "convert", s(&hex), "-o", s(&back)]);
    assert_eq!(code(&o), 0);
    let v = json(&o);
    assert_eq!(v["from"], "ihex");
    assert_eq!(v["baseAddress"], 0x0800_0000);
    assert_eq!(std::fs::read(&back).unwrap(), data);

    for (fmt, ext) in [
        ("srec", "s19"),
        ("titxt", "txt"),
        ("tek", "tek"),
        ("xxd", "xxd"),
    ] {
        let t = d.path(&format!("fw.{ext}"));
        let o = run(&["convert", s(&bin), "--to", fmt, "-o", s(&t)]);
        assert_eq!(code(&o), 0, "to {fmt}");
        let b = d.path(&format!("from-{fmt}.bin"));
        let o = run(&["convert", s(&t), "--from", fmt, "--to", "bin", "-o", s(&b)]);
        assert_eq!(
            code(&o),
            0,
            "from {fmt}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert_eq!(std::fs::read(&b).unwrap(), data, "{fmt} round trip");
    }

    let o = run(&["convert", s(&bin), "--to", "c", "--name", "fw"]);
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("uint8_t fw["));
    let small = d.file("v.bin", b"123");
    assert_eq!(
        stdout(&run(&["convert", s(&small), "--to", "plain"])),
        "313233\n"
    );

    // A corrupted checksum is a data error unless ignored.
    let bad = d.file("bad.hex", b":0300000001020300\n:00000001FF\n");
    assert_eq!(code(&run(&["convert", s(&bad), "--to", "bin"])), 4);
    let o = run(&["convert", s(&bad), "--to", "bin", "--ignore-errors"]);
    assert_eq!(code(&o), 0);
    assert_eq!(
        code(&run(&["convert", s(&bin), "-o", s(&d.path("x.unknown"))])),
        2
    );
}

#[test]
fn transform_and_reverse() {
    let d = Dir::new();
    let f = d.file("t.bin", b"hello");
    let recipe = d.file(
        "r.json",
        br#"{"steps":[{"kind":"bitwise","op":"xor","operand":"0x20"}]}"#,
    );
    let out = d.path("out.bin");
    let o = run(&["transform", s(&f), "-r", s(&recipe), "-o", s(&out)]);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(std::fs::read(&out).unwrap(), b"HELLO");
    let o = run(&["transform", s(&out), "-r", s(&recipe), "--reverse"]);
    assert_eq!(o.stdout, b"hello");
    let o = run(&[
        "transform",
        s(&f),
        "-r",
        r#"[{"kind":"not"}]"#,
        "--start",
        "1",
        "--length",
        "2",
        "--splice",
    ]);
    assert_eq!(code(&o), 0);
    assert_eq!(o.stdout, [b'h', !b'e', !b'l', b'l', b'o']);
    assert_eq!(
        code(&run(&[
            "transform",
            s(&f),
            "-r",
            r#"{"steps":[{"kind":"nope"}]}"#
        ])),
        4
    );
}

#[test]
fn merge3_exit_codes() {
    let d = Dir::new();
    let base = noise(2048, 5);
    let mut ours = base.clone();
    ours[10] = 1;
    let mut theirs = base.clone();
    theirs[1000] = 2;
    let (b, o_, t) = (
        d.file("base", &base),
        d.file("ours", &ours),
        d.file("theirs", &theirs),
    );
    let out = d.path("merged");
    let o = run(&["merge3", s(&b), s(&o_), s(&t), "-o", s(&out)]);
    assert_eq!(code(&o), 0);
    let merged = std::fs::read(&out).unwrap();
    assert_eq!((merged[10], merged[1000]), (1, 2));

    let mut theirs2 = base.clone();
    theirs2[10] = 3;
    let t2 = d.file("theirs2", &theirs2);
    let o = run(&["--json", "merge3", s(&b), s(&o_), s(&t2), "-o", s(&out)]);
    assert_eq!(code(&o), 1);
    assert_eq!(json(&o)["conflicts"].as_array().unwrap().len(), 1);
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 == 1 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// A valid 1×1 RGB PNG (stored deflate block).
fn tiny_png() -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, ty: &[u8], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let mut body = ty.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend(crc32(&body).to_be_bytes());
    }
    let raw = [0u8, 0xFF, 0x00, 0x00];
    let adler = {
        let (mut a, mut b) = (1u32, 0u32);
        for &x in &raw {
            a = (a + u32::from(x)) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    };
    let mut z = vec![0x78, 0x01, 0x01, 4, 0, 0xFB, 0xFF];
    z.extend_from_slice(&raw);
    z.extend(adler.to_be_bytes());
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend(1u32.to_be_bytes());
    ihdr.extend(1u32.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &z);
    chunk(&mut png, b"IEND", &[]);
    png
}

#[test]
fn json_output_is_valid() {
    let d = Dir::new();
    let mut data = noise(512, 9);
    data.extend(tiny_png());
    data.extend_from_slice(b"some readable string here\0");
    let f = d.file("x.bin", &data);
    for args in [
        vec!["info", s(&f)],
        vec!["info", s(&f), "--classify", "64"],
        vec!["strings", s(&f)],
        vec!["embedded", s(&f)],
        vec!["hash", "--list"],
        vec!["template", "--list"],
        vec!["search", s(&f), "--text", "readable"],
    ] {
        let mut full = vec!["--json"];
        full.extend(&args);
        let o = run(&full);
        assert!(
            code(&o) == 0,
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        json(&o);
    }
    let o = run(&["--json", "strings", s(&f), "-n", "8"]);
    assert!(
        json(&o)["strings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["text"] == "some readable string here")
    );
    let o = run(&["--json", "info", s(&f)]);
    let v = json(&o);
    assert_eq!(v["size"], data.len());
    assert_eq!(v["entropy"].as_f64().map(|e| e > 0.0), Some(true));
    let o = run(&["--json", "embedded", s(&f), "-x", s(&d.path("carved"))]);
    assert_eq!(code(&o), 0);
    let v = json(&o);
    assert_eq!(v["found"][0]["kind"], "png");
    assert_eq!(v["found"][0]["offset"], 512);
    let carved = std::fs::read(d.path("carved").join("00000200.png")).unwrap();
    assert_eq!(carved, tiny_png());
    let png = d.file("t.png", &tiny_png());
    let o = run(&["--json", "info", s(&png)]);
    assert!(
        json(&o)["types"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == "png")
    );
    let o = run(&["--json", "template", s(&png)]);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(json(&o)["template"], "library:png");
    assert_eq!(code(&run(&["embedded", s(&png)])), 1);
}

#[test]
fn exe_and_disassembly() {
    let me = env!("CARGO_BIN_EXE_hexa-cli");
    let o = run(&["--json", "exe", me]);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    assert!(json(&o)["executable"]["sections"].is_array());
    let o = run(&["--json", "exe", me, "--disasm", "-n", "3"]);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(json(&o)["disassembly"].as_array().unwrap().len(), 3);

    let d = Dir::new();
    let code_bin = d.file("code.bin", b"\x55\x48\x89\xe5\xc3");
    let o = run(&[
        "exe",
        s(&code_bin),
        "--arch",
        "x86_64",
        "--disasm",
        "0",
        "--base",
        "0x1000",
    ]);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    let text = stdout(&o);
    assert!(text.contains("push rbp"), "{text}");
    assert!(text.contains("0000000000001004") && text.contains("ret"));
    assert_eq!(code(&run(&["exe", s(&code_bin)])), 4);
    assert_eq!(
        code(&run(&["exe", s(&code_bin), "--arch", "z80", "--disasm"])),
        2
    );
}

#[test]
fn exe_patch_check_and_apply() {
    let d = Dir::new();
    let f = d.file("p.bin", b"\x00\x01\x02\x03\x04\x05");
    let list = d.file(
        "l.json",
        br#"{"patches":[{"offset":2,"original":"0203","replacement":"AABB"}]}"#,
    );
    let o = run(&["--json", "exe-patch", s(&f), s(&list), "--check"]);
    assert_eq!(code(&o), 0);
    assert_eq!(json(&o)["patches"][0]["state"], "original");
    let out = d.path("out.bin");
    assert_eq!(
        code(&run(&["exe-patch", s(&f), s(&list), "-o", s(&out)])),
        0
    );
    assert_eq!(std::fs::read(&out).unwrap(), b"\x00\x01\xAA\xBB\x04\x05");
    let o = run(&["--json", "exe-patch", s(&out), s(&list), "--check"]);
    assert_eq!(json(&o)["patches"][0]["state"], "applied");
    let other = d.file("o.bin", b"\x09\x09\x09\x09\x09\x09");
    assert_eq!(
        code(&run(&["exe-patch", s(&other), s(&list), "--check"])),
        1
    );
    let o = run(&["exe-patch", s(&out), s(&list), "--revert"]);
    assert_eq!(o.stdout, b"\x00\x01\x02\x03\x04\x05");
    // Not a PE: nothing to fix, stored == computed == none.
    assert_eq!(code(&run(&["pe-checksum", s(&f)])), 0);
}

#[test]
fn template_to_json() {
    let d = Dir::new();
    let f = d.file("h.bin", b"HXA\x01\x10\x00\x00\x00rest");
    let t = d.file(
        "h.hexpat",
        b"struct Header { char magic[3]; u8 version; u32 len; };\nHeader header @ 0x00;\n",
    );
    let o = run(&["--json", "template", s(&f), s(&t)]);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    let v = json(&o);
    let header = &v["nodes"][0];
    assert_eq!(header["name"], "header");
    assert_eq!(header["size"], 8);
    let kids = header["children"].as_array().unwrap();
    assert_eq!(kids[1]["name"], "version");
    assert_eq!(kids[1]["raw"], 1);
    assert_eq!(kids[2]["raw"], 16);
    let bad = d.file("bad.hexpat", b"struct {");
    assert_eq!(code(&run(&["template", s(&f), s(&bad)])), 4);
    assert_eq!(code(&run(&["template", s(&f), "no-such-template"])), 2);
}

#[test]
fn completions_generate() {
    let o = run(&["completions", "bash"]);
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("hexa-cli"));
}

#[test]
fn stdin_input() {
    use std::io::Write;
    let mut child = bin()
        .args(["hash", "-", "-a", "crc32"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"123456789").unwrap();
    let o = child.wait_with_output().unwrap();
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).to_ascii_lowercase().contains("cbf43926"));
}
