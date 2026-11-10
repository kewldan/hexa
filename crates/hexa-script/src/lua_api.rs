//! Registration of the `hexa` Lua table mirroring [`ScriptHost`] (SCR-01).

use std::sync::{Arc, Mutex};

use mlua::{FromLuaMulti, IntoLuaMulti, Lua, LuaSerdeExt, LuaString, MultiValue, Table, Value};

use crate::engine::OutputBuffer;
use crate::host::{ByteRange, DocId, DocumentInfo, FindQuery, LogLevel, QueryKind, ScriptHost};
use crate::sandbox::lock;

/// Names of all entries of the `hexa` table, used for documentation and REPL
/// completion (SCR-02).
pub const API_NAMES: &[&str] = &[
    "active",
    "array",
    "clear_selection",
    "command",
    "confirm",
    "cursor",
    "delete",
    "documents",
    "find_all",
    "find_next",
    "go_to",
    "hash",
    "hex",
    "insert",
    "inspect",
    "len",
    "log",
    "message",
    "null",
    "open",
    "print",
    "prompt",
    "read",
    "redo",
    "replace",
    "save",
    "select",
    "selection",
    "set_cursor",
    "template",
    "transform",
    "undo",
    "unhex",
    "version",
    "write",
];

/// Default `limit` of `hexa.find_all`.
pub const DEFAULT_FIND_LIMIT: usize = 10_000;

const DEFAULT_TITLE: &str = "Script";

fn host_err(message: String) -> mlua::Error {
    mlua::Error::RuntimeError(message)
}

trait HostResultExt<T> {
    fn lua(self) -> mlua::Result<T>;
}

impl<T> HostResultExt<T> for Result<T, String> {
    fn lua(self) -> mlua::Result<T> {
        self.map_err(host_err)
    }
}

fn resolve(host: &dyn ScriptHost, doc: Option<DocId>) -> mlua::Result<DocId> {
    doc.or_else(|| host.active_document())
        .ok_or_else(|| host_err("no active document".to_owned()))
}

/// Explicit `offset`/`len`, otherwise the selection, otherwise the whole document.
fn range_or_default(
    host: &dyn ScriptHost,
    doc: DocId,
    offset: Option<u64>,
    len: Option<u64>,
) -> mlua::Result<ByteRange> {
    if let Some(offset) = offset {
        let len = match len {
            Some(len) => len,
            None => host.len(doc).lua()?.saturating_sub(offset),
        };
        return Ok(ByteRange::new(offset, len));
    }
    if let Some(sel) = host.selection(doc).lua()? {
        return Ok(sel);
    }
    Ok(ByteRange::new(0, host.len(doc).lua()?))
}

fn range_table(lua: &Lua, range: ByteRange) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("offset", range.offset)?;
    t.set("len", range.len)?;
    Ok(t)
}

fn doc_table(lua: &Lua, doc: &DocumentInfo) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("id", doc.id)?;
    t.set("name", doc.name.as_str())?;
    t.set("path", doc.path.as_deref())?;
    t.set("len", doc.len)?;
    t.set("read_only", doc.read_only)?;
    t.set("modified", doc.modified)?;
    Ok(t)
}

/// `"text"` or `{text = "...", ignore_case = true}` or `{hex = "DE AD"}`.
fn parse_query(value: &Value) -> mlua::Result<FindQuery> {
    match value {
        Value::String(s) => Ok(FindQuery::text(s.to_str()?.to_owned())),
        Value::Table(t) => {
            let ignore_case = t.get::<Option<bool>>("ignore_case")?.unwrap_or(false);
            if let Some(hex) = t.get::<Option<String>>("hex")? {
                Ok(FindQuery {
                    kind: QueryKind::Hex,
                    pattern: hex,
                    ignore_case,
                })
            } else if let Some(text) = t.get::<Option<String>>("text")? {
                Ok(FindQuery {
                    kind: QueryKind::Text,
                    pattern: text,
                    ignore_case,
                })
            } else {
                Err(host_err(
                    "query table needs a `hex` or `text` field".to_owned(),
                ))
            }
        }
        other => Err(host_err(format!(
            "query must be a string or table, got {}",
            other.type_name()
        ))),
    }
}

fn to_json(lua: &Lua, value: Value) -> mlua::Result<serde_json::Value> {
    match value {
        Value::String(s) => {
            let text = s.to_str()?;
            serde_json::from_str(&text).map_err(|e| host_err(format!("invalid JSON: {e}")))
        }
        other => lua.from_value(other),
    }
}

fn add<A, R, F>(
    lua: &Lua,
    table: &Table,
    host: &Arc<dyn ScriptHost>,
    name: &str,
    f: F,
) -> mlua::Result<()>
where
    A: FromLuaMulti,
    R: IntoLuaMulti,
    F: Fn(&Lua, &dyn ScriptHost, A) -> mlua::Result<R> + Send + 'static,
{
    let host = Arc::clone(host);
    table.set(
        name,
        lua.create_function(move |lua, args| f(lua, host.as_ref(), args))?,
    )
}

fn print_args(lua: &Lua, args: &MultiValue) -> mlua::Result<String> {
    let tostring: mlua::Function = lua.globals().get("tostring")?;
    let mut line = String::new();
    for (i, v) in args.iter().enumerate() {
        if i > 0 {
            line.push('\t');
        }
        let s: LuaString = tostring.call(v.clone())?;
        line.push_str(&s.to_string_lossy());
    }
    Ok(line)
}

/// Installs the global `hexa` table and replaces `print`.
pub(crate) fn install(
    lua: &Lua,
    host: Arc<dyn ScriptHost>,
    output: Arc<Mutex<OutputBuffer>>,
) -> mlua::Result<()> {
    let hexa = lua.create_table()?;
    let h = &host;

    add(lua, &hexa, h, "documents", |lua, host, ()| {
        let t = lua.create_table()?;
        for doc in host.documents() {
            t.push(doc_table(lua, &doc)?)?;
        }
        Ok(t)
    })?;
    add(lua, &hexa, h, "active", |_, host, ()| {
        Ok(host.active_document())
    })?;
    add(
        lua,
        &hexa,
        h,
        "open",
        |_, host, (path, read_only): (String, Option<bool>)| {
            host.open_file(&path, read_only.unwrap_or(false)).lua()
        },
    )?;
    add(lua, &hexa, h, "len", |_, host, doc: Option<DocId>| {
        host.len(resolve(host, doc)?).lua()
    })?;
    add(
        lua,
        &hexa,
        h,
        "read",
        |lua, host, (offset, len, doc): (u64, u64, Option<DocId>)| {
            let bytes = host.read(resolve(host, doc)?, offset, len).lua()?;
            lua.create_string(bytes)
        },
    )?;
    add(
        lua,
        &hexa,
        h,
        "write",
        |_, host, (offset, data, doc): (u64, LuaString, Option<DocId>)| {
            host.write(resolve(host, doc)?, offset, &data.as_bytes())
                .lua()
        },
    )?;
    add(
        lua,
        &hexa,
        h,
        "insert",
        |_, host, (offset, data, doc): (u64, LuaString, Option<DocId>)| {
            host.insert(resolve(host, doc)?, offset, &data.as_bytes())
                .lua()
        },
    )?;
    add(
        lua,
        &hexa,
        h,
        "delete",
        |_, host, (offset, len, doc): (u64, u64, Option<DocId>)| {
            host.delete(resolve(host, doc)?, offset, len).lua()
        },
    )?;
    add(
        lua,
        &hexa,
        h,
        "replace",
        |_, host, (offset, len, data, doc): (u64, u64, LuaString, Option<DocId>)| {
            host.replace(resolve(host, doc)?, offset, len, &data.as_bytes())
                .lua()
        },
    )?;
    add(
        lua,
        &hexa,
        h,
        "selection",
        |lua, host, doc: Option<DocId>| {
            host.selection(resolve(host, doc)?)
                .lua()?
                .map(|r| range_table(lua, r))
                .transpose()
        },
    )?;
    add(
        lua,
        &hexa,
        h,
        "select",
        |_, host, (offset, len, doc): (u64, u64, Option<DocId>)| {
            host.set_selection(resolve(host, doc)?, Some(ByteRange::new(offset, len)))
                .lua()
        },
    )?;
    add(
        lua,
        &hexa,
        h,
        "clear_selection",
        |_, host, doc: Option<DocId>| host.set_selection(resolve(host, doc)?, None).lua(),
    )?;
    add(lua, &hexa, h, "cursor", |_, host, doc: Option<DocId>| {
        host.cursor(resolve(host, doc)?).lua()
    })?;
    add(
        lua,
        &hexa,
        h,
        "set_cursor",
        |_, host, (offset, doc): (u64, Option<DocId>)| {
            host.set_cursor(resolve(host, doc)?, offset).lua()
        },
    )?;
    add(
        lua,
        &hexa,
        h,
        "go_to",
        |_, host, (offset, doc): (u64, Option<DocId>)| host.goto(resolve(host, doc)?, offset).lua(),
    )?;
    add(
        lua,
        &hexa,
        h,
        "find_next",
        |lua, host, (query, from, doc): (Value, Option<u64>, Option<DocId>)| {
            let doc = resolve(host, doc)?;
            let query = parse_query(&query)?;
            let from = match from {
                Some(from) => from,
                None => host.cursor(doc).lua()?,
            };
            host.find_next(doc, &query, from)
                .lua()?
                .map(|r| range_table(lua, r))
                .transpose()
        },
    )?;
    add(
        lua,
        &hexa,
        h,
        "find_all",
        |lua, host, (query, limit, doc): (Value, Option<usize>, Option<DocId>)| {
            let doc = resolve(host, doc)?;
            let query = parse_query(&query)?;
            let matches = host
                .find_all(doc, &query, limit.unwrap_or(DEFAULT_FIND_LIMIT))
                .lua()?;
            let t = lua.create_table_with_capacity(matches.len(), 0)?;
            for m in matches {
                t.push(range_table(lua, m)?)?;
            }
            Ok(t)
        },
    )?;
    add(
        lua,
        &hexa,
        h,
        "hash",
        |_,
         host,
         (algorithm, offset, len, doc): (String, Option<u64>, Option<u64>, Option<DocId>)| {
            let doc = resolve(host, doc)?;
            let range = range_or_default(host, doc, offset, len)?;
            host.hash(doc, range, &algorithm).lua()
        },
    )?;
    add(
        lua,
        &hexa,
        h,
        "inspect",
        |_, host, (offset, value_type, doc): (u64, String, Option<DocId>)| {
            host.inspect(resolve(host, doc)?, offset, &value_type).lua()
        },
    )?;
    add(
        lua,
        &hexa,
        h,
        "template",
        |lua, host, (name, offset, doc): (String, Option<u64>, Option<DocId>)| {
            let doc = resolve(host, doc)?;
            let json = host.apply_template(doc, &name, offset.unwrap_or(0)).lua()?;
            let value: serde_json::Value = serde_json::from_str(&json)
                .map_err(|e| host_err(format!("template returned invalid JSON: {e}")))?;
            lua.to_value(&value)
        },
    )?;
    add(
        lua,
        &hexa,
        h,
        "transform",
        |lua,
         host,
         (recipe, offset, len, doc): (Value, Option<u64>, Option<u64>, Option<DocId>)| {
            let doc = resolve(host, doc)?;
            let recipe = to_json(lua, recipe)?;
            let range = range_or_default(host, doc, offset, len)?;
            let bytes = host.transform(doc, &recipe.to_string(), range).lua()?;
            lua.create_string(bytes)
        },
    )?;
    add(lua, &hexa, h, "undo", |_, host, doc: Option<DocId>| {
        host.undo(resolve(host, doc)?).lua()
    })?;
    add(lua, &hexa, h, "redo", |_, host, doc: Option<DocId>| {
        host.redo(resolve(host, doc)?).lua()
    })?;
    add(lua, &hexa, h, "save", |_, host, doc: Option<DocId>| {
        host.save(resolve(host, doc)?).lua()
    })?;
    add(
        lua,
        &hexa,
        h,
        "command",
        |lua, host, (command, args): (String, Option<Value>)| {
            let args = match args {
                None | Some(Value::Nil) => serde_json::Value::Null,
                Some(v) => lua.from_value(v)?,
            };
            host.run_command(&command, &args).lua()
        },
    )?;
    add(
        lua,
        &hexa,
        h,
        "log",
        |_, host, (message, level): (String, Option<String>)| {
            let level = match level {
                None => LogLevel::Info,
                Some(l) => LogLevel::parse(&l)
                    .ok_or_else(|| host_err(format!("unknown log level {l:?}")))?,
            };
            host.log(level, &message);
            Ok(())
        },
    )?;
    add(
        lua,
        &hexa,
        h,
        "prompt",
        |_, host, (message, default, title): (String, Option<String>, Option<String>)| {
            Ok(host.prompt(
                title.as_deref().unwrap_or(DEFAULT_TITLE),
                &message,
                default.as_deref().unwrap_or(""),
            ))
        },
    )?;
    add(
        lua,
        &hexa,
        h,
        "confirm",
        |_, host, (message, title): (String, Option<String>)| {
            Ok(host.confirm(title.as_deref().unwrap_or(DEFAULT_TITLE), &message))
        },
    )?;
    add(
        lua,
        &hexa,
        h,
        "message",
        |_, host, (message, title): (String, Option<String>)| {
            host.message(title.as_deref().unwrap_or(DEFAULT_TITLE), &message);
            Ok(())
        },
    )?;
    hexa.set(
        "hex",
        lua.create_function(|_, data: LuaString| Ok(hex::encode(&*data.as_bytes())))?,
    )?;
    hexa.set(
        "unhex",
        lua.create_function(|lua, text: String| {
            let bytes = crate::host::parse_hex(&text).map_err(host_err)?;
            lua.create_string(bytes)
        })?,
    )?;
    hexa.set(
        "array",
        lua.create_function(|lua, t: Option<Table>| {
            let t = match t {
                Some(t) => t,
                None => lua.create_table()?,
            };
            t.set_metatable(Some(lua.array_metatable()))?;
            Ok(t)
        })?,
    )?;
    let print = lua.create_function(move |lua, args: MultiValue| {
        let line = print_args(lua, &args)?;
        host.print(&line);
        lock(&output).push_line(&line);
        Ok(())
    })?;
    lua.globals().set("print", print.clone())?;
    hexa.set("print", print)?;
    hexa.set("null", Value::NULL)?;
    hexa.set("version", env!("CARGO_PKG_VERSION"))?;
    lua.globals().set("hexa", hexa)
}
