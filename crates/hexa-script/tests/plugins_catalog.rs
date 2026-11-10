//! SCR-04 WASM plugins (WAT test plugin) and SCR-05 catalog.

use std::path::Path;
use std::time::Duration;

use hexa_script::{
    Catalog, CatalogError, InspectRow, Plugin, PluginError, PluginKind, PluginLimits,
    PluginManifest, PluginRegistry, sha256_hex, verify_sha256,
};

/// Test plugin implementing every kind:
/// transform = reverse bytes (and logs), inspect = sum/len rows,
/// format = lowercase hex, provide = request + "!".
const ALL_KINDS_WAT: &str = r#"
(module
  (import "hexa" "hexa_log" (func $log (param i32 i32)))
  (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 4096))
  (data (i32.const 0) "[{\"label\":\"sum\",\"value\":\"")
  (data (i32.const 64) "\"},{\"label\":\"len\",\"value\":\"")
  (data (i32.const 128) "\"}]")
  (data (i32.const 160) "0123456789abcdef")
  (data (i32.const 192) "transform called")

  (func $alloc (export "hexa_alloc") (param $len i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $heap))
    (global.set $heap
      (i32.and (i32.add (i32.add (local.get $ptr) (local.get $len)) (i32.const 7))
               (i32.const -8)))
    (block $ok
      (loop $grow
        (br_if $ok (i32.le_u (global.get $heap)
                             (i32.mul (memory.size) (i32.const 65536))))
        (if (i32.eq (memory.grow (i32.const 1)) (i32.const -1)) (then unreachable))
        (br $grow)))
    (local.get $ptr))

  (func (export "hexa_free") (param i32 i32))

  (func $pack (param $ptr i32) (param $len i32) (result i64)
    (i64.or (i64.shl (i64.extend_i32_u (local.get $ptr)) (i64.const 32))
            (i64.extend_i32_u (local.get $len))))

  (func $copy (param $dst i32) (param $src i32) (param $len i32) (result i32)
    (memory.copy (local.get $dst) (local.get $src) (local.get $len))
    (i32.add (local.get $dst) (local.get $len)))

  (func $utoa (param $dst i32) (param $n i32) (result i32)
    (local $count i32) (local $t i32) (local $p i32)
    (local.set $count (i32.const 1))
    (local.set $t (local.get $n))
    (block $done
      (loop $l
        (br_if $done (i32.lt_u (local.get $t) (i32.const 10)))
        (local.set $t (i32.div_u (local.get $t) (i32.const 10)))
        (local.set $count (i32.add (local.get $count) (i32.const 1)))
        (br $l)))
    (local.set $p (i32.add (local.get $dst) (local.get $count)))
    (loop $w
      (local.set $p (i32.sub (local.get $p) (i32.const 1)))
      (i32.store8 (local.get $p)
        (i32.add (i32.const 48) (i32.rem_u (local.get $n) (i32.const 10))))
      (local.set $n (i32.div_u (local.get $n) (i32.const 10)))
      (br_if $w (local.get $n)))
    (i32.add (local.get $dst) (local.get $count)))

  (func (export "transform") (param $ptr i32) (param $len i32) (result i64)
    (local $out i32) (local $i i32)
    (call $log (i32.const 192) (i32.const 16))
    (local.set $out (call $alloc (local.get $len)))
    (block $done
      (loop $l
        (br_if $done (i32.ge_u (local.get $i) (local.get $len)))
        (i32.store8 (i32.add (local.get $out) (local.get $i))
          (i32.load8_u (i32.sub (i32.add (local.get $ptr) (local.get $len))
                                (i32.add (local.get $i) (i32.const 1)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (call $pack (local.get $out) (local.get $len)))

  (func (export "inspect") (param $ptr i32) (param $len i32) (result i64)
    (local $sum i32) (local $i i32) (local $out i32) (local $p i32)
    (block $done
      (loop $l
        (br_if $done (i32.ge_u (local.get $i) (local.get $len)))
        (local.set $sum (i32.add (local.get $sum)
          (i32.load8_u (i32.add (local.get $ptr) (local.get $i)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (local.set $out (call $alloc (i32.const 80)))
    (local.set $p (call $copy (local.get $out) (i32.const 0) (i32.const 25)))
    (local.set $p (call $utoa (local.get $p) (local.get $sum)))
    (local.set $p (call $copy (local.get $p) (i32.const 64) (i32.const 27)))
    (local.set $p (call $utoa (local.get $p) (local.get $len)))
    (local.set $p (call $copy (local.get $p) (i32.const 128) (i32.const 3)))
    (call $pack (local.get $out) (i32.sub (local.get $p) (local.get $out))))

  (func (export "format") (param $ptr i32) (param $len i32) (result i64)
    (local $out i32) (local $i i32) (local $b i32) (local $o i32)
    (local.set $out (call $alloc (i32.shl (local.get $len) (i32.const 1))))
    (block $done
      (loop $l
        (br_if $done (i32.ge_u (local.get $i) (local.get $len)))
        (local.set $b (i32.load8_u (i32.add (local.get $ptr) (local.get $i))))
        (local.set $o (i32.add (local.get $out) (i32.shl (local.get $i) (i32.const 1))))
        (i32.store8 (local.get $o)
          (i32.load8_u (i32.add (i32.const 160) (i32.shr_u (local.get $b) (i32.const 4)))))
        (i32.store8 (i32.add (local.get $o) (i32.const 1))
          (i32.load8_u (i32.add (i32.const 160) (i32.and (local.get $b) (i32.const 15)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (call $pack (local.get $out) (i32.shl (local.get $len) (i32.const 1))))

  (func (export "provide") (param $ptr i32) (param $len i32) (result i64)
    (local $out i32)
    (local.set $out (call $alloc (i32.add (local.get $len) (i32.const 1))))
    (drop (call $copy (local.get $out) (local.get $ptr) (local.get $len)))
    (i32.store8 (i32.add (local.get $out) (local.get $len)) (i32.const 33))
    (call $pack (local.get $out) (i32.add (local.get $len) (i32.const 1))))
)
"#;

const SPIN_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "hexa_alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "hexa_free") (param i32 i32))
  (func (export "transform") (param i32 i32) (result i64)
    (loop $l (br $l))
    (i64.const 0))
)
"#;

const GROW_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "hexa_alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "hexa_free") (param i32 i32))
  (func (export "transform") (param i32 i32) (result i64)
    (if (i32.eq (memory.grow (i32.const 100)) (i32.const -1)) (then unreachable))
    (i64.const 0))
)
"#;

const BAD_OUTPUT_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "hexa_alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "hexa_free") (param i32 i32))
  (func (export "transform") (param i32 i32) (result i64)
    (i64.const 0x0000FFFF00001000))
)
"#;

fn wasm(wat: &str) -> Vec<u8> {
    wat::parse_str(wat).unwrap()
}

fn manifest(id: &str, kinds: &[PluginKind]) -> PluginManifest {
    PluginManifest {
        id: id.into(),
        name: format!("{id} plugin"),
        version: "1.0.0".into(),
        description: "test".into(),
        kinds: kinds.to_vec(),
        module: "plugin.wasm".into(),
    }
}

fn all_kinds_plugin() -> Plugin {
    Plugin::from_bytes(
        manifest("test.all", &PluginKind::ALL),
        &wasm(ALL_KINDS_WAT),
        PluginLimits::default(),
    )
    .unwrap()
}

// ---------------------------------------------------------------- SCR-04

#[test]
fn manifest_json_format() {
    let json = r#"{"id":"x.y","name":"X","version":"0.1","kinds":["transform","inspector","formatter","provider"]}"#;
    let m = PluginManifest::from_json(json).unwrap();
    assert_eq!(m.kinds, PluginKind::ALL);
    assert_eq!(m.module, "plugin.wasm");
    assert_eq!(m.description, "");
    let back: serde_json::Value = serde_json::from_str(&m.to_json()).unwrap();
    assert_eq!(back["kinds"][1], "inspector");
    for bad in [
        r#"{"id":"../x","name":"X","version":"1","kinds":["transform"]}"#,
        r#"{"id":"x","name":"X","version":"1","kinds":[]}"#,
        r#"{"id":"x","name":"X","version":"1","kinds":["transform"],"module":"../a.wasm"}"#,
        r#"{"id":"x","name":"X","version":"1","kinds":["bogus"]}"#,
    ] {
        assert!(PluginManifest::from_json(bad).is_err(), "{bad}");
    }
}

#[test]
fn transform_reverses_and_logs() {
    let p = all_kinds_plugin();
    let call = p.transform(b"hello").unwrap();
    assert_eq!(call.output, b"olleh");
    assert_eq!(call.logs, ["transform called"]);
    assert!(call.fuel_used > 0);
    assert_eq!(p.transform(b"").unwrap().output, b"");
    // Larger than the initial page: exercises memory.grow in hexa_alloc.
    let big: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
    let mut expected = big.clone();
    expected.reverse();
    assert_eq!(p.transform(&big).unwrap().output, expected);
}

#[test]
fn inspect_returns_rows() {
    let p = all_kinds_plugin();
    let call = p.inspect(&[1, 2, 3, 250]).unwrap();
    assert_eq!(
        call.output,
        [
            InspectRow {
                label: "sum".into(),
                value: "256".into()
            },
            InspectRow {
                label: "len".into(),
                value: "4".into()
            },
        ]
    );
}

#[test]
fn format_and_provide() {
    let p = all_kinds_plugin();
    assert_eq!(p.format(&[0xde, 0xad, 0x01]).unwrap().output, "dead01");
    assert_eq!(p.provide("mem://x").unwrap().output, b"mem://x!");
}

#[test]
fn module_validation() {
    // Declares inspector, but SPIN only exports transform.
    let err = Plugin::from_bytes(
        manifest("spin", &[PluginKind::Transform, PluginKind::Inspector]),
        &wasm(SPIN_WAT),
        PluginLimits::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, PluginError::InvalidModule(ref m) if m.contains("inspect")),
        "{err}"
    );

    let foreign = r#"(module (import "wasi" "fd_write" (func (param i32)))
        (memory (export "memory") 1)
        (func (export "hexa_alloc") (param i32) (result i32) (i32.const 0))
        (func (export "hexa_free") (param i32 i32))
        (func (export "transform") (param i32 i32) (result i64) (i64.const 0)))"#;
    assert!(matches!(
        Plugin::from_bytes(
            manifest("f", &[PluginKind::Transform]),
            &wasm(foreign),
            PluginLimits::default()
        ),
        Err(PluginError::InvalidModule(_))
    ));
    let wrong_sig = r#"(module (memory (export "memory") 1)
        (func (export "hexa_alloc") (param i32) (result i32) (i32.const 0))
        (func (export "hexa_free") (param i32 i32))
        (func (export "transform") (param i32) (result i32) (i32.const 0)))"#;
    assert!(matches!(
        Plugin::from_bytes(
            manifest("w", &[PluginKind::Transform]),
            &wasm(wrong_sig),
            PluginLimits::default()
        ),
        Err(PluginError::InvalidModule(_))
    ));
    assert!(matches!(
        Plugin::from_bytes(
            manifest("g", &[PluginKind::Transform]),
            b"garbage",
            PluginLimits::default()
        ),
        Err(PluginError::InvalidModule(_))
    ));

    let spin = Plugin::from_bytes(
        manifest("spin", &[PluginKind::Transform]),
        &wasm(SPIN_WAT),
        PluginLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        spin.format(b"x"),
        Err(PluginError::UnsupportedKind(PluginKind::Formatter))
    ));
}

#[test]
fn fuel_timeout_and_cancel() {
    let spin = |limits| {
        Plugin::from_bytes(
            manifest("spin", &[PluginKind::Transform]),
            &wasm(SPIN_WAT),
            limits,
        )
        .unwrap()
    };
    let p = spin(PluginLimits {
        fuel: 5_000_000,
        timeout: Duration::from_secs(60),
        ..PluginLimits::default()
    });
    assert!(matches!(
        p.transform(b"x"),
        Err(PluginError::FuelExhausted { fuel: 5_000_000 })
    ));

    let p = spin(PluginLimits {
        fuel: u64::MAX,
        timeout: Duration::from_millis(50),
        ..PluginLimits::default()
    });
    assert!(matches!(
        p.transform(b"x"),
        Err(PluginError::Timeout { millis: 50 })
    ));

    let p = spin(PluginLimits {
        fuel: u64::MAX,
        timeout: Duration::from_secs(60),
        ..PluginLimits::default()
    });
    let token = p.cancel_token();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        token.cancel();
    });
    assert!(matches!(p.transform(b"x"), Err(PluginError::Cancelled)));
    t.join().unwrap();
}

#[test]
fn memory_limits() {
    let limits = PluginLimits {
        max_memory_pages: 16,
        ..PluginLimits::default()
    };
    let grow = Plugin::from_bytes(
        manifest("grow", &[PluginKind::Transform]),
        &wasm(GROW_WAT),
        limits,
    )
    .unwrap();
    assert!(matches!(grow.transform(b"x"), Err(PluginError::Trap(_))));

    let huge = r#"(module (memory (export "memory") 64)
        (func (export "hexa_alloc") (param i32) (result i32) (i32.const 0))
        (func (export "hexa_free") (param i32 i32))
        (func (export "transform") (param i32 i32) (result i64) (i64.const 0)))"#;
    let p = Plugin::from_bytes(
        manifest("huge", &[PluginKind::Transform]),
        &wasm(huge),
        limits,
    )
    .unwrap();
    assert!(matches!(
        p.transform(b"x"),
        Err(PluginError::Instantiate(_))
    ));

    let all = Plugin::from_bytes(
        manifest("all", &[PluginKind::Transform]),
        &wasm(ALL_KINDS_WAT),
        limits,
    )
    .unwrap();
    assert!(matches!(
        all.transform(&vec![0; 2_000_000]),
        Err(PluginError::Trap(_))
    ));

    let bad = Plugin::from_bytes(
        manifest("bad", &[PluginKind::Transform]),
        &wasm(BAD_OUTPUT_WAT),
        PluginLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        bad.transform(b"x"),
        Err(PluginError::BadOutput(_))
    ));
}

fn write_plugin(root: &Path, dir: &str, m: &PluginManifest, wasm: &[u8]) {
    let d = root.join(dir);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("manifest.json"), m.to_json()).unwrap();
    std::fs::write(d.join(&m.module), wasm).unwrap();
}

#[test]
fn registry_loads_directory() {
    let root = tempfile::tempdir().unwrap();
    write_plugin(
        root.path(),
        "b",
        &manifest("b.rev", &PluginKind::ALL),
        &wasm(ALL_KINDS_WAT),
    );
    write_plugin(
        root.path(),
        "a",
        &manifest("a.spin", &[PluginKind::Transform]),
        &wasm(SPIN_WAT),
    );
    write_plugin(
        root.path(),
        "dup",
        &manifest("b.rev", &[PluginKind::Transform]),
        &wasm(SPIN_WAT),
    );
    std::fs::create_dir_all(root.path().join("broken")).unwrap();
    std::fs::write(root.path().join("broken/manifest.json"), "{nope").unwrap();
    std::fs::create_dir_all(root.path().join(".hidden")).unwrap();

    let (reg, failures) = PluginRegistry::load_dir(root.path(), PluginLimits::default());
    let ids: Vec<_> = reg
        .plugins()
        .iter()
        .map(|p| p.manifest().id.as_str())
        .collect();
    assert_eq!(ids, ["a.spin", "b.rev"]);
    assert_eq!(failures.len(), 2, "{failures:?}");
    assert!(failures.iter().any(|f| f.path.ends_with("broken")));
    assert!(failures.iter().any(|f| f.error.contains("duplicate")));
    assert_eq!(reg.by_kind(PluginKind::Inspector).count(), 1);
    let p = reg.get("b.rev").unwrap();
    assert_eq!(p.dir(), Some(root.path().join("b").as_path()));
    assert_eq!(p.transform(b"ab").unwrap().output, b"ba");
    assert_eq!(reg.manifests().len(), 2);

    let (empty, failures) =
        PluginRegistry::load_dir(&root.path().join("missing"), PluginLimits::default());
    assert!(empty.plugins().is_empty() && failures.is_empty());
}

// ---------------------------------------------------------------- SCR-05

fn catalog_json(plugin_sha: &str, template_sha: &str) -> String {
    serde_json::json!({
        "schemaVersion": 1,
        "plugins": [{
            "id": "rev", "name": "Reverse", "version": "1.2.0",
            "url": "https://example.invalid/rev.wasm", "sha256": plugin_sha,
            "kinds": ["transform", "inspector", "formatter", "provider"]
        }],
        "templates": [{
            "id": "png", "name": "PNG", "version": "1",
            "url": "https://example.invalid/t/png.hexpat?v=1", "sha256": template_sha
        }]
    })
    .to_string()
}

#[test]
fn catalog_parse_and_validate() {
    let bytes = wasm(ALL_KINDS_WAT);
    let sha = sha256_hex(&bytes);
    let cat = Catalog::from_json(&catalog_json(&sha, &sha)).unwrap();
    assert_eq!(cat.plugin("rev").unwrap().kinds.len(), 4);
    assert_eq!(
        cat.template("png").unwrap().target_file_name().unwrap(),
        "png.hexpat"
    );
    let again = Catalog::from_json(&cat.to_json().unwrap()).unwrap();
    assert_eq!(again, cat);
    assert!(matches!(
        Catalog::from_json(&catalog_json("abc", &sha)),
        Err(CatalogError::Invalid(_))
    ));
    let dup = serde_json::json!({"plugins": [
        {"id":"a","name":"A","version":"1","url":"u","sha256":sha,"kinds":["transform"]},
        {"id":"a","name":"A","version":"1","url":"u","sha256":sha,"kinds":["transform"]}
    ]});
    assert!(Catalog::from_json(&dup.to_string()).is_err());
    assert!(Catalog::from_json(r#"{"schemaVersion": 99}"#).is_err());
    assert_eq!(Catalog::from_json("{}").unwrap().plugins, Vec::new());
}

#[test]
fn sha256_verification() {
    let sha = sha256_hex(b"abc");
    assert_eq!(
        sha,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    verify_sha256(b"abc", &sha.to_uppercase()).unwrap();
    assert!(matches!(
        verify_sha256(b"abd", &sha),
        Err(CatalogError::ChecksumMismatch { .. })
    ));
}

#[test]
fn catalog_install_plugin_and_template() {
    let v1 = wasm(ALL_KINDS_WAT);
    let sha = sha256_hex(&v1);
    let template = b"struct Png {};".to_vec();
    let cat = Catalog::from_json(&catalog_json(&sha, &sha256_hex(&template))).unwrap();
    let root = tempfile::tempdir().unwrap();
    let plugins = root.path().join("plugins");

    assert!(matches!(
        cat.install_plugin("rev", b"tampered", &plugins),
        Err(CatalogError::ChecksumMismatch { .. })
    ));
    assert!(!plugins.join("rev").exists());

    let dir = cat.install_plugin("rev", &v1, &plugins).unwrap();
    assert_eq!(dir, plugins.join("rev"));
    let loaded = Plugin::load_dir(&dir, PluginLimits::default()).unwrap();
    assert_eq!(loaded.manifest().version, "1.2.0");
    assert_eq!(loaded.transform(b"xyz").unwrap().output, b"zyx");
    // Reinstall replaces atomically and leaves no staging directories.
    cat.install_plugin("rev", &v1, &plugins).unwrap();
    let names: Vec<_> = std::fs::read_dir(&plugins)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["rev"]);
    let (reg, failures) = PluginRegistry::load_dir(&plugins, PluginLimits::default());
    assert_eq!(failures, Vec::new());
    assert_eq!(reg.plugins().len(), 1);

    // A module whose hash matches but which lacks declared exports is rejected.
    let spin = wasm(SPIN_WAT);
    let mut bad = cat.clone();
    bad.plugins[0].sha256 = sha256_hex(&spin);
    assert!(matches!(
        bad.install_plugin("rev", &spin, &root.path().join("other")),
        Err(CatalogError::Plugin(PluginError::InvalidModule(_)))
    ));

    let templates = root.path().join("templates");
    let path = cat.install_template("png", &template, &templates).unwrap();
    assert_eq!(path, templates.join("png.hexpat"));
    assert_eq!(std::fs::read(&path).unwrap(), template);
    assert!(matches!(
        cat.install_template("nope", &template, &templates),
        Err(CatalogError::NotFound(_))
    ));
}
