//! SCR-01: Lua engine, `hexa` API and sandbox limits.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::MemoryHost;
use hexa_script::{API_NAMES, LogLevel, ScriptEngine, ScriptError, ScriptHost, ScriptLimits};

fn engine(host: &Arc<MemoryHost>) -> ScriptEngine {
    ScriptEngine::new(host.clone() as Arc<dyn ScriptHost>, ScriptLimits::default()).unwrap()
}

fn ok(engine: &ScriptEngine, src: &str) -> Vec<String> {
    let out = engine.run(src, "test");
    assert!(out.error.is_none(), "unexpected error: {:?}", out.error);
    out.values
}

#[test]
fn read_write_insert_delete_replace() {
    let host = MemoryHost::with_data(b"Hello, world");
    let e = engine(&host);
    assert_eq!(ok(&e, "return hexa.len()"), ["12"]);
    assert_eq!(ok(&e, "return hexa.read(0, 5)"), ["\"Hello\""]);
    assert_eq!(ok(&e, "return #hexa.read(10, 100)"), ["2"]);
    ok(&e, "hexa.write(0, 'J')");
    ok(&e, "hexa.insert(5, '!!')");
    ok(&e, "hexa.delete(0, 1)");
    ok(&e, "hexa.replace(0, 4, '\\0\\255')");
    assert_eq!(host.data(1), b"\0\xff!!, world");
    assert_eq!(ok(&e, "return hexa.read(0, 2)"), ["\"\\x00\\xff\""]);
    assert_eq!(ok(&e, "return hexa.undo(), hexa.redo()"), ["true", "true"]);
}

#[test]
fn byte_strings_and_hex_helpers() {
    let host = MemoryHost::with_data(&[0xde, 0xad, 0xbe, 0xef]);
    let e = engine(&host);
    assert_eq!(ok(&e, "return hexa.hex(hexa.read(0, 4))"), ["\"deadbeef\""]);
    assert_eq!(
        ok(&e, "return string.byte(hexa.unhex('0xCA fe'), 1, -1)"),
        ["202", "254"]
    );
    assert_eq!(
        ok(&e, "return string.unpack('<I4', hexa.read(0, 4))"),
        ["4022250974", "5"]
    );
}

#[test]
fn selection_cursor_goto_and_find() {
    let host = MemoryHost::with_data(b"abcABCabc");
    let e = engine(&host);
    assert_eq!(ok(&e, "return hexa.selection()"), ["nil"]);
    ok(&e, "hexa.select(3, 3)");
    assert_eq!(ok(&e, "return hexa.selection()"), ["{len = 3, offset = 3}"]);
    ok(&e, "hexa.clear_selection()");
    ok(&e, "hexa.go_to(4)");
    assert_eq!(ok(&e, "return hexa.cursor()"), ["4"]);
    ok(&e, "hexa.set_cursor(0)");
    assert_eq!(
        ok(
            &e,
            "local r = hexa.find_next('abc', 1) return r.offset, r.len"
        ),
        ["6", "3"]
    );
    assert_eq!(
        ok(
            &e,
            "return hexa.find_next({text = 'ABC', ignore_case = true}).offset"
        ),
        ["0"]
    );
    assert_eq!(ok(&e, "return #hexa.find_all({hex = '61 62'})"), ["2"]);
    assert_eq!(ok(&e, "return #hexa.find_all('b', 1)"), ["1"]);
    let out = e.run("hexa.go_to(100)", "test");
    assert!(matches!(
        out.error,
        Some(ScriptError::Runtime { line: Some(1), .. })
    ));
}

#[test]
fn hash_inspect_template_transform() {
    let host = MemoryHost::with_data(&[1, 2, 3, 4]);
    let e = engine(&host);
    assert_eq!(ok(&e, "return hexa.hash('sum8')"), ["\"0a\""]);
    assert_eq!(ok(&e, "return hexa.hash('sum8', 2)"), ["\"07\""]);
    ok(&e, "hexa.select(0, 2)");
    assert_eq!(ok(&e, "return hexa.hash('sum8')"), ["\"03\""]);
    assert_eq!(ok(&e, "return hexa.inspect(0, 'u32le')"), ["\"67305985\""]);
    assert_eq!(
        ok(
            &e,
            "local t = hexa.template('header') return t.name, t.fields[1].value"
        ),
        ["\"header\"", "\"01020304\""]
    );
    assert_eq!(
        ok(
            &e,
            "return hexa.hex(hexa.transform({op = 'xor', key = 255}, 0, 4))"
        ),
        ["\"fefdfcfb\""]
    );
    assert_eq!(
        ok(
            &e,
            "return hexa.hex(hexa.transform('{\"op\":\"reverse\"}', 0))"
        ),
        ["\"04030201\""]
    );
    assert_eq!(host.data(1), [1, 2, 3, 4], "transform must not modify");
}

#[test]
fn documents_open_and_explicit_doc() {
    let host = MemoryHost::with_data(b"first");
    host.add_doc("second.bin", b"second!");
    let e = engine(&host);
    assert_eq!(
        ok(
            &e,
            "local d = hexa.documents() return #d, d[2].name, d[2].len, hexa.active()"
        ),
        ["2", "\"second.bin\"", "7", "1"]
    );
    assert_eq!(ok(&e, "return hexa.read(0, 3, 2)"), ["\"sec\""]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.bin");
    std::fs::write(&path, b"xyz").unwrap();
    let src = format!(
        "local id = hexa.open({:?}) return id, hexa.len(id)",
        path.to_str().unwrap()
    );
    assert_eq!(ok(&e, &src), ["3", "3"]);
}

#[test]
fn print_log_dialogs_and_commands() {
    let host = MemoryHost::with_data(b"");
    host.lock().prompts.push_back(Some("42".into()));
    let e = engine(&host);
    let out = e.run(
        "print('a', 1, nil) hexa.print('b') hexa.log('careful', 'warn') \
         local v = hexa.prompt('n?') hexa.message('got ' .. v, 'T') \
         hexa.command('edit.fill', {value = 255, range = hexa.array(), extra = hexa.null}) \
         return hexa.confirm('sure?')",
        "test",
    );
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output, "a\t1\tnil\nb\n");
    assert_eq!(out.values, ["true"]);
    let st = host.lock();
    assert_eq!(st.printed, ["a\t1\tnil", "b"]);
    assert_eq!(st.logs, [(LogLevel::Warn, "careful".to_owned())]);
    assert!(st.messages.contains(&"T:got 42".to_owned()));
    assert_eq!(
        st.commands,
        [(
            "edit.fill".to_owned(),
            serde_json::json!({"value": 255, "range": [], "extra": null})
        )]
    );
}

#[test]
fn globals_persist_between_runs() {
    let host = MemoryHost::with_data(b"");
    let e = engine(&host);
    ok(
        &e,
        "function greet(name) return 'hi ' .. name end counter = 1",
    );
    assert_eq!(ok(&e, "counter = counter + 1 return counter"), ["2"]);
    assert_eq!(e.call_global("greet", &["bob"]).values, ["\"hi bob\""]);
}

#[test]
fn errors_carry_lines() {
    let host = MemoryHost::with_data(b"");
    let e = engine(&host);
    let out = e.run("local x = 1\nlocal y = \nend", "my.lua");
    match out.error {
        Some(ScriptError::Syntax { line, .. }) => assert_eq!(line, Some(3)),
        other => panic!("{other:?}"),
    }
    let out = e.run("local a = 1\n\nerror('boom')", "my.lua");
    assert_eq!(
        out.error,
        Some(ScriptError::Runtime {
            message: "boom".into(),
            line: Some(3)
        })
    );
    let out = e.run("\nhexa.delete(0, 99)", "edit");
    match out.error {
        Some(ScriptError::Runtime { message, line }) => {
            assert!(message.contains("beyond end"), "{message}");
            assert_eq!(line, Some(2));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn sandbox_hides_unsafe_libraries() {
    let host = MemoryHost::with_data(b"");
    let e = engine(&host);
    assert_eq!(
        ok(
            &e,
            "return io, os, package, debug, require, dofile, loadfile, string.dump"
        ),
        ["nil"; 8]
    );
    assert_eq!(ok(&e, "return load('return 1 + 1')()"), ["2"]);
    assert_eq!(
        ok(
            &e,
            "return math.max(1, 5), utf8.char(65), table.concat({1, 2}, ',')"
        ),
        ["5", "\"A\"", "\"1,2\""]
    );
    let out = e.run("return load('\\27Lua')", "t");
    assert_eq!(out.values.first().map(String::as_str), Some("nil"));
    assert!(e.run("collectgarbage('stop')", "t").error.is_some());
}

#[test]
fn instruction_budget_is_enforced() {
    let host = MemoryHost::with_data(b"");
    let e = ScriptEngine::new(
        host as Arc<dyn ScriptHost>,
        ScriptLimits {
            instruction_budget: Some(100_000),
            timeout: None,
            ..ScriptLimits::default()
        },
    )
    .unwrap();
    let out = e.run("while true do end", "loop");
    assert_eq!(
        out.error,
        Some(ScriptError::BudgetExceeded { budget: 100_000 })
    );
    let out = e.run(
        "while true do pcall(function() for i = 1, 100 do end end) end",
        "loop",
    );
    assert_eq!(
        out.error,
        Some(ScriptError::BudgetExceeded { budget: 100_000 })
    );
    // The engine is reusable after an abort.
    assert_eq!(e.run("return 1", "t").values, ["1"]);
}

#[test]
fn timeout_is_enforced() {
    let host = MemoryHost::with_data(b"");
    let e = ScriptEngine::new(
        host as Arc<dyn ScriptHost>,
        ScriptLimits {
            instruction_budget: None,
            timeout: Some(Duration::from_millis(50)),
            ..ScriptLimits::default()
        },
    )
    .unwrap();
    let out = e.run("while true do end", "loop");
    assert_eq!(out.error, Some(ScriptError::Timeout { millis: 50 }));
}

#[test]
fn memory_limit_is_enforced() {
    let host = MemoryHost::with_data(b"");
    let e = ScriptEngine::new(
        host as Arc<dyn ScriptHost>,
        ScriptLimits {
            memory_bytes: 4 * 1024 * 1024,
            ..ScriptLimits::default()
        },
    )
    .unwrap();
    let out = e.run(
        "local t = {} for i = 1, 1e8 do t[i] = string.rep('x', 64) .. i end",
        "mem",
    );
    assert_eq!(
        out.error,
        Some(ScriptError::MemoryLimit {
            limit: 4 * 1024 * 1024
        })
    );
    assert_eq!(e.run("return 2", "t").values, ["2"]);
}

#[test]
fn cancellation_from_another_thread() {
    let host = MemoryHost::with_data(b"");
    let e = ScriptEngine::new(
        host as Arc<dyn ScriptHost>,
        ScriptLimits {
            instruction_budget: None,
            timeout: None,
            ..ScriptLimits::default()
        },
    )
    .unwrap();
    let token = e.cancel_token();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        token.cancel();
    });
    let out = e.run("while true do end", "loop");
    t.join().unwrap();
    assert_eq!(out.error, Some(ScriptError::Cancelled));
    e.cancel_token().reset();
    assert_eq!(e.run("return 3", "t").values, ["3"]);
}

#[test]
fn coroutines_are_budgeted_too() {
    let host = MemoryHost::with_data(b"");
    let e = ScriptEngine::new(
        host as Arc<dyn ScriptHost>,
        ScriptLimits {
            instruction_budget: Some(50_000),
            timeout: None,
            ..ScriptLimits::default()
        },
    )
    .unwrap();
    let out = e.run(
        "local co = coroutine.wrap(function() while true do end end) co()",
        "co",
    );
    assert!(out.error.is_some(), "coroutine escaped the budget");
}

#[test]
fn api_names_match_table() {
    let host = MemoryHost::with_data(b"");
    let e = engine(&host);
    let values = ok(
        &e,
        "local n = {} for k in pairs(hexa) do n[#n + 1] = k end table.sort(n) return table.concat(n, ',')",
    );
    let mut expected = API_NAMES.to_vec();
    expected.sort_unstable();
    assert_eq!(values, [format!("\"{}\"", expected.join(","))]);
}

#[test]
fn no_active_document_is_an_error() {
    let host = Arc::new(MemoryHost::default());
    let e = engine(&host);
    let out = e.run("return hexa.len()", "t");
    assert!(
        matches!(out.error, Some(ScriptError::Runtime { ref message, .. }) if message.contains("no active document"))
    );
}

#[test]
fn script_output_serializes_camel_case() {
    let host = MemoryHost::with_data(b"");
    let e = engine(&host);
    let out = e.run("error('x')", "t");
    let json = serde_json::to_value(&out).unwrap();
    assert_eq!(json["error"]["kind"], "runtime");
    assert_eq!(json["error"]["line"], 1);
}
