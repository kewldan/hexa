//! SCR-02 REPL, SCR-03 macros, SRCH-08 predicate search.

mod common;

use std::sync::Arc;

use common::MemoryHost;
use hexa_script::{
    Macro, MacroError, MacroRecorder, PredicateOptions, PredicateSearch, Repl, ScriptEngine,
    ScriptError, ScriptHost, ScriptLimits,
};

fn repl(host: &Arc<MemoryHost>) -> Repl {
    Repl::new(host.clone() as Arc<dyn ScriptHost>, ScriptLimits::default()).unwrap()
}

// ---------------------------------------------------------------- SCR-02

#[test]
fn repl_expressions_statements_and_state() {
    let host = MemoryHost::with_data(b"\x01\x02\x03");
    let mut r = repl(&host);
    assert_eq!(r.eval("1 + 2").values, ["3"]);
    assert_eq!(r.eval("=hexa.len()").values, ["3"]);
    let resp = r.eval("x = 10");
    assert!(resp.values.is_empty() && resp.error.is_none());
    assert_eq!(r.eval("x * 2").values, ["20"]);
    let resp = r.eval("print('hello') return 5, 'a'");
    assert_eq!(resp.output, "hello\n");
    assert_eq!(resp.values, ["5", "\"a\""]);
    assert_eq!(r.eval("{1, 2, k = 'v'}").values, ["{1, 2, k = \"v\"}"]);
    assert_eq!(
        r.eval("1.5, 2^53, 1e300, 0/0 ~= 0/0").values,
        ["1.5", "9007199254740992.0", "1e300", "true"]
    );
    assert_eq!(r.eval("   ").values, Vec::<String>::new());
}

#[test]
fn repl_multiline_and_errors() {
    let host = MemoryHost::with_data(b"");
    let mut r = repl(&host);
    let resp = r.eval("for i = 1, 3 do");
    assert!(resp.incomplete);
    assert!(r.is_pending());
    assert!(r.eval("  print(i)").incomplete);
    let resp = r.eval("end");
    assert!(!resp.incomplete);
    assert_eq!(resp.output, "1\n2\n3\n");
    assert!(!r.is_pending());

    let resp = r.eval("error('bad')");
    assert_eq!(
        resp.error,
        Some(ScriptError::Runtime {
            message: "bad".into(),
            line: Some(1)
        })
    );
    let resp = r.eval("local = 3");
    assert!(matches!(
        resp.error,
        Some(ScriptError::Syntax {
            line: Some(1),
            incomplete: false,
            ..
        })
    ));

    assert!(r.eval("function f()").incomplete);
    r.clear_pending();
    assert!(!r.is_pending());
}

#[test]
fn repl_completion() {
    let host = MemoryHost::with_data(b"");
    let mut r = repl(&host);
    assert_eq!(
        r.complete("x = hexa.re"),
        ["hexa.read", "hexa.redo", "hexa.replace"]
    );
    assert!(r.complete("hexa.").contains(&"hexa.find_all".to_owned()));
    assert_eq!(r.complete("str"), ["string"]);
    assert_eq!(r.complete("string.up"), ["string.upper"]);
    r.eval("myvar = {alpha = 1, beta = 2}");
    assert_eq!(r.complete("print(myvar.a"), ["myvar.alpha"]);
    assert_eq!(r.complete("nope.x"), Vec::<String>::new());
}

#[test]
fn repl_shares_engine() {
    let host = MemoryHost::with_data(b"");
    let engine = ScriptEngine::new(host as Arc<dyn ScriptHost>, ScriptLimits::default()).unwrap();
    engine.run("shared = 7", "init");
    let mut r = Repl::from_engine(engine);
    assert_eq!(r.eval("shared").values, ["7"]);
}

// ---------------------------------------------------------------- SCR-03

fn sample_macro() -> Macro {
    let mut m = Macro::new("Fill header");
    m.description = "zero the header\nthen jump".into();
    m.push(
        "edit.fill",
        serde_json::json!({"offset": 0, "len": 4, "value": 0}),
    );
    m.push("view.goto", serde_json::json!({"offset": 16}));
    m.push("edit.undo", serde_json::Value::Null);
    m.push(
        "search.find",
        serde_json::json!({"query": {"kind": "hex", "pattern": "DE AD"}, "flags": [], "x": null, "weird key": 1.5, "list": [1, "two", true]}),
    );
    m
}

#[test]
fn macro_recorder() {
    let mut rec = MacroRecorder::new();
    assert!(!rec.record("edit.cut", &serde_json::Value::Null));
    rec.start("m");
    assert!(rec.is_recording());
    assert!(rec.record("edit.cut", &serde_json::Value::Null));
    assert!(!rec.record("macro.stop", &serde_json::Value::Null));
    assert!(rec.record("edit.paste", &serde_json::json!({"offset": 3})));
    assert_eq!(rec.step_count(), 2);
    let m = rec.stop().unwrap();
    assert_eq!(m.steps.len(), 2);
    assert!(!rec.is_recording());
    rec.start("x");
    rec.cancel();
    assert!(rec.stop().is_none());
}

#[test]
fn macro_save_load_roundtrip() {
    let m = sample_macro();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.json");
    m.save(&path).unwrap();
    assert_eq!(Macro::load(&path).unwrap(), m);
    let json: serde_json::Value = serde_json::from_str(&m.to_json().unwrap()).unwrap();
    assert_eq!(json["steps"][0]["command"], "edit.fill");
    assert!(Macro::from_json("{").is_err());
    assert!(matches!(
        Macro::load(&dir.path().join("missing.json")),
        Err(MacroError::Io { .. })
    ));
}

#[test]
fn macro_replay_and_failure() {
    let host = MemoryHost::with_data(b"");
    let m = sample_macro();
    assert_eq!(m.replay_on(host.as_ref()).unwrap(), 4);
    assert_eq!(host.lock().commands.len(), 4);

    let mut broken = Macro::new("broken");
    broken.push("a", serde_json::Value::Null);
    broken.push("fail", serde_json::Value::Null);
    broken.push("b", serde_json::Value::Null);
    match broken.replay_on(host.as_ref()) {
        Err(MacroError::Step { index, command, .. }) => {
            assert_eq!((index, command.as_str()), (1, "fail"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn macro_to_lua_reproduces_commands() {
    let m = sample_macro();
    let script = m.to_lua();
    assert!(script.starts_with("-- Macro: Fill header\n-- zero the header\n-- then jump\n"));

    let replayed = MemoryHost::with_data(b"");
    m.replay_on(replayed.as_ref()).unwrap();

    let scripted = MemoryHost::with_data(b"");
    let engine = ScriptEngine::new(
        scripted.clone() as Arc<dyn ScriptHost>,
        ScriptLimits::default(),
    )
    .unwrap();
    let out = engine.run(&script, "macro");
    assert!(out.error.is_none(), "{:?}\n{script}", out.error);
    assert_eq!(scripted.lock().commands, replayed.lock().commands);
}

// ---------------------------------------------------------------- SRCH-08

fn search(src: &str, options: PredicateOptions) -> PredicateSearch {
    PredicateSearch::compile(src, options).unwrap()
}

#[test]
fn predicate_finds_across_chunk_boundaries() {
    let mut data = vec![0u8; 1000];
    for &pos in &[0usize, 7, 9, 511, 513, 998] {
        data[pos] = 0x4d;
        data[pos + 1] = 0x5a;
    }
    let src = "function(offset, bytes) return bytes:sub(1, 2) == 'MZ' end";
    for chunk in [1usize, 3, 64, 1000, 5000] {
        let mut s = search(src, PredicateOptions::default());
        let found = s
            .scan(0, data.len() as u64, chunk, |off, len| {
                Ok(data[off as usize..off as usize + len].to_vec())
            })
            .unwrap();
        assert_eq!(found, [0, 7, 9, 511, 513, 998], "chunk {chunk}");
    }
}

#[test]
fn predicate_window_alignment_and_tail() {
    let data: Vec<u8> = (0..32).collect();
    let mut s = search(
        "function(o, b) return #b < 4 or b:byte(1) % 2 == 0 end",
        PredicateOptions {
            window: 4,
            alignment: 4,
            ..PredicateOptions::default()
        },
    );
    let mut found = s.feed(100, &data[..10]).unwrap();
    found.extend(s.feed(110, &data[10..]).unwrap());
    found.extend(s.finish().unwrap());
    // Offsets 100..132 aligned to 4; byte at offset o is o - 100.
    assert_eq!(found, [100, 104, 108, 112, 116, 120, 124, 128]);

    let mut tail = search(
        "function(o, b) return #b < 3 end",
        PredicateOptions {
            window: 3,
            ..PredicateOptions::default()
        },
    );
    let mut found = tail.feed(0, b"abcdef").unwrap();
    found.extend(tail.finish().unwrap());
    assert_eq!(found, [4, 5]);
}

#[test]
fn predicate_chunk_forms_and_errors() {
    let mut s = search(
        "local function p(o, b) return o == 2 end return p",
        PredicateOptions::default(),
    );
    assert_eq!(
        s.scan(0, 5, 2, |o, l| Ok(vec![0; l.min(5 - o as usize)]))
            .unwrap(),
        [2]
    );

    assert!(matches!(
        PredicateSearch::compile("42", PredicateOptions::default()),
        Err(ScriptError::InvalidInput { .. })
    ));
    assert!(matches!(
        PredicateSearch::compile("function(o, b", PredicateOptions::default()),
        Err(ScriptError::Syntax { .. })
    ));
    let mut s = search(
        "function(o, b) error('nope') end",
        PredicateOptions::default(),
    );
    assert!(matches!(
        s.feed(0, &[0; 64]),
        Err(ScriptError::Runtime { line: Some(1), .. })
    ));
    let mut s = search(
        "function(o, b) return true end",
        PredicateOptions::default(),
    );
    s.feed(0, &[0; 4]).unwrap();
    assert!(matches!(
        s.feed(10, &[0; 4]),
        Err(ScriptError::InvalidInput { .. })
    ));
    assert!(
        PredicateSearch::compile(
            "function() end",
            PredicateOptions {
                window: 0,
                ..PredicateOptions::default()
            }
        )
        .is_err()
    );
    let mut s = search(
        "function(o, b) return io ~= nil or os ~= nil end",
        PredicateOptions::default(),
    );
    assert_eq!(
        s.scan(0, 4, 4, |_, l| Ok(vec![0; l])).unwrap(),
        Vec::<u64>::new()
    );
}

#[test]
fn predicate_limits() {
    let mut s = search(
        "function(o, b) return true end",
        PredicateOptions {
            max_matches: 3,
            ..PredicateOptions::default()
        },
    );
    let found = s.scan(0, 1000, 100, |_, l| Ok(vec![0; l])).unwrap();
    assert_eq!(found, [0, 1, 2]);
    assert!(s.is_limit_reached());

    let mut s = search(
        "function(o, b) for i = 1, 1000 do end return false end",
        PredicateOptions {
            limits: ScriptLimits {
                instruction_budget: Some(200_000),
                timeout: None,
                ..ScriptLimits::default()
            },
            ..PredicateOptions::default()
        },
    );
    assert_eq!(
        s.scan(0, 100_000, 4096, |_, l| Ok(vec![0; l])),
        Err(ScriptError::BudgetExceeded { budget: 200_000 })
    );

    let mut s = search(
        "function(o, b) return false end",
        PredicateOptions::default(),
    );
    s.cancel_token().cancel();
    assert_eq!(s.feed(0, &[0; 100]), Err(ScriptError::Cancelled));
}
