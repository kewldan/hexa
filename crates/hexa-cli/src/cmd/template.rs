//! `template`: apply a structure template and print the tree as JSON.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use hexa_core::Cancel;
use hexa_template::{
    Dialect, EvalOptions, Node, NodeKind, ParseOptions, Program, TemplateResult, Value as TValue,
    children, expand_element, library, parse_strict,
};
use serde_json::{Value, json};

use super::name_of;
use crate::error::{CliError, CliResult, Outcome};
use crate::io::{self, open_tree, parse_u64, read_text};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DialectArg {
    /// Native / `ImHex` pattern language.
    Hexpat,
    /// 010 Editor binary template.
    Bt,
    /// C header (use with --type).
    C,
}

#[derive(Debug, Args)]
pub struct TemplateArgs {
    /// Input file (`-` = stdin).
    #[arg(required_unless_present = "list")]
    pub file: Option<PathBuf>,
    /// Template file, or a built-in library id (png, zip, elf, pe …);
    /// default: detected from the data.
    pub template: Option<String>,
    /// List the built-in templates and exit.
    #[arg(long)]
    pub list: bool,
    /// Template dialect (default: from the extension: .bt, .h, else hexpat).
    #[arg(long, value_enum)]
    pub dialect: Option<DialectArg>,
    /// Read this type at --base-offset instead of running the template's
    /// top-level statements (e.g. a struct from a C header).
    #[arg(long = "type", value_name = "NAME")]
    pub root_type: Option<String>,
    /// Initial value of `$`.
    #[arg(long, value_parser = parse_u64, default_value = "0")]
    pub base_offset: u64,
    /// Elements emitted per array (larger arrays are cut, `truncated` set).
    #[arg(long, default_value_t = 1000)]
    pub max_elements: u64,
    /// Include hidden nodes.
    #[arg(long)]
    pub hidden: bool,
    /// Also list the elements of primitive arrays (their formatted value
    /// is always included).
    #[arg(long)]
    pub elements: bool,
}

fn dialect_of(a: &TemplateArgs, path: &Path) -> Dialect {
    match a.dialect {
        Some(DialectArg::Hexpat) => Dialect::Hexpat,
        Some(DialectArg::Bt) => Dialect::Bt,
        Some(DialectArg::C) => Dialect::C,
        None => match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("bt") => Dialect::Bt,
            Some("h" | "hpp") => Dialect::C,
            _ => Dialect::Hexpat,
        },
    }
}

/// Loads the template program; returns it with a display name.
fn load_program(a: &TemplateArgs, head: &[u8], file: &Path) -> CliResult<(Program, String)> {
    let spec = if let Some(t) = &a.template {
        t.clone()
    } else {
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned());
        let found = library::detect(head, name.as_deref());
        let id = found
            .first()
            .ok_or_else(|| CliError::data("no built-in template matches this file"))?;
        (*id).to_owned()
    };
    let path = Path::new(&spec);
    if path.is_file() {
        let src = read_text(path)?;
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let resolve = move |name: &str| -> Option<String> {
            std::fs::read_to_string(dir.join(name))
                .ok()
                .or_else(|| library::get(name).map(|e| e.source.to_owned()))
        };
        let opts = ParseOptions {
            dialect: dialect_of(a, path),
            resolver: Some(&resolve),
            ..ParseOptions::default()
        };
        let program = parse_strict(&src, &opts).map_err(|e| CliError::data(e.to_string()))?;
        return Ok((program, path.display().to_string()));
    }
    if library::get(&spec).is_some() {
        return Ok((library::program(&spec)?, format!("library:{spec}")));
    }
    Err(CliError::usage(format!(
        "template `{spec}` is neither a file nor a built-in template (see --list)"
    )))
}

struct Render<'a> {
    max: u64,
    hidden: bool,
    elements: bool,
    result: &'a TemplateResult,
}

fn raw_json(v: &TValue) -> Value {
    match v {
        TValue::Int(i) => i64::try_from(*i)
            .map(Value::from)
            .or_else(|_| u64::try_from(*i).map(Value::from))
            .unwrap_or_else(|_| Value::String(i.to_string())),
        TValue::Float(f) => json!(f),
        TValue::Bool(b) => json!(b),
        TValue::Str(s) => json!(s),
    }
}

impl Render<'_> {
    fn node(&self, n: &Node) -> Value {
        let mut o = serde_json::Map::new();
        o.insert(
            "name".into(),
            json!(n.display_name.as_ref().unwrap_or(&n.name)),
        );
        o.insert("type".into(), json!(n.type_name));
        o.insert("kind".into(), json!(name_of(&n.kind)));
        o.insert("offset".into(), json!(n.offset));
        o.insert("size".into(), json!(n.size));
        if let Some(b) = n.bit_offset {
            o.insert("bitOffset".into(), json!(b));
        }
        if let Some(b) = n.bit_size {
            o.insert("bitSize".into(), json!(b));
        }
        if let Some(v) = &n.value {
            o.insert("value".into(), json!(v));
        }
        if let Some(r) = &n.raw {
            o.insert("raw".into(), raw_json(r));
        }
        if let Some(e) = &n.enum_name {
            o.insert("enum".into(), json!(e));
        }
        if let Some(c) = &n.comment {
            o.insert("comment".into(), json!(c));
        }
        if let Some(e) = &n.error {
            o.insert("error".into(), json!(e));
        }
        // Primitive arrays carry a formatted value; their elements are
        // only listed on request.
        let kids = if n.is_array && n.value.is_some() && !self.elements {
            Vec::new()
        } else {
            self.children(n)
        };
        if !kids.is_empty() || n.is_array {
            if n.is_array {
                o.insert("len".into(), json!(n.len));
                if n.len > self.max {
                    o.insert("truncated".into(), json!(true));
                }
            }
            o.insert("children".into(), Value::Array(kids));
        }
        Value::Object(o)
    }

    fn children(&self, n: &Node) -> Vec<Value> {
        if n.lazy && n.elem_index.is_none() {
            // A lazy array: elements past the eager ones are virtual.
            return children(self.result, n.id, 0..self.max)
                .iter()
                .enumerate()
                .filter(|(_, c)| self.hidden || !c.hidden)
                .map(|(i, c)| {
                    if c.lazy
                        && matches!(c.kind, NodeKind::Struct | NodeKind::Union)
                        && let Some(sub) = expand_element(self.result, n.id, i as u64)
                    {
                        let r = Render {
                            result: &sub,
                            ..*self
                        };
                        if let Some(root) = sub.roots.first().and_then(|&id| sub.node(id)) {
                            return r.node(root);
                        }
                    }
                    self.node(c)
                })
                .collect();
        }
        n.children
            .iter()
            .take(usize::try_from(self.max).unwrap_or(usize::MAX))
            .filter_map(|&id| self.result.node(id))
            .filter(|c| self.hidden || !c.hidden)
            .map(|c| self.node(c))
            .collect()
    }
}

fn render_text(out: &mut String, v: &Value, depth: usize) {
    let indent = "  ".repeat(depth);
    let _ = write!(
        out,
        "{indent}{} {} @0x{:X} [{}]",
        v["type"].as_str().unwrap_or_default(),
        v["name"].as_str().unwrap_or_default(),
        v["offset"].as_u64().unwrap_or(0),
        v["size"].as_u64().unwrap_or(0)
    );
    if let Some(val) = v["value"].as_str() {
        let _ = write!(out, " = {val}");
    }
    if let Some(e) = v["error"].as_str() {
        let _ = write!(out, "  !! {e}");
    }
    out.push('\n');
    if let Some(kids) = v["children"].as_array() {
        for k in kids {
            render_text(out, k, depth + 1);
        }
        if v["truncated"].as_bool() == Some(true) {
            let _ = writeln!(out, "{indent}  … {} elements in total", v["len"]);
        }
    }
}

pub fn run(a: &TemplateArgs, json_out: bool) -> CliResult {
    if a.list {
        let list = library::list();
        if json_out {
            io::print_json(&list)?;
        } else {
            let mut s = String::new();
            for t in &list {
                let _ = writeln!(s, "{:<10} {:<28} {}", t.id, t.name, t.extensions.join(", "));
            }
            io::print(&s)?;
        }
        return Ok(Outcome::Ok);
    }
    let file = a
        .file
        .as_deref()
        .ok_or_else(|| CliError::usage("missing input file"))?;
    let tree = open_tree(file)?;
    let head = tree.read_vec(0, tree.len().min(64 << 10) as usize)?;
    let (program, template_name) = load_program(a, &head, file)?;
    let opts = EvalOptions {
        base_offset: a.base_offset,
        root_type: a.root_type.clone(),
        ..EvalOptions::default()
    };
    let result = hexa_template::evaluate(&program, &tree, &opts, &Cancel::new())?;
    let r = Render {
        max: a.max_elements.max(1),
        hidden: a.hidden,
        elements: a.elements,
        result: &result,
    };
    let roots: Vec<Value> = result
        .roots
        .iter()
        .filter_map(|&id| result.node(id))
        .filter(|n| a.hidden || !n.hidden)
        .map(|n| r.node(n))
        .collect();
    let errors: Vec<&_> = result.errors.iter().filter(|e| !e.warning).collect();
    let warnings: Vec<&_> = result.errors.iter().filter(|e| e.warning).collect();
    if json_out {
        io::print_json(&json!({
            "template": template_name,
            "nodes": roots,
            "log": result.log,
            "errors": errors,
            "warnings": warnings,
            "truncated": result.truncated,
        }))?;
    } else {
        let mut s = String::new();
        for v in &roots {
            render_text(&mut s, v, 0);
        }
        for l in &result.log {
            let _ = writeln!(s, "log: {l}");
        }
        io::print(&s)?;
    }
    for w in &warnings {
        eprintln!(
            "hexa-cli: warning: line {}: {} (at 0x{:X})",
            w.line, w.message, w.offset
        );
    }
    for e in &errors {
        eprintln!(
            "hexa-cli: error: line {}: {} (at 0x{:X})",
            e.line, e.message, e.offset
        );
    }
    if errors.is_empty() {
        Ok(Outcome::Ok)
    } else {
        Err(CliError::data(format!(
            "template evaluation reported {} error(s)",
            errors.len()
        )))
    }
}
