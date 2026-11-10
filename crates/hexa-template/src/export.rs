//! Template export to Kaitai `.ksy` and C headers (STRC-21).
//!
//! Both exporters translate the parsed [`Program`] statically, without
//! evaluating it against data. Constructs without an equivalent in the
//! target format are reported as [`Error::Unsupported`] naming the construct,
//! the type it appears in and its source line.
//!
//! The module also hosts the type-resolution, constant-folding and naming
//! helpers shared with the parser generators in [`crate::codegen`].

use std::collections::{HashMap, HashSet, VecDeque};

use crate::ast::{
    ArraySpec, Attr, BinOp, BitfieldDef, Endian, EnumDef, Expr, ExprKind, FieldDecl, MatchArm,
    Pattern, Program, SizeOfArg, Stmt, StmtKind, StructDef, StructKind, TypeDef, TypeRef, UnOp,
    find_attr,
};
use crate::prim::Prim;
use crate::{Error, Result};

// ===========================================================================
// Shared helpers (also used by `codegen`)
// ===========================================================================

/// Default byte order of a program (`#pragma endian big`), as the evaluator
/// interprets it.
pub(crate) fn default_endian(program: &Program) -> Endian {
    match program.pragma("endian").map(str::trim) {
        Some("big") => Endian::Big,
        _ => Endian::Little,
    }
}

/// Looks up a type definition by full or namespace-less name.
pub(crate) fn lookup<'a>(program: &'a Program, name: &str) -> Option<&'a TypeDef> {
    if let Some(t) = program.type_def(name) {
        return Some(t);
    }
    let short = name.rsplit("::").next()?;
    program
        .type_defs()
        .find(|t| t.name().rsplit("::").next() == Some(short))
}

/// Source line of a type definition (0 when unknown).
pub(crate) fn def_line(program: &Program, name: &str) -> u32 {
    program
        .items
        .iter()
        .find_map(|s| match &s.kind {
            StmtKind::TypeDef(t) if t.name() == name => Some(s.span.line),
            _ => None,
        })
        .unwrap_or(0)
}

/// What a type reference ultimately denotes.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Base<'a> {
    Prim(Prim),
    Struct(&'a StructDef),
    Enum(&'a EnumDef),
    Bitfield(&'a BitfieldDef),
}

/// A type reference with aliases followed.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Resolved<'a> {
    pub base: Base<'a>,
    /// Innermost explicit byte order (use site, then alias targets), as the
    /// evaluator scopes it.
    pub endian: Option<Endian>,
    /// Array dimension carried by an alias (`typedef char NAME[8];`).
    pub array: Option<&'a Expr>,
}

/// Follows aliases of `ty` down to a primitive, struct, enum or bitfield.
pub(crate) fn resolve<'a>(program: &'a Program, ty: &'a TypeRef) -> Result<Resolved<'a>> {
    let mut endian = ty.endian;
    let mut array: Option<&'a Expr> = None;
    let mut cur = ty;
    for _ in 0..32 {
        if !cur.args.is_empty() {
            return Err(Error::Unsupported(format!(
                "parameterized type `{}<...>`",
                cur.name
            )));
        }
        let base = match lookup(program, &cur.name) {
            Some(TypeDef::Alias(a)) => {
                if let Some(d) = &a.array {
                    if array.is_some() {
                        return Err(Error::Unsupported(format!(
                            "multi-dimensional array alias `{}`",
                            a.name
                        )));
                    }
                    array = Some(d);
                }
                cur = &a.target;
                if cur.endian.is_some() {
                    endian = cur.endian;
                }
                continue;
            }
            Some(TypeDef::Struct(s)) => Base::Struct(s),
            Some(TypeDef::Enum(e)) => Base::Enum(e),
            Some(TypeDef::Bitfield(b)) => Base::Bitfield(b),
            None => match Prim::from_name(&cur.name) {
                Some(p) => Base::Prim(p),
                None => return Err(Error::Invalid(format!("unknown type `{}`", cur.name))),
            },
        };
        return Ok(Resolved {
            base,
            endian,
            array,
        });
    }
    Err(Error::Invalid(format!("alias cycle through `{}`", ty.name)))
}

/// Human-readable location: "in `Name`, line N".
pub(crate) fn place(type_name: &str, line: u32) -> String {
    if line == 0 {
        format!("in `{type_name}`")
    } else {
        format!("in `{type_name}`, line {line}")
    }
}

/// Appends a location to an [`Error::Unsupported`] / [`Error::Invalid`].
pub(crate) fn located(err: Error, place: &str) -> Error {
    match err {
        Error::Unsupported(m) => Error::Unsupported(format!("{m} ({place})")),
        Error::Invalid(m) => Error::Invalid(format!("{m} ({place})")),
        other => other,
    }
}

/// Statements of a struct body with inherited parent bodies first and
/// `{ ... }` blocks flattened.
pub(crate) fn flat_body<'a>(program: &'a Program, s: &'a StructDef) -> Result<Vec<&'a Stmt>> {
    let mut chain = vec![s];
    let mut cur = s;
    while let Some(parent) = &cur.inherits {
        if chain.len() > 16 {
            return Err(Error::Invalid(format!(
                "inheritance cycle through `{}`",
                s.name
            )));
        }
        match resolve(program, parent)?.base {
            Base::Struct(p) => {
                chain.push(p);
                cur = p;
            }
            _ => {
                return Err(Error::Unsupported(format!(
                    "inheritance from non-struct `{}`",
                    parent.name
                )));
            }
        }
    }
    let mut out = Vec::new();
    for def in chain.iter().rev() {
        push_flat(&def.body, &mut out);
    }
    Ok(out)
}

fn push_flat<'a>(stmts: &'a [Stmt], out: &mut Vec<&'a Stmt>) {
    for s in stmts {
        match &s.kind {
            StmtKind::Block(b) => push_flat(b, out),
            _ => out.push(s),
        }
    }
}

/// Borrowed array dimension (field `[...]` or an alias's array).
#[derive(Debug, Clone, Copy)]
pub(crate) enum Dim<'a> {
    Fixed(&'a Expr),
    While(&'a Expr),
    Until(&'a Expr),
    Eof,
}

impl<'a> Dim<'a> {
    pub(crate) fn of(spec: &'a ArraySpec) -> Self {
        match spec {
            ArraySpec::Fixed(e) => Dim::Fixed(e),
            ArraySpec::While(e) => Dim::While(e),
            ArraySpec::Until(e) => Dim::Until(e),
            ArraySpec::Eof => Dim::Eof,
        }
    }
}

/// First string argument of attribute `name`.
pub(crate) fn attr_str<'a>(attrs: &'a [Attr], name: &str) -> Option<&'a str> {
    match &find_attr(attrs, name)?.args.first()?.kind {
        ExprKind::Str(s) => Some(s),
        _ => None,
    }
}

/// One enum variant with its (inclusive) value range.
#[derive(Debug, Clone)]
pub(crate) struct EnumValue {
    pub name: String,
    pub lo: i128,
    pub hi: i128,
}

/// Values of every variant of `e` (implicit values continue from the
/// previous variant, as in the evaluator).
pub(crate) fn enum_values(program: &Program, e: &EnumDef) -> Result<Vec<EnumValue>> {
    enum_values_depth(program, e, 0).map_err(Error::Unsupported)
}

fn enum_values_depth(
    program: &Program,
    e: &EnumDef,
    depth: u32,
) -> std::result::Result<Vec<EnumValue>, String> {
    let mut out: Vec<EnumValue> = Vec::new();
    let mut next: i128 = 0;
    for v in &e.variants {
        let value = |x: &Expr| {
            eval_const(program, x, &out, depth + 1)
                .ok_or_else(|| format!("non-constant value of `{}::{}`", e.name, v.name))
        };
        let lo = match &v.value {
            Some(x) => value(x)?,
            None => next,
        };
        let hi = match &v.end {
            Some(x) => value(x)?,
            None => lo,
        };
        next = hi.wrapping_add(1);
        out.push(EnumValue {
            name: v.name.clone(),
            lo,
            hi,
        });
    }
    Ok(out)
}

/// Folds a constant integer expression (literals, arithmetic, enum
/// constants, `sizeof` of fixed-size types).
pub(crate) fn const_int(program: &Program, e: &Expr) -> Option<i128> {
    eval_const(program, e, &[], 0)
}

fn eval_const(program: &Program, e: &Expr, scope: &[EnumValue], depth: u32) -> Option<i128> {
    if depth > 8 {
        return None;
    }
    let ev = |x: &Expr| eval_const(program, x, scope, depth + 1);
    match &e.kind {
        ExprKind::Int(v) => Some(*v),
        ExprKind::Bool(b) => Some(i128::from(*b)),
        ExprKind::Unary(op, x) => {
            let v = ev(x)?;
            match op {
                UnOp::Neg => v.checked_neg(),
                UnOp::Plus => Some(v),
                UnOp::BitNot => Some(!v),
                UnOp::Not => Some(i128::from(v == 0)),
            }
        }
        ExprKind::Binary(op, l, r) => binary_const(*op, ev(l)?, ev(r)?),
        ExprKind::Ternary(c, a, b) => {
            if ev(c)? != 0 {
                ev(a)
            } else {
                ev(b)
            }
        }
        ExprKind::Ident(name) => scope
            .iter()
            .find(|v| v.name == *name)
            .map(|v| v.lo)
            .or_else(|| enum_const(program, name, depth + 1)),
        ExprKind::SizeOf(SizeOfArg::Type(t)) => {
            size_of_ty(program, t, false, depth + 1).map(i128::from)
        }
        ExprKind::Cast(_, x) => ev(x),
        _ => None,
    }
}

fn binary_const(op: BinOp, a: i128, b: i128) -> Option<i128> {
    let shift = || u32::try_from(b).ok();
    Some(match op {
        BinOp::Add => a.checked_add(b)?,
        BinOp::Sub => a.checked_sub(b)?,
        BinOp::Mul => a.checked_mul(b)?,
        BinOp::Div => a.checked_div(b)?,
        BinOp::Rem => a.checked_rem(b)?,
        BinOp::Shl => a.checked_shl(shift()?)?,
        BinOp::Shr => a.checked_shr(shift()?)?,
        BinOp::BitAnd => a & b,
        BinOp::BitOr => a | b,
        BinOp::BitXor => a ^ b,
        BinOp::Eq => i128::from(a == b),
        BinOp::Ne => i128::from(a != b),
        BinOp::Lt => i128::from(a < b),
        BinOp::Le => i128::from(a <= b),
        BinOp::Gt => i128::from(a > b),
        BinOp::Ge => i128::from(a >= b),
        BinOp::And => i128::from(a != 0 && b != 0),
        BinOp::Or => i128::from(a != 0 || b != 0),
        BinOp::Xor => i128::from((a != 0) != (b != 0)),
    })
}

fn enum_const(program: &Program, path: &str, depth: u32) -> Option<i128> {
    let Some((en, var)) = path.rsplit_once("::") else {
        // Unqualified constant (C / 010 style): first enum declaring it.
        return program.type_defs().find_map(|t| match t {
            TypeDef::Enum(e) if e.variants.iter().any(|v| v.name == path) => {
                enum_values_depth(program, e, depth)
                    .ok()?
                    .into_iter()
                    .find(|v| v.name == path)
                    .map(|v| v.lo)
            }
            _ => None,
        });
    };
    let Some(TypeDef::Enum(e)) = lookup(program, en) else {
        return None;
    };
    enum_values_depth(program, e, depth)
        .ok()?
        .into_iter()
        .find(|v| v.name == var)
        .map(|v| v.lo)
}

/// Size in bytes of a fixed-layout type (`None` when it depends on data).
/// With `bits_ok == false`, types containing bit-fields have no static size.
pub(crate) fn static_size(program: &Program, ty: &TypeRef, bits_ok: bool) -> Option<u64> {
    size_of_ty(program, ty, bits_ok, 0)
}

fn size_of_ty(program: &Program, ty: &TypeRef, bits_ok: bool, depth: u32) -> Option<u64> {
    if depth > 16 {
        return None;
    }
    let r = resolve(program, ty).ok()?;
    let one = match r.base {
        Base::Prim(p) => p.size()?,
        Base::Enum(e) => size_of_ty(program, &e.base, bits_ok, depth + 1)?,
        Base::Bitfield(b) => {
            if !bits_ok {
                return None;
            }
            let mut bits = 0u64;
            for m in &b.members {
                bits = bits.checked_add(u64::try_from(const_int(program, &m.bits)?).ok()?)?;
            }
            bits.div_ceil(8)
        }
        Base::Struct(s) => struct_size(program, s, bits_ok, depth + 1)?,
    };
    match r.array {
        Some(n) => one.checked_mul(u64::try_from(const_int(program, n)?).ok()?),
        None => Some(one),
    }
}

fn struct_size(program: &Program, s: &StructDef, bits_ok: bool, depth: u32) -> Option<u64> {
    if !s.params.is_empty() || find_attr(&s.attrs, "size").is_some() {
        return None;
    }
    let mut total = 0u64;
    for st in flat_body(program, s).ok()? {
        match &st.kind {
            StmtKind::Field(f) => {
                if f.placement.is_some() {
                    continue;
                }
                if f.bits.is_some() || find_attr(&f.attrs, "size").is_some() {
                    return None;
                }
                let elem = match &f.pointer {
                    Some(pt) => size_of_ty(program, pt, bits_ok, depth)?,
                    None => size_of_ty(program, &f.ty, bits_ok, depth)?,
                };
                let n = match &f.array {
                    None => 1,
                    Some(ArraySpec::Fixed(e)) => u64::try_from(const_int(program, e)?).ok()?,
                    Some(_) => return None,
                };
                let size = elem.checked_mul(n)?;
                total = if s.kind == StructKind::Union {
                    total.max(size)
                } else {
                    total.checked_add(size)?
                };
            }
            StmtKind::Let(_) | StmtKind::Assert { .. } | StmtKind::TypeDef(_) => {}
            _ => return None,
        }
    }
    Some(total)
}

/// Splits an identifier into lowercase words (`HeaderV2` → `header`, `v2`;
/// `XMLData` → `xml`, `data`; `fmt::H` → `fmt`, `h`).
pub(crate) fn words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if c.is_ascii_uppercase() && !cur.is_empty() {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(char::is_ascii_lowercase);
            if prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase() && next_lower)
            {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c.to_ascii_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn capitalize(w: &str) -> String {
    let mut c = w.chars();
    c.next().map_or_else(String::new, |f| {
        f.to_ascii_uppercase().to_string() + c.as_str()
    })
}

pub(crate) fn snake_case(name: &str) -> String {
    words(name).join("_")
}

pub(crate) fn upper_snake_case(name: &str) -> String {
    snake_case(name).to_ascii_uppercase()
}

pub(crate) fn pascal_case(name: &str) -> String {
    words(name).iter().map(|w| capitalize(w)).collect()
}

pub(crate) fn camel_case(name: &str) -> String {
    let w = words(name);
    let mut out = String::new();
    for (i, word) in w.iter().enumerate() {
        if i == 0 {
            out.push_str(word);
        } else {
            out.push_str(&capitalize(word));
        }
    }
    out
}

/// Source-like rendering of an expression (for comments and messages).
pub(crate) fn expr_text(e: &Expr) -> String {
    fn wrap(e: &Expr) -> String {
        if matches!(e.kind, ExprKind::Binary(..) | ExprKind::Ternary(..)) {
            format!("({})", expr_text(e))
        } else {
            expr_text(e)
        }
    }
    match &e.kind {
        ExprKind::Int(v) => v.to_string(),
        ExprKind::Float(f) => f.to_string(),
        ExprKind::Bool(b) => b.to_string(),
        ExprKind::Str(s) => format!("{s:?}"),
        ExprKind::Ident(n) => n.clone(),
        ExprKind::Dollar => "$".into(),
        ExprKind::This => "this".into(),
        ExprKind::Parent => "parent".into(),
        ExprKind::Root => "root".into(),
        ExprKind::Unary(op, x) => {
            let sym = match op {
                UnOp::Neg => "-",
                UnOp::Not => "!",
                UnOp::BitNot => "~",
                UnOp::Plus => "+",
            };
            format!("{sym}{}", wrap(x))
        }
        ExprKind::Binary(op, l, r) => format!("{} {} {}", wrap(l), op.symbol(), wrap(r)),
        ExprKind::Ternary(c, a, b) => format!("{} ? {} : {}", wrap(c), wrap(a), wrap(b)),
        ExprKind::Member(b, n) => format!("{}.{n}", wrap(b)),
        ExprKind::Index(a, i) => format!("{}[{}]", wrap(a), expr_text(i)),
        ExprKind::Call(n, args) => format!(
            "{n}({})",
            args.iter().map(expr_text).collect::<Vec<_>>().join(", ")
        ),
        ExprKind::SizeOf(SizeOfArg::Expr(x)) => format!("sizeof({})", expr_text(x)),
        ExprKind::SizeOf(SizeOfArg::Type(t)) => format!("sizeof({})", t.name),
        ExprKind::AddressOf(x) => format!("addressof({})", expr_text(x)),
        ExprKind::Cast(t, x) => format!("({}){}", t.name, wrap(x)),
        ExprKind::TypeArg(t) => t.name.clone(),
    }
}

/// Appends formatted text to a string.
fn put(out: &mut impl std::fmt::Write, args: std::fmt::Arguments<'_>) {
    // Writing into a `String` cannot fail.
    let _ = out.write_fmt(args);
}

fn endian_str(e: Endian) -> &'static str {
    match e {
        Endian::Little => "le",
        Endian::Big => "be",
    }
}

fn endian_word(e: Endian) -> &'static str {
    match e {
        Endian::Little => "little-endian",
        Endian::Big => "big-endian",
    }
}

/// Name of a statement kind that only matters for error messages.
pub(crate) fn stmt_construct(kind: &StmtKind) -> &'static str {
    match kind {
        StmtKind::Field(_) => "field",
        StmtKind::TypeDef(_) => "nested type definition",
        StmtKind::Fn(_) => "function definition",
        StmtKind::Let(_) => "computed field",
        StmtKind::Assign { .. } => "assignment",
        StmtKind::Expr(_) => "expression statement",
        StmtKind::If { .. } => "conditional (`if`)",
        StmtKind::Match { .. } => "`match` statement",
        StmtKind::While { .. } => "`while` loop",
        StmtKind::DoWhile { .. } => "`do ... while` loop",
        StmtKind::For { .. } => "`for` loop",
        StmtKind::Break => "`break`",
        StmtKind::Continue => "`continue`",
        StmtKind::Return(_) => "`return`",
        StmtKind::Assert { .. } => "`assert`",
        StmtKind::Block(_) => "block",
    }
}

// ===========================================================================
// Kaitai Struct
// ===========================================================================

/// Minimal YAML document model, written in block style.
#[derive(Debug, Clone, PartialEq)]
enum Y {
    /// Scalar written verbatim (integers, booleans).
    Raw(String),
    /// String scalar, quoted when needed.
    Str(String),
    Map(Vec<(String, Y)>),
    Seq(Vec<Y>),
}

type Entries = Vec<(String, Y)>;

fn kv(key: &str, value: Y) -> (String, Y) {
    (key.to_string(), value)
}

fn ystr(s: impl Into<String>) -> Y {
    Y::Str(s.into())
}

fn yraw(v: impl std::fmt::Display) -> Y {
    Y::Raw(v.to_string())
}

fn yaml_scalar(s: &str) -> String {
    let plain = s
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        && !matches!(
            s.to_ascii_lowercase().as_str(),
            "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "y" | "n"
        );
    if plain {
        return s.to_string();
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                put(&mut out, format_args!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn yaml_key(k: &str) -> String {
    if k.parse::<i128>().is_ok() {
        k.to_string()
    } else {
        yaml_scalar(k)
    }
}

fn emit_map(entries: &[(String, Y)], indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    for (k, v) in entries {
        out.push_str(&pad);
        out.push_str(&yaml_key(k));
        out.push(':');
        match v {
            Y::Raw(r) => {
                out.push(' ');
                out.push_str(r);
                out.push('\n');
            }
            Y::Str(s) => {
                out.push(' ');
                out.push_str(&yaml_scalar(s));
                out.push('\n');
            }
            Y::Map(m) if m.is_empty() => out.push_str(" {}\n"),
            Y::Seq(s) if s.is_empty() => out.push_str(" []\n"),
            Y::Map(m) => {
                out.push('\n');
                emit_map(m, indent + 2, out);
            }
            Y::Seq(s) => {
                out.push('\n');
                emit_seq(s, indent + 2, out);
            }
        }
    }
}

fn emit_seq(items: &[Y], indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    for item in items {
        match item {
            Y::Map(m) if !m.is_empty() => {
                let mut sub = String::new();
                emit_map(m, indent + 2, &mut sub);
                out.push_str(&pad);
                out.push_str("- ");
                out.push_str(&sub[indent + 2..]);
            }
            Y::Seq(s) if !s.is_empty() => {
                out.push_str(&pad);
                out.push_str("-\n");
                emit_seq(s, indent + 2, out);
            }
            Y::Map(_) => put(out, format_args!("{pad}- {{}}\n")),
            Y::Seq(_) => put(out, format_args!("{pad}- []\n")),
            Y::Raw(r) => put(out, format_args!("{pad}- {r}\n")),
            Y::Str(s) => put(out, format_args!("{pad}- {}\n", yaml_scalar(s))),
        }
    }
}

const KS_RESERVED: &[&str] = &[
    "and",
    "or",
    "not",
    "true",
    "false",
    "as",
    "sizeof",
    "bitsizeof",
];

/// Kaitai identifier (`^[a-z][a-z0-9_]*$`).
fn ks_id(name: &str) -> String {
    let mut id = snake_case(name);
    if id.is_empty() {
        id = "unnamed".into();
    }
    if id.starts_with(|c: char| c.is_ascii_digit()) {
        id.insert_str(0, "f_");
    }
    if KS_RESERVED.contains(&id.as_str()) {
        id.push('_');
    }
    id
}

fn ks_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn ks_unsupported(what: &str, place: &str) -> Error {
    Error::Unsupported(format!(
        "{what} cannot be exported to Kaitai Struct ({place})"
    ))
}

/// Kaitai type name of a primitive read with `endian` inside a type whose
/// default byte order is `ctx`.
fn ks_prim(p: Prim, endian: Endian, ctx: Endian) -> Option<String> {
    let sfx = if endian == ctx {
        ""
    } else {
        endian_str(endian)
    };
    Some(match p {
        Prim::U(1) | Prim::Bool => "u1".into(),
        Prim::S(1) => "s1".into(),
        Prim::U(n @ (2 | 4 | 8)) => format!("u{n}{sfx}"),
        Prim::S(n @ (2 | 4 | 8)) => format!("s{n}{sfx}"),
        Prim::U(n @ (3 | 5 | 6 | 7)) => format!("b{}{}", u32::from(n) * 8, endian_str(endian)),
        Prim::F32 => format!("f4{sfx}"),
        Prim::F64 => format!("f8{sfx}"),
        _ => return None,
    })
}

/// Joins a condition stack with `and`.
fn cond_str(conds: &[String]) -> Option<String> {
    match conds {
        [] => None,
        [one] => Some(one.clone()),
        many => Some(
            many.iter()
                .map(|c| paren_if_compound(c))
                .collect::<Vec<_>>()
                .join(" and "),
        ),
    }
}

fn paren_if_compound(s: &str) -> String {
    if s.contains(' ') {
        format!("({s})")
    } else {
        s.to_string()
    }
}

// Kaitai expression precedence (Python-like), lowest first.
const P_TERNARY: u8 = 1;
const P_OR: u8 = 2;
const P_AND: u8 = 3;
const P_NOT: u8 = 4;
const P_CMP: u8 = 5;
const P_BOR: u8 = 6;
const P_BXOR: u8 = 7;
const P_BAND: u8 = 8;
const P_SHIFT: u8 = 9;
const P_ADD: u8 = 10;
const P_MUL: u8 = 11;
const P_UNARY: u8 = 12;
const P_POSTFIX: u8 = 13;
const P_ATOM: u8 = 14;

#[derive(Debug, Clone, Copy)]
enum Pending<'a> {
    Struct(&'a StructDef),
    Bitfield(&'a BitfieldDef),
}

/// Output of one template struct.
struct Body {
    name: String,
    endian: Endian,
    seq: Vec<Y>,
    instances: Entries,
    ids: HashSet<String>,
}

impl Body {
    fn claim(&mut self, id: &str, place: &str) -> Result<()> {
        if self.ids.insert(id.to_string()) {
            Ok(())
        } else {
            Err(ks_unsupported(
                &format!("field `{id}` declared more than once (ids must be unique)"),
                place,
            ))
        }
    }
}

struct Ksy<'a> {
    program: &'a Program,
    default: Endian,
    ids: HashMap<(String, Endian), String>,
    taken: HashSet<String>,
    queue: VecDeque<(Pending<'a>, Endian, String)>,
}

/// Exports `program` as a Kaitai Struct specification whose top-level type
/// is the struct named `root`.
///
/// Supported: structs (including inheritance, flattened), enums, bitfields
/// (as `bN` members), primitives (`u1`..`u8`, `s1`..`s8`, `f4`, `f8`, odd
/// unsigned widths as `bN`), `le`/`be` and `#pragma endian`, fixed / `[]` /
/// `[until(...)]` arrays, `char` strings and NUL-terminated `string`,
/// `padding`, conditional fields (`if:`), `match` (as `switch-on` when every
/// arm declares the same field, else as `if:` conditions), computed `auto`
/// fields and placed fields (`instances`), `[[comment]]` (`doc`) and
/// `[[size]]`. `assert`s are dropped. Unions, loops, functions, pointers,
/// `[while(...)]` arrays and non-constant bit widths are rejected.
pub fn to_ksy(program: &Program, root: &str) -> Result<String> {
    let Some(TypeDef::Struct(rs)) = lookup(program, root) else {
        return Err(Error::Invalid(format!(
            "root type `{root}` is not a struct of the template"
        )));
    };
    let default = default_endian(program);
    let root_id = ks_id(&rs.name);
    let mut k = Ksy {
        program,
        default,
        ids: HashMap::new(),
        taken: HashSet::new(),
        queue: VecDeque::new(),
    };
    k.ids.insert((rs.name.clone(), default), root_id.clone());
    k.taken.insert(root_id.clone());

    let e = endian_str(default);
    let mut meta = vec![kv("id", ystr(root_id))];
    if let Some(title) = program.pragma("description").filter(|t| !t.is_empty()) {
        meta.push(kv("title", ystr(title.trim_matches('"'))));
    }
    meta.push(kv("endian", ystr(e)));
    meta.push(kv("bit-endian", ystr(e)));
    let mut top = vec![kv("meta", Y::Map(meta))];
    top.extend(k.struct_entries(rs, default)?);

    let mut types = Vec::new();
    while let Some((pending, endian, id)) = k.queue.pop_front() {
        let mut entries = Vec::new();
        if endian != default {
            let e = endian_str(endian);
            entries.push(kv(
                "meta",
                Y::Map(vec![kv("endian", ystr(e)), kv("bit-endian", ystr(e))]),
            ));
        }
        entries.extend(match pending {
            Pending::Struct(s) => k.struct_entries(s, endian)?,
            Pending::Bitfield(b) => k.bitfield_entries(b)?,
        });
        types.push((id, Y::Map(entries)));
    }
    if !types.is_empty() {
        top.push(kv("types", Y::Map(types)));
    }

    let mut enums = Vec::new();
    for def in program.type_defs() {
        if let TypeDef::Enum(en) = def {
            enums.push((ks_id(&en.name), k.enum_entry(en)?));
        }
    }
    if !enums.is_empty() {
        top.push(kv("enums", Y::Map(enums)));
    }

    let mut out = String::new();
    emit_map(&top, 0, &mut out);
    Ok(out)
}

impl<'a> Ksy<'a> {
    fn type_id(&mut self, name: &str, pending: Pending<'a>, endian: Endian) -> String {
        let key = (name.to_string(), endian);
        if let Some(id) = self.ids.get(&key) {
            return id.clone();
        }
        let mut base = ks_id(name);
        if endian != self.default {
            base.push('_');
            base.push_str(endian_str(endian));
        }
        let mut id = base.clone();
        let mut n = 2;
        while !self.taken.insert(id.clone()) {
            id = format!("{base}_{n}");
            n += 1;
        }
        self.ids.insert(key, id.clone());
        self.queue.push_back((pending, endian, id.clone()));
        id
    }

    fn struct_entries(&mut self, s: &'a StructDef, endian: Endian) -> Result<Entries> {
        let at = place(&s.name, def_line(self.program, &s.name));
        if s.kind == StructKind::Union {
            return Err(ks_unsupported(&format!("union `{}`", s.name), &at));
        }
        if !s.params.is_empty() {
            return Err(ks_unsupported(
                &format!("parameterized struct `{}`", s.name),
                &at,
            ));
        }
        if find_attr(&s.attrs, "size").is_some() {
            return Err(ks_unsupported(
                &format!("`[[size]]` on struct `{}`", s.name),
                &at,
            ));
        }
        let stmts = flat_body(self.program, s).map_err(|e| located(e, &at))?;
        let mut b = Body {
            name: s.name.clone(),
            endian,
            seq: Vec::new(),
            instances: Vec::new(),
            ids: HashSet::new(),
        };
        self.stmts(&mut b, &stmts, &mut Vec::new())?;
        let mut out = Vec::new();
        if let Some(doc) = attr_str(&s.attrs, "comment") {
            out.push(kv("doc", ystr(doc)));
        }
        if !b.seq.is_empty() {
            out.push(kv("seq", Y::Seq(b.seq)));
        }
        if !b.instances.is_empty() {
            out.push(kv("instances", Y::Map(b.instances)));
        }
        Ok(out)
    }

    fn stmts(&mut self, b: &mut Body, stmts: &[&'a Stmt], conds: &mut Vec<String>) -> Result<()> {
        for st in stmts {
            let at = place(&b.name, st.span.line);
            match &st.kind {
                StmtKind::Field(f) => self.field(b, f, &at, conds)?,
                StmtKind::Let(l) => {
                    let Some(init) = &l.init else {
                        return Err(ks_unsupported(
                            &format!("local variable `{}` without initializer", l.name),
                            &at,
                        ));
                    };
                    if l.array.is_some() {
                        return Err(ks_unsupported(&format!("local array `{}`", l.name), &at));
                    }
                    let id = ks_id(&l.name);
                    b.claim(&id, &at)?;
                    let mut inst = vec![kv("value", ystr(self.expr(init, &at)?))];
                    if let Some(c) = cond_str(conds) {
                        inst.push(kv("if", ystr(c)));
                    }
                    if let Some(doc) = attr_str(&l.attrs, "comment") {
                        inst.push(kv("doc", ystr(doc)));
                    }
                    b.instances.push((id, Y::Map(inst)));
                }
                StmtKind::If { cond, then, els } => {
                    let c = self.expr(cond, &at)?;
                    conds.push(c.clone());
                    self.stmts(b, &then.iter().collect::<Vec<_>>(), conds)?;
                    conds.pop();
                    if !els.is_empty() {
                        conds.push(format!("not {}", paren_if_compound(&c)));
                        self.stmts(b, &els.iter().collect::<Vec<_>>(), conds)?;
                        conds.pop();
                    }
                }
                StmtKind::Match { scrutinee, arms } => {
                    self.match_stmt(b, scrutinee, arms, &at, conds)?;
                }
                StmtKind::Block(body) => {
                    self.stmts(b, &body.iter().collect::<Vec<_>>(), conds)?;
                }
                StmtKind::Assert { .. } | StmtKind::TypeDef(_) => {}
                other => return Err(ks_unsupported(stmt_construct(other), &at)),
            }
        }
        Ok(())
    }

    fn field(&mut self, b: &mut Body, f: &'a FieldDecl, at: &str, conds: &[String]) -> Result<()> {
        let (mut props, padding) = self.field_props(f, b.endian, at)?;
        if let Some(c) = cond_str(conds) {
            props.push(kv("if", ystr(c)));
        }
        if let Some(doc) = attr_str(&f.attrs, "comment") {
            props.push(kv("doc", ystr(doc)));
        }
        if let Some(pos) = &f.placement {
            let id = ks_id(&f.name);
            b.claim(&id, at)?;
            let mut inst = vec![kv("pos", ystr(self.expr(pos, at)?))];
            inst.extend(props);
            b.instances.push((id, Y::Map(inst)));
        } else {
            let mut item = Vec::new();
            if !padding {
                let id = ks_id(&f.name);
                b.claim(&id, at)?;
                item.push(kv("id", ystr(id)));
            }
            item.extend(props);
            b.seq.push(Y::Map(item));
        }
        Ok(())
    }

    /// Properties of a field except `id`, `if` and `doc`; the flag marks
    /// padding (emitted without an id).
    fn field_props(
        &mut self,
        f: &'a FieldDecl,
        endian: Endian,
        at: &str,
    ) -> Result<(Entries, bool)> {
        if f.pointer.is_some() {
            return Err(ks_unsupported(&format!("pointer field `{}`", f.name), at));
        }
        if let Some(bits) = &f.bits {
            let w = const_int(self.program, bits)
                .filter(|w| (1..=64).contains(w))
                .ok_or_else(|| {
                    ks_unsupported(
                        &format!("bit-field `{}` with a non-constant width", f.name),
                        at,
                    )
                })?;
            if f.array.is_some() {
                return Err(ks_unsupported(
                    &format!("array of bit-fields `{}`", f.name),
                    at,
                ));
            }
            return Ok((vec![kv("type", ystr(format!("b{w}")))], false));
        }
        let r = resolve(self.program, &f.ty).map_err(|e| located(e, at))?;
        let eff = r.endian.unwrap_or(endian);
        let array = match (&f.array, r.array) {
            (Some(a), None) => Some(Dim::of(a)),
            (None, Some(e)) => Some(Dim::Fixed(e)),
            (None, None) => None,
            (Some(_), Some(_)) => {
                return Err(ks_unsupported(
                    &format!("array of array type `{}`", f.ty.name),
                    at,
                ));
            }
        };
        let mut props = Vec::new();
        let mut padding = false;
        let mut repeat = true;
        match r.base {
            Base::Prim(Prim::Char) => {
                props.push(kv("type", ystr("str")));
                props.extend(self.sized(array, 1, "char", at)?);
                props.push(kv("encoding", ystr("ASCII")));
                repeat = false;
            }
            Base::Prim(Prim::Char16) => {
                props.push(kv("type", ystr("str")));
                props.extend(self.sized(array, 2, "char16", at)?);
                let enc = match eff {
                    Endian::Little => "UTF-16LE",
                    Endian::Big => "UTF-16BE",
                };
                props.push(kv("encoding", ystr(enc)));
                repeat = false;
            }
            Base::Prim(Prim::Padding) => {
                props.extend(self.sized(array, 1, "padding", at)?);
                padding = true;
                repeat = false;
            }
            Base::Prim(Prim::CStr) => {
                props.push(kv("type", ystr("strz")));
                props.push(kv("encoding", ystr("ASCII")));
            }
            Base::Prim(p) => {
                let t = ks_prim(p, eff, endian)
                    .ok_or_else(|| ks_unsupported(&format!("primitive type `{}`", p.name()), at))?;
                props.push(kv("type", ystr(t)));
            }
            Base::Enum(e) => {
                let base = resolve(self.program, &e.base).map_err(|x| located(x, at))?;
                let base_endian = base.endian.or(r.endian).unwrap_or(endian);
                let t = match base.base {
                    Base::Prim(p) if p.is_int() => ks_prim(p, base_endian, endian),
                    _ => None,
                }
                .ok_or_else(|| ks_unsupported(&format!("base type of enum `{}`", e.name), at))?;
                props.push(kv("type", ystr(t)));
                props.push(kv("enum", ystr(ks_id(&e.name))));
            }
            Base::Struct(s) => {
                let id = self.type_id(&s.name, Pending::Struct(s), eff);
                props.push(kv("type", ystr(id)));
            }
            Base::Bitfield(bf) => {
                let id = self.type_id(&bf.name, Pending::Bitfield(bf), eff);
                props.push(kv("type", ystr(id)));
            }
        }
        if repeat && let Some(a) = array {
            match a {
                Dim::Fixed(n) => {
                    props.push(kv("repeat", ystr("expr")));
                    props.push(kv("repeat-expr", self.count(n, 1, at)?));
                }
                Dim::Eof => props.push(kv("repeat", ystr("eos"))),
                Dim::Until(c) => {
                    props.push(kv("repeat", ystr("until")));
                    props.push(kv("repeat-until", ystr(self.expr(c, at)?)));
                }
                Dim::While(_) => {
                    return Err(ks_unsupported(
                        &format!(
                            "`[while(...)]` array `{}` (Kaitai has no repeat-while)",
                            f.name
                        ),
                        at,
                    ));
                }
            }
        }
        if let Some(sz) = find_attr(&f.attrs, "size").and_then(|a| a.args.first())
            && !props.iter().any(|(k, _)| k == "size" || k == "size-eos")
        {
            props.push(kv("size", self.count(sz, 1, at)?));
        }
        Ok((props, padding))
    }

    /// `size`/`size-eos` of a byte-sized run (`char`, `char16`, `padding`).
    fn sized(
        &mut self,
        array: Option<Dim<'a>>,
        scale: i128,
        what: &str,
        at: &str,
    ) -> Result<Entries> {
        Ok(match array {
            None => vec![kv("size", yraw(scale))],
            Some(Dim::Fixed(n)) => vec![kv("size", self.count(n, scale, at)?)],
            Some(Dim::Eof) => vec![kv("size-eos", yraw("true"))],
            Some(Dim::While(_) | Dim::Until(_)) => {
                return Err(ks_unsupported(
                    &format!("`[while(...)]`/`[until(...)]` {what} array"),
                    at,
                ));
            }
        })
    }

    /// A count expression multiplied by `scale`, folded when constant.
    fn count(&mut self, n: &'a Expr, scale: i128, at: &str) -> Result<Y> {
        if let Some(v) = const_int(self.program, n).and_then(|v| v.checked_mul(scale)) {
            return Ok(yraw(v));
        }
        if scale == 1 {
            return Ok(ystr(self.expr(n, at)?));
        }
        Ok(ystr(format!("{} * {scale}", self.sub(n, P_MUL, at)?)))
    }

    fn match_stmt(
        &mut self,
        b: &mut Body,
        scrutinee: &'a [Expr],
        arms: &'a [MatchArm],
        at: &str,
        conds: &[String],
    ) -> Result<()> {
        if self.try_switch(b, scrutinee, arms, at, conds)? {
            return Ok(());
        }
        let subjects = scrutinee
            .iter()
            .map(|s| self.expr(s, at))
            .collect::<Result<Vec<_>>>()?;
        let mut prev: Vec<String> = Vec::new();
        for arm in arms {
            if arm.patterns.len() != subjects.len() {
                return Err(Error::Invalid(format!(
                    "match arm has {} patterns for {} values ({at})",
                    arm.patterns.len(),
                    subjects.len()
                )));
            }
            let mut terms = Vec::new();
            for (s, p) in subjects.iter().zip(&arm.patterns) {
                if let Some(t) = self.pattern_cond(s, p, at)? {
                    terms.push(t);
                }
            }
            let cond = cond_str(&terms);
            let mut local = conds.to_vec();
            local.extend(prev.iter().map(|p| format!("not {}", paren_if_compound(p))));
            if let Some(c) = &cond {
                local.push(c.clone());
            }
            self.stmts(b, &arm.body.iter().collect::<Vec<_>>(), &mut local)?;
            match cond {
                Some(c) => prev.push(c),
                None => break,
            }
        }
        Ok(())
    }

    fn pattern_cond(&mut self, subject: &str, p: &'a Pattern, at: &str) -> Result<Option<String>> {
        let s = paren_if_compound(subject);
        Ok(match p {
            Pattern::Wildcard => None,
            Pattern::Value(v) => Some(format!("{s} == {}", self.sub(v, P_SHIFT, at)?)),
            Pattern::Range(lo, hi) => Some(format!(
                "{s} >= {} and {s} <= {}",
                self.sub(lo, P_SHIFT, at)?,
                self.sub(hi, P_SHIFT, at)?
            )),
            Pattern::Or(alts) => {
                let mut parts = Vec::new();
                for a in alts {
                    match self.pattern_cond(subject, a, at)? {
                        Some(c) => parts.push(paren_if_compound(&c)),
                        None => return Ok(None),
                    }
                }
                Some(parts.join(" or "))
            }
        })
    }

    /// `match (x) { (1): T a; (2 | 3): U a; (_): V a; }` → one field with
    /// `type: switch-on`. Returns `false` when the shape does not fit.
    fn try_switch(
        &mut self,
        b: &mut Body,
        scrutinee: &'a [Expr],
        arms: &'a [MatchArm],
        at: &str,
        conds: &[String],
    ) -> Result<bool> {
        let [subject] = scrutinee else {
            return Ok(false);
        };
        let mut fields: Vec<&'a FieldDecl> = Vec::new();
        for arm in arms {
            let [
                Stmt {
                    kind: StmtKind::Field(f),
                    ..
                },
            ] = arm.body.as_slice()
            else {
                return Ok(false);
            };
            let simple = |p: &Pattern| matches!(p, Pattern::Value(_) | Pattern::Wildcard);
            let ok = match arm.patterns.as_slice() {
                [Pattern::Or(alts)] => alts.iter().all(simple),
                [p] => simple(p),
                _ => false,
            };
            if !ok
                || f.placement.is_some()
                || fields.first().is_some_and(|first| first.name != f.name)
            {
                return Ok(false);
            }
            fields.push(f);
        }
        let Some(first) = fields.first().copied() else {
            return Ok(false);
        };
        let mut rest: Option<Entries> = None;
        let mut cases: Entries = Vec::new();
        let mut seen = HashSet::new();
        for (arm, f) in arms.iter().zip(&fields) {
            let (props, padding) = self.field_props(f, b.endian, at)?;
            if padding {
                return Ok(false);
            }
            let mut ty = None;
            let mut others = Vec::new();
            for (k, v) in props {
                if k == "type" {
                    ty = Some(v);
                } else {
                    others.push((k, v));
                }
            }
            let Some(ty) = ty else {
                return Ok(false);
            };
            match &rest {
                Some(r) if *r != others => return Ok(false),
                Some(_) => {}
                None => rest = Some(others),
            }
            let pats: Vec<&Pattern> = match &arm.patterns[0] {
                Pattern::Or(alts) => alts.iter().collect(),
                p => vec![p],
            };
            for p in pats {
                let key = match p {
                    Pattern::Value(v) => self.expr(v, at)?,
                    _ => "_".to_string(),
                };
                if seen.insert(key.clone()) {
                    cases.push((key, ty.clone()));
                }
            }
        }
        let id = ks_id(&first.name);
        b.claim(&id, at)?;
        let switch = Y::Map(vec![
            kv("switch-on", ystr(self.expr(subject, at)?)),
            kv("cases", Y::Map(cases)),
        ]);
        let mut item = vec![kv("id", ystr(id)), kv("type", switch)];
        item.extend(rest.unwrap_or_default());
        if let Some(c) = cond_str(conds) {
            item.push(kv("if", ystr(c)));
        }
        if let Some(doc) = attr_str(&first.attrs, "comment") {
            item.push(kv("doc", ystr(doc)));
        }
        b.seq.push(Y::Map(item));
        Ok(true)
    }

    fn bitfield_entries(&mut self, bf: &'a BitfieldDef) -> Result<Entries> {
        let at = place(&bf.name, def_line(self.program, &bf.name));
        let mut seq = Vec::new();
        let mut total = 0i128;
        for m in &bf.members {
            let w = const_int(self.program, &m.bits)
                .filter(|w| (0..=64).contains(w))
                .ok_or_else(|| {
                    ks_unsupported(
                        &format!(
                            "bitfield member `{}` with a non-constant width",
                            m.name.as_deref().unwrap_or("padding")
                        ),
                        &at,
                    )
                })?;
            if w == 0 {
                continue;
            }
            total += w;
            let mut item = Vec::new();
            if let Some(n) = &m.name {
                item.push(kv("id", ystr(ks_id(n))));
            }
            item.push(kv("type", ystr(format!("b{w}"))));
            if let Some(t) = &m.ty {
                match resolve(self.program, t).map_err(|e| located(e, &at))?.base {
                    Base::Enum(e) => item.push(kv("enum", ystr(ks_id(&e.name)))),
                    Base::Prim(p) if p.is_int() || p == Prim::Bool => {}
                    _ => {
                        return Err(ks_unsupported(
                            &format!("bitfield member of type `{}`", t.name),
                            &at,
                        ));
                    }
                }
            }
            if let Some(doc) = attr_str(&m.attrs, "comment") {
                item.push(kv("doc", ystr(doc)));
            }
            seq.push(Y::Map(item));
        }
        if total % 8 != 0 {
            // Bitfields occupy whole bytes; consume the rest explicitly so a
            // following bitfield starts on a byte boundary.
            seq.push(Y::Map(vec![kv(
                "type",
                ystr(format!("b{}", 8 - total % 8)),
            )]));
        }
        let mut out = Vec::new();
        if let Some(doc) = attr_str(&bf.attrs, "comment") {
            out.push(kv("doc", ystr(doc)));
        }
        out.push(kv("seq", Y::Seq(seq)));
        Ok(out)
    }

    fn enum_entry(&self, e: &EnumDef) -> Result<Y> {
        let at = place(&e.name, def_line(self.program, &e.name));
        let values = enum_values(self.program, e).map_err(|x| located(x, &at))?;
        let mut seen = HashSet::new();
        let mut map = Vec::new();
        for v in values {
            if !seen.insert(v.lo) {
                continue;
            }
            let id = ks_id(&v.name);
            let value = if v.hi == v.lo {
                ystr(id)
            } else {
                Y::Map(vec![
                    kv("id", ystr(id)),
                    kv("doc", ystr(format!("range {} ... {}", v.lo, v.hi))),
                ])
            };
            map.push((v.lo.to_string(), value));
        }
        Ok(Y::Map(map))
    }

    // ----- expressions -----------------------------------------------------

    fn expr(&mut self, e: &'a Expr, at: &str) -> Result<String> {
        Ok(self.ex(e, at)?.0)
    }

    fn sub(&mut self, e: &'a Expr, min: u8, at: &str) -> Result<String> {
        let (s, p) = self.ex(e, at)?;
        Ok(if p < min { format!("({s})") } else { s })
    }

    fn ex(&mut self, e: &'a Expr, at: &str) -> Result<(String, u8)> {
        Ok(match &e.kind {
            ExprKind::Int(v) => (v.to_string(), if *v < 0 { P_UNARY } else { P_ATOM }),
            ExprKind::Float(f) => (format!("{f:?}"), P_ATOM),
            ExprKind::Bool(b) => (b.to_string(), P_ATOM),
            ExprKind::Str(s) => (ks_string(s), P_ATOM),
            ExprKind::Ident(n) => (self.ident(n, at)?, P_ATOM),
            ExprKind::Dollar => ("_io.pos".into(), P_POSTFIX),
            ExprKind::Parent => ("_parent".into(), P_ATOM),
            ExprKind::Root => ("_root".into(), P_ATOM),
            ExprKind::This => return Err(ks_unsupported("bare `this`", at)),
            ExprKind::Unary(op, x) => match op {
                UnOp::Not => (format!("not {}", self.sub(x, P_UNARY, at)?), P_NOT),
                UnOp::Neg => (format!("-{}", self.sub(x, P_UNARY, at)?), P_UNARY),
                UnOp::BitNot => (format!("~{}", self.sub(x, P_UNARY, at)?), P_UNARY),
                UnOp::Plus => return self.ex(x, at),
            },
            ExprKind::Binary(op, l, r) => {
                let (sym, p) = match op {
                    BinOp::Add => ("+", P_ADD),
                    BinOp::Sub => ("-", P_ADD),
                    BinOp::Mul => ("*", P_MUL),
                    BinOp::Div => ("/", P_MUL),
                    BinOp::Rem => ("%", P_MUL),
                    BinOp::Shl => ("<<", P_SHIFT),
                    BinOp::Shr => (">>", P_SHIFT),
                    BinOp::BitAnd => ("&", P_BAND),
                    BinOp::BitOr => ("|", P_BOR),
                    BinOp::BitXor => ("^", P_BXOR),
                    BinOp::Eq | BinOp::Xor => ("==", P_CMP),
                    BinOp::Ne => ("!=", P_CMP),
                    BinOp::Lt => ("<", P_CMP),
                    BinOp::Le => ("<=", P_CMP),
                    BinOp::Gt => (">", P_CMP),
                    BinOp::Ge => (">=", P_CMP),
                    BinOp::And => ("and", P_AND),
                    BinOp::Or => ("or", P_OR),
                };
                // Logical xor of booleans is inequality.
                let sym = if *op == BinOp::Xor { "!=" } else { sym };
                if p == P_CMP {
                    // Parenthesize bitwise operands: C and Kaitai disagree
                    // on their precedence relative to comparisons.
                    let ls = self.sub(l, P_SHIFT, at)?;
                    let rs = self.sub(r, P_SHIFT, at)?;
                    (format!("{ls} {sym} {rs}"), p)
                } else {
                    let ls = self.sub(l, p, at)?;
                    let rs = self.sub(r, p + 1, at)?;
                    (format!("{ls} {sym} {rs}"), p)
                }
            }
            ExprKind::Ternary(c, a, b) => {
                let cs = self.sub(c, P_OR, at)?;
                let a_s = self.sub(a, P_OR, at)?;
                let bs = self.sub(b, P_TERNARY, at)?;
                (format!("{cs} ? {a_s} : {bs}"), P_TERNARY)
            }
            ExprKind::Member(base, name) => match base.kind {
                ExprKind::This => (ks_id(name), P_ATOM),
                _ => (
                    format!("{}.{}", self.sub(base, P_POSTFIX, at)?, ks_id(name)),
                    P_POSTFIX,
                ),
            },
            ExprKind::Index(a, i) => (
                format!("{}[{}]", self.sub(a, P_POSTFIX, at)?, self.expr(i, at)?),
                P_POSTFIX,
            ),
            ExprKind::Call(name, args) => match (name.as_str(), args.as_slice()) {
                ("std::mem::size", []) => ("_io.size".into(), P_POSTFIX),
                ("std::mem::eof", []) => ("_io.eof".into(), P_POSTFIX),
                ("std::string::length", [s]) => {
                    (format!("{}.length", self.sub(s, P_POSTFIX, at)?), P_POSTFIX)
                }
                _ => return Err(ks_unsupported(&format!("call to `{name}()`"), at)),
            },
            ExprKind::SizeOf(SizeOfArg::Expr(x)) => (
                format!("{}._sizeof", self.sub(x, P_POSTFIX, at)?),
                P_POSTFIX,
            ),
            ExprKind::SizeOf(SizeOfArg::Type(t)) => {
                if let Some(n) = static_size(self.program, t, true) {
                    (n.to_string(), P_ATOM)
                } else if let Ok(Resolved {
                    base: Base::Struct(s),
                    array: None,
                    ..
                }) = resolve(self.program, t)
                {
                    let id = self.type_id(&s.name, Pending::Struct(s), self.default);
                    (format!("sizeof<{id}>"), P_ATOM)
                } else {
                    return Err(ks_unsupported(&format!("`sizeof({})`", t.name), at));
                }
            }
            ExprKind::AddressOf(_) => return Err(ks_unsupported("`addressof`", at)),
            ExprKind::Cast(_, x) => return self.ex(x, at),
            ExprKind::TypeArg(t) => {
                return Err(ks_unsupported(&format!("type argument `{}`", t.name), at));
            }
        })
    }

    fn ident(&self, n: &str, at: &str) -> Result<String> {
        if n == "_" {
            return Ok("_".into());
        }
        if let Some((en, var)) = n.rsplit_once("::") {
            return match lookup(self.program, en) {
                Some(TypeDef::Enum(e)) => Ok(format!("{}::{}", ks_id(&e.name), ks_id(var))),
                _ => Err(ks_unsupported(&format!("qualified name `{n}`"), at)),
            };
        }
        Ok(ks_id(n))
    }
}

// ===========================================================================
// C header
// ===========================================================================

const C_KEYWORDS: &[&str] = &[
    "auto",
    "bool",
    "break",
    "case",
    "char",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "false",
    "float",
    "for",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "register",
    "restrict",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "true",
    "typedef",
    "union",
    "unsigned",
    "void",
    "volatile",
    "while",
    "_Bool",
    "_Static_assert",
];

/// C identifier for a template name (`ns::Foo` → `ns_Foo`).
fn c_ident(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    if out.is_empty() {
        out.push_str("unnamed");
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    if C_KEYWORDS.contains(&out.as_str()) {
        out.push('_');
    }
    out
}

/// Text safe inside a `/* ... */` comment.
fn c_comment(s: &str) -> String {
    s.replace("*/", "* /").replace(['\n', '\r'], " ")
}

fn c_unsupported(what: &str, place: &str) -> Error {
    Error::Unsupported(format!(
        "{what} cannot be expressed in a C header ({place})"
    ))
}

/// A C type at a use site: `{name} field{dims}{suffix}`.
struct CRef {
    name: String,
    /// Extra trailing dimension (`[3]` for `u24`).
    suffix: String,
    notes: Vec<String>,
    /// NUL-terminated string: only representable as a flexible array member.
    tail: bool,
}

struct CHeader<'a> {
    program: &'a Program,
    default: Endian,
    body: String,
    /// Definition state by template name: `false` while being emitted.
    state: HashMap<String, bool>,
    /// Structs ending in a flexible array member.
    tail_types: HashSet<String>,
    uses_bool: bool,
}

/// Exports every type definition of `program` as packed C declarations:
/// include guard, `<stdint.h>` types, `#pragma pack(push, 1)`, enums as a
/// typedef of the base integer plus enumeration constants, structs and
/// unions, bitfields as C bit-fields, fixed arrays with folded constant
/// sizes, a variable-length (or NUL-terminated) last field as a flexible
/// array member, `[[comment]]`s, and `_Static_assert`s for fixed sizes.
/// Types are emitted in dependency order. Conditionals, `match`, loops and
/// variable-length fields that are not last are rejected.
pub fn to_c_header(program: &Program) -> Result<String> {
    let mut c = CHeader {
        program,
        default: default_endian(program),
        body: String::new(),
        state: HashMap::new(),
        tail_types: HashSet::new(),
        uses_bool: false,
    };
    for def in program.type_defs() {
        c.emit(def)?;
    }
    let guard = "HEXA_TEMPLATE_H_";
    let mut out = String::new();
    out.push_str("/* Generated by Hexa from a structure template. */\n");
    if let Some(d) = program.pragma("description").filter(|d| !d.is_empty()) {
        put(
            &mut out,
            format_args!("/* {} */\n", c_comment(d.trim_matches('"'))),
        );
    }
    put(
        &mut out,
        format_args!(
            "/* Multi-byte fields are {} unless noted; layouts are packed. */\n\n",
            endian_word(c.default)
        ),
    );
    put(
        &mut out,
        format_args!("#ifndef {guard}\n#define {guard}\n\n"),
    );
    if c.uses_bool {
        out.push_str("#include <stdbool.h>\n");
    }
    out.push_str("#include <stdint.h>\n\n#pragma pack(push, 1)\n\n");
    out.push_str(&c.body);
    put(
        &mut out,
        format_args!("#pragma pack(pop)\n\n#endif /* {guard} */\n"),
    );
    Ok(out)
}

impl<'a> CHeader<'a> {
    fn emit(&mut self, def: &'a TypeDef) -> Result<()> {
        match self.state.get(def.name()) {
            Some(true) => return Ok(()),
            Some(false) => {
                let at = place(def.name(), def_line(self.program, def.name()));
                return Err(c_unsupported(
                    &format!("type `{}` containing itself", def.name()),
                    &at,
                ));
            }
            None => {}
        }
        self.state.insert(def.name().to_string(), false);
        let text = match def {
            TypeDef::Struct(s) => self.c_struct(s)?,
            TypeDef::Enum(e) => self.c_enum(e)?,
            TypeDef::Bitfield(b) => self.c_bitfield(b)?,
            TypeDef::Alias(a) => {
                let at = place(&a.name, def_line(self.program, &a.name));
                let t = self.c_type_ref(&a.target, &at)?;
                let dims = match &a.array {
                    None => String::new(),
                    Some(e) => match const_int(self.program, e) {
                        Some(n) if n >= 0 => format!("[{n}]"),
                        _ => "[]".to_string(),
                    },
                };
                let dims = if t.tail { format!("{dims}[]") } else { dims };
                let mut notes = t.notes;
                if let Some(c) = attr_str(&a.attrs, "comment") {
                    notes.insert(0, c_comment(c));
                }
                format!(
                    "typedef {} {}{dims}{};{}\n",
                    t.name,
                    c_ident(&a.name),
                    t.suffix,
                    trailing(&notes)
                )
            }
        };
        self.body.push_str(&text);
        self.body.push('\n');
        self.state.insert(def.name().to_string(), true);
        Ok(())
    }

    fn c_type_ref(&mut self, ty: &'a TypeRef, at: &str) -> Result<CRef> {
        if !ty.args.is_empty() {
            return Err(c_unsupported(
                &format!("parameterized type `{}<...>`", ty.name),
                at,
            ));
        }
        if let Some(def) = lookup(self.program, &ty.name) {
            self.emit(def)?;
            if self.tail_types.contains(def.name()) {
                return Err(c_unsupported(
                    &format!(
                        "embedding `{}`, which ends with a variable-length field",
                        def.name()
                    ),
                    at,
                ));
            }
            let notes = ty
                .endian
                .filter(|e| *e != self.default)
                .map(|e| vec![endian_word(e).to_string()])
                .unwrap_or_default();
            return Ok(CRef {
                name: c_ident(def.name()),
                suffix: String::new(),
                notes,
                tail: false,
            });
        }
        let p = Prim::from_name(&ty.name)
            .ok_or_else(|| Error::Invalid(format!("unknown type `{}` ({at})", ty.name)))?;
        let mut notes = Vec::new();
        if let Some(e) = ty.endian
            && e != self.default
            && p.size().is_some_and(|s| s > 1)
        {
            notes.push(endian_word(e).to_string());
        }
        let mut suffix = String::new();
        let mut tail = false;
        let name = match p {
            Prim::U(1) | Prim::Padding => "uint8_t".to_string(),
            Prim::U(n @ (2 | 4 | 8)) => format!("uint{}_t", u32::from(n) * 8),
            Prim::S(n @ (1 | 2 | 4 | 8)) => format!("int{}_t", u32::from(n) * 8),
            Prim::U(n) | Prim::S(n) => {
                suffix = format!("[{n}]");
                notes.push(p.name());
                "uint8_t".to_string()
            }
            Prim::F32 => "float".to_string(),
            Prim::F64 => "double".to_string(),
            Prim::F16 => {
                notes.push("IEEE 754 half precision".into());
                "uint16_t".to_string()
            }
            Prim::Char => "char".to_string(),
            Prim::Char16 => "uint16_t".to_string(),
            Prim::Bool => {
                self.uses_bool = true;
                "bool".to_string()
            }
            Prim::CStr => {
                tail = true;
                notes.push("NUL-terminated".into());
                "char".to_string()
            }
            Prim::WStr => {
                tail = true;
                notes.push("NUL-terminated UTF-16".into());
                "uint16_t".to_string()
            }
        };
        Ok(CRef {
            name,
            suffix,
            notes,
            tail,
        })
    }

    fn c_struct(&mut self, s: &'a StructDef) -> Result<String> {
        let line = def_line(self.program, &s.name);
        let at = place(&s.name, line);
        if !s.params.is_empty() {
            return Err(c_unsupported(
                &format!("parameterized struct `{}`", s.name),
                &at,
            ));
        }
        if find_attr(&s.attrs, "size").is_some() {
            return Err(c_unsupported(
                &format!("`[[size]]` on struct `{}`", s.name),
                &at,
            ));
        }
        let union = s.kind == StructKind::Union;
        let mut st = CStructOut {
            name: s.name.clone(),
            union,
            lines: Vec::new(),
            names: HashMap::new(),
            tail: None,
            bits: false,
        };
        if let Some(parent) = &s.inherits {
            let t = self.c_type_ref(parent, &at)?;
            st.lines.push(format!(
                "    {} base; /* inherited from {} */",
                t.name, parent.name
            ));
        }
        self.c_members(&mut st, &s.body.iter().collect::<Vec<_>>())?;
        let name = c_ident(&s.name);
        let kw = if union { "union" } else { "struct" };
        let mut out = String::new();
        if let Some(c) = attr_str(&s.attrs, "comment") {
            put(&mut out, format_args!("/* {} */\n", c_comment(c)));
        }
        put(&mut out, format_args!("typedef {kw} {name} {{\n"));
        for l in &st.lines {
            out.push_str(l);
            out.push('\n');
        }
        put(&mut out, format_args!("}} {name};\n"));
        if st.tail.is_some() {
            self.tail_types.insert(s.name.clone());
        } else if !st.bits
            && let Some(n) = static_size(self.program, &TypeRef::named(s.name.clone()), false)
            && n > 0
        {
            put(
                &mut out,
                format_args!(
                    "_Static_assert(sizeof({name}) == {n}, \"{name} must be {n} bytes\");\n"
                ),
            );
        }
        Ok(out)
    }

    fn c_members(&mut self, st: &mut CStructOut, stmts: &[&'a Stmt]) -> Result<()> {
        for s in stmts {
            let at = place(&st.name, s.span.line);
            if let Some(t) = &st.tail
                && matches!(&s.kind, StmtKind::Field(f) if f.placement.is_none())
            {
                return Err(c_unsupported(
                    &format!("variable-length field `{t}` that is not the last field"),
                    &at,
                ));
            }
            match &s.kind {
                StmtKind::Field(f) => self.c_member(st, f, &at)?,
                StmtKind::Let(l) => st.lines.push(format!(
                    "    /* computed: {} (not stored) */",
                    c_comment(&l.name)
                )),
                StmtKind::Block(b) => self.c_members(st, &b.iter().collect::<Vec<_>>())?,
                StmtKind::Assert { .. } | StmtKind::TypeDef(_) => {}
                other => return Err(c_unsupported(stmt_construct(other), &at)),
            }
        }
        Ok(())
    }

    fn c_member(&mut self, st: &mut CStructOut, f: &'a FieldDecl, at: &str) -> Result<()> {
        if f.placement.is_some() {
            st.lines.push(format!(
                "    /* {} is placed at an absolute offset (not part of this layout) */",
                c_comment(&f.name)
            ));
            return Ok(());
        }
        let name = st.unique(&f.name);
        let mut notes = Vec::new();
        if let Some(c) = attr_str(&f.attrs, "comment") {
            notes.push(c_comment(c));
        }
        let t = match &f.pointer {
            Some(pt) => {
                notes.push(format!("offset of {}", c_comment(&f.ty.name)));
                self.c_type_ref(pt, at)?
            }
            None => self.c_type_ref(&f.ty, at)?,
        };
        notes.extend(t.notes.iter().cloned());
        if let Some(bits) = &f.bits {
            let w = const_int(self.program, bits)
                .filter(|w| *w >= 0)
                .ok_or_else(|| {
                    c_unsupported(
                        &format!("bit-field `{}` with a non-constant width", f.name),
                        at,
                    )
                })?;
            if !t.suffix.is_empty() || t.tail || f.array.is_some() {
                return Err(c_unsupported(
                    &format!("bit-field `{}` of this type", f.name),
                    at,
                ));
            }
            st.bits = true;
            st.lines
                .push(format!("    {} {name} : {w};{}", t.name, trailing(&notes)));
            return Ok(());
        }
        let mut variable = t.tail;
        let dims = match f.array.as_ref().map(Dim::of) {
            None => String::new(),
            Some(Dim::Fixed(e)) => match const_int(self.program, e) {
                Some(n) if n >= 0 => format!("[{n}]"),
                _ => {
                    variable = true;
                    notes.push(format!("length: {}", c_comment(&expr_text(e))));
                    "[]".to_string()
                }
            },
            Some(spec) => {
                variable = true;
                notes.push(match spec {
                    Dim::While(c) => format!("while {}", c_comment(&expr_text(c))),
                    Dim::Until(c) => format!("until {}", c_comment(&expr_text(c))),
                    Dim::Fixed(_) | Dim::Eof => "until the end of the data".to_string(),
                });
                "[]".to_string()
            }
        };
        if t.tail && f.array.is_some() {
            return Err(c_unsupported(
                &format!("array of NUL-terminated strings `{}`", f.name),
                at,
            ));
        }
        let dims = if t.tail { "[]".to_string() } else { dims };
        if variable {
            if st.union {
                return Err(c_unsupported(
                    &format!("variable-length field `{}` in a union", f.name),
                    at,
                ));
            }
            if st.lines.is_empty() {
                return Err(c_unsupported(
                    &format!("variable-length field `{}` as the only field", f.name),
                    at,
                ));
            }
            st.tail = Some(f.name.clone());
        }
        st.lines.push(format!(
            "    {} {name}{dims}{};{}",
            t.name,
            t.suffix,
            trailing(&notes)
        ));
        Ok(())
    }

    fn c_enum(&mut self, e: &'a EnumDef) -> Result<String> {
        let at = place(&e.name, def_line(self.program, &e.name));
        let base = resolve(self.program, &e.base).map_err(|x| located(x, &at))?;
        let Base::Prim(p) = base.base else {
            return Err(c_unsupported(
                &format!("base type of enum `{}`", e.name),
                &at,
            ));
        };
        if !p.is_int() || !matches!(p.size(), Some(1 | 2 | 4 | 8)) {
            return Err(c_unsupported(
                &format!("base type of enum `{}`", e.name),
                &at,
            ));
        }
        let base_t = self.c_type_ref(&e.base, &at)?;
        let name = c_ident(&e.name);
        let prefix = upper_snake_case(&e.name);
        let values = enum_values(self.program, e).map_err(|x| located(x, &at))?;
        let mut out = String::new();
        if let Some(c) = attr_str(&e.attrs, "comment") {
            put(&mut out, format_args!("/* {} */\n", c_comment(c)));
        }
        put(
            &mut out,
            format_args!(
                "typedef {} {name};{}\n",
                base_t.name,
                trailing(&base_t.notes)
            ),
        );
        let mut used = HashSet::new();
        let mut consts = Vec::new();
        let mut defines = Vec::new();
        for v in &values {
            let mut cname = format!("{prefix}_{}", upper_snake_case(&v.name));
            let stem = cname.clone();
            let mut n = 2;
            while !used.insert(cname.clone()) {
                cname = format!("{stem}_{n}");
                n += 1;
            }
            let note = if v.hi == v.lo {
                String::new()
            } else {
                format!(" /* ... {} */", v.hi)
            };
            if i32::try_from(v.lo).is_ok() {
                consts.push(format!("    {cname} = {},{note}", v.lo));
            } else {
                let sfx = if v.lo < 0 { "LL" } else { "ULL" };
                defines.push(format!("#define {cname} (({name}){}{sfx}){note}", v.lo));
            }
        }
        if !consts.is_empty() {
            out.push_str("enum {\n");
            for c in consts {
                out.push_str(&c);
                out.push('\n');
            }
            out.push_str("};\n");
        }
        for d in defines {
            out.push_str(&d);
            out.push('\n');
        }
        Ok(out)
    }

    fn c_bitfield(&mut self, b: &'a BitfieldDef) -> Result<String> {
        let at = place(&b.name, def_line(self.program, &b.name));
        let mut widths = Vec::new();
        let mut total = 0i128;
        for m in &b.members {
            let w = const_int(self.program, &m.bits)
                .filter(|w| (0..=64).contains(w))
                .ok_or_else(|| {
                    c_unsupported(
                        &format!(
                            "bitfield member `{}` with a non-constant width",
                            m.name.as_deref().unwrap_or("padding")
                        ),
                        &at,
                    )
                })?;
            total += w;
            widths.push(w);
        }
        let storage = match total {
            0..=8 => "uint8_t",
            9..=16 => "uint16_t",
            17..=32 => "uint32_t",
            33..=64 => "uint64_t",
            _ => {
                return Err(c_unsupported(
                    &format!("bitfield `{}` wider than 64 bits", b.name),
                    &at,
                ));
            }
        };
        let name = c_ident(&b.name);
        let mut out = String::new();
        if let Some(c) = attr_str(&b.attrs, "comment") {
            put(&mut out, format_args!("/* {} */\n", c_comment(c)));
        }
        let order = match self.default {
            Endian::Little => "least",
            Endian::Big => "most",
        };
        put(
            &mut out,
            format_args!(
                "/* Bit-field allocation is compiler-defined; the template reads members from the \
             {order} significant bit. */\n"
            ),
        );
        put(&mut out, format_args!("typedef struct {name} {{\n"));
        let mut names: HashMap<String, usize> = HashMap::new();
        for (m, w) in b.members.iter().zip(widths) {
            let mut notes = Vec::new();
            if let Some(t) = &m.ty {
                notes.push(c_comment(&t.name));
                if let Ok(Resolved {
                    base: Base::Enum(e),
                    ..
                }) = resolve(self.program, t)
                    && let Some(def) = lookup(self.program, &e.name)
                {
                    self.emit(def)?;
                }
            }
            if let Some(c) = attr_str(&m.attrs, "comment") {
                notes.insert(0, c_comment(c));
            }
            match &m.name {
                Some(n) if w > 0 => {
                    let n = unique_name(&mut names, n);
                    put(
                        &mut out,
                        format_args!("    {storage} {n} : {w};{}\n", trailing(&notes)),
                    );
                }
                _ => put(&mut out, format_args!("    {storage} : {w};\n")),
            }
        }
        put(&mut out, format_args!("}} {name};\n"));
        Ok(out)
    }
}

struct CStructOut {
    name: String,
    union: bool,
    lines: Vec<String>,
    names: HashMap<String, usize>,
    /// Name of the variable-length field, once seen.
    tail: Option<String>,
    bits: bool,
}

impl CStructOut {
    fn unique(&mut self, name: &str) -> String {
        unique_name(&mut self.names, name)
    }
}

fn unique_name(names: &mut HashMap<String, usize>, name: &str) -> String {
    let base = c_ident(name);
    let n = names.entry(base.clone()).or_insert(0);
    *n += 1;
    if *n == 1 { base } else { format!("{base}_{n}") }
}

fn trailing(notes: &[String]) -> String {
    if notes.is_empty() {
        String::new()
    } else {
        format!(" /* {} */", notes.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    #[test]
    fn naming() {
        assert_eq!(snake_case("HeaderV2"), "header_v2");
        assert_eq!(snake_case("XMLData"), "xml_data");
        assert_eq!(snake_case("fmt::H"), "fmt_h");
        assert_eq!(pascal_case("num_items"), "NumItems");
        assert_eq!(camel_case("NumItems"), "numItems");
        assert_eq!(upper_snake_case("Read"), "READ");
        assert_eq!(ks_id("2nd"), "f_2nd");
        assert_eq!(ks_id("not"), "not_");
        assert_eq!(c_ident("int"), "int_");
        assert_eq!(c_ident("ns::Foo"), "ns_Foo");
    }

    #[test]
    fn yaml_quoting() {
        assert_eq!(yaml_scalar("u4"), "u4");
        assert_eq!(yaml_scalar("count * 2"), "\"count * 2\"");
        assert_eq!(yaml_scalar("yes"), "\"yes\"");
        assert_eq!(yaml_scalar("a \"b\""), "\"a \\\"b\\\"\"");
        assert_eq!(yaml_key("-1"), "-1");
    }

    #[test]
    fn constants_and_sizes() {
        let p = parse("enum E : u16 { A = 2, B, C = B * 4 }; struct S { u32 a; E e; u8 b[C]; };")
            .expect("parse");
        let Some(TypeDef::Enum(e)) = p.type_def("E") else {
            panic!()
        };
        let v = enum_values(&p, e).expect("values");
        assert_eq!(v.iter().map(|v| v.lo).collect::<Vec<_>>(), [2, 3, 12]);
        assert_eq!(const_int(&p, &Expr::ident("E::C")), Some(12));
        assert_eq!(static_size(&p, &TypeRef::named("S"), false), Some(18));
    }
}
