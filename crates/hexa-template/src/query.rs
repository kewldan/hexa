//! Working with evaluation results: tree filter and export (STRC-15),
//! structural diff (DIFF-10) and value editing (STRC-14).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::ast::Endian;
use crate::diag::{Error, Result};
use crate::prim::{Prim, encode_uint, f64_to_f16};
use crate::result::{Node, NodeId, NodeKind, TemplateResult, Value, children};

// ----- filter ------------------------------------------------------------------

/// Finds nodes matching `query` (STRC-15), in document order.
///
/// The query is a whitespace-separated list of terms that must all match.
/// A plain term matches (case-insensitively) the name, display name, type,
/// value, enum name or comment. Prefixed terms narrow the field:
/// `name:`, `type:`, `value:`, `comment:`, `kind:` (`struct`, `enum`, ...),
/// `offset:` (nodes covering that offset, decimal or `0x` hex) and
/// `error:` (nodes with an error; the text after the colon is optional).
pub fn filter(result: &TemplateResult, query: &str) -> Vec<NodeId> {
    let terms: Vec<Term> = query.split_whitespace().map(Term::parse).collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for &r in &result.roots {
        walk(result, r, &mut |n| {
            if terms.iter().all(|t| t.matches(n)) {
                out.push(n.id);
            }
        });
    }
    out
}

/// `id` and its ancestors (root first) — handy to expand a filtered tree.
pub fn ancestors(result: &TemplateResult, id: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut cur = result.node(id);
    while let Some(n) = cur {
        out.push(n.id);
        cur = n.parent.and_then(|p| result.node(p));
    }
    out.reverse();
    out
}

fn walk(result: &TemplateResult, id: NodeId, f: &mut impl FnMut(&Node)) {
    let Some(n) = result.node(id) else { return };
    f(n);
    for &c in &n.children {
        walk(result, c, f);
    }
}

enum Term {
    Any(String),
    Name(String),
    Type(String),
    Value(String),
    Comment(String),
    Kind(String),
    Offset(Option<u64>),
    Error(String),
}

fn parse_num(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(&h.replace('_', ""), 16).ok()
    } else {
        s.replace('_', "").parse().ok()
    }
}

impl Term {
    fn parse(t: &str) -> Term {
        let lower = t.to_lowercase();
        match lower.split_once(':') {
            Some(("name", v)) => Term::Name(v.into()),
            Some(("type", v)) => Term::Type(v.into()),
            Some(("value", v)) => Term::Value(v.into()),
            Some(("comment", v)) => Term::Comment(v.into()),
            Some(("kind", v)) => Term::Kind(v.into()),
            Some(("offset" | "at", v)) => Term::Offset(parse_num(v)),
            Some(("error", v)) => Term::Error(v.into()),
            _ => Term::Any(lower),
        }
    }

    fn matches(&self, n: &Node) -> bool {
        let has = |hay: Option<&str>, needle: &str| {
            hay.is_some_and(|h| h.to_lowercase().contains(needle))
        };
        match self {
            Term::Any(q) => {
                has(Some(&n.name), q)
                    || has(n.display_name.as_deref(), q)
                    || has(Some(&n.type_name), q)
                    || has(n.value.as_deref(), q)
                    || has(n.enum_name.as_deref(), q)
                    || has(n.comment.as_deref(), q)
            }
            Term::Name(q) => has(Some(&n.name), q) || has(n.display_name.as_deref(), q),
            Term::Type(q) => has(Some(&n.type_name), q),
            Term::Value(q) => has(n.value.as_deref(), q) || has(n.enum_name.as_deref(), q),
            Term::Comment(q) => has(n.comment.as_deref(), q),
            Term::Kind(q) => format!("{:?}", n.kind).to_lowercase() == *q,
            Term::Offset(Some(o)) => n.range().contains(o),
            Term::Offset(None) => false,
            Term::Error(q) => n
                .error
                .as_deref()
                .is_some_and(|e| e.to_lowercase().contains(q.as_str())),
        }
    }
}

// ----- export ------------------------------------------------------------------

/// Export format (STRC-15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExportFormat {
    Json,
    Csv,
    Xml,
}

/// Export options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOptions {
    /// Elements of lazy arrays written per array (the rest is summarized).
    pub max_array_elements: u64,
    /// Include nodes marked `[[hidden]]`.
    pub include_hidden: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            max_array_elements: 4096,
            include_hidden: false,
        }
    }
}

/// Exported node (JSON shape).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportNode {
    pub name: String,
    pub path: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub offset: u64,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ExportNode>,
    /// Elements of a lazy array that were not exported.
    #[serde(skip_serializing_if = "is_zero")]
    pub omitted: u64,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(v: &u64) -> bool {
    *v == 0
}

/// Builds the exported tree.
pub fn export_tree(result: &TemplateResult, opts: &ExportOptions) -> Vec<ExportNode> {
    result
        .roots
        .iter()
        .filter_map(|&r| result.node(r))
        .filter(|n| opts.include_hidden || !n.hidden)
        .map(|n| export_node(result, n, &result.path(n.id), opts))
        .collect()
}

fn export_node(result: &TemplateResult, n: &Node, path: &str, opts: &ExportOptions) -> ExportNode {
    let mut out = ExportNode {
        name: n.label().to_string(),
        path: path.to_string(),
        type_name: n.type_name.clone(),
        offset: n.offset,
        size: n.size,
        value: n.value.clone(),
        raw: n.raw.clone(),
        comment: n.comment.clone(),
        error: n.error.clone(),
        children: Vec::new(),
        omitted: 0,
    };
    let kids: Vec<Node> = if n.lazy {
        let take = n.len.min(opts.max_array_elements);
        out.omitted = n.len - take;
        children(result, n.id, 0..take)
    } else {
        n.children
            .iter()
            .filter_map(|c| result.node(*c).cloned())
            .collect()
    };
    for c in kids {
        if c.hidden && !opts.include_hidden {
            continue;
        }
        let cp = if c.name.starts_with('[') {
            format!("{path}{}", c.name)
        } else {
            format!("{path}.{}", c.name)
        };
        out.children.push(export_node(result, &c, &cp, opts));
    }
    out
}

/// Serializes a result as JSON, CSV or XML (STRC-15).
pub fn export(result: &TemplateResult, format: ExportFormat, opts: &ExportOptions) -> String {
    let tree = export_tree(result, opts);
    match format {
        ExportFormat::Json => serde_json::to_string_pretty(&tree).unwrap_or_default(),
        ExportFormat::Csv => {
            let mut s = String::from("path,type,offset,size,value,comment\n");
            csv_rows(&mut s, &tree);
            s
        }
        ExportFormat::Xml => {
            let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<template>\n");
            for n in &tree {
                xml_node(&mut s, n, 1);
            }
            s.push_str("</template>\n");
            s
        }
    }
}

fn csv_rows(s: &mut String, nodes: &[ExportNode]) {
    for n in nodes {
        let _ = writeln!(
            s,
            "{},{},{},{},{},{}",
            csv(&n.path),
            csv(&n.type_name),
            n.offset,
            n.size,
            csv(n.value.as_deref().unwrap_or("")),
            csv(n.comment.as_deref().unwrap_or(""))
        );
        csv_rows(s, &n.children);
    }
}

fn xml_node(s: &mut String, n: &ExportNode, depth: usize) {
    let pad = "  ".repeat(depth);
    let _ = write!(
        s,
        "{pad}<node name=\"{}\" type=\"{}\" offset=\"{}\" size=\"{}\"",
        xml(&n.name),
        xml(&n.type_name),
        n.offset,
        n.size
    );
    if let Some(v) = &n.value {
        let _ = write!(s, " value=\"{}\"", xml(v));
    }
    if let Some(c) = &n.comment {
        let _ = write!(s, " comment=\"{}\"", xml(c));
    }
    if let Some(e) = &n.error {
        let _ = write!(s, " error=\"{}\"", xml(e));
    }
    if n.omitted > 0 {
        let _ = write!(s, " omitted=\"{}\"", n.omitted);
    }
    if n.children.is_empty() {
        s.push_str("/>\n");
    } else {
        s.push_str(">\n");
        for c in &n.children {
            xml_node(s, c, depth + 1);
        }
        let _ = writeln!(s, "{pad}</node>");
    }
}

fn csv(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {
                let _ = write!(out, "&#x{:X};", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

// ----- diff --------------------------------------------------------------------

/// Kind of a structural change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeKind {
    Added,
    Removed,
    Changed,
}

/// One field that differs between two results (DIFF-10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldChange {
    pub path: String,
    pub kind: ChangeKind,
    pub old: Option<String>,
    pub new: Option<String>,
    /// Location in the first / second data.
    pub old_offset: Option<u64>,
    pub new_offset: Option<u64>,
}

/// Compares two results field by field (by path), e.g. the same template
/// applied to two files. Only leaf values are compared; lazy arrays are
/// compared element by element up to 65 536 elements.
pub fn diff(a: &TemplateResult, b: &TemplateResult) -> Vec<FieldChange> {
    let la = leaves(a);
    let lb = leaves(b);
    let mut out = Vec::new();
    for (path, (va, oa)) in &la {
        match lb.get(path) {
            Some((vb, ob)) if vb != va => out.push(FieldChange {
                path: path.clone(),
                kind: ChangeKind::Changed,
                old: Some(va.clone()),
                new: Some(vb.clone()),
                old_offset: Some(*oa),
                new_offset: Some(*ob),
            }),
            Some(_) => {}
            None => out.push(FieldChange {
                path: path.clone(),
                kind: ChangeKind::Removed,
                old: Some(va.clone()),
                new: None,
                old_offset: Some(*oa),
                new_offset: None,
            }),
        }
    }
    for (path, (vb, ob)) in &lb {
        if !la.contains_key(path) {
            out.push(FieldChange {
                path: path.clone(),
                kind: ChangeKind::Added,
                old: None,
                new: Some(vb.clone()),
                old_offset: None,
                new_offset: Some(*ob),
            });
        }
    }
    let order = |p: &str| (la.get(p).or_else(|| lb.get(p)).map(|x| x.1), p.to_string());
    out.sort_by_key(|c| order(&c.path));
    out
}

fn leaves(r: &TemplateResult) -> BTreeMap<String, (String, u64)> {
    let mut out = BTreeMap::new();
    for &root in &r.roots {
        if let Some(n) = r.node(root) {
            collect(r, n, &r.path(root), &mut out);
        }
    }
    out
}

fn collect(r: &TemplateResult, n: &Node, path: &str, out: &mut BTreeMap<String, (String, u64)>) {
    let kids: Vec<Node> = if n.lazy {
        children(r, n.id, 0..n.len.min(1 << 16))
    } else {
        n.children
            .iter()
            .filter_map(|c| r.node(*c).cloned())
            .collect()
    };
    if kids.is_empty() || n.kind == NodeKind::Bitfield {
        let v = n
            .value
            .clone()
            .unwrap_or_else(|| format!("<{} bytes>", n.size));
        out.insert(path.to_string(), (v, n.offset));
        if n.kind != NodeKind::Bitfield {
            return;
        }
    }
    for c in kids {
        let cp = if c.name.starts_with('[') {
            format!("{path}{}", c.name)
        } else {
            format!("{path}.{}", c.name)
        };
        collect(r, &c, &cp, out);
    }
}

// ----- editing -------------------------------------------------------------------

/// Encodes `text` as the new bytes of `node` (STRC-14). The returned bytes
/// replace `node.offset .. node.offset + node.size`; for bit-field members
/// the surrounding storage unit is read and only the member's bits change.
pub fn encode_value(result: &TemplateResult, node: &Node, text: &str) -> Result<Vec<u8>> {
    let text = text.trim();
    let endian = node.endian;
    match node.kind {
        NodeKind::String => encode_string(node, text),
        NodeKind::BitMember => {
            let width = node.bit_size.unwrap_or(1);
            let v = parse_member_value(result, node, text)?;
            let max = if width >= 128 {
                u128::MAX
            } else {
                (1u128 << width) - 1
            };
            let signed = node
                .raw
                .as_ref()
                .is_some_and(|r| matches!(r, Value::Int(i) if *i < 0))
                || Prim::from_name(&node.type_name).is_some_and(Prim::is_signed);
            let bits = if signed {
                let lo = -(1i128 << (width - 1));
                let hi = (1i128 << (width - 1)) - 1;
                if v < lo || v > hi {
                    return Err(Error::invalid(format!(
                        "{v} does not fit in {width} signed bits"
                    )));
                }
                (v as u128) & max
            } else {
                if v < 0 || v as u128 > max {
                    return Err(Error::invalid(format!("{v} does not fit in {width} bits")));
                }
                v as u128
            };
            let tree = result
                .tree
                .as_ref()
                .ok_or_else(|| Error::invalid("result has no data"))?;
            let mut buf = vec![0u8; node.size as usize];
            tree.read(node.offset, &mut buf)?;
            let total = (node.size * 8) as u32;
            let rel = node.bit_offset.unwrap_or(0);
            let unit = crate::prim::decode_uint(&buf, endian);
            let shift = if endian == Endian::Big {
                total - rel - width
            } else {
                rel
            };
            let new = (unit & !(max << shift)) | (bits << shift);
            Ok(encode_uint(new, buf.len(), endian))
        }
        NodeKind::Enum => {
            let v = parse_member_value(result, node, text)?;
            let p = node_prim(node)?;
            encode_int(p, v, endian)
        }
        NodeKind::Primitive | NodeKind::Pointer | NodeKind::Bitfield | NodeKind::Padding => {
            let p = match node.kind {
                NodeKind::Bitfield | NodeKind::Padding => Prim::U(node.size.clamp(1, 16) as u8),
                _ => node_prim(node)?,
            };
            match p {
                Prim::F16 | Prim::F32 | Prim::F64 => {
                    let f: f64 = text
                        .parse()
                        .map_err(|_| Error::invalid(format!("`{text}` is not a number")))?;
                    Ok(match p {
                        Prim::F16 => encode_uint(u128::from(f64_to_f16(f)), 2, endian),
                        Prim::F32 => {
                            let x = f as f32;
                            if f.is_finite() && !x.is_finite() {
                                return Err(Error::invalid(format!(
                                    "{f} is out of range for float"
                                )));
                            }
                            encode_uint(u128::from(x.to_bits()), 4, endian)
                        }
                        _ => encode_uint(u128::from(f.to_bits()), 8, endian),
                    })
                }
                Prim::Bool => match text.to_ascii_lowercase().as_str() {
                    "true" | "1" => Ok(vec![1]),
                    "false" | "0" => Ok(vec![0]),
                    _ => Err(Error::invalid(format!("`{text}` is not a boolean"))),
                },
                Prim::Char | Prim::Char16 => {
                    let v = parse_char_or_int(text)?;
                    let n = p.size().unwrap_or(1) as usize;
                    if v < 0 || v >= 1i128 << (8 * n) {
                        return Err(Error::invalid(format!(
                            "`{text}` does not fit in {n} byte(s)"
                        )));
                    }
                    Ok(encode_uint(v as u128, n, endian))
                }
                _ => encode_int(p, parse_int(text)?, endian),
            }
        }
        NodeKind::Struct | NodeKind::Union | NodeKind::Array | NodeKind::Computed => {
            Err(Error::invalid(format!(
                "`{}` ({:?}) cannot be edited as a value",
                node.name, node.kind
            )))
        }
    }
}

fn node_prim(node: &Node) -> Result<Prim> {
    node.prim
        .as_deref()
        .and_then(Prim::from_name)
        .ok_or_else(|| Error::invalid(format!("`{}` has no primitive type", node.name)))
}

fn encode_int(p: Prim, v: i128, endian: Endian) -> Result<Vec<u8>> {
    let n = p.size().unwrap_or(1) as usize;
    let bits = (n * 8) as u32;
    let (lo, hi) = if p.is_signed() {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else if bits >= 127 {
        (0, i128::MAX)
    } else {
        (0, (1i128 << bits) - 1)
    };
    if v < lo || v > hi {
        return Err(Error::invalid(format!(
            "{v} is out of range for {} ({lo} ..= {hi})",
            p.name()
        )));
    }
    Ok(encode_uint(v as u128, n, endian))
}

/// Integer literal: decimal, `0x`, `0b`, `0o`, optional sign, `_` separators.
pub fn parse_int(text: &str) -> Result<i128> {
    let t = text.trim().replace('_', "");
    let (neg, body) = match t.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, t.strip_prefix('+').unwrap_or(&t)),
    };
    let lower = body.to_ascii_lowercase();
    let parsed = if let Some(h) = lower.strip_prefix("0x") {
        u128::from_str_radix(h, 16)
    } else if let Some(b) = lower.strip_prefix("0b") {
        u128::from_str_radix(b, 2)
    } else if let Some(o) = lower.strip_prefix("0o") {
        u128::from_str_radix(o, 8)
    } else {
        lower.parse::<u128>()
    };
    let v = parsed.map_err(|_| Error::invalid(format!("`{text}` is not an integer")))?;
    let v = i128::try_from(v).map_err(|_| Error::invalid(format!("`{text}` is too large")))?;
    Ok(if neg { -v } else { v })
}

fn parse_char_or_int(text: &str) -> Result<i128> {
    let inner = text
        .strip_prefix('\'')
        .and_then(|t| t.strip_suffix('\''))
        .unwrap_or(text);
    let mut chars = inner.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && !(text.len() == 1 && c.is_ascii_digit())
    {
        return Ok(i128::from(u32::from(c)));
    }
    parse_int(text)
}

/// Integer, enum variant name or `A | B` flag combination.
fn parse_member_value(result: &TemplateResult, node: &Node, text: &str) -> Result<i128> {
    if let Ok(v) = parse_int(text) {
        return Ok(v);
    }
    if text.eq_ignore_ascii_case("true") {
        return Ok(1);
    }
    if text.eq_ignore_ascii_case("false") {
        return Ok(0);
    }
    let variants = result.enum_variants(&node.type_name);
    if variants.is_empty() {
        return Err(Error::invalid(format!("`{text}` is not an integer")));
    }
    let mut v = 0i128;
    for part in text.split('|') {
        let part = part.trim();
        let part = part.rsplit("::").next().unwrap_or(part);
        if let Ok(n) = parse_int(part) {
            v |= n;
            continue;
        }
        let Some((_, x)) = variants.iter().find(|(n, _)| n == part) else {
            return Err(Error::invalid(format!(
                "`{part}` is not a variant of `{}`",
                node.type_name
            )));
        };
        v |= *x;
    }
    Ok(v)
}

fn encode_string(node: &Node, text: &str) -> Result<Vec<u8>> {
    let size = node.size as usize;
    let wide = node.type_name.starts_with("char16") || node.type_name == "wstring";
    let terminated = matches!(node.type_name.as_str(), "string" | "wstring");
    let mut bytes = if wide {
        text.encode_utf16()
            .flat_map(|u| match node.endian {
                Endian::Little => u.to_le_bytes(),
                Endian::Big => u.to_be_bytes(),
            })
            .collect()
    } else {
        text.as_bytes().to_vec()
    };
    let unit = if wide { 2 } else { 1 };
    let room = if terminated {
        size.saturating_sub(unit)
    } else {
        size
    };
    if bytes.len() > room {
        return Err(Error::invalid(format!(
            "text needs {} bytes but `{}` holds {room}",
            bytes.len(),
            node.name
        )));
    }
    bytes.resize(size, 0);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_parse() {
        assert_eq!(parse_int("0x1F").unwrap(), 31);
        assert_eq!(parse_int("-0b101").unwrap(), -5);
        assert_eq!(parse_int("1_000").unwrap(), 1000);
        assert_eq!(parse_int("0o17").unwrap(), 15);
        assert!(parse_int("abc").is_err());
        assert_eq!(parse_char_or_int("'A'").unwrap(), 65);
        assert_eq!(parse_char_or_int("7").unwrap(), 7);
    }

    #[test]
    fn escaping() {
        assert_eq!(csv("a,b"), "\"a,b\"");
        assert_eq!(csv("say \"x\""), "\"say \"\"x\"\"\"");
        assert_eq!(xml("<a & 'b'>\u{1}"), "&lt;a &amp; &apos;b&apos;&gt;&#x1;");
    }
}
