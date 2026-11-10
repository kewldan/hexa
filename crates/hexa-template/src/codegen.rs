//! Parser generation for Python, Go, TypeScript and Rust (STRC-22).
//!
//! [`generate`] turns a template struct into a self-contained parser in the
//! chosen language. The supported subset is the part of the template
//! language with a static layout: structs of primitives (8- to 64-bit
//! integers, `float`, `double`, `bool`, `char`), fixed arrays, arrays counted
//! by earlier integer fields of the same struct (optionally with `+ - *` and
//! division / remainder by a non-zero constant), `char` strings (fixed or
//! counted), NUL-terminated `string`s, nested structs and arrays of them
//! (struct inheritance is flattened), enums, per-field `le`/`be`, `#pragma
//! endian` and `padding[n]`. Everything else (conditionals, `match`, loops,
//! pointers, placement, unions, bitfields, `[while]`/`[until]`/`[]` arrays,
//! functions, computed fields, ...) is reported as [`Error::Unsupported`]
//! naming the construct, the struct and the source line.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::ast::{
    BinOp, Endian, EnumDef, Expr, ExprKind, FieldDecl, Program, StructDef, StructKind, TypeDef,
    UnOp, find_attr,
};
use crate::export::{
    Base, Dim, attr_str, camel_case, const_int, def_line, default_endian, enum_values, expr_text,
    flat_body, located, lookup, pascal_case, place, resolve, snake_case, stmt_construct,
    upper_snake_case,
};
use crate::prim::Prim;
use crate::{Error, Result};

/// Target language of [`generate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Language {
    Python,
    Go,
    TypeScript,
    Rust,
}

impl Language {
    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            Language::Python => "Python",
            Language::Go => "Go",
            Language::TypeScript => "TypeScript",
            Language::Rust => "Rust",
        }
    }

    /// Conventional file extension (without the dot).
    pub fn extension(self) -> &'static str {
        match self {
            Language::Python => "py",
            Language::Go => "go",
            Language::TypeScript => "ts",
            Language::Rust => "rs",
        }
    }
}

/// Generates a parser for the struct named `root` of `program`.
///
/// * Python: dataclasses with `parse_from(data, offset)` classmethods using
///   `struct.unpack_from`, and a module-level `parse(data: bytes) -> Root`.
/// * Go: package `parser` with plain structs and
///   `func Parse<Root>(data []byte) (*Root, error)` over `encoding/binary`.
/// * TypeScript: interfaces and `parse<Root>(view: DataView, offset = 0)`
///   returning `{ value, size }`.
/// * Rust: `#[derive(Debug, Clone, PartialEq)]` structs with
///   `fn parse(data: &[u8]) -> Result<(Self, usize), ParseError>` and a
///   cursor-based `fn read(data, &mut offset)`; no external crates.
///
/// Every read is bounds-checked and reports the failing offset.
pub fn generate(program: &Program, root: &str, lang: Language) -> Result<String> {
    let model = Model::build(program, root, lang)?;
    Ok(match lang {
        Language::Python => Python::new(&model).emit(),
        Language::Go => Go::new(&model).emit(),
        Language::TypeScript => Ts::new(&model).emit(),
        Language::Rust => Rs::new(&model).emit(),
    })
}

// ===========================================================================
// Language-neutral model
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scalar {
    U(u8),
    S(u8),
    F32,
    F64,
    Bool,
    Char,
}

impl Scalar {
    fn from_prim(p: Prim) -> Option<Scalar> {
        Some(match p {
            Prim::U(n @ (1 | 2 | 4 | 8)) => Scalar::U(n),
            Prim::S(n @ (1 | 2 | 4 | 8)) => Scalar::S(n),
            Prim::F32 => Scalar::F32,
            Prim::F64 => Scalar::F64,
            Prim::Bool => Scalar::Bool,
            Prim::Char => Scalar::Char,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            Scalar::U(n) | Scalar::S(n) => usize::from(n),
            Scalar::F32 => 4,
            Scalar::F64 => 8,
            Scalar::Bool | Scalar::Char => 1,
        }
    }

    fn is_int(self) -> bool {
        matches!(self, Scalar::U(_) | Scalar::S(_))
    }

    fn bits(self) -> usize {
        self.size() * 8
    }

    /// Inclusive value range of an integer scalar.
    fn range(self) -> (i128, i128) {
        let bits = u32::try_from(self.bits()).unwrap_or(64);
        match self {
            Scalar::S(_) => (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1),
            _ => (0, (1i128 << bits) - 1),
        }
    }
}

/// One element read from the data.
#[derive(Debug, Clone, Copy)]
enum Elem {
    /// Scalar with its explicit byte order (`None` inherits the struct's).
    Scalar(Scalar, Option<Endian>),
    Enum(usize, Option<Endian>),
    Struct(usize, Option<Endian>),
    /// NUL-terminated 8-bit string.
    CStr,
}

#[derive(Debug, Clone, Copy)]
enum CBin {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// Array size computed from earlier fields.
#[derive(Debug, Clone)]
enum CExpr {
    Lit(i128),
    /// Index of an earlier integer field of the same struct.
    Field(usize),
    Bin(CBin, Box<CExpr>, Box<CExpr>),
}

#[derive(Debug, Clone)]
enum Count {
    Const(u64),
    Expr(CExpr),
}

#[derive(Debug, Clone)]
enum Kind {
    One(Elem),
    Array(Elem, Count),
    /// `char s[n]`: Latin-1 text up to the first NUL.
    Chars(Count),
    /// `padding[n]`.
    Skip(Count),
}

#[derive(Debug, Clone)]
struct Field {
    name: String,
    kind: Kind,
    doc: Option<String>,
}

#[derive(Debug, Clone)]
struct StructModel {
    name: String,
    doc: Option<String>,
    fields: Vec<Field>,
    /// Byte orders this struct is decoded with (default order first).
    instances: Vec<Endian>,
}

#[derive(Debug, Clone)]
struct Variant {
    name: String,
    value: i128,
    /// Upper bound of a range variant.
    end: Option<i128>,
}

#[derive(Debug, Clone)]
struct EnumModel {
    name: String,
    doc: Option<String>,
    base: Scalar,
    /// Byte order carried by the base type (`enum E : be u16`).
    endian: Option<Endian>,
    variants: Vec<Variant>,
}

#[derive(Debug, Clone)]
struct Model {
    default: Endian,
    root: usize,
    structs: Vec<StructModel>,
    enums: Vec<EnumModel>,
}

impl Model {
    fn build(program: &Program, root: &str, lang: Language) -> Result<Model> {
        let Some(TypeDef::Struct(rs)) = lookup(program, root) else {
            return Err(Error::Invalid(format!(
                "root type `{root}` is not a struct of the template"
            )));
        };
        let mut b = Builder {
            program,
            lang,
            structs: Vec::new(),
            struct_ix: HashMap::new(),
            enums: Vec::new(),
            enum_ix: HashMap::new(),
        };
        let at = place(&rs.name, def_line(program, &rs.name));
        let root = b.struct_index(rs, &at)?;
        let mut m = Model {
            default: default_endian(program),
            root,
            structs: b.structs,
            enums: b.enums,
        };
        m.check_cycles(lang)?;
        m.collect_instances();
        Ok(m)
    }

    /// Rejects structs that contain themselves without an array.
    fn check_cycles(&self, lang: Language) -> Result<()> {
        for start in 0..self.structs.len() {
            let mut stack = vec![start];
            let mut seen = HashSet::new();
            while let Some(i) = stack.pop() {
                for f in &self.structs[i].fields {
                    if let Kind::One(Elem::Struct(j, _)) = f.kind {
                        if j == start {
                            return Err(Error::Unsupported(format!(
                                "struct `{}` containing itself is not supported by the {} \
                                 generator",
                                self.structs[start].name,
                                lang.name()
                            )));
                        }
                        if seen.insert(j) {
                            stack.push(j);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn collect_instances(&mut self) {
        let mut queue = vec![(self.root, self.default)];
        while let Some((i, e)) = queue.pop() {
            if self.structs[i].instances.contains(&e) {
                continue;
            }
            self.structs[i].instances.push(e);
            for f in &self.structs[i].fields {
                if let Kind::One(Elem::Struct(j, fe)) | Kind::Array(Elem::Struct(j, fe), _) = f.kind
                {
                    queue.push((j, fe.unwrap_or(e)));
                }
            }
        }
        let default = self.default;
        for s in &mut self.structs {
            s.instances.sort_by_key(|e| *e != default);
        }
    }

    /// Byte order of an element read inside an instance with order `inst`.
    fn elem_endian(e: Elem, inst: Endian) -> Endian {
        match e {
            Elem::Scalar(_, x) | Elem::Struct(_, x) | Elem::Enum(_, x) => x.unwrap_or(inst),
            Elem::CStr => inst,
        }
    }

    /// Scalar read for a simple element (scalar or enum).
    fn scalar_of(&self, e: Elem) -> Option<Scalar> {
        match e {
            Elem::Scalar(s, _) => Some(s),
            Elem::Enum(i, _) => Some(self.enums[i].base),
            _ => None,
        }
    }

    /// Minimum encoded size of a struct (for bounding element counts).
    fn min_size(&self, i: usize, depth: usize) -> usize {
        if depth > 16 {
            return 0;
        }
        self.structs[i]
            .fields
            .iter()
            .map(|f| match &f.kind {
                Kind::One(e) => self.elem_min(*e, depth),
                Kind::Array(e, Count::Const(n)) => self
                    .elem_min(*e, depth)
                    .saturating_mul(usize::try_from(*n).unwrap_or(0)),
                Kind::Chars(Count::Const(n)) | Kind::Skip(Count::Const(n)) => {
                    usize::try_from(*n).unwrap_or(0)
                }
                _ => 0,
            })
            .fold(0usize, usize::saturating_add)
    }

    fn elem_min(&self, e: Elem, depth: usize) -> usize {
        match e {
            Elem::Scalar(s, _) => s.size(),
            Elem::Enum(i, _) => self.enums[i].base.size(),
            Elem::Struct(j, _) => self.min_size(j, depth + 1),
            Elem::CStr => 1,
        }
    }
}

struct Builder<'a> {
    program: &'a Program,
    lang: Language,
    structs: Vec<StructModel>,
    struct_ix: HashMap<String, usize>,
    enums: Vec<EnumModel>,
    enum_ix: HashMap<String, usize>,
}

impl<'a> Builder<'a> {
    fn unsupported(&self, what: &str, at: &str) -> Error {
        Error::Unsupported(format!(
            "{what} is not supported by the {} generator ({at})",
            self.lang.name()
        ))
    }

    fn struct_index(&mut self, s: &'a StructDef, at: &str) -> Result<usize> {
        if let Some(&i) = self.struct_ix.get(&s.name) {
            return Ok(i);
        }
        if s.kind == StructKind::Union {
            return Err(self.unsupported(&format!("union `{}`", s.name), at));
        }
        if !s.params.is_empty() {
            return Err(self.unsupported(&format!("parameterized struct `{}`", s.name), at));
        }
        if find_attr(&s.attrs, "size").is_some() {
            return Err(self.unsupported(&format!("`[[size]]` on struct `{}`", s.name), at));
        }
        let i = self.structs.len();
        self.structs.push(StructModel {
            name: s.name.clone(),
            doc: attr_str(&s.attrs, "comment").map(str::to_string),
            fields: Vec::new(),
            instances: Vec::new(),
        });
        self.struct_ix.insert(s.name.clone(), i);
        let sat = place(&s.name, def_line(self.program, &s.name));
        let stmts = flat_body(self.program, s).map_err(|e| located(e, &sat))?;
        let mut fields: Vec<Field> = Vec::new();
        for st in stmts {
            let at = place(&s.name, st.span.line);
            match &st.kind {
                crate::ast::StmtKind::Field(f) => {
                    let field = self.field(f, &fields, &at)?;
                    fields.push(field);
                }
                crate::ast::StmtKind::TypeDef(_) => {}
                other => return Err(self.unsupported(stmt_construct(other), &at)),
            }
        }
        self.structs[i].fields = fields;
        Ok(i)
    }

    fn field(&mut self, f: &'a FieldDecl, prev: &[Field], at: &str) -> Result<Field> {
        let n = &f.name;
        if f.pointer.is_some() {
            return Err(self.unsupported(&format!("pointer field `{n}`"), at));
        }
        if f.placement.is_some() {
            return Err(self.unsupported(&format!("placed field `{n}` (`@`)"), at));
        }
        if f.bits.is_some() {
            return Err(self.unsupported(&format!("bit-field member `{n}`"), at));
        }
        if find_attr(&f.attrs, "size").is_some() {
            return Err(self.unsupported(&format!("`[[size]]` on field `{n}`"), at));
        }
        let r = resolve(self.program, &f.ty).map_err(|e| located(e, at))?;
        let dim = match (&f.array, r.array) {
            (Some(a), None) => Some(Dim::of(a)),
            (None, Some(e)) => Some(Dim::Fixed(e)),
            (None, None) => None,
            (Some(_), Some(_)) => {
                return Err(self.unsupported(&format!("multi-dimensional array `{n}`"), at));
            }
        };
        let count = match dim {
            None => None,
            Some(Dim::Fixed(e)) => Some(self.count(e, prev, n, at)?),
            Some(Dim::While(_)) => {
                return Err(self.unsupported(&format!("`[while(...)]` array `{n}`"), at));
            }
            Some(Dim::Until(_)) => {
                return Err(self.unsupported(&format!("`[until(...)]` array `{n}`"), at));
            }
            Some(Dim::Eof) => {
                return Err(
                    self.unsupported(&format!("`[]` array `{n}` (until the end of the data)"), at)
                );
            }
        };
        let e = r.endian;
        let wrap = |elem: Elem, count: Option<Count>| match count {
            Some(c) => Kind::Array(elem, c),
            None => Kind::One(elem),
        };
        let kind = match r.base {
            Base::Prim(Prim::Char) => match count {
                Some(c) => Kind::Chars(c),
                None => Kind::One(Elem::Scalar(Scalar::Char, e)),
            },
            Base::Prim(Prim::Padding) => Kind::Skip(count.unwrap_or(Count::Const(1))),
            Base::Prim(Prim::CStr) => wrap(Elem::CStr, count),
            Base::Prim(p) => {
                let s = Scalar::from_prim(p).ok_or_else(|| {
                    self.unsupported(&format!("primitive type `{}` of `{n}`", p.name()), at)
                })?;
                wrap(Elem::Scalar(s, e), count)
            }
            Base::Enum(en) => {
                let i = self.enum_index(en)?;
                wrap(Elem::Enum(i, self.enums[i].endian.or(e)), count)
            }
            Base::Struct(s) => wrap(Elem::Struct(self.struct_index(s, at)?, e), count),
            Base::Bitfield(b) => {
                return Err(self.unsupported(&format!("bitfield `{}`", b.name), at));
            }
        };
        Ok(Field {
            name: f.name.clone(),
            kind,
            doc: attr_str(&f.attrs, "comment").map(str::to_string),
        })
    }

    fn count(&self, e: &Expr, prev: &[Field], field: &str, at: &str) -> Result<Count> {
        if let Some(v) = const_int(self.program, e) {
            return u64::try_from(v).map(Count::Const).map_err(|_| {
                Error::Invalid(format!("negative array size {v} of `{field}` ({at})"))
            });
        }
        self.cexpr(e, prev).map(Count::Expr).ok_or_else(|| {
            self.unsupported(
                &format!(
                    "array size `{}` of `{field}` (only constants and arithmetic on earlier \
                     integer fields)",
                    expr_text(e)
                ),
                at,
            )
        })
    }

    fn cexpr(&self, e: &Expr, prev: &[Field]) -> Option<CExpr> {
        if let Some(v) = const_int(self.program, e) {
            return Some(CExpr::Lit(v));
        }
        match &e.kind {
            ExprKind::Ident(n) => field_ref(prev, n),
            ExprKind::Member(b, n) if matches!(b.kind, ExprKind::This) => field_ref(prev, n),
            ExprKind::Unary(UnOp::Plus, x) => self.cexpr(x, prev),
            ExprKind::Binary(op, l, r) => {
                let op = match op {
                    BinOp::Add => CBin::Add,
                    BinOp::Sub => CBin::Sub,
                    BinOp::Mul => CBin::Mul,
                    BinOp::Div => CBin::Div,
                    BinOp::Rem => CBin::Rem,
                    _ => return None,
                };
                let rhs = self.cexpr(r, prev)?;
                if matches!(op, CBin::Div | CBin::Rem) && !matches!(rhs, CExpr::Lit(v) if v != 0) {
                    return None;
                }
                Some(CExpr::Bin(
                    op,
                    Box::new(self.cexpr(l, prev)?),
                    Box::new(rhs),
                ))
            }
            _ => None,
        }
    }

    fn enum_index(&mut self, e: &'a EnumDef) -> Result<usize> {
        if let Some(&i) = self.enum_ix.get(&e.name) {
            return Ok(i);
        }
        let at = place(&e.name, def_line(self.program, &e.name));
        let base = resolve(self.program, &e.base).map_err(|x| located(x, &at))?;
        let scalar = match base.base {
            Base::Prim(p) => Scalar::from_prim(p).filter(|s| s.is_int()),
            _ => None,
        }
        .ok_or_else(|| {
            self.unsupported(
                &format!("base type `{}` of enum `{}`", e.base.name, e.name),
                &at,
            )
        })?;
        let (lo, hi) = scalar.range();
        let mut variants = Vec::new();
        for v in enum_values(self.program, e).map_err(|x| located(x, &at))? {
            if v.lo < lo || v.lo > hi {
                return Err(Error::Invalid(format!(
                    "value {} of `{}::{}` does not fit the enum's base type ({at})",
                    v.lo, e.name, v.name
                )));
            }
            variants.push(Variant {
                name: v.name,
                value: v.lo,
                end: (v.hi != v.lo).then_some(v.hi),
            });
        }
        let i = self.enums.len();
        self.enums.push(EnumModel {
            name: e.name.clone(),
            doc: attr_str(&e.attrs, "comment").map(str::to_string),
            base: scalar,
            endian: base.endian,
            variants,
        });
        self.enum_ix.insert(e.name.clone(), i);
        Ok(i)
    }
}

/// Latest earlier field named `name`, if it is an integer.
fn field_ref(prev: &[Field], name: &str) -> Option<CExpr> {
    let i = prev.iter().rposition(|f| f.name == name)?;
    match prev[i].kind {
        Kind::One(Elem::Scalar(s, _)) if s.is_int() => Some(CExpr::Field(i)),
        _ => None,
    }
}

// ===========================================================================
// Naming
// ===========================================================================

/// Identifiers chosen for one target language.
struct Names {
    structs: Vec<String>,
    enums: Vec<String>,
    /// Per struct, per field (empty for padding).
    fields: Vec<Vec<String>>,
    /// Per struct, per field: local variable used while decoding.
    locals: Vec<Vec<String>>,
    variants: Vec<Vec<String>>,
}

const PY_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];
/// Names the generated Python code uses in parse methods.
const PY_RESERVED: &[&str] = &[
    "data",
    "offset",
    "cls",
    "value",
    "struct",
    "enum",
    "dataclass",
    "range",
    "list",
    "bytes",
    "len",
    "max",
];
const TS_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "as",
    "implements",
    "interface",
    "let",
    "package",
    "private",
    "protected",
    "public",
    "static",
    "yield",
    "await",
    "any",
    "boolean",
    "number",
    "string",
    "symbol",
    "type",
    "of",
    "undefined",
    "arguments",
    "eval",
    "async",
    "get",
    "set",
];
const TS_RESERVED: &[&str] = &[
    "view", "offset", "start", "need", "count", "chars", "cstr", "i", "r", "Number", "String",
    "Math",
];
const TS_TYPES: &[&str] = &[
    "ParseError",
    "Parsed",
    "DataView",
    "Error",
    "Uint8Array",
    "Array",
    "Number",
    "String",
    "Object",
    "Math",
    "BigInt",
    "Boolean",
    "Symbol",
    "Record",
    "Map",
    "Set",
    "Date",
    "Promise",
];
const RS_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
];
const RS_RESERVED: &[&str] = &[
    "data", "off", "n", "v", "value", "slice", "take", "count", "latin1", "cstr", "chars",
];
const RS_TYPES: &[&str] = &[
    "ParseError",
    "Self",
    "String",
    "Vec",
    "Option",
    "Result",
    "Box",
    "Ok",
    "Err",
    "Some",
    "None",
    "Default",
    "Clone",
    "Copy",
    "Debug",
    "PartialEq",
    "Eq",
    "Hash",
    "Sized",
    "Send",
    "Sync",
];

fn nonempty(s: String, fallback: &str) -> String {
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

fn digit_prefix(s: String, prefix: &str) -> String {
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        format!("{prefix}{s}")
    } else {
        s
    }
}

fn unique(base: String, used: &mut HashSet<String>, sep: &str) -> String {
    if used.insert(base.clone()) {
        return base;
    }
    let mut n = 2;
    loop {
        let cand = format!("{base}{sep}{n}");
        if used.insert(cand.clone()) {
            return cand;
        }
        n += 1;
    }
}

impl Names {
    fn new(m: &Model, lang: Language) -> Names {
        let reserved_types: &[&str] = match lang {
            Language::Python => &["ParseError"],
            Language::Go => &[],
            Language::TypeScript => TS_TYPES,
            Language::Rust => RS_TYPES,
        };
        let type_name = |name: &str| {
            let base = digit_prefix(nonempty(pascal_case(name), "Type"), "T");
            if reserved_types.contains(&base.as_str()) {
                format!("{base}_")
            } else {
                base
            }
        };
        let sep = if lang == Language::Go { "" } else { "_" };
        let mut used = HashSet::new();
        let enums: Vec<String> = m
            .enums
            .iter()
            .map(|e| unique(type_name(&e.name), &mut used, sep))
            .collect();
        let structs: Vec<String> = m
            .structs
            .iter()
            .map(|s| unique(type_name(&s.name), &mut used, sep))
            .collect();
        let field_name = |name: &str| -> String {
            match lang {
                Language::Python => {
                    let s = digit_prefix(nonempty(snake_case(name), "field"), "f_");
                    if PY_KEYWORDS.contains(&s.as_str()) || PY_RESERVED.contains(&s.as_str()) {
                        format!("{s}_")
                    } else {
                        s
                    }
                }
                Language::Go => digit_prefix(nonempty(pascal_case(name), "Field"), "F"),
                Language::TypeScript => digit_prefix(nonempty(camel_case(name), "field"), "f"),
                Language::Rust => {
                    let s = digit_prefix(nonempty(snake_case(name), "field"), "f_");
                    if matches!(s.as_str(), "self" | "super" | "crate")
                        || RS_RESERVED.contains(&s.as_str())
                    {
                        format!("{s}_")
                    } else if RS_KEYWORDS.contains(&s.as_str()) {
                        format!("r#{s}")
                    } else {
                        s
                    }
                }
            }
        };
        let mut fields = Vec::new();
        let mut locals = Vec::new();
        for s in &m.structs {
            let mut used = HashSet::new();
            let names: Vec<String> = s
                .fields
                .iter()
                .map(|f| match f.kind {
                    Kind::Skip(_) => String::new(),
                    _ => unique(field_name(&f.name), &mut used, sep),
                })
                .collect();
            let mut used_locals = HashSet::new();
            let locs: Vec<String> = names
                .iter()
                .map(|n| {
                    if n.is_empty() {
                        return String::new();
                    }
                    let cand = match lang {
                        Language::TypeScript
                            if TS_KEYWORDS.contains(&n.as_str())
                                || TS_RESERVED.contains(&n.as_str()) =>
                        {
                            format!("{n}_")
                        }
                        _ => n.clone(),
                    };
                    unique(cand, &mut used_locals, sep)
                })
                .collect();
            fields.push(names);
            locals.push(locs);
        }
        let variants = m
            .enums
            .iter()
            .map(|e| {
                let mut used = HashSet::new();
                e.variants
                    .iter()
                    .map(|v| {
                        let base = match lang {
                            Language::Python | Language::Rust => {
                                digit_prefix(nonempty(upper_snake_case(&v.name), "VALUE"), "V_")
                            }
                            Language::Go | Language::TypeScript => {
                                digit_prefix(nonempty(pascal_case(&v.name), "Value"), "V")
                            }
                        };
                        unique(base, &mut used, sep)
                    })
                    .collect()
            })
            .collect();
        Names {
            structs,
            enums,
            fields,
            locals,
            variants,
        }
    }
}

// ===========================================================================
// Output helpers
// ===========================================================================

struct Out {
    buf: String,
    unit: &'static str,
}

impl Out {
    fn new(unit: &'static str) -> Out {
        Out {
            buf: String::new(),
            unit,
        }
    }

    fn line(&mut self, depth: usize, text: &str) {
        if !text.is_empty() {
            for _ in 0..depth {
                self.buf.push_str(self.unit);
            }
            self.buf.push_str(text);
        }
        self.buf.push('\n');
    }

    fn blank(&mut self) {
        self.buf.push('\n');
    }
}

/// Doc text on one line.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn endian_suffix(e: Endian) -> &'static str {
    match e {
        Endian::Little => "le",
        Endian::Big => "be",
    }
}

fn cbin_symbol(op: CBin) -> &'static str {
    match op {
        CBin::Add => "+",
        CBin::Sub => "-",
        CBin::Mul => "*",
        CBin::Div => "/",
        CBin::Rem => "%",
    }
}

// ===========================================================================
// Python
// ===========================================================================

struct Python<'m> {
    m: &'m Model,
    n: Names,
    used: BTreeSet<&'static str>,
}

impl<'m> Python<'m> {
    fn new(m: &'m Model) -> Self {
        Python {
            m,
            n: Names::new(m, Language::Python),
            used: BTreeSet::new(),
        }
    }

    fn method(&self, e: Endian) -> String {
        if e == self.m.default {
            "parse_from".into()
        } else {
            format!("_parse_from_{}", endian_suffix(e))
        }
    }

    fn emit(mut self) -> String {
        let mut body = Out::new("    ");
        for (i, e) in self.m.enums.iter().enumerate() {
            body.blank();
            body.blank();
            body.line(0, &format!("class {}(enum.IntEnum):", self.n.enums[i]));
            let doc = e
                .doc
                .clone()
                .unwrap_or_else(|| format!("Values of the template enum `{}`.", e.name));
            body.line(1, &format!("\"\"\"{}\"\"\"", py_doc(&doc)));
            if !e.variants.is_empty() {
                body.blank();
            }
            for (j, v) in e.variants.iter().enumerate() {
                let note = v
                    .end
                    .map(|end| format!("  # ... {end}"))
                    .unwrap_or_default();
                body.line(1, &format!("{} = {}{note}", self.n.variants[i][j], v.value));
            }
        }
        for i in 0..self.m.structs.len() {
            self.structure(i, &mut body);
        }
        let root = &self.n.structs[self.m.root];
        body.blank();
        body.blank();
        body.line(0, &format!("def parse(data: bytes) -> {root}:"));
        body.line(
            1,
            &format!("\"\"\"Parses a ``{root}`` from the start of ``data``.\"\"\""),
        );
        body.line(1, &format!("value, _ = {root}.parse_from(data, 0)"));
        body.line(1, "return value");

        let mut out = Out::new("    ");
        out.line(
            0,
            &format!(
                "\"\"\"Parser for ``{root}`` generated by Hexa from a structure template.\"\"\""
            ),
        );
        out.blank();
        out.line(0, "from __future__ import annotations");
        out.blank();
        if !self.m.enums.is_empty() {
            out.line(0, "import enum");
        }
        if self.used.contains("struct") {
            out.line(0, "import struct");
        }
        out.line(0, "from dataclasses import dataclass");
        out.blank();
        out.blank();
        out.line(0, "class ParseError(ValueError):");
        out.line(
            1,
            "\"\"\"Raised when the data does not match the structure.\"\"\"",
        );
        for (name, code) in PY_HELPERS {
            if self.used.contains(name) {
                out.blank();
                out.blank();
                out.buf.push_str(code);
            }
        }
        out.buf.push_str(&body.buf);
        out.buf
    }

    fn structure(&mut self, i: usize, o: &mut Out) {
        let s = &self.m.structs[i];
        let name = self.n.structs[i].clone();
        o.blank();
        o.blank();
        o.line(0, "@dataclass");
        o.line(0, &format!("class {name}:"));
        let doc = s
            .doc
            .clone()
            .unwrap_or_else(|| format!("Template struct `{}`.", s.name));
        o.line(1, &format!("\"\"\"{}\"\"\"", py_doc(&doc)));
        o.blank();
        for (j, f) in s.fields.iter().enumerate() {
            if matches!(f.kind, Kind::Skip(_)) {
                continue;
            }
            if let Some(d) = &f.doc {
                o.line(1, &format!("#: {}", one_line(d)));
            }
            o.line(1, &format!("{}: {}", self.n.fields[i][j], self.ty(&f.kind)));
        }
        for &inst in &s.instances {
            o.blank();
            o.line(1, "@classmethod");
            o.line(
                1,
                &format!(
                    "def {}(cls, data: bytes, offset: int = 0) -> tuple[{name}, int]:",
                    self.method(inst)
                ),
            );
            if inst == self.m.default {
                o.line(
                    2,
                    &format!(
                        "\"\"\"Parses a ``{name}`` at ``offset``; returns it and the offset \
                         just past it.\"\"\""
                    ),
                );
            } else {
                o.line(
                    2,
                    &format!(
                        "\"\"\"Like ``parse_from``, with {}-endian defaults.\"\"\"",
                        match inst {
                            Endian::Little => "little",
                            Endian::Big => "big",
                        }
                    ),
                );
            }
            self.body(i, inst, o);
            let args: Vec<String> = self.n.fields[i]
                .iter()
                .filter(|n| !n.is_empty())
                .map(|n| format!("{n}={n}"))
                .collect();
            if args.is_empty() {
                o.line(2, "return cls(), offset");
            } else {
                o.line(2, "value = cls(");
                for a in args {
                    o.line(3, &format!("{a},"));
                }
                o.line(2, ")");
                o.line(2, "return value, offset");
            }
        }
    }

    fn ty(&self, k: &Kind) -> String {
        match k {
            Kind::One(e) => self.elem_ty(*e),
            Kind::Array(Elem::Scalar(Scalar::U(1), _), _) => "bytes".into(),
            Kind::Array(e, _) => format!("list[{}]", self.elem_ty(*e)),
            Kind::Chars(_) | Kind::Skip(_) => "str".into(),
        }
    }

    fn elem_ty(&self, e: Elem) -> String {
        match e {
            Elem::Scalar(s, _) => match s {
                Scalar::U(_) | Scalar::S(_) => "int".into(),
                Scalar::F32 | Scalar::F64 => "float".into(),
                Scalar::Bool => "bool".into(),
                Scalar::Char => "str".into(),
            },
            Elem::Enum(i, _) => format!("{} | int", self.n.enums[i]),
            Elem::Struct(i, _) => self.n.structs[i].clone(),
            Elem::CStr => "str".into(),
        }
    }

    fn body(&mut self, si: usize, inst: Endian, o: &mut Out) {
        let fields = &self.m.structs[si].fields;
        let mut i = 0;
        while i < fields.len() {
            // Runs of scalars share one bounds check and one `unpack_from`.
            if let Kind::One(e) = fields[i].kind
                && let Some(first) = self.m.scalar_of(e)
            {
                let mut run = vec![(i, e, first)];
                let mut order = (first.size() > 1).then(|| Model::elem_endian(e, inst));
                let mut j = i + 1;
                while j < fields.len() && run.len() < 6 {
                    let Kind::One(e2) = fields[j].kind else { break };
                    let Some(s2) = self.m.scalar_of(e2) else {
                        break;
                    };
                    let o2 = (s2.size() > 1).then(|| Model::elem_endian(e2, inst));
                    if order.is_some() && o2.is_some() && order != o2 {
                        break;
                    }
                    order = order.or(o2);
                    run.push((j, e2, s2));
                    j += 1;
                }
                self.run(si, &run, order.unwrap_or(inst), o);
                i = j;
                continue;
            }
            self.field(si, i, inst, o);
            i += 1;
        }
    }

    fn run(&mut self, si: usize, run: &[(usize, Elem, Scalar)], order: Endian, o: &mut Out) {
        self.used.insert("struct");
        self.used.insert("_need");
        let prefix = if order == Endian::Big { ">" } else { "<" };
        let fmt: String = run.iter().map(|(_, _, s)| py_code(*s)).collect();
        let total: usize = run.iter().map(|(_, _, s)| s.size()).sum();
        let names: Vec<&str> = run
            .iter()
            .map(|(j, _, _)| self.n.fields[si][*j].as_str())
            .collect();
        let lhs = if names.len() == 1 {
            format!("({},)", names[0])
        } else {
            names.join(", ")
        };
        o.line(2, &format!("_need(data, offset, {total})"));
        o.line(
            2,
            &format!("{lhs} = struct.unpack_from(\"{prefix}{fmt}\", data, offset)"),
        );
        o.line(2, &format!("offset += {total}"));
        for (j, e, s) in run {
            let l = &self.n.fields[si][*j];
            if let Elem::Enum(ei, _) = e {
                self.used.insert("_enum");
                o.line(2, &format!("{l} = _enum({}, {l})", self.n.enums[*ei]));
            } else if *s == Scalar::Char {
                o.line(2, &format!("{l} = {l}.decode(\"latin-1\")"));
            }
        }
    }

    /// Count as a Python expression (may emit a `_n = _count(...)` line).
    fn count(&mut self, si: usize, c: &Count, o: &mut Out) -> (String, Option<u64>) {
        match c {
            Count::Const(n) => (n.to_string(), Some(*n)),
            Count::Expr(e) => {
                self.used.insert("_count");
                let expr = py_cexpr(e, &self.n.fields[si], true);
                o.line(2, &format!("_n = _count({expr}, offset)"));
                ("_n".into(), None)
            }
        }
    }

    fn field(&mut self, si: usize, fi: usize, inst: Endian, o: &mut Out) {
        let f = &self.m.structs[si].fields[fi];
        let l = self.n.fields[si][fi].clone();
        match &f.kind {
            Kind::One(Elem::Struct(j, e)) => {
                let method = self.method(e.unwrap_or(inst));
                o.line(
                    2,
                    &format!(
                        "{l}, offset = {}.{method}(data, offset)",
                        self.n.structs[*j]
                    ),
                );
            }
            Kind::One(Elem::CStr) => {
                self.used.insert("_cstr");
                o.line(2, &format!("{l}, offset = _cstr(data, offset)"));
            }
            Kind::One(_) => {}
            Kind::Chars(c) => {
                let (n, _) = self.count(si, c, o);
                self.used.insert("_chars");
                o.line(2, &format!("{l} = _chars(data, offset, {n})"));
                o.line(2, &format!("offset += {n}"));
            }
            Kind::Skip(c) => {
                let (n, _) = self.count(si, c, o);
                self.used.insert("_need");
                o.line(2, &format!("_need(data, offset, {n})"));
                o.line(2, &format!("offset += {n}"));
            }
            Kind::Array(elem, c) => {
                let (n, fixed) = self.count(si, c, o);
                match *elem {
                    Elem::Scalar(Scalar::U(1), _) => {
                        self.used.insert("_need");
                        o.line(2, &format!("_need(data, offset, {n})"));
                        o.line(2, &format!("{l} = bytes(data[offset : offset + {n}])"));
                        o.line(2, &format!("offset += {n}"));
                    }
                    Elem::Scalar(..) | Elem::Enum(..) => {
                        let s = self.m.scalar_of(*elem).unwrap_or(Scalar::U(1));
                        let order = if Model::elem_endian(*elem, inst) == Endian::Big {
                            ">"
                        } else {
                            "<"
                        };
                        let size = s.size();
                        let (fmt, bytes) = match fixed {
                            Some(k) => (
                                format!("\"{order}{k}{}\"", py_code(s)),
                                (k.saturating_mul(size as u64)).to_string(),
                            ),
                            None => (
                                format!("f\"{order}{{_n}}{}\"", py_code(s)),
                                if size == 1 {
                                    n.clone()
                                } else {
                                    format!("{n} * {size}")
                                },
                            ),
                        };
                        self.used.insert("_need");
                        self.used.insert("struct");
                        o.line(2, &format!("_need(data, offset, {bytes})"));
                        let unpack = format!("struct.unpack_from({fmt}, data, offset)");
                        match elem {
                            Elem::Enum(ei, _) => {
                                self.used.insert("_enum");
                                o.line(
                                    2,
                                    &format!(
                                        "{l} = [_enum({}, v) for v in {unpack}]",
                                        self.n.enums[*ei]
                                    ),
                                );
                            }
                            _ => o.line(2, &format!("{l} = list({unpack})")),
                        }
                        o.line(2, &format!("offset += {bytes}"));
                    }
                    Elem::Struct(j, e) => {
                        let method = self.method(e.unwrap_or(inst));
                        o.line(2, &format!("{l} = []"));
                        o.line(2, &format!("for _ in range({n}):"));
                        o.line(
                            3,
                            &format!(
                                "_item, offset = {}.{method}(data, offset)",
                                self.n.structs[j]
                            ),
                        );
                        o.line(3, &format!("{l}.append(_item)"));
                    }
                    Elem::CStr => {
                        self.used.insert("_cstr");
                        o.line(2, &format!("{l} = []"));
                        o.line(2, &format!("for _ in range({n}):"));
                        o.line(3, "_item, offset = _cstr(data, offset)");
                        o.line(3, &format!("{l}.append(_item)"));
                    }
                }
            }
        }
    }
}

fn py_code(s: Scalar) -> char {
    match s {
        Scalar::U(1) => 'B',
        Scalar::S(1) => 'b',
        Scalar::U(2) => 'H',
        Scalar::S(2) => 'h',
        Scalar::U(4) => 'I',
        Scalar::S(4) => 'i',
        Scalar::U(_) => 'Q',
        Scalar::S(_) => 'q',
        Scalar::F32 => 'f',
        Scalar::F64 => 'd',
        Scalar::Bool => '?',
        Scalar::Char => 'c',
    }
}

fn py_doc(s: &str) -> String {
    one_line(s).replace('\\', "\\\\").replace('"', "\\\"")
}

fn py_cexpr(e: &CExpr, locals: &[String], top: bool) -> String {
    match e {
        CExpr::Lit(v) => v.to_string(),
        CExpr::Field(i) => locals[*i].clone(),
        CExpr::Bin(op, a, b) => {
            let sym = match op {
                CBin::Div => "//",
                other => cbin_symbol(*other),
            };
            let s = format!(
                "{} {sym} {}",
                py_cexpr(a, locals, false),
                py_cexpr(b, locals, false)
            );
            if top { s } else { format!("({s})") }
        }
    }
}

const PY_HELPERS: &[(&str, &str)] = &[
    (
        "_need",
        "def _need(data: bytes, offset: int, size: int) -> None:
    if size < 0 or offset + size > len(data):
        have = max(len(data) - offset, 0)
        raise ParseError(f\"need {size} bytes at offset {offset}, have {have}\")
",
    ),
    (
        "_count",
        "def _count(n: int, offset: int) -> int:
    if n < 0:
        raise ParseError(f\"invalid element count {n} at offset {offset}\")
    return n
",
    ),
    (
        "_enum",
        "def _enum(cls: type[enum.IntEnum], value: int) -> enum.IntEnum | int:
    try:
        return cls(value)
    except ValueError:
        return value
",
    ),
    (
        "_chars",
        "def _chars(data: bytes, offset: int, size: int) -> str:
    _need(data, offset, size)
    raw = bytes(data[offset : offset + size])
    end = raw.find(b\"\\0\")
    return (raw if end < 0 else raw[:end]).decode(\"latin-1\")
",
    ),
    (
        "_cstr",
        "def _cstr(data: bytes, offset: int) -> tuple[str, int]:
    end = bytes(data).find(b\"\\0\", offset)
    if end < 0:
        raise ParseError(f\"unterminated string at offset {offset}\")
    return bytes(data[offset:end]).decode(\"latin-1\"), end + 1
",
    ),
];

// ===========================================================================
// Go
// ===========================================================================

struct Go<'m> {
    m: &'m Model,
    n: Names,
    used: BTreeSet<&'static str>,
}

impl<'m> Go<'m> {
    fn new(m: &'m Model) -> Self {
        Go {
            m,
            n: Names::new(m, Language::Go),
            used: BTreeSet::new(),
        }
    }

    fn method(&self, e: Endian) -> String {
        if e == self.m.default {
            "decode".into()
        } else {
            format!("decode{}", endian_suffix(e).to_ascii_uppercase())
        }
    }

    fn emit(mut self) -> String {
        let mut body = Out::new("\t");
        for (i, e) in self.m.enums.iter().enumerate() {
            let name = &self.n.enums[i];
            body.blank();
            let doc = e
                .doc
                .clone()
                .unwrap_or_else(|| format!("holds values of the template enum {}.", e.name));
            body.line(0, &format!("// {name} {}", go_doc(&doc)));
            body.line(0, &format!("type {name} {}", go_scalar(e.base)));
            if e.variants.is_empty() {
                continue;
            }
            body.blank();
            body.line(0, &format!("// {name} values."));
            body.line(0, "const (");
            let consts: Vec<String> = self.n.variants[i]
                .iter()
                .map(|v| format!("{name}{v}"))
                .collect();
            let width = consts.iter().map(String::len).max().unwrap_or(0);
            for (v, c) in e.variants.iter().zip(&consts) {
                if let Some(end) = v.end {
                    body.line(
                        1,
                        &format!("// {c} starts the range {} ... {end}.", v.value),
                    );
                }
                body.line(1, &format!("{c:<width$} {name} = {}", v.value));
            }
            body.line(0, ")");
        }
        for i in 0..self.m.structs.len() {
            self.structure(i, &mut body);
        }
        let root = self.n.structs[self.m.root].clone();
        body.blank();
        body.line(
            0,
            &format!("// Parse{root} decodes a {root} from the start of data."),
        );
        body.line(
            0,
            &format!("func Parse{root}(data []byte) (*{root}, error) {{"),
        );
        body.line(1, "d := &decoder{data: data}");
        body.line(1, &format!("v := new({root})"));
        body.line(1, "v.decode(d)");
        body.line(1, "if d.err != nil {");
        body.line(2, "return nil, d.err");
        body.line(1, "}");
        body.line(1, "return v, nil");
        body.line(0, "}");

        // Helper dependencies.
        let deps: &[(&str, &[&str])] = &[
            ("i8", &["u8"]),
            ("i16", &["u16"]),
            ("i32", &["u32"]),
            ("i64", &["u64"]),
            ("f32", &["u32"]),
            ("f64", &["u64"]),
            ("boolean", &["u8"]),
            ("char", &["u8"]),
            ("chars", &["latin1"]),
            ("cstr", &["latin1"]),
        ];
        for (h, ds) in deps {
            if self.used.contains(h) {
                self.used.extend(ds.iter().copied());
            }
        }
        let binary = ["u16", "u32", "u64"].iter().any(|h| self.used.contains(h));
        let math = ["f32", "f64"].iter().any(|h| self.used.contains(h));

        let mut out = Out::new("\t");
        out.line(
            0,
            "// Code generated by Hexa from a structure template. DO NOT EDIT.",
        );
        out.blank();
        out.line(
            0,
            &format!("// Package parser decodes {root} values from binary data."),
        );
        out.line(0, "package parser");
        out.blank();
        out.line(0, "import (");
        if binary {
            out.line(1, "\"encoding/binary\"");
        }
        out.line(1, "\"fmt\"");
        if math {
            out.line(1, "\"math\"");
        }
        out.line(0, ")");
        out.buf.push_str(&body.buf);
        out.blank();
        out.buf.push_str(GO_DECODER);
        for (name, code) in GO_HELPERS {
            if self.used.contains(name) {
                out.blank();
                out.buf.push_str(code);
            }
        }
        out.buf
    }

    fn structure(&mut self, i: usize, o: &mut Out) {
        let s = &self.m.structs[i];
        let name = self.n.structs[i].clone();
        o.blank();
        let doc = s
            .doc
            .clone()
            .unwrap_or_else(|| format!("is the template struct {}.", s.name));
        o.line(0, &format!("// {name} {}", go_doc(&doc)));
        o.line(0, &format!("type {name} struct {{"));
        let width = self.n.fields[i].iter().map(String::len).max().unwrap_or(0);
        for (j, f) in s.fields.iter().enumerate() {
            if matches!(f.kind, Kind::Skip(_)) {
                continue;
            }
            if let Some(d) = &f.doc {
                o.line(1, &format!("// {}", one_line(d)));
            }
            o.line(
                1,
                &format!("{:<width$} {}", self.n.fields[i][j], self.ty(&f.kind)),
            );
        }
        o.line(0, "}");
        for &inst in &s.instances {
            o.blank();
            o.line(
                0,
                &format!("func (v *{name}) {}(d *decoder) {{", self.method(inst)),
            );
            for j in 0..s.fields.len() {
                self.field(i, j, inst, o);
            }
            o.line(0, "}");
        }
    }

    fn ty(&self, k: &Kind) -> String {
        match k {
            Kind::One(e) => self.elem_ty(*e),
            Kind::Array(e, _) => format!("[]{}", self.elem_ty(*e)),
            Kind::Chars(_) | Kind::Skip(_) => "string".into(),
        }
    }

    fn elem_ty(&self, e: Elem) -> String {
        match e {
            Elem::Scalar(s, _) => go_scalar(s).into(),
            Elem::Enum(i, _) => self.n.enums[i].clone(),
            Elem::Struct(i, _) => self.n.structs[i].clone(),
            Elem::CStr => "string".into(),
        }
    }

    /// Expression reading one element (non-struct).
    fn read(&mut self, e: Elem, inst: Endian) -> String {
        let order = match Model::elem_endian(e, inst) {
            Endian::Little => "binary.LittleEndian",
            Endian::Big => "binary.BigEndian",
        };
        let scalar = |s: Scalar, used: &mut BTreeSet<&'static str>| -> String {
            let (h, ordered) = match s {
                Scalar::U(1) => ("u8", false),
                Scalar::S(1) => ("i8", false),
                Scalar::U(2) => ("u16", true),
                Scalar::S(2) => ("i16", true),
                Scalar::U(4) => ("u32", true),
                Scalar::S(4) => ("i32", true),
                Scalar::U(_) => ("u64", true),
                Scalar::S(_) => ("i64", true),
                Scalar::F32 => ("f32", true),
                Scalar::F64 => ("f64", true),
                Scalar::Bool => ("boolean", false),
                Scalar::Char => ("char", false),
            };
            used.insert(h);
            if ordered {
                format!("d.{h}({order})")
            } else {
                format!("d.{h}()")
            }
        };
        match e {
            Elem::Scalar(s, _) => scalar(s, &mut self.used),
            Elem::Enum(i, _) => {
                let r = scalar(self.m.enums[i].base, &mut self.used);
                format!("{}({r})", self.n.enums[i])
            }
            Elem::CStr => {
                self.used.insert("cstr");
                "d.cstr()".into()
            }
            Elem::Struct(..) => String::new(),
        }
    }

    fn count(&mut self, si: usize, c: &Count, elem_size: usize) -> String {
        match c {
            Count::Const(n) => n.to_string(),
            Count::Expr(e) => {
                self.used.insert("count");
                let expr = go_cexpr(e, &self.n.fields[si], true);
                format!("d.count({expr}, {elem_size})")
            }
        }
    }

    fn field(&mut self, si: usize, fi: usize, inst: Endian, o: &mut Out) {
        let f = &self.m.structs[si].fields[fi];
        let l = format!("v.{}", self.n.fields[si][fi]);
        match &f.kind {
            Kind::One(Elem::Struct(_, e)) => {
                o.line(1, &format!("{l}.{}(d)", self.method(e.unwrap_or(inst))));
            }
            Kind::One(e) => {
                let r = self.read(*e, inst);
                o.line(1, &format!("{l} = {r}"));
            }
            Kind::Chars(c) => {
                let n = self.count(si, c, 1);
                self.used.insert("chars");
                o.line(1, &format!("{l} = d.chars({n})"));
            }
            Kind::Skip(c) => {
                let n = self.count(si, c, 1);
                o.line(1, &format!("d.take({n})"));
            }
            Kind::Array(Elem::Scalar(Scalar::U(1), _), c) => {
                let n = self.count(si, c, 1);
                self.used.insert("bytes");
                o.line(1, &format!("{l} = d.bytes({n})"));
            }
            Kind::Array(elem, c) => {
                let size = self.m.elem_min(*elem, 0).max(1);
                let n = self.count(si, c, size);
                let ty = self.elem_ty(*elem);
                o.line(1, &format!("{l} = make([]{ty}, {n})"));
                o.line(1, &format!("for i := range {l} {{"));
                if let Elem::Struct(_, e) = elem {
                    o.line(2, &format!("{l}[i].{}(d)", self.method(e.unwrap_or(inst))));
                } else {
                    let r = self.read(*elem, inst);
                    o.line(2, &format!("{l}[i] = {r}"));
                }
                o.line(1, "}");
            }
        }
    }
}

fn go_scalar(s: Scalar) -> &'static str {
    match s {
        Scalar::U(1) => "uint8",
        Scalar::S(1) => "int8",
        Scalar::U(2) => "uint16",
        Scalar::S(2) => "int16",
        Scalar::U(4) => "uint32",
        Scalar::S(4) => "int32",
        Scalar::U(_) => "uint64",
        Scalar::S(_) => "int64",
        Scalar::F32 => "float32",
        Scalar::F64 => "float64",
        Scalar::Bool => "bool",
        Scalar::Char => "rune",
    }
}

fn go_doc(s: &str) -> String {
    let s = one_line(s);
    if s.ends_with('.') { s } else { format!("{s}.") }
}

fn go_cexpr(e: &CExpr, fields: &[String], top: bool) -> String {
    match e {
        CExpr::Lit(v) => v.to_string(),
        CExpr::Field(i) => format!("int64(v.{})", fields[*i]),
        CExpr::Bin(op, a, b) => {
            let s = format!(
                "{} {} {}",
                go_cexpr(a, fields, false),
                cbin_symbol(*op),
                go_cexpr(b, fields, false)
            );
            if top { s } else { format!("({s})") }
        }
    }
}

const GO_DECODER: &str = "// decoder reads values from data; the first error sticks and turns later
// reads into no-ops returning zero values.
type decoder struct {
\tdata []byte
\toff  int
\terr  error
}

// take consumes n bytes, or records an error and returns nil.
func (d *decoder) take(n int) []byte {
\tif d.err != nil {
\t\treturn nil
\t}
\tif n < 0 || n > len(d.data)-d.off {
\t\td.err = fmt.Errorf(\"parser: need %d bytes at offset %d, have %d\", n, d.off, len(d.data)-d.off)
\t\treturn nil
\t}
\tb := d.data[d.off : d.off+n]
\td.off += n
\treturn b
}
";

const GO_HELPERS: &[(&str, &str)] = &[
    (
        "count",
        "// count validates an element count against the remaining data.
func (d *decoder) count(n int64, size int) int {
\tif d.err != nil {
\t\treturn 0
\t}
\tif n < 0 || n > int64((len(d.data)-d.off)/size) {
\t\td.err = fmt.Errorf(\"parser: invalid element count %d at offset %d\", n, d.off)
\t\treturn 0
\t}
\treturn int(n)
}
",
    ),
    (
        "u8",
        "func (d *decoder) u8() uint8 {
\tif b := d.take(1); b != nil {
\t\treturn b[0]
\t}
\treturn 0
}
",
    ),
    (
        "i8",
        "func (d *decoder) i8() int8 { return int8(d.u8()) }
",
    ),
    (
        "u16",
        "func (d *decoder) u16(o binary.ByteOrder) uint16 {
\tif b := d.take(2); b != nil {
\t\treturn o.Uint16(b)
\t}
\treturn 0
}
",
    ),
    (
        "i16",
        "func (d *decoder) i16(o binary.ByteOrder) int16 { return int16(d.u16(o)) }
",
    ),
    (
        "u32",
        "func (d *decoder) u32(o binary.ByteOrder) uint32 {
\tif b := d.take(4); b != nil {
\t\treturn o.Uint32(b)
\t}
\treturn 0
}
",
    ),
    (
        "i32",
        "func (d *decoder) i32(o binary.ByteOrder) int32 { return int32(d.u32(o)) }
",
    ),
    (
        "u64",
        "func (d *decoder) u64(o binary.ByteOrder) uint64 {
\tif b := d.take(8); b != nil {
\t\treturn o.Uint64(b)
\t}
\treturn 0
}
",
    ),
    (
        "i64",
        "func (d *decoder) i64(o binary.ByteOrder) int64 { return int64(d.u64(o)) }
",
    ),
    (
        "f32",
        "func (d *decoder) f32(o binary.ByteOrder) float32 { return math.Float32frombits(d.u32(o)) }
",
    ),
    (
        "f64",
        "func (d *decoder) f64(o binary.ByteOrder) float64 { return math.Float64frombits(d.u64(o)) }
",
    ),
    (
        "boolean",
        "func (d *decoder) boolean() bool { return d.u8() != 0 }
",
    ),
    (
        "char",
        "func (d *decoder) char() rune { return rune(d.u8()) }
",
    ),
    (
        "bytes",
        "func (d *decoder) bytes(n int) []byte { return append([]byte(nil), d.take(n)...) }
",
    ),
    (
        "chars",
        "// chars decodes n bytes of Latin-1 text up to the first NUL.
func (d *decoder) chars(n int) string {
\tb := d.take(n)
\tfor i, c := range b {
\t\tif c == 0 {
\t\t\treturn latin1(b[:i])
\t\t}
\t}
\treturn latin1(b)
}
",
    ),
    (
        "cstr",
        "// cstr decodes NUL-terminated Latin-1 text.
func (d *decoder) cstr() string {
\tif d.err != nil {
\t\treturn \"\"
\t}
\tfor i := d.off; i < len(d.data); i++ {
\t\tif d.data[i] == 0 {
\t\t\ts := latin1(d.data[d.off:i])
\t\t\td.off = i + 1
\t\t\treturn s
\t\t}
\t}
\td.err = fmt.Errorf(\"parser: unterminated string at offset %d\", d.off)
\treturn \"\"
}
",
    ),
    (
        "latin1",
        "func latin1(b []byte) string {
\tr := make([]rune, len(b))
\tfor i, c := range b {
\t\tr[i] = rune(c)
\t}
\treturn string(r)
}
",
    ),
];

// ===========================================================================
// TypeScript
// ===========================================================================

struct Ts<'m> {
    m: &'m Model,
    n: Names,
    used: BTreeSet<&'static str>,
}

impl<'m> Ts<'m> {
    fn new(m: &'m Model) -> Self {
        Ts {
            m,
            n: Names::new(m, Language::TypeScript),
            used: BTreeSet::new(),
        }
    }

    fn func(&self, si: usize, e: Endian) -> String {
        let base = format!("parse{}", self.n.structs[si]);
        if e == self.m.default {
            base
        } else {
            format!("{base}{}", pascal_case(endian_suffix(e)))
        }
    }

    fn emit(mut self) -> String {
        let mut body = Out::new("  ");
        for (i, e) in self.m.enums.iter().enumerate() {
            let name = &self.n.enums[i];
            body.blank();
            let doc = e
                .doc
                .clone()
                .unwrap_or_else(|| format!("Values of the template enum `{}`.", e.name));
            body.line(0, &format!("/** {} */", ts_doc(&doc)));
            if e.base.size() == 8 {
                body.line(0, &format!("export const {name} = {{"));
                for (j, v) in e.variants.iter().enumerate() {
                    body.line(1, &format!("{}: {}n,", self.n.variants[i][j], v.value));
                }
                body.line(0, "} as const;");
                body.line(0, &format!("export type {name} = bigint;"));
            } else {
                body.line(0, &format!("export enum {name} {{"));
                for (j, v) in e.variants.iter().enumerate() {
                    let note = v
                        .end
                        .map(|end| format!(" // ... {end}"))
                        .unwrap_or_default();
                    body.line(
                        1,
                        &format!("{} = {},{note}", self.n.variants[i][j], v.value),
                    );
                }
                body.line(0, "}");
            }
        }
        for i in 0..self.m.structs.len() {
            self.interface(i, &mut body);
        }
        for i in 0..self.m.structs.len() {
            for inst in self.m.structs[i].instances.clone() {
                self.function(i, inst, &mut body);
            }
        }

        let mut out = Out::new("  ");
        let root = &self.n.structs[self.m.root];
        out.line(
            0,
            &format!(
                "// Parser for `{root}` generated by Hexa from a structure template. Do not edit."
            ),
        );
        out.blank();
        out.line(
            0,
            "/** Thrown when the data does not match the structure. */",
        );
        out.line(0, "export class ParseError extends Error {}");
        out.blank();
        out.line(
            0,
            "/** A decoded value and the number of bytes it occupies. */",
        );
        out.line(0, "export interface Parsed<T> {");
        out.line(1, "value: T;");
        out.line(1, "size: number;");
        out.line(0, "}");
        out.buf.push_str(&body.buf);
        for (name, code) in TS_HELPERS {
            if self.used.contains(name) {
                out.blank();
                out.buf.push_str(code);
            }
        }
        out.buf
    }

    fn interface(&self, i: usize, o: &mut Out) {
        let s = &self.m.structs[i];
        o.blank();
        let doc = s
            .doc
            .clone()
            .unwrap_or_else(|| format!("Template struct `{}`.", s.name));
        o.line(0, &format!("/** {} */", ts_doc(&doc)));
        o.line(0, &format!("export interface {} {{", self.n.structs[i]));
        for (j, f) in s.fields.iter().enumerate() {
            if matches!(f.kind, Kind::Skip(_)) {
                continue;
            }
            if let Some(d) = &f.doc {
                o.line(1, &format!("/** {} */", ts_doc(d)));
            }
            o.line(
                1,
                &format!("{}: {};", self.n.fields[i][j], self.ty(&f.kind)),
            );
        }
        o.line(0, "}");
    }

    fn ty(&self, k: &Kind) -> String {
        match k {
            Kind::One(e) => self.elem_ty(*e),
            Kind::Array(Elem::Scalar(Scalar::U(1), _), _) => "Uint8Array".into(),
            Kind::Array(e, _) => format!("{}[]", self.elem_ty(*e)),
            Kind::Chars(_) | Kind::Skip(_) => "string".into(),
        }
    }

    fn elem_ty(&self, e: Elem) -> String {
        match e {
            Elem::Scalar(s, _) => match s {
                Scalar::U(8) | Scalar::S(8) => "bigint".into(),
                Scalar::U(_) | Scalar::S(_) | Scalar::F32 | Scalar::F64 => "number".into(),
                Scalar::Bool => "boolean".into(),
                Scalar::Char => "string".into(),
            },
            Elem::Enum(i, _) => self.n.enums[i].clone(),
            Elem::Struct(i, _) => self.n.structs[i].clone(),
            Elem::CStr => "string".into(),
        }
    }

    /// Expression reading a scalar or enum at `at`.
    fn get(&self, e: Elem, inst: Endian, at: &str) -> String {
        let Some(s) = self.m.scalar_of(e) else {
            return String::new();
        };
        let le = Model::elem_endian(e, inst) == Endian::Little;
        let (getter, ordered) = match s {
            Scalar::U(1) | Scalar::Bool | Scalar::Char => ("getUint8", false),
            Scalar::S(1) => ("getInt8", false),
            Scalar::U(2) => ("getUint16", true),
            Scalar::S(2) => ("getInt16", true),
            Scalar::U(4) => ("getUint32", true),
            Scalar::S(4) => ("getInt32", true),
            Scalar::U(_) => ("getBigUint64", true),
            Scalar::S(_) => ("getBigInt64", true),
            Scalar::F32 => ("getFloat32", true),
            Scalar::F64 => ("getFloat64", true),
        };
        let call = if ordered {
            format!("view.{getter}({at}, {le})")
        } else {
            format!("view.{getter}({at})")
        };
        match (e, s) {
            (Elem::Enum(i, _), _) if s.size() < 8 => format!("{call} as {}", self.n.enums[i]),
            (_, Scalar::Bool) => format!("{call} !== 0"),
            (_, Scalar::Char) => format!("String.fromCharCode({call})"),
            _ => call,
        }
    }

    fn function(&mut self, si: usize, inst: Endian, o: &mut Out) {
        let s = &self.m.structs[si];
        let name = self.n.structs[si].clone();
        let fname = self.func(si, inst);
        o.blank();
        if inst == self.m.default {
            o.line(
                0,
                &format!(
                    "/** Parses a `{name}` at `offset`; `size` is the number of bytes read. */"
                ),
            );
            o.line(
                0,
                &format!("export function {fname}(view: DataView, offset = 0): Parsed<{name}> {{"),
            );
        } else {
            o.line(
                0,
                &format!("function {fname}(view: DataView, offset: number): Parsed<{name}> {{"),
            );
        }
        o.line(1, "const start = offset;");
        let mut temps: HashSet<String> = self.n.locals[si].iter().cloned().collect();
        let fields = s.fields.clone();
        let mut i = 0;
        while i < fields.len() {
            if let Kind::One(e) = fields[i].kind
                && self.m.scalar_of(e).is_some()
            {
                let mut run = vec![(i, e)];
                let mut j = i + 1;
                while j < fields.len() && run.len() < 8 {
                    match fields[j].kind {
                        Kind::One(e2) if self.m.scalar_of(e2).is_some() => run.push((j, e2)),
                        _ => break,
                    }
                    j += 1;
                }
                self.used.insert("need");
                let total: usize = run
                    .iter()
                    .filter_map(|(_, e)| self.m.scalar_of(*e))
                    .map(Scalar::size)
                    .sum();
                o.line(1, &format!("need(view, offset, {total});"));
                let mut rel = 0;
                for (k, e) in run {
                    let at = if rel == 0 {
                        "offset".to_string()
                    } else {
                        format!("offset + {rel}")
                    };
                    let local = &self.n.locals[si][k];
                    o.line(1, &format!("const {local} = {};", self.get(e, inst, &at)));
                    rel += self.m.scalar_of(e).map_or(0, Scalar::size);
                }
                o.line(1, &format!("offset += {total};"));
                i = j;
                continue;
            }
            self.field(si, i, inst, &mut temps, o);
            i += 1;
        }
        let pairs: Vec<String> = self.n.fields[si]
            .iter()
            .zip(&self.n.locals[si])
            .filter(|(f, _)| !f.is_empty())
            .map(|(f, l)| {
                if f == l {
                    f.clone()
                } else {
                    format!("{f}: {l}")
                }
            })
            .collect();
        o.line(1, "return {");
        if pairs.is_empty() {
            o.line(2, "value: {},");
        } else {
            o.line(2, "value: {");
            for p in pairs {
                o.line(3, &format!("{p},"));
            }
            o.line(2, "},");
        }
        o.line(2, "size: offset - start,");
        o.line(1, "};");
        o.line(0, "}");
    }

    fn count(
        &mut self,
        si: usize,
        c: &Count,
        base: &str,
        temps: &mut HashSet<String>,
        o: &mut Out,
    ) -> String {
        match c {
            Count::Const(n) => n.to_string(),
            Count::Expr(e) => {
                self.used.insert("count");
                let expr = ts_cexpr(e, self.m, si, &self.n.locals[si], true);
                let t = unique(format!("{base}Length"), temps, "");
                o.line(1, &format!("const {t} = count({expr}, offset);"));
                t
            }
        }
    }

    fn field(
        &mut self,
        si: usize,
        fi: usize,
        inst: Endian,
        temps: &mut HashSet<String>,
        o: &mut Out,
    ) {
        let f = self.m.structs[si].fields[fi].clone();
        let l = self.n.locals[si][fi].clone();
        let base = if l.is_empty() {
            "padding".to_string()
        } else {
            l.trim_end_matches('_').to_string()
        };
        match &f.kind {
            Kind::One(Elem::Struct(j, e)) => {
                let size = unique(format!("{base}Size"), temps, "");
                let func = self.func(*j, e.unwrap_or(inst));
                o.line(
                    1,
                    &format!("const {{ value: {l}, size: {size} }} = {func}(view, offset);"),
                );
                o.line(1, &format!("offset += {size};"));
            }
            Kind::One(Elem::CStr) => {
                self.used.insert("cstr");
                let size = unique(format!("{base}Size"), temps, "");
                o.line(
                    1,
                    &format!("const {{ value: {l}, size: {size} }} = cstr(view, offset);"),
                );
                o.line(1, &format!("offset += {size};"));
            }
            Kind::One(_) => {}
            Kind::Chars(c) => {
                let n = self.count(si, c, &base, temps, o);
                self.used.insert("chars");
                o.line(1, &format!("const {l} = chars(view, offset, {n});"));
                o.line(1, &format!("offset += {n};"));
            }
            Kind::Skip(c) => {
                let n = self.count(si, c, &base, temps, o);
                self.used.insert("need");
                o.line(1, &format!("need(view, offset, {n});"));
                o.line(1, &format!("offset += {n};"));
            }
            Kind::Array(Elem::Scalar(Scalar::U(1), _), c) => {
                let n = self.count(si, c, &base, temps, o);
                self.used.insert("need");
                o.line(1, &format!("need(view, offset, {n});"));
                o.line(
                    1,
                    &format!(
                        "const {l} = new Uint8Array(view.buffer, view.byteOffset + offset, {n}).slice();"
                    ),
                );
                o.line(1, &format!("offset += {n};"));
            }
            Kind::Array(elem, c) => {
                let n = self.count(si, c, &base, temps, o);
                let ty = self.elem_ty(*elem);
                match elem {
                    Elem::Struct(j, e) => {
                        let func = self.func(*j, e.unwrap_or(inst));
                        o.line(1, &format!("const {l}: {ty}[] = [];"));
                        o.line(1, &format!("for (let i = 0; i < {n}; i++) {{"));
                        o.line(2, &format!("const r = {func}(view, offset);"));
                        o.line(2, &format!("{l}.push(r.value);"));
                        o.line(2, "offset += r.size;");
                        o.line(1, "}");
                    }
                    Elem::CStr => {
                        self.used.insert("cstr");
                        o.line(1, &format!("const {l}: {ty}[] = [];"));
                        o.line(1, &format!("for (let i = 0; i < {n}; i++) {{"));
                        o.line(2, "const r = cstr(view, offset);");
                        o.line(2, &format!("{l}.push(r.value);"));
                        o.line(2, "offset += r.size;");
                        o.line(1, "}");
                    }
                    _ => {
                        let size = self.m.scalar_of(*elem).map_or(1, Scalar::size);
                        let bytes = match c {
                            Count::Const(k) => k.saturating_mul(size as u64).to_string(),
                            Count::Expr(_) if size == 1 => n.clone(),
                            Count::Expr(_) => format!("{n} * {size}"),
                        };
                        self.used.insert("need");
                        o.line(1, &format!("need(view, offset, {bytes});"));
                        o.line(1, &format!("const {l}: {ty}[] = [];"));
                        o.line(1, &format!("for (let i = 0; i < {n}; i++) {{"));
                        let at = if size == 1 {
                            "offset + i".to_string()
                        } else {
                            format!("offset + i * {size}")
                        };
                        o.line(2, &format!("{l}.push({});", self.get(*elem, inst, &at)));
                        o.line(1, "}");
                        o.line(1, &format!("offset += {bytes};"));
                    }
                }
            }
        }
    }
}

fn ts_doc(s: &str) -> String {
    one_line(s).replace("*/", "* /")
}

fn ts_cexpr(e: &CExpr, m: &Model, si: usize, locals: &[String], top: bool) -> String {
    match e {
        CExpr::Lit(v) => v.to_string(),
        CExpr::Field(i) => {
            let big = matches!(
                m.structs[si].fields[*i].kind,
                Kind::One(Elem::Scalar(Scalar::U(8) | Scalar::S(8), _))
            );
            if big {
                format!("Number({})", locals[*i])
            } else {
                locals[*i].clone()
            }
        }
        CExpr::Bin(CBin::Div, a, b) => format!(
            "Math.trunc({} / {})",
            ts_cexpr(a, m, si, locals, false),
            ts_cexpr(b, m, si, locals, false)
        ),
        CExpr::Bin(op, a, b) => {
            let s = format!(
                "{} {} {}",
                ts_cexpr(a, m, si, locals, false),
                cbin_symbol(*op),
                ts_cexpr(b, m, si, locals, false)
            );
            if top { s } else { format!("({s})") }
        }
    }
}

const TS_HELPERS: &[(&str, &str)] = &[
    (
        "need",
        "function need(view: DataView, offset: number, size: number): void {
  if (size < 0 || offset + size > view.byteLength) {
    const have = Math.max(view.byteLength - offset, 0);
    throw new ParseError(`need ${size} bytes at offset ${offset}, have ${have}`);
  }
}
",
    ),
    (
        "count",
        "function count(n: number, offset: number): number {
  if (!Number.isSafeInteger(n) || n < 0) {
    throw new ParseError(`invalid element count ${n} at offset ${offset}`);
  }
  return n;
}
",
    ),
    (
        "chars",
        "function chars(view: DataView, offset: number, size: number): string {
  need(view, offset, size);
  let s = \"\";
  for (let i = 0; i < size; i++) {
    const c = view.getUint8(offset + i);
    if (c === 0) {
      break;
    }
    s += String.fromCharCode(c);
  }
  return s;
}
",
    ),
    (
        "cstr",
        "function cstr(view: DataView, offset: number): Parsed<string> {
  let s = \"\";
  for (let i = offset; i < view.byteLength; i++) {
    const c = view.getUint8(i);
    if (c === 0) {
      return { value: s, size: i + 1 - offset };
    }
    s += String.fromCharCode(c);
  }
  throw new ParseError(`unterminated string at offset ${offset}`);
}
",
    ),
];

// ===========================================================================
// Rust
// ===========================================================================

struct Rs<'m> {
    m: &'m Model,
    n: Names,
    used: BTreeSet<&'static str>,
}

impl<'m> Rs<'m> {
    fn new(m: &'m Model) -> Self {
        Rs {
            m,
            n: Names::new(m, Language::Rust),
            used: BTreeSet::new(),
        }
    }

    fn method(&self, e: Endian) -> String {
        if e == self.m.default {
            "read".into()
        } else {
            format!("read_{}", endian_suffix(e))
        }
    }

    fn emit(mut self) -> String {
        let mut body = Out::new("    ");
        for (i, e) in self.m.enums.iter().enumerate() {
            let name = &self.n.enums[i];
            body.blank();
            let doc = e
                .doc
                .clone()
                .unwrap_or_else(|| format!("Values of the template enum `{}`.", e.name));
            body.line(0, &format!("/// {}", one_line(&doc)));
            body.line(0, "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]");
            body.line(0, &format!("pub struct {name}(pub {});", rs_scalar(e.base)));
            if e.variants.is_empty() {
                continue;
            }
            body.blank();
            body.line(0, &format!("impl {name} {{"));
            for (j, v) in e.variants.iter().enumerate() {
                if let Some(end) = v.end {
                    body.line(1, &format!("/// Start of the range {} ..= {end}.", v.value));
                }
                body.line(
                    1,
                    &format!(
                        "pub const {}: Self = Self({});",
                        self.n.variants[i][j], v.value
                    ),
                );
            }
            body.line(0, "}");
        }
        for i in 0..self.m.structs.len() {
            self.structure(i, &mut body);
        }

        let deps: &[(&str, &[&str])] = &[("take", &["slice"]), ("chars", &["slice", "latin1"])];
        for (h, ds) in deps {
            if self.used.contains(h) {
                self.used.extend(ds.iter().copied());
            }
        }
        let root = &self.n.structs[self.m.root];
        let mut out = Out::new("    ");
        out.line(
            0,
            &format!(
                "// Parser for `{root}` generated by Hexa from a structure template. Do not edit."
            ),
        );
        out.blank();
        out.buf.push_str(RS_ERROR);
        out.buf.push_str(&body.buf);
        for (name, code) in RS_HELPERS {
            if self.used.contains(name) {
                out.blank();
                out.buf.push_str(code);
            }
        }
        out.buf
    }

    fn structure(&mut self, i: usize, o: &mut Out) {
        let s = self.m.structs[i].clone();
        let name = self.n.structs[i].clone();
        o.blank();
        let doc = s
            .doc
            .clone()
            .unwrap_or_else(|| format!("Template struct `{}`.", s.name));
        o.line(0, &format!("/// {}", one_line(&doc)));
        o.line(0, "#[derive(Debug, Clone, PartialEq)]");
        let named: Vec<usize> = (0..s.fields.len())
            .filter(|j| !self.n.fields[i][*j].is_empty())
            .collect();
        if named.is_empty() {
            o.line(0, &format!("pub struct {name} {{}}"));
        } else {
            o.line(0, &format!("pub struct {name} {{"));
            for &j in &named {
                if let Some(d) = &s.fields[j].doc {
                    o.line(1, &format!("/// {}", one_line(d)));
                }
                o.line(
                    1,
                    &format!(
                        "pub {}: {},",
                        self.n.fields[i][j],
                        self.ty(&s.fields[j].kind)
                    ),
                );
            }
            o.line(0, "}");
        }
        o.blank();
        o.line(0, &format!("impl {name} {{"));
        o.line(
            1,
            "/// Parses a value from the start of `data`; returns it and its size in bytes.",
        );
        o.line(
            1,
            "pub fn parse(data: &[u8]) -> Result<(Self, usize), ParseError> {",
        );
        o.line(2, "let mut off = 0;");
        o.line(2, "let value = Self::read(data, &mut off)?;");
        o.line(2, "Ok((value, off))");
        o.line(1, "}");
        let (d, of) = if s.fields.is_empty() {
            ("_data", "_off")
        } else {
            ("data", "off")
        };
        for &inst in &s.instances {
            o.blank();
            if inst == self.m.default {
                o.line(1, "/// Reads a value at `*off` and advances `off` past it.");
                o.line(
                    1,
                    &format!(
                        "pub fn read({d}: &[u8], {of}: &mut usize) -> Result<Self, ParseError> {{"
                    ),
                );
            } else {
                o.line(
                    1,
                    &format!(
                        "fn {}({d}: &[u8], {of}: &mut usize) -> Result<Self, ParseError> {{",
                        self.method(inst)
                    ),
                );
            }
            for j in 0..s.fields.len() {
                self.field(i, j, inst, o);
            }
            let fields: Vec<&str> = named
                .iter()
                .map(|j| self.n.fields[i][*j].as_str())
                .collect();
            if fields.is_empty() {
                o.line(2, "Ok(Self {})");
            } else {
                o.line(2, "Ok(Self {");
                for f in fields {
                    o.line(3, &format!("{f},"));
                }
                o.line(2, "})");
            }
            o.line(1, "}");
        }
        o.line(0, "}");
    }

    fn ty(&self, k: &Kind) -> String {
        match k {
            Kind::One(e) => self.elem_ty(*e),
            Kind::Array(e, _) => format!("Vec<{}>", self.elem_ty(*e)),
            Kind::Chars(_) | Kind::Skip(_) => "String".into(),
        }
    }

    fn elem_ty(&self, e: Elem) -> String {
        match e {
            Elem::Scalar(s, _) => rs_scalar(s).into(),
            Elem::Enum(i, _) => self.n.enums[i].clone(),
            Elem::Struct(i, _) => self.n.structs[i].clone(),
            Elem::CStr => "String".into(),
        }
    }

    fn read(&mut self, e: Elem, inst: Endian) -> String {
        match e {
            Elem::Struct(j, x) => format!(
                "{}::{}(data, off)?",
                self.n.structs[j],
                self.method(x.unwrap_or(inst))
            ),
            Elem::CStr => {
                self.used.insert("cstr");
                "cstr(data, off)?".into()
            }
            Elem::Scalar(..) | Elem::Enum(..) => {
                self.used.insert("take");
                let s = self.m.scalar_of(e).unwrap_or(Scalar::U(1));
                let from = match Model::elem_endian(e, inst) {
                    Endian::Little => "from_le_bytes",
                    Endian::Big => "from_be_bytes",
                };
                let raw = match s {
                    Scalar::Bool | Scalar::Char => "u8::from_le_bytes(take(data, off)?)".into(),
                    Scalar::U(1) | Scalar::S(1) => {
                        format!("{}::from_le_bytes(take(data, off)?)", rs_scalar(s))
                    }
                    _ => format!("{}::{from}(take(data, off)?)", rs_scalar(s)),
                };
                match (e, s) {
                    (Elem::Enum(i, _), _) => format!("{}({raw})", self.n.enums[i]),
                    (_, Scalar::Bool) => format!("{raw} != 0"),
                    (_, Scalar::Char) => format!("char::from({raw})"),
                    _ => raw,
                }
            }
        }
    }

    fn count(&mut self, si: usize, c: &Count) -> String {
        match c {
            Count::Const(n) => n.to_string(),
            Count::Expr(e) => {
                self.used.insert("count");
                format!("count({}, *off)?", rs_cexpr(e, &self.n.fields[si]))
            }
        }
    }

    fn field(&mut self, si: usize, fi: usize, inst: Endian, o: &mut Out) {
        let f = self.m.structs[si].fields[fi].clone();
        let l = self.n.fields[si][fi].clone();
        match &f.kind {
            Kind::One(e) => {
                let r = self.read(*e, inst);
                o.line(2, &format!("let {l} = {r};"));
            }
            Kind::Chars(c) => {
                let n = self.count(si, c);
                self.used.insert("chars");
                o.line(2, &format!("let {l} = latin1(slice(data, off, {n})?);"));
            }
            Kind::Skip(c) => {
                let n = self.count(si, c);
                self.used.insert("slice");
                o.line(2, &format!("slice(data, off, {n})?;"));
            }
            Kind::Array(Elem::Scalar(Scalar::U(1), _), c) => {
                let n = self.count(si, c);
                self.used.insert("slice");
                o.line(2, &format!("let {l} = slice(data, off, {n})?.to_vec();"));
            }
            Kind::Array(elem, c) => {
                let n = self.count(si, c);
                let r = self.read(*elem, inst);
                o.line(2, &format!("let {l} = {{"));
                match c {
                    Count::Const(k) => {
                        o.line(3, &format!("let mut v = Vec::with_capacity({k});"));
                        o.line(3, &format!("for _ in 0..{k} {{"));
                    }
                    Count::Expr(_) => {
                        o.line(3, &format!("let n = {n};"));
                        o.line(
                            3,
                            "let mut v = Vec::with_capacity(n.min(data.len().saturating_sub(*off)));",
                        );
                        o.line(3, "for _ in 0..n {");
                    }
                }
                o.line(4, &format!("v.push({r});"));
                o.line(3, "}");
                o.line(3, "v");
                o.line(2, "};");
            }
        }
    }
}

fn rs_scalar(s: Scalar) -> &'static str {
    match s {
        Scalar::U(1) => "u8",
        Scalar::S(1) => "i8",
        Scalar::U(2) => "u16",
        Scalar::S(2) => "i16",
        Scalar::U(4) => "u32",
        Scalar::S(4) => "i32",
        Scalar::U(_) => "u64",
        Scalar::S(_) => "i64",
        Scalar::F32 => "f32",
        Scalar::F64 => "f64",
        Scalar::Bool => "bool",
        Scalar::Char => "char",
    }
}

fn rs_cexpr(e: &CExpr, fields: &[String]) -> String {
    match e {
        CExpr::Lit(v) => format!("{v}_i128"),
        CExpr::Field(i) => format!("i128::from({})", fields[*i]),
        CExpr::Bin(op, a, b) => {
            let (a, b) = (rs_cexpr(a, fields), rs_cexpr(b, fields));
            match op {
                CBin::Add => format!("{a}.saturating_add({b})"),
                CBin::Sub => format!("{a}.saturating_sub({b})"),
                CBin::Mul => format!("{a}.saturating_mul({b})"),
                CBin::Div => format!("({a} / {b})"),
                CBin::Rem => format!("({a} % {b})"),
            }
        }
    }
}

const RS_ERROR: &str = "/// Error returned when the data does not match the structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Offset at which decoding failed.
    pub offset: usize,
    /// What went wrong.
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, \"{} at offset {}\", self.message, self.offset)
    }
}

impl std::error::Error for ParseError {}
";

const RS_HELPERS: &[(&str, &str)] = &[
    (
        "slice",
        "fn slice<'a>(data: &'a [u8], off: &mut usize, len: usize) -> Result<&'a [u8], ParseError> {
    let start = *off;
    match start.checked_add(len) {
        Some(end) if end <= data.len() => {
            *off = end;
            Ok(&data[start..end])
        }
        _ => Err(ParseError {
            offset: start,
            message: format!(\"need {len} bytes, have {}\", data.len().saturating_sub(start)),
        }),
    }
}
",
    ),
    (
        "take",
        "fn take<const N: usize>(data: &[u8], off: &mut usize) -> Result<[u8; N], ParseError> {
    let mut buf = [0u8; N];
    buf.copy_from_slice(slice(data, off, N)?);
    Ok(buf)
}
",
    ),
    (
        "count",
        "fn count(n: i128, off: usize) -> Result<usize, ParseError> {
    usize::try_from(n).map_err(|_| ParseError {
        offset: off,
        message: format!(\"invalid element count {n}\"),
    })
}
",
    ),
    (
        "latin1",
        "fn latin1(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    bytes[..end].iter().map(|&b| char::from(b)).collect()
}
",
    ),
    (
        "cstr",
        "fn cstr(data: &[u8], off: &mut usize) -> Result<String, ParseError> {
    let start = *off;
    let rest = data.get(start..).unwrap_or(&[]);
    match rest.iter().position(|&b| b == 0) {
        Some(len) => {
            *off = start + len + 1;
            Ok(rest[..len].iter().map(|&b| char::from(b)).collect())
        }
        None => Err(ParseError {
            offset: start,
            message: \"unterminated string\".to_string(),
        }),
    }
}
",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    #[test]
    fn scalar_ranges() {
        assert_eq!(Scalar::U(1).range(), (0, 255));
        assert_eq!(Scalar::S(2).range(), (-32768, 32767));
        assert_eq!(Scalar::U(8).range().1, i128::from(u64::MAX));
    }

    #[test]
    fn model_counts_and_instances() {
        let p = parse(
            "struct In { u16 a; }; struct R { u8 n; u8 b[n * 2 + 1]; In x; be In y; padding[2]; };",
        )
        .expect("parse");
        let m = Model::build(&p, "R", Language::Rust).expect("model");
        let r = &m.structs[m.root];
        assert!(matches!(r.fields[1].kind, Kind::Array(_, Count::Expr(_))));
        assert!(matches!(r.fields[4].kind, Kind::Skip(Count::Const(2))));
        let inner = &m.structs[1];
        assert_eq!(inner.instances, [Endian::Little, Endian::Big]);
        assert_eq!(m.min_size(m.root, 0), 1 + 2 + 2 + 2);
    }

    #[test]
    fn names_escape_keywords() {
        let p = parse("struct S { u8 type; u8 data; u8 class; };").expect("parse");
        let m = Model::build(&p, "S", Language::Rust).expect("model");
        assert_eq!(
            Names::new(&m, Language::Rust).fields[0],
            ["r#type", "data_", "class"]
        );
        assert_eq!(
            Names::new(&m, Language::Python).fields[0],
            ["type", "data_", "class_"]
        );
        let ts = Names::new(&m, Language::TypeScript);
        assert_eq!(ts.fields[0], ["type", "data", "class"]);
        assert_eq!(ts.locals[0], ["type_", "data", "class_"]);
        assert_eq!(
            Names::new(&m, Language::Go).fields[0],
            ["Type", "Data", "Class"]
        );
    }
}
