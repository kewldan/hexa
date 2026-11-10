//! Kaitai Struct `.ksy` import (STRC-03): YAML specification → template AST.
//!
//! Supported: `meta` (`id`, `endian`, `encoding`, `bit-endian`, `imports`
//! through a resolver), `seq`, `types` (nested, with `params`), `enums`
//! (incl. `id`/`doc` maps), `instances` (`pos`, `value`, `size`, `type`,
//! `repeat`, `if`), primitive types `u1..u8`/`s1..s8`/`f4`/`f8` with
//! `be`/`le` suffixes, bit-sized integers `bN`, `str`/`strz` with `size`,
//! `size-eos`, `terminator` and `encoding`, `contents`, raw byte fields,
//! user types with `size`/`size-eos` substreams, `repeat` (`expr`, `eos`,
//! `until`), `if`, `enum`, `switch-on`/`cases`, `valid` and `process: none`.
//! Instances are evaluated after the `seq` fields of their type.

use std::collections::{HashMap, HashSet};

use saphyr::{LoadableYamlNode, Yaml};

use crate::ast::{
    ArraySpec, Attr, BinOp, BitfieldDef, BitfieldMember, Endian, EnumDef, EnumVariant, Expr,
    ExprKind, FieldDecl, LetDecl, MatchArm, Param, Pattern, Program, Stmt, StmtKind, StructDef,
    StructKind, TypeDef, TypeRef, UnOp,
};
use crate::diag::{Error, Result};
use crate::lexer::Resolver;
use crate::parser::{Dialect, parse_expr};

/// Converts a Kaitai Struct specification to a template program. The root
/// type is instantiated at offset 0 as a field named after `meta.id`.
pub fn from_ksy(src: &str) -> Result<Program> {
    from_ksy_with(src, None)
}

/// Like [`from_ksy`], resolving `meta.imports` through `resolver` (called
/// with the import path plus `.ksy`).
pub fn from_ksy_with(src: &str, resolver: Option<Resolver<'_>>) -> Result<Program> {
    let docs =
        Yaml::load_from_str(src).map_err(|e| Error::invalid(format!("invalid YAML: {e}")))?;
    let doc = docs
        .into_iter()
        .next()
        .ok_or_else(|| Error::invalid("empty Kaitai specification"))?;
    let mut c = Conv {
        resolver,
        ..Conv::default()
    };
    let root = c.spec(&doc, true)?;
    let mut items = std::mem::take(&mut c.items);
    let mut pragmas = Vec::new();
    if c.endian == Some(Endian::Big) {
        pragmas.push(("endian".to_string(), "big".to_string()));
    }
    items.push(Stmt::synthetic(StmtKind::Field(FieldDecl {
        ty: TypeRef::named(root.clone()),
        name: root,
        array: None,
        pointer: None,
        placement: None,
        bits: None,
        attrs: vec![],
    })));
    Ok(Program { items, pragmas })
}

#[derive(Default)]
struct Conv<'r> {
    resolver: Option<Resolver<'r>>,
    endian: Option<Endian>,
    encoding: String,
    bit_le: bool,
    types: HashSet<String>,
    enums: HashMap<String, Vec<EnumVariant>>,
    enum_defs: HashMap<(String, String), String>,
    items: Vec<Stmt>,
    anon: u32,
    imported: HashSet<String>,
}

fn s<'a>(y: &'a Yaml<'a>) -> Option<&'a str> {
    y.as_str()
}

/// Scalar as text (strings, integers, booleans, floats).
fn text(y: &Yaml) -> Option<String> {
    if let Some(v) = y.as_str() {
        return Some(v.to_string());
    }
    if let Some(v) = y.as_integer() {
        return Some(v.to_string());
    }
    if let Some(v) = y.as_bool() {
        return Some(v.to_string());
    }
    y.as_floating_point().map(|v| v.to_string())
}

fn get<'a>(y: &'a Yaml<'a>, key: &str) -> Option<&'a Yaml<'a>> {
    y.as_mapping_get(key)
        .filter(|v| !v.is_null() && !v.is_badvalue())
}

fn qualify(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}::{name}")
    }
}

/// Kaitai single-quoted strings become double-quoted ones.
fn requote(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut in_double = false;
    let mut in_single = false;
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if in_double => {
                out.push(c);
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '"' if !in_single => {
                in_double = !in_double;
                out.push(c);
            }
            '\'' if !in_double => {
                in_single = !in_single;
                out.push('"');
            }
            '"' if in_single => out.push_str("\\\""),
            '\\' if in_single => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out
}

fn rewrite(e: &mut Expr) {
    match &mut e.kind {
        ExprKind::Ident(n) if n == "_index" => {
            e.kind = ExprKind::Call("std::core::array_index".into(), vec![]);
        }
        ExprKind::Unary(_, x)
        | ExprKind::Member(x, _)
        | ExprKind::AddressOf(x)
        | ExprKind::Cast(_, x) => {
            rewrite(x);
        }
        ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) => {
            rewrite(a);
            rewrite(b);
        }
        ExprKind::Ternary(a, b, c) => {
            rewrite(a);
            rewrite(b);
            rewrite(c);
        }
        ExprKind::Call(_, args) => args.iter_mut().for_each(rewrite),
        _ => {}
    }
}

fn expr_of(y: &Yaml) -> Result<Expr> {
    let src = text(y).ok_or_else(|| Error::invalid("expected an expression"))?;
    expr_str(&src)
}

fn expr_str(src: &str) -> Result<Expr> {
    let mut e = parse_expr(&requote(src), Dialect::Ksy)
        .map_err(|d| Error::invalid(format!("invalid Kaitai expression `{src}`: {}", d.message)))?;
    rewrite(&mut e);
    Ok(e)
}

/// Kaitai primitive (`u4be`, `f8`, ...) → template type.
fn prim(name: &str) -> Option<TypeRef> {
    let (base, endian) = if let Some(b) = name.strip_suffix("be") {
        (b, Some(Endian::Big))
    } else if let Some(b) = name.strip_suffix("le") {
        (b, Some(Endian::Little))
    } else {
        (name, None)
    };
    let t = match base {
        "u1" => "u8",
        "u2" => "u16",
        "u4" => "u32",
        "u8" => "u64",
        "s1" => "s8",
        "s2" => "s16",
        "s4" => "s32",
        "s8" => "s64",
        "f4" => "float",
        "f8" => "double",
        _ => return None,
    };
    Some(TypeRef::named(t).with_endian(endian))
}

fn bit_width(name: &str) -> Option<u32> {
    let n = name.strip_prefix('b')?;
    let n = n
        .strip_suffix("be")
        .or_else(|| n.strip_suffix("le"))
        .unwrap_or(n);
    n.parse().ok().filter(|w| (1..=64).contains(w))
}

impl Conv<'_> {
    fn anon(&mut self, what: &str) -> String {
        self.anon += 1;
        format!("__anon_{what}_{}", self.anon)
    }

    /// Converts a top-level spec (root or import); returns its type name.
    fn spec(&mut self, doc: &Yaml, root: bool) -> Result<String> {
        let meta = get(doc, "meta");
        let id = meta
            .and_then(|m| get(m, "id"))
            .and_then(s)
            .ok_or_else(|| Error::invalid("`meta.id` is required"))?
            .to_string();
        if root {
            if let Some(m) = meta {
                self.endian = match get(m, "endian").and_then(s) {
                    Some("be") => Some(Endian::Big),
                    Some("le") => Some(Endian::Little),
                    Some(other) => {
                        return Err(Error::Unsupported(format!("endian `{other}`")));
                    }
                    None if get(m, "endian").is_some() => {
                        return Err(Error::Unsupported(
                            "calculated endianness (`switch-on`)".into(),
                        ));
                    }
                    None => None,
                };
                self.encoding = get(m, "encoding")
                    .and_then(s)
                    .unwrap_or("ASCII")
                    .to_string();
                self.bit_le = get(m, "bit-endian").and_then(s) == Some("le");
            } else {
                self.encoding = "ASCII".into();
            }
        }
        if let Some(imports) = meta.and_then(|m| get(m, "imports")).and_then(Yaml::as_vec) {
            for imp in imports {
                let path = s(imp).ok_or_else(|| Error::invalid("import must be a string"))?;
                if !self.imported.insert(path.to_string()) {
                    continue;
                }
                let file = format!("{}.ksy", path.trim_start_matches('/'));
                let src = self
                    .resolver
                    .and_then(|r| r(&file).or_else(|| r(path)))
                    .ok_or_else(|| Error::invalid(format!("cannot resolve import `{path}`")))?;
                let docs = Yaml::load_from_str(&src)
                    .map_err(|e| Error::invalid(format!("invalid YAML in `{path}`: {e}")))?;
                if let Some(d) = docs.first() {
                    self.collect(d, "", true)?;
                    self.spec(d, false)?;
                }
            }
        }
        self.collect(doc, "", root)?;
        self.types.insert(id.clone());
        self.type_spec(doc, &id, "")?;
        Ok(id)
    }

    /// Records every type and enum name (qualified) before conversion.
    fn collect(&mut self, spec: &Yaml, prefix: &str, top: bool) -> Result<()> {
        if !top && let Some(id) = get(spec, "meta").and_then(|m| get(m, "id")).and_then(s) {
            self.types.insert(id.to_string());
        }
        if let Some(enums) = get(spec, "enums").and_then(Yaml::as_mapping) {
            for (k, v) in enums {
                let name = text(k).ok_or_else(|| Error::invalid("bad enum name"))?;
                let q = qualify(prefix, &name);
                let vars = enum_variants(v)?;
                self.enums.insert(q, vars);
            }
        }
        if let Some(types) = get(spec, "types").and_then(Yaml::as_mapping) {
            for (k, v) in types {
                let name = text(k).ok_or_else(|| Error::invalid("bad type name"))?;
                let q = qualify(prefix, &name);
                self.types.insert(q.clone());
                self.collect(v, &q, false)?;
            }
        }
        Ok(())
    }

    fn resolve_in(set: impl Fn(&str) -> bool, scope: &str, name: &str) -> Option<String> {
        let mut prefix = scope.to_string();
        loop {
            let q = qualify(&prefix, name);
            if set(&q) {
                return Some(q);
            }
            if prefix.is_empty() {
                return None;
            }
            prefix = prefix
                .rsplit_once("::")
                .map_or(String::new(), |(p, _)| p.to_string());
        }
    }

    fn resolve_type(&self, scope: &str, name: &str) -> Option<String> {
        Self::resolve_in(|q| self.types.contains(q), scope, name)
    }

    fn resolve_enum(&self, scope: &str, name: &str) -> Option<String> {
        Self::resolve_in(|q| self.enums.contains_key(q), scope, name)
    }

    /// Enum type for enum `q` stored as primitive `base`.
    fn enum_type(&mut self, q: &str, base: &TypeRef) -> String {
        let key = (q.to_string(), base.name.clone());
        if let Some(n) = self.enum_defs.get(&key) {
            return n.clone();
        }
        let name = if self.enum_defs.keys().any(|(e, _)| e == q) {
            format!("{q}__{}", base.name)
        } else {
            q.to_string()
        };
        self.items
            .push(Stmt::synthetic(StmtKind::TypeDef(TypeDef::Enum(EnumDef {
                name: name.clone(),
                base: TypeRef::named(base.name.clone()),
                variants: self.enums.get(q).cloned().unwrap_or_default(),
                attrs: vec![],
            }))));
        self.enum_defs.insert(key, name.clone());
        name
    }

    fn type_spec(&mut self, spec: &Yaml, qname: &str, prefix_for_children: &str) -> Result<()> {
        // Nested types first so references resolve in order.
        if let Some(types) = get(spec, "types").and_then(Yaml::as_mapping) {
            for (k, v) in types {
                let name = text(k).unwrap_or_default();
                let q = qualify(prefix_for_children, &name);
                self.type_spec(v, &q, &q)?;
            }
        }
        let scope = prefix_for_children;
        let mut params = Vec::new();
        if let Some(ps) = get(spec, "params").and_then(Yaml::as_vec) {
            for p in ps {
                let id = get(p, "id")
                    .and_then(s)
                    .ok_or_else(|| Error::invalid("param without id"))?;
                let ty = get(p, "type")
                    .and_then(s)
                    .and_then(prim)
                    .unwrap_or_else(|| TypeRef::named("auto"));
                params.push(Param {
                    name: id.to_string(),
                    ty: Some(ty),
                });
            }
        }
        let mut body = Vec::new();
        let mut bits: Vec<(String, u32, Vec<Attr>)> = Vec::new();
        if let Some(seq) = get(spec, "seq").and_then(Yaml::as_vec) {
            for (i, a) in seq.iter().enumerate() {
                let ty = get(a, "type").and_then(s);
                if let Some(w) = ty.and_then(bit_width)
                    && get(a, "repeat").is_none()
                    && get(a, "if").is_none()
                {
                    let id = get(a, "id")
                        .and_then(s)
                        .map_or_else(|| format!("_unnamed{i}"), str::to_string);
                    let attrs = doc_attrs(a);
                    bits.push((id, w, attrs));
                    continue;
                }
                self.flush_bits(&mut bits, &mut body);
                self.attribute(a, i, scope, None, &mut body)
                    .map_err(|e| context(e, qname, a, i))?;
            }
        }
        self.flush_bits(&mut bits, &mut body);
        if let Some(inst) = get(spec, "instances").and_then(Yaml::as_mapping) {
            for (i, (k, v)) in inst.iter().enumerate() {
                let name = text(k).unwrap_or_default();
                self.attribute(v, i, scope, Some(&name), &mut body)
                    .map_err(|e| context(e, qname, v, i))?;
            }
        }
        let mut attrs = Vec::new();
        if let Some(d) = get(spec, "doc").and_then(s) {
            attrs.push(Attr::string("comment", d.trim()));
        }
        self.items
            .push(Stmt::synthetic(StmtKind::TypeDef(TypeDef::Struct(
                StructDef {
                    name: qname.to_string(),
                    kind: StructKind::Struct,
                    params,
                    inherits: None,
                    body,
                    attrs,
                },
            ))));
        Ok(())
    }

    fn flush_bits(&mut self, bits: &mut Vec<(String, u32, Vec<Attr>)>, body: &mut Vec<Stmt>) {
        if bits.is_empty() {
            return;
        }
        let name = self.anon("bits");
        let members = bits
            .drain(..)
            .map(|(id, w, attrs)| BitfieldMember {
                name: Some(id),
                bits: Expr::int(i128::from(w)),
                ty: (w == 1).then(|| TypeRef::named("bool")),
                attrs,
            })
            .collect();
        self.items
            .push(Stmt::synthetic(StmtKind::TypeDef(TypeDef::Bitfield(
                BitfieldDef {
                    name: name.clone(),
                    members,
                    attrs: vec![],
                },
            ))));
        let endian = if self.bit_le {
            Endian::Little
        } else {
            Endian::Big
        };
        body.push(field(
            TypeRef::named(name.clone()).with_endian(Some(endian)),
            &name,
            None,
            vec![],
        ));
    }

    /// Converts one `seq` entry or instance into statements.
    fn attribute(
        &mut self,
        a: &Yaml,
        idx: usize,
        scope: &str,
        instance: Option<&str>,
        out: &mut Vec<Stmt>,
    ) -> Result<()> {
        let id = instance
            .map(str::to_string)
            .or_else(|| get(a, "id").and_then(s).map(str::to_string));
        let id = id.unwrap_or_else(|| format!("_unnamed{idx}"));
        let mut attrs = doc_attrs(a);
        if let Some(p) = get(a, "process").and_then(s)
            && p != "none"
        {
            return Err(Error::Unsupported(format!("`process: {p}`")));
        }
        if let Some(v) = get(a, "value") {
            let e = expr_of(v)?;
            let st = Stmt::synthetic(StmtKind::Let(LetDecl {
                name: id,
                ty: None,
                init: Some(e),
                array: None,
                visible: true,
                attrs,
            }));
            out.push(Self::wrap_if(a, st)?);
            return Ok(());
        }
        let placement = get(a, "pos").map(expr_of).transpose()?;
        let array = match get(a, "repeat").and_then(s) {
            None => None,
            Some("expr") => Some(ArraySpec::Fixed(expr_of(
                get(a, "repeat-expr")
                    .ok_or_else(|| Error::invalid("`repeat: expr` needs `repeat-expr`"))?,
            )?)),
            Some("eos") => Some(ArraySpec::Eof),
            Some("until") => Some(ArraySpec::Until(expr_of(
                get(a, "repeat-until")
                    .ok_or_else(|| Error::invalid("`repeat: until` needs `repeat-until`"))?,
            )?)),
            Some(other) => return Err(Error::Unsupported(format!("`repeat: {other}`"))),
        };
        let size = match (get(a, "size"), get(a, "size-eos").and_then(Yaml::as_bool)) {
            (Some(sz), _) => Some(expr_of(sz)?),
            (None, Some(true)) => Some(rest_of_stream()),
            _ => None,
        };
        let enc = get(a, "encoding")
            .and_then(s)
            .unwrap_or(&self.encoding)
            .to_ascii_uppercase();
        let utf16 = enc.starts_with("UTF-16");
        let utf16_endian = if enc.ends_with("BE") {
            Endian::Big
        } else {
            Endian::Little
        };
        let mut stmts = Vec::new();

        if let Some(contents) = get(a, "contents") {
            let bytes = contents_bytes(contents)?;
            let n = bytes.len();
            let mut cond: Option<Expr> = None;
            for (i, b) in bytes.iter().enumerate() {
                let c = Expr::binary(
                    BinOp::Eq,
                    Expr::new(ExprKind::Index(
                        Box::new(Expr::ident(&id)),
                        Box::new(Expr::int(i as i128)),
                    )),
                    Expr::int(i128::from(*b)),
                );
                cond = Some(match cond {
                    Some(prev) => Expr::binary(BinOp::And, prev, c),
                    None => c,
                });
            }
            stmts.push(field(
                TypeRef::named("u8"),
                &id,
                Some(ArraySpec::Fixed(Expr::int(n as i128))),
                attrs,
            ));
            if let Some(cond) = cond {
                stmts.push(Stmt::synthetic(StmtKind::Assert {
                    cond,
                    msg: Some(Expr::str(format!(
                        "`{id}` does not match the expected contents"
                    ))),
                    warn: false,
                }));
            }
            return Self::finish(a, stmts, placement, out);
        }

        let ty_node = get(a, "type");
        match ty_node {
            None => {
                // Raw bytes.
                let spec = if let Some(sz) = size {
                    if array.is_some() {
                        let w = self.wrapper("bytes", TypeRef::named("u8"), ArraySpec::Fixed(sz));
                        stmts.push(field(TypeRef::named(w), &id, array, attrs));
                        return Self::finish(a, stmts, placement, out);
                    }
                    ArraySpec::Fixed(sz)
                } else if let Some(t) = get(a, "terminator") {
                    let t = expr_of(t)?;
                    ArraySpec::Until(Expr::binary(BinOp::Eq, Expr::ident("_"), t))
                } else {
                    return Err(Error::invalid(format!(
                        "`{id}` has neither `type` nor `size`"
                    )));
                };
                stmts.push(field(TypeRef::named("u8"), &id, Some(spec), attrs));
            }
            Some(t) if t.as_mapping().is_some() => {
                let on = expr_of(
                    get(t, "switch-on")
                        .ok_or_else(|| Error::invalid("switch without `switch-on`"))?,
                )?;
                let cases = get(t, "cases")
                    .and_then(Yaml::as_mapping)
                    .ok_or_else(|| Error::invalid("switch without `cases`"))?;
                let mut arms = Vec::new();
                let mut has_default = false;
                for (k, v) in cases {
                    let key = text(k).ok_or_else(|| Error::invalid("bad case key"))?;
                    let pat = if key == "_" {
                        has_default = true;
                        Pattern::Wildcard
                    } else {
                        Pattern::Value(expr_str(&key)?)
                    };
                    let tname = s(v).ok_or_else(|| Error::invalid("case type must be a string"))?;
                    let ty = self.type_ref(tname, scope)?;
                    let mut fattrs = attrs.clone();
                    if let Some(sz) = &size {
                        if array.is_some() {
                            fattrs.push(Attr::new("elem_size", vec![sz.clone()]));
                        } else {
                            fattrs.push(Attr::new("size", vec![sz.clone()]));
                        }
                    }
                    arms.push(MatchArm {
                        patterns: vec![pat],
                        body: vec![field(ty, &id, array.clone(), fattrs)],
                    });
                }
                if !has_default && let Some(sz) = &size {
                    let body = if array.is_some() {
                        let w = self.wrapper(
                            "bytes",
                            TypeRef::named("u8"),
                            ArraySpec::Fixed(sz.clone()),
                        );
                        field(TypeRef::named(w), &id, array.clone(), attrs.clone())
                    } else {
                        field(
                            TypeRef::named("u8"),
                            &id,
                            Some(ArraySpec::Fixed(sz.clone())),
                            attrs.clone(),
                        )
                    };
                    arms.push(MatchArm {
                        patterns: vec![Pattern::Wildcard],
                        body: vec![body],
                    });
                }
                stmts.push(Stmt::synthetic(StmtKind::Match {
                    scrutinee: vec![on],
                    arms,
                }));
            }
            Some(t) => {
                let tname = s(t).ok_or_else(|| Error::invalid("`type` must be a string"))?;
                match tname {
                    "str" | "strz" => {
                        let zero_term = tname == "strz"
                            || get(a, "terminator").and_then(Yaml::as_integer) == Some(0);
                        let ch = if utf16 {
                            TypeRef::named("char16").with_endian(Some(utf16_endian))
                        } else {
                            TypeRef::named("char")
                        };
                        attrs.push(Attr::string("encoding", enc.clone()));
                        let base = if let Some(sz) = size {
                            let count = if utf16 {
                                Expr::binary(BinOp::Div, sz, Expr::int(2))
                            } else {
                                sz
                            };
                            Some((ch, ArraySpec::Fixed(count)))
                        } else if zero_term {
                            None
                        } else if let Some(term) = get(a, "terminator") {
                            let term = expr_of(term)?;
                            Some((
                                ch,
                                ArraySpec::Until(Expr::binary(BinOp::Eq, Expr::ident("_"), term)),
                            ))
                        } else {
                            return Err(Error::invalid(format!(
                                "string `{id}` needs `size`, `size-eos` or a terminator"
                            )));
                        };
                        match (base, array) {
                            (Some((ch, spec)), None) => {
                                stmts.push(field(ch, &id, Some(spec), attrs));
                            }
                            (Some((ch, spec)), Some(arr)) => {
                                let w = self.wrapper("str", ch, spec);
                                stmts.push(field(TypeRef::named(w), &id, Some(arr), attrs));
                            }
                            (None, arr) => {
                                let t = if utf16 {
                                    TypeRef::named("wstring").with_endian(Some(utf16_endian))
                                } else {
                                    TypeRef::named("string")
                                };
                                stmts.push(field(t, &id, arr, attrs));
                            }
                        }
                    }
                    _ => {
                        let mut ty = self.type_ref(tname, scope)?;
                        if let Some(en) = get(a, "enum").and_then(s) {
                            let q = self
                                .resolve_enum(scope, en)
                                .ok_or_else(|| Error::invalid(format!("unknown enum `{en}`")))?;
                            let e = self.enum_type(&q, &ty);
                            ty = TypeRef::named(e).with_endian(ty.endian);
                        }
                        if let Some(sz) = size {
                            attrs.push(Attr::new(
                                if array.is_some() { "elem_size" } else { "size" },
                                vec![sz],
                            ));
                        }
                        stmts.push(field(ty, &id, array, attrs));
                    }
                }
            }
        }
        Self::valid(a, &id, &mut stmts)?;
        Self::finish(a, stmts, placement, out)
    }

    /// `valid:` → assertions.
    fn valid(a: &Yaml, id: &str, stmts: &mut Vec<Stmt>) -> Result<()> {
        let Some(v) = get(a, "valid") else {
            return Ok(());
        };
        let me = || Expr::ident(id);
        let mut conds = Vec::new();
        if v.as_mapping().is_some() {
            if let Some(eq) = get(v, "eq") {
                conds.push(Expr::binary(BinOp::Eq, me(), expr_of(eq)?));
            }
            if let Some(min) = get(v, "min") {
                conds.push(Expr::binary(BinOp::Ge, me(), expr_of(min)?));
            }
            if let Some(max) = get(v, "max") {
                conds.push(Expr::binary(BinOp::Le, me(), expr_of(max)?));
            }
            if let Some(any) = get(v, "any-of").and_then(Yaml::as_vec) {
                let mut alt: Option<Expr> = None;
                for x in any {
                    let c = Expr::binary(BinOp::Eq, me(), expr_of(x)?);
                    alt = Some(match alt {
                        Some(p) => Expr::binary(BinOp::Or, p, c),
                        None => c,
                    });
                }
                conds.extend(alt);
            }
            if let Some(e) = get(v, "expr") {
                let mut e = expr_of(e)?;
                substitute_underscore(&mut e, id);
                conds.push(e);
            }
        } else {
            conds.push(Expr::binary(BinOp::Eq, me(), expr_of(v)?));
        }
        for c in conds {
            stmts.push(Stmt::synthetic(StmtKind::Assert {
                cond: c,
                msg: Some(Expr::str(format!("`{id}` failed validation"))),
                warn: false,
            }));
        }
        Ok(())
    }

    fn finish(
        a: &Yaml,
        mut stmts: Vec<Stmt>,
        placement: Option<Expr>,
        out: &mut Vec<Stmt>,
    ) -> Result<()> {
        if let Some(p) = placement {
            for st in &mut stmts {
                if let StmtKind::Field(f) = &mut st.kind {
                    f.placement = Some(p.clone());
                }
                if let StmtKind::Match { arms, .. } = &mut st.kind {
                    for arm in arms {
                        for b in &mut arm.body {
                            if let StmtKind::Field(f) = &mut b.kind {
                                f.placement = Some(p.clone());
                            }
                        }
                    }
                }
            }
        }
        if let Some(c) = get(a, "if") {
            let cond = expr_of(c)?;
            out.push(Stmt::synthetic(StmtKind::If {
                cond,
                then: stmts,
                els: vec![],
            }));
        } else {
            out.extend(stmts);
        }
        Ok(())
    }

    fn wrap_if(a: &Yaml, st: Stmt) -> Result<Stmt> {
        Ok(match get(a, "if") {
            Some(c) => Stmt::synthetic(StmtKind::If {
                cond: expr_of(c)?,
                then: vec![st],
                els: vec![],
            }),
            None => st,
        })
    }

    /// Single-field struct used for repeated sized elements.
    fn wrapper(&mut self, what: &str, ty: TypeRef, spec: ArraySpec) -> String {
        let name = self.anon(what);
        self.items
            .push(Stmt::synthetic(StmtKind::TypeDef(TypeDef::Struct(
                StructDef {
                    name: name.clone(),
                    kind: StructKind::Struct,
                    params: vec![],
                    inherits: None,
                    body: vec![field(ty, "value", Some(spec), vec![])],
                    attrs: vec![],
                },
            ))));
        name
    }

    /// Resolves a Kaitai type name (primitive, user type, `type(args)`).
    fn type_ref(&self, name: &str, scope: &str) -> Result<TypeRef> {
        if let Some(p) = prim(name) {
            return Ok(p);
        }
        if bit_width(name).is_some() {
            return Err(Error::Unsupported(format!(
                "bit-sized type `{name}` with `repeat` or `if`"
            )));
        }
        let (base, args) = match name.split_once('(') {
            Some((b, rest)) => {
                let inner = rest
                    .strip_suffix(')')
                    .ok_or_else(|| Error::invalid(format!("bad type `{name}`")))?;
                let mut args = Vec::new();
                for part in split_args(inner) {
                    args.push(expr_str(part.trim())?);
                }
                (b.trim(), args)
            }
            None => (name, Vec::new()),
        };
        let q = self
            .resolve_type(scope, base)
            .ok_or_else(|| Error::invalid(format!("unknown type `{base}`")))?;
        let mut t = TypeRef::named(q);
        t.args = args;
        Ok(t)
    }
}

/// Splits `a, f(b, c), d` at top-level commas.
fn split_args(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    let mut in_str = false;
    for (i, c) in s.char_indices() {
        match c {
            '"' | '\'' => in_str = !in_str,
            '(' | '[' if !in_str => depth += 1,
            ')' | ']' if !in_str => depth -= 1,
            ',' if depth == 0 && !in_str => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if !s[start..].trim().is_empty() {
        out.push(&s[start..]);
    }
    out
}

fn substitute_underscore(e: &mut Expr, id: &str) {
    match &mut e.kind {
        ExprKind::Ident(n) if n == "_" => *n = id.to_string(),
        ExprKind::Unary(_, x)
        | ExprKind::Member(x, _)
        | ExprKind::AddressOf(x)
        | ExprKind::Cast(_, x) => {
            substitute_underscore(x, id);
        }
        ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) => {
            substitute_underscore(a, id);
            substitute_underscore(b, id);
        }
        ExprKind::Ternary(a, b, c) => {
            substitute_underscore(a, id);
            substitute_underscore(b, id);
            substitute_underscore(c, id);
        }
        ExprKind::Call(_, args) => args.iter_mut().for_each(|a| substitute_underscore(a, id)),
        _ => {}
    }
}

/// Bytes remaining in the current (sub)stream.
fn rest_of_stream() -> Expr {
    Expr::binary(
        BinOp::Sub,
        Expr::binary(
            BinOp::Add,
            Expr::call("std::mem::size", vec![]),
            Expr::call("std::mem::base_address", vec![]),
        ),
        Expr::new(ExprKind::Dollar),
    )
}

fn field(ty: TypeRef, name: &str, array: Option<ArraySpec>, attrs: Vec<Attr>) -> Stmt {
    Stmt::synthetic(StmtKind::Field(FieldDecl {
        ty,
        name: name.to_string(),
        array,
        pointer: None,
        placement: None,
        bits: None,
        attrs,
    }))
}

fn doc_attrs(a: &Yaml) -> Vec<Attr> {
    let mut attrs = Vec::new();
    if let Some(d) = get(a, "doc").and_then(s) {
        attrs.push(Attr::string("comment", d.trim()));
    }
    attrs
}

fn context(e: Error, ty: &str, a: &Yaml, i: usize) -> Error {
    let id = get(a, "id")
        .and_then(s)
        .map_or_else(|| format!("#{i}"), |s| format!("`{s}`"));
    match e {
        Error::Invalid(m) => Error::Invalid(format!("{m} (in type `{ty}`, attribute {id})")),
        Error::Unsupported(m) => {
            Error::Unsupported(format!("{m} (in type `{ty}`, attribute {id})"))
        }
        other => other,
    }
}

fn enum_variants(v: &Yaml) -> Result<Vec<EnumVariant>> {
    let map = v
        .as_mapping()
        .ok_or_else(|| Error::invalid("enum must be a mapping"))?;
    let mut out = Vec::new();
    for (k, val) in map {
        let key = if let Some(i) = k.as_integer() {
            i128::from(i)
        } else {
            let t = text(k).ok_or_else(|| Error::invalid("bad enum key"))?;
            match expr_str(&t)?.kind {
                ExprKind::Int(i) => i,
                ExprKind::Unary(UnOp::Neg, x) => match x.kind {
                    ExprKind::Int(i) => -i,
                    _ => return Err(Error::invalid(format!("bad enum key `{t}`"))),
                },
                _ => return Err(Error::invalid(format!("bad enum key `{t}`"))),
            }
        };
        let name = match s(val) {
            Some(n) => n.to_string(),
            None => get(val, "id")
                .and_then(s)
                .ok_or_else(|| Error::invalid("enum value needs a name"))?
                .to_string(),
        };
        out.push(EnumVariant {
            name,
            value: Some(Expr::int(key)),
            end: None,
        });
    }
    Ok(out)
}

fn contents_bytes(y: &Yaml) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut push = |item: &Yaml| -> Result<()> {
        if let Some(i) = item.as_integer() {
            out.push(
                u8::try_from(i)
                    .map_err(|_| Error::invalid(format!("contents byte {i} out of range")))?,
            );
        } else if let Some(t) = item.as_str() {
            out.extend_from_slice(t.as_bytes());
        } else {
            return Err(Error::invalid("contents must be bytes or strings"));
        }
        Ok(())
    };
    match y.as_vec() {
        Some(items) => {
            for i in items {
                push(i)?;
            }
        }
        None => push(y)?,
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_mapping() {
        assert_eq!(prim("u4be").unwrap().endian, Some(Endian::Big));
        assert_eq!(prim("s2").unwrap().name, "s16");
        assert_eq!(prim("f8le").unwrap().name, "double");
        assert!(prim("str").is_none());
        assert_eq!(bit_width("b12"), Some(12));
        assert_eq!(bit_width("b0"), None);
        assert_eq!(bit_width("bytes"), None);
    }

    #[test]
    fn quotes_and_args() {
        assert_eq!(requote("a == 'x\"y'"), "a == \"x\\\"y\"");
        assert_eq!(split_args("a, f(b, c), 'x,y'"), ["a", " f(b, c)", " 'x,y'"]);
    }

    #[test]
    fn errors_are_clear() {
        assert!(from_ksy("").is_err());
        assert!(
            from_ksy("seq: []")
                .unwrap_err()
                .to_string()
                .contains("meta.id")
        );
        let e = from_ksy("meta: {id: x}\nseq:\n  - id: a\n    type: u1\n    process: zlib\n")
            .unwrap_err()
            .to_string();
        assert!(e.contains("process: zlib") && e.contains("`a`"), "{e}");
        let e = from_ksy("meta: {id: x}\nseq:\n  - id: a\n    type: nope\n")
            .unwrap_err()
            .to_string();
        assert!(e.contains("unknown type `nope`"), "{e}");
    }
}
