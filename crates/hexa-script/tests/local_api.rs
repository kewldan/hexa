//! SCR-09: local JSON API dispatcher.

mod common;

use std::sync::Arc;

use common::MemoryHost;
use hexa_script::{ApiConfig, ApiErrorCode, LocalApi, ScriptHost};
use serde_json::{Value, json};

const AUTH: Option<&str> = Some("Bearer s3cret");

fn setup(data: &[u8]) -> (LocalApi, Arc<MemoryHost>, Arc<dyn ScriptHost>) {
    let host = MemoryHost::with_data(data);
    let dyn_host: Arc<dyn ScriptHost> = host.clone();
    (LocalApi::new(ApiConfig::new("s3cret")), host, dyn_host)
}

fn call(api: &LocalApi, host: &Arc<dyn ScriptHost>, req: &Value) -> Value {
    serde_json::from_str(&api.handle(AUTH, &req.to_string(), host)).unwrap()
}

#[test]
fn bearer_token_is_required() {
    let (api, _, host) = setup(b"");
    let req = json!({"id": 1, "method": "version"}).to_string();
    for auth in [
        None,
        Some("Bearer wrong"),
        Some("Basic s3cret"),
        Some("s3cret"),
        Some("Bearer "),
    ] {
        let resp = api.handle_request(auth, &req, &host);
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().code, ApiErrorCode::Unauthorized);
    }
    let resp = api.handle_request(Some("bearer  s3cret "), &req, &host);
    assert!(resp.ok);
    assert_eq!(resp.http_status(), 200);
    assert_eq!(resp.id, json!(1));

    let open = LocalApi::new(ApiConfig::new(""));
    assert!(!open.is_authorized(Some("Bearer ")));
    assert_eq!(
        open.handle_request(Some("Bearer "), &req, &host)
            .http_status(),
        401
    );
}

#[test]
fn version_and_bad_requests() {
    let (api, _, host) = setup(b"");
    let v = call(&api, &host, &json!({"id": "a", "method": "version"}));
    assert_eq!(v["ok"], true);
    assert_eq!(v["result"]["apiVersion"], 1);

    let resp = api.handle_request(AUTH, "not json", &host);
    assert_eq!(resp.error.unwrap().code, ApiErrorCode::BadRequest);
    let resp = api.handle_request(AUTH, r#"{"id": 7, "method": "explode"}"#, &host);
    assert_eq!(resp.id, json!(7));
    assert_eq!(resp.http_status(), 400);
    let resp = api.handle_request(
        AUTH,
        r#"{"id": 8, "method": "readRange", "params": {}}"#,
        &host,
    );
    assert_eq!(resp.http_status(), 400);
}

#[test]
fn documents_read_write_goto_find() {
    let (api, mem, host) = setup(b"hello hello");
    let docs = call(&api, &host, &json!({"id": 1, "method": "listDocuments"}));
    assert_eq!(docs["result"][0]["name"], "test.bin");
    assert_eq!(docs["result"][0]["readOnly"], false);

    let read = call(
        &api,
        &host,
        &json!({"id": 2, "method": "readRange", "params": {"offset": 0, "len": 5}}),
    );
    assert_eq!(read["result"]["data"], hex::encode("hello"));
    assert_eq!(read["result"]["doc"], 1);

    let w = call(
        &api,
        &host,
        &json!({"id": 3, "method": "write", "params": {"doc": 1, "offset": 0, "data": "48 45"}}),
    );
    assert_eq!(w["result"]["len"], 2);
    let ins = call(
        &api,
        &host,
        &json!({"id": 4, "method": "write", "params": {"offset": 0, "data": "3e", "mode": "insert"}}),
    );
    assert_eq!(ins["ok"], true);
    assert_eq!(mem.data(1), b">HEllo hello");

    let g = call(
        &api,
        &host,
        &json!({"id": 5, "method": "goto", "params": {"offset": 3}}),
    );
    assert_eq!(g["ok"], true);
    assert_eq!(mem.lock().docs[0].cursor, 3);
    let g = call(
        &api,
        &host,
        &json!({"id": 6, "method": "goto", "params": {"offset": 999}}),
    );
    assert_eq!(g["error"]["code"], "hostError");

    let f = call(
        &api,
        &host,
        &json!({"id": 7, "method": "find", "params": {"query": {"pattern": "llo"}, "from": 5}}),
    );
    assert_eq!(f["result"]["matches"], json!([{"offset": 9, "len": 3}]));
    let f = call(
        &api,
        &host,
        &json!({"id": 8, "method": "find", "params": {"query": {"kind": "hex", "pattern": "6c6c"}, "all": true}}),
    );
    assert_eq!(f["result"]["matches"].as_array().unwrap().len(), 2);
}

#[test]
fn open_file() {
    let (api, _, host) = setup(b"");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.bin");
    std::fs::write(&path, b"abc").unwrap();
    let r = call(
        &api,
        &host,
        &json!({"id": 1, "method": "openFile", "params": {"path": path, "readOnly": true}}),
    );
    assert_eq!(r["result"]["doc"], 2);
    let r = call(
        &api,
        &host,
        &json!({"id": 2, "method": "openFile", "params": {"path": dir.path().join("missing")}}),
    );
    assert_eq!(r["error"]["code"], "hostError");
}

#[test]
fn limits_and_permissions() {
    let host = MemoryHost::with_data(b"abc");
    let dyn_host: Arc<dyn ScriptHost> = host;
    let mut config = ApiConfig::new("s3cret");
    config.max_read_len = 2;
    config.allow_write = false;
    config.allow_scripts = false;
    let api = LocalApi::new(config);
    let r = api.handle_request(
        AUTH,
        &json!({"method": "readRange", "params": {"offset": 0, "len": 3}}).to_string(),
        &dyn_host,
    );
    assert_eq!(r.http_status(), 413);
    let r = api.handle_request(
        AUTH,
        &json!({"method": "write", "params": {"offset": 0, "data": "00"}}).to_string(),
        &dyn_host,
    );
    assert_eq!(r.http_status(), 403);
    let r = api.handle_request(
        AUTH,
        &json!({"method": "runScript", "params": {"source": "return 1"}}).to_string(),
        &dyn_host,
    );
    assert_eq!(r.http_status(), 403);
    let (api, _, host) = setup(b"");
    let r = call(
        &api,
        &host,
        &json!({"method": "write", "params": {"offset": 0, "data": "zz"}}),
    );
    assert_eq!(r["error"]["code"], "badRequest");
}

#[test]
fn run_script() {
    let (api, mem, host) = setup(b"\x01\x02");
    let r = call(
        &api,
        &host,
        &json!({"id": 1, "method": "runScript", "params": {"source": "print(hexa.len()) hexa.write(0, 'Z') return 'done'"}}),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["result"]["output"], "2\n");
    assert_eq!(r["result"]["values"], json!(["\"done\""]));
    assert_eq!(mem.data(1), b"Z\x02");

    let r = call(
        &api,
        &host,
        &json!({"id": 2, "method": "runScript", "params": {"source": "print('x')\nerror('bad')", "name": "job"}}),
    );
    assert_eq!(r["ok"], false);
    assert_eq!(r["error"]["code"], "scriptError");
    assert_eq!(r["result"]["output"], "x\n");
    assert_eq!(r["result"]["error"]["line"], 2);
}
