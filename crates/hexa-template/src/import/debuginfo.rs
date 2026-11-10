//! DWARF / PDB type import (STRC-07).
//!
//! Debug information describes the exact layout a compiler chose for every
//! structure, so imported types are emitted with *explicit* layout: fields in
//! offset order, `padding[n];` for every gap and up to the recorded byte size,
//! overlapping members wrapped in synthesized unions, and bit-fields grouped
//! into `bitfield` types that cover exactly the bytes they occupy. The
//! evaluator therefore reproduces the compiler's offsets without having to
//! know its alignment rules.
//!
//! The import runs in three stages:
//!
//! 1. a front-end ([`dwarf_model`], [`pdb_model`]) reads the debug format into
//!    a small [`TypeModel`] of records, enums and aliases with byte (and bit)
//!    offsets;
//! 2. the shared naming pass gives anonymous types stable names, merges
//!    identical definitions repeated across compilation units and suffixes
//!    differing ones (`Point`, `Point_2`);
//! 3. the shared emitter ([`emit`]) turns the model into a template
//!    [`Program`].

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use crate::ast::{
    AliasDef, ArraySpec, Attr, BitfieldDef, BitfieldMember, Endian, EnumDef, EnumVariant, Expr,
    FieldDecl, Program, Stmt, StmtKind, StructDef, StructKind, TypeDef, TypeRef,
};
use crate::prim::{Prim, sign_extend};
use crate::{Error, Result};

// ===========================================================================
// Intermediate model
// ===========================================================================

/// Index of a type in [`TypeModel::types`].
pub type TypeId = usize;

/// Type of a member, alias target or array element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    /// Built-in primitive by template name (`u32`, `s8`, `char`, `float`, ...).
    Prim(String),
    /// Another type of the model.
    Ref(TypeId),
    /// Fixed-size array; `0` elements for unknown bounds.
    Array(Box<Ty>, u64),
    /// Data pointer: an unsigned integer of `size` bytes shown as `pointee*`.
    Pointer { pointee: String, size: u64 },
    /// Opaque bytes (types without a template equivalent, `void`, ...).
    Bytes(u64),
}

impl Ty {
    /// Primitive by template name.
    pub fn prim(name: &str) -> Ty {
        Ty::Prim(name.to_string())
    }

    /// `count` elements of `elem`.
    pub fn array(elem: Ty, count: u64) -> Ty {
        Ty::Array(Box::new(elem), count)
    }
}

/// Placement of a bit-field member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitField {
    /// Absolute bit offset from the start of the record, numbered like
    /// `DW_AT_data_bit_offset`: from the least significant bit of byte 0 on
    /// little-endian targets, from its most significant bit on big-endian
    /// ones.
    pub offset: u64,
    /// Width in bits (1..=128).
    pub width: u32,
}

/// Data member of a [`Record`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// `None` for anonymous members (unnamed unions, unnamed bit-fields).
    pub name: Option<String>,
    pub ty: Ty,
    /// Byte offset from the start of the record (ignored for bit-fields).
    pub offset: u64,
    pub bit_field: Option<BitField>,
}

impl Member {
    /// Regular member at a byte offset.
    pub fn new(name: Option<&str>, ty: Ty, offset: u64) -> Self {
        Self {
            name: name.map(str::to_string),
            ty,
            offset,
            bit_field: None,
        }
    }

    /// Bit-field member at an absolute bit offset (see [`BitField::offset`]).
    pub fn bits(name: Option<&str>, ty: Ty, bit_offset: u64, width: u32) -> Self {
        Self {
            name: name.map(str::to_string),
            ty,
            offset: bit_offset / 8,
            bit_field: Some(BitField {
                offset: bit_offset,
                width,
            }),
        }
    }
}

/// Structure, class or union with its compiler-chosen layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// `None` for anonymous types (named by the naming pass).
    pub name: Option<String>,
    pub union: bool,
    /// Size in bytes, including trailing padding.
    pub size: u64,
    /// Members in any order; overlapping members are allowed.
    pub members: Vec<Member>,
}

/// Enumeration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumType {
    pub name: Option<String>,
    /// Size of the underlying integer in bytes.
    pub size: u64,
    pub signed: bool,
    pub variants: Vec<(String, i128)>,
}

/// Typedef.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alias {
    pub name: String,
    pub target: Ty,
}

/// One type of a [`TypeModel`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelType {
    Record(Record),
    Enum(EnumType),
    Alias(Alias),
}

impl ModelType {
    fn name(&self) -> Option<&str> {
        match self {
            ModelType::Record(r) => r.name.as_deref(),
            ModelType::Enum(e) => e.name.as_deref(),
            ModelType::Alias(a) => Some(&a.name),
        }
    }

    fn kind_word(&self) -> &'static str {
        match self {
            ModelType::Record(r) if r.union => "union",
            ModelType::Record(_) => "struct",
            ModelType::Enum(_) => "enum",
            ModelType::Alias(_) => "typedef",
        }
    }
}

/// Front-end independent description of imported types.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypeModel {
    /// Byte order of the target (also selects the bit-field numbering).
    pub endian: Endian,
    pub types: Vec<ModelType>,
}

impl TypeModel {
    pub fn new(endian: Endian) -> Self {
        Self {
            endian,
            types: Vec::new(),
        }
    }

    /// Appends a type and returns its id.
    pub fn add(&mut self, t: ModelType) -> TypeId {
        self.types.push(t);
        self.types.len() - 1
    }

    /// Size of `ty` in bytes (0 when unknown).
    pub fn size_of(&self, ty: &Ty) -> u64 {
        self.size_in(ty, 0)
    }

    fn size_in(&self, ty: &Ty, depth: u32) -> u64 {
        if depth > 64 {
            return 0;
        }
        match ty {
            Ty::Prim(p) => Prim::from_name(p).and_then(Prim::size).unwrap_or(0),
            Ty::Ref(t) => match self.types.get(*t) {
                Some(ModelType::Record(r)) => r.size,
                Some(ModelType::Enum(e)) => e.size,
                Some(ModelType::Alias(a)) => self.size_in(&a.target, depth + 1),
                None => 0,
            },
            Ty::Array(e, n) => self.size_in(e, depth + 1).saturating_mul(*n),
            Ty::Pointer { size, .. } => *size,
            Ty::Bytes(n) => *n,
        }
    }

    /// Converts the model into a template program (see [`emit`]).
    pub fn to_program(&self) -> Result<Program> {
        emit(self)
    }

    fn validate(&self) -> Result<()> {
        for (id, t) in self.types.iter().enumerate() {
            let what = || format!("{} #{id}", t.kind_word());
            match t {
                ModelType::Record(r) => {
                    for m in &r.members {
                        self.validate_ty(&m.ty, &what)?;
                        if let Some(b) = m.bit_field
                            && b.width > 128
                        {
                            return Err(Error::Invalid(format!(
                                "{}: bit-field `{}` is {} bits wide (at most 128 supported)",
                                what(),
                                m.name.as_deref().unwrap_or("<anonymous>"),
                                b.width
                            )));
                        }
                    }
                }
                ModelType::Alias(a) => self.validate_ty(&a.target, &what)?,
                ModelType::Enum(_) => {}
            }
        }
        Ok(())
    }

    fn validate_ty(&self, ty: &Ty, what: &dyn Fn() -> String) -> Result<()> {
        match ty {
            Ty::Prim(p) => match Prim::from_name(p).and_then(Prim::size) {
                Some(_) => Ok(()),
                None => Err(Error::Invalid(format!(
                    "{}: `{p}` is not a fixed-size primitive type",
                    what()
                ))),
            },
            Ty::Ref(t) if *t >= self.types.len() => Err(Error::Invalid(format!(
                "{}: reference to type #{t} out of range",
                what()
            ))),
            Ty::Array(e, _) => self.validate_ty(e, what),
            Ty::Ref(_) | Ty::Pointer { .. } | Ty::Bytes(_) => Ok(()),
        }
    }
}

// ===========================================================================
// Naming
// ===========================================================================

static OPAQUE: Ty = Ty::Bytes(0);

/// Follows references to transparent aliases.
fn canon<'a>(model: &'a TypeModel, transparent: &[bool], ty: &'a Ty) -> &'a Ty {
    let mut ty = ty;
    for _ in 0..64 {
        match ty {
            Ty::Ref(t) if transparent[*t] => match &model.types[*t] {
                ModelType::Alias(a) => ty = &a.target,
                _ => return ty,
            },
            _ => return ty,
        }
    }
    &OPAQUE
}

/// Keeps identifier characters and `::` scopes; everything else becomes `_`.
fn sanitize(name: &str) -> String {
    let mut s = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == ':' {
            s.push(c);
        } else if !s.ends_with('_') {
            s.push('_');
        }
    }
    if s.is_empty() || s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    s
}

/// Referenced type when `ty` is a (possibly array of) model type.
fn inner_ref(ty: &Ty) -> Option<TypeId> {
    match ty {
        Ty::Ref(t) => Some(*t),
        Ty::Array(e, _) => inner_ref(e),
        _ => None,
    }
}

struct Naming {
    names: Vec<String>,
    transparent: Vec<bool>,
    emit: Vec<bool>,
    used: HashSet<String>,
}

impl Naming {
    fn compute(model: &TypeModel) -> Naming {
        let n = model.types.len();
        let mut base: Vec<Option<String>> =
            model.types.iter().map(|t| t.name().map(sanitize)).collect();
        // `typedef struct { ... } Foo;` names the anonymous struct `Foo`.
        for t in &model.types {
            if let ModelType::Alias(a) = t
                && let Ty::Ref(target) = a.target
                && base[target].is_none()
                && !matches!(model.types[target], ModelType::Alias(_))
            {
                base[target] = Some(sanitize(&a.name));
            }
        }
        // Anonymous member types are named after their owner and member.
        loop {
            let mut changed = false;
            for (id, t) in model.types.iter().enumerate() {
                let ModelType::Record(r) = t else { continue };
                let Some(owner) = base[id].clone() else {
                    continue;
                };
                for (k, m) in r.members.iter().enumerate() {
                    if let Some(target) = inner_ref(&m.ty)
                        && base[target].is_none()
                    {
                        let member = m
                            .name
                            .as_deref()
                            .map_or_else(|| format!("anon{k}"), sanitize);
                        base[target] = Some(format!("{owner}__{member}"));
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let mut base: Vec<String> = base
            .into_iter()
            .enumerate()
            .map(|(id, b)| b.unwrap_or_else(|| format!("anon_{}{id}", model.types[id].kind_word())))
            .collect();
        // Records and enums must not shadow built-in types.
        for (id, t) in model.types.iter().enumerate() {
            if !matches!(t, ModelType::Alias(_)) {
                while Prim::from_name(&base[id]).is_some() {
                    base[id].push('_');
                }
            }
        }
        let mut transparent = vec![false; n];
        for (id, t) in model.types.iter().enumerate() {
            if let ModelType::Alias(a) = t {
                transparent[id] = alias_is_transparent(&base, &base[id], &a.target);
            }
        }
        let taken: HashSet<&str> = base
            .iter()
            .enumerate()
            .filter(|(id, _)| !transparent[*id])
            .map(|(_, b)| b.as_str())
            .collect();
        let mut used: HashSet<String> = HashSet::new();
        let mut names = vec![String::new(); n];
        let mut emit = vec![false; n];
        let mut groups: HashMap<&str, Vec<(String, String)>> = HashMap::new();
        for id in 0..n {
            if transparent[id] {
                names[id].clone_from(&base[id]);
                continue;
            }
            let sig = signature(model, &base, &transparent, id);
            let group = groups.entry(base[id].as_str()).or_default();
            if let Some((_, f)) = group.iter().find(|(s, _)| *s == sig) {
                names[id].clone_from(f);
                continue;
            }
            let mut name = base[id].clone();
            if !group.is_empty() || used.contains(&name) {
                let mut k = group.len() + 1;
                loop {
                    let candidate = format!("{}_{k}", base[id]);
                    if !used.contains(&candidate) && !taken.contains(candidate.as_str()) {
                        name = candidate;
                        break;
                    }
                    k += 1;
                }
            }
            used.insert(name.clone());
            group.push((sig, name.clone()));
            names[id] = name;
            emit[id] = true;
        }
        Naming {
            names,
            transparent,
            emit,
            used,
        }
    }
}

/// Aliases that would only restate their target, or would shadow a built-in
/// type with a different meaning, are resolved away instead of emitted.
fn alias_is_transparent(base: &[String], name: &str, target: &Ty) -> bool {
    match target {
        Ty::Ref(t) if base[*t] == name => return true,
        Ty::Bytes(0) => return true,
        _ => {}
    }
    if let Some(p) = Prim::from_name(name) {
        return !matches!(target, Ty::Prim(tp) if tp != name && Prim::from_name(tp) == Some(p));
    }
    false
}

fn ty_signature(model: &TypeModel, base: &[String], transparent: &[bool], ty: &Ty) -> String {
    match canon(model, transparent, ty) {
        Ty::Prim(p) => p.clone(),
        Ty::Ref(t) => format!("@{}", base[*t]),
        Ty::Array(e, n) => format!("{}[{n}]", ty_signature(model, base, transparent, e)),
        Ty::Pointer { pointee, size } => format!("*{pointee}/{size}"),
        Ty::Bytes(n) => format!("b{n}"),
    }
}

/// Shallow structural signature used to merge repeated definitions.
fn signature(model: &TypeModel, base: &[String], transparent: &[bool], id: TypeId) -> String {
    match &model.types[id] {
        ModelType::Record(r) => {
            let mut s = format!("R{}:{}{{", u8::from(r.union), r.size);
            for m in &r.members {
                let _ = write!(s, "{}@{}", m.name.as_deref().unwrap_or(""), m.offset);
                if let Some(b) = m.bit_field {
                    let _ = write!(s, "#{}:{}", b.offset, b.width);
                }
                let _ = write!(s, "={};", ty_signature(model, base, transparent, &m.ty));
            }
            s
        }
        ModelType::Enum(e) => format!("E{}:{}:{:?}", e.size, e.signed, e.variants),
        ModelType::Alias(a) => format!("A{}", ty_signature(model, base, transparent, &a.target)),
    }
}

// ===========================================================================
// Emitter
// ===========================================================================

/// Converts a [`TypeModel`] into a template program with explicit layout.
///
/// Every record becomes a `struct`/`union` without `c_layout`: gaps and
/// trailing bytes are `padding[n];`, bit-fields are grouped into `bitfield`
/// types (fields `__anon_bitsN`), overlapping members are wrapped in
/// synthesized unions (`__anon_uN`), and pointers are integers carrying a
/// `type_name("T*")` attribute. Members named `__anon...` are searched
/// transparently by member access in expressions.
pub fn emit(model: &TypeModel) -> Result<Program> {
    model.validate()?;
    let naming = Naming::compute(model);
    let mut em = Emitter {
        model,
        names: naming.names,
        transparent: naming.transparent,
        used: naming.used,
        out: Vec::new(),
    };
    for (id, t) in model.types.iter().enumerate() {
        if !naming.emit[id] || em.transparent[id] {
            continue;
        }
        match t {
            ModelType::Record(r) => em.record(id, r),
            ModelType::Enum(e) => em.enumeration(id, e),
            ModelType::Alias(a) => em.alias(id, a),
        }
    }
    let mut pragmas = Vec::new();
    if model.endian == Endian::Big {
        pragmas.push(("endian".to_string(), "big".to_string()));
    }
    Ok(Program {
        items: em.out,
        pragmas,
    })
}

/// A laid-out member: the bytes it covers and its statement.
struct Item {
    begin: u64,
    end: u64,
    stmt: Stmt,
}

type BitEntry<'m> = (&'m Member, BitField);

struct Emitter<'m> {
    model: &'m TypeModel,
    names: Vec<String>,
    transparent: Vec<bool>,
    used: HashSet<String>,
    out: Vec<Stmt>,
}

fn field_stmt(f: FieldDecl) -> Stmt {
    Stmt::synthetic(StmtKind::Field(f))
}

fn plain_field(ty: &str, name: &str) -> FieldDecl {
    FieldDecl {
        ty: TypeRef::named(ty),
        name: name.to_string(),
        array: None,
        pointer: None,
        placement: None,
        bits: None,
        attrs: Vec::new(),
    }
}

fn padding(n: u64) -> Stmt {
    let mut f = plain_field("padding", "padding");
    f.array = Some(ArraySpec::Fixed(Expr::int(i128::from(n))));
    field_stmt(f)
}

fn struct_def(name: String, union: bool, body: Vec<Stmt>) -> TypeDef {
    TypeDef::Struct(StructDef {
        name,
        kind: if union {
            StructKind::Union
        } else {
            StructKind::Struct
        },
        params: Vec::new(),
        inherits: None,
        body,
        attrs: Vec::new(),
    })
}

fn member_name(m: &Member, k: usize) -> String {
    m.name
        .as_deref()
        .map_or_else(|| format!("__anon{k}"), sanitize)
}

fn bit_range(b: BitField) -> (u64, u64) {
    (b.offset / 8, (b.offset + u64::from(b.width)).div_ceil(8))
}

fn group_range(g: &[BitEntry<'_>]) -> (u64, u64) {
    g.iter().fold((u64::MAX, 0), |(lo, hi), (_, b)| {
        let (s, e) = bit_range(*b);
        (lo.min(s), hi.max(e))
    })
}

/// Integer primitive of `size` bytes, if the template language has one.
fn int_prim(size: u64, signed: bool) -> Option<String> {
    matches!(size, 1 | 2 | 3 | 4 | 6 | 8 | 12 | 16)
        .then(|| format!("{}{}", if signed { "s" } else { "u" }, size * 8))
}

impl<'m> Emitter<'m> {
    fn canon<'a>(&self, ty: &'a Ty) -> &'a Ty
    where
        'm: 'a,
    {
        canon(self.model, &self.transparent, ty)
    }

    fn fresh(&mut self, base: String) -> String {
        if self.used.insert(base.clone()) {
            return base;
        }
        let mut k = 2;
        loop {
            let c = format!("{base}_{k}");
            if self.used.insert(c.clone()) {
                return c;
            }
            k += 1;
        }
    }

    fn push(&mut self, def: TypeDef) {
        self.out.push(Stmt::synthetic(StmtKind::TypeDef(def)));
    }

    /// Element type and flattened element count of (nested) arrays.
    fn flatten<'a>(&self, ty: &'a Ty) -> (&'a Ty, Option<u64>)
    where
        'm: 'a,
    {
        let mut t = self.canon(ty);
        let mut count: Option<u64> = None;
        while let Ty::Array(e, n) = t {
            count = Some(count.unwrap_or(1).saturating_mul(*n));
            t = self.canon(e);
        }
        (t, count)
    }

    fn field(&self, name: &str, ty: &Ty) -> FieldDecl {
        let (elem, count) = self.flatten(ty);
        let mut attrs = Vec::new();
        let (tname, count) = match elem {
            Ty::Prim(p) => (p.clone(), count),
            Ty::Ref(t) => (self.names[*t].clone(), count),
            Ty::Pointer { pointee, size } => match int_prim(*size, false) {
                Some(p) => {
                    let shown = match count {
                        Some(c) => format!("{pointee}*[{c}]"),
                        None => format!("{pointee}*"),
                    };
                    attrs.push(Attr::string("type_name", shown));
                    attrs.push(Attr::new("format", vec![Expr::ident("hex")]));
                    (p, count)
                }
                None => (
                    "u8".to_string(),
                    Some(count.unwrap_or(1).saturating_mul(*size)),
                ),
            },
            Ty::Bytes(n) => (
                "u8".to_string(),
                Some(count.unwrap_or(1).saturating_mul(*n)),
            ),
            Ty::Array(..) => unreachable!("flatten strips arrays"),
        };
        let mut f = plain_field(&tname, name);
        f.array = count.map(|c| ArraySpec::Fixed(Expr::int(i128::from(c))));
        f.attrs = attrs;
        f
    }

    fn record(&mut self, id: TypeId, r: &'m Record) {
        let owner = self.names[id].clone();
        let items = self.items(&owner, &r.members, r.union);
        let body = if r.union {
            self.union_body(&owner, items, r.size)
        } else {
            self.struct_body(&owner, items, r.size)
        };
        self.push(struct_def(owner, r.union, body));
    }

    fn items(&mut self, owner: &str, members: &'m [Member], union: bool) -> Vec<Item> {
        let mut items = Vec::new();
        let mut bits: Vec<BitEntry<'m>> = Vec::new();
        for (k, m) in members.iter().enumerate() {
            if let Some(b) = m.bit_field {
                if b.width > 0 {
                    bits.push((m, b));
                }
                continue;
            }
            let size = self.model.size_of(&m.ty);
            items.push(Item {
                begin: m.offset,
                end: m.offset.saturating_add(size),
                stmt: field_stmt(self.field(&member_name(m, k), &m.ty)),
            });
        }
        bits.sort_by_key(|(_, b)| b.offset);
        let mut groups: Vec<Vec<BitEntry<'m>>> = Vec::new();
        for e in bits {
            match groups.last_mut() {
                Some(g) if !union && self.joins(g, &e) => g.push(e),
                _ => groups.push(vec![e]),
            }
        }
        for (gi, g) in groups.iter().enumerate() {
            let (begin, end) = group_range(g);
            let tname = self.bit_group(owner, gi, begin, end, g);
            items.push(Item {
                begin,
                end,
                stmt: field_stmt(plain_field(&tname, &format!("__anon_bits{gi}"))),
            });
        }
        items
    }

    /// Whether bit-field `e` continues group `g`: it shares a byte with it,
    /// or directly follows it within the same declared storage unit.
    fn joins(&self, g: &[BitEntry<'_>], e: &BitEntry<'_>) -> bool {
        let (gb, ge) = group_range(g);
        let (mb, me) = bit_range(e.1);
        if mb < ge {
            return true;
        }
        if mb > ge || me.max(ge) - gb > 16 {
            return false;
        }
        let unit = |m: &Member| self.model.size_of(&m.ty).max(1) * 8;
        let (first, fb) = g[0];
        let (u1, u2) = (unit(first), unit(e.0));
        u1 == u2 && fb.offset / u1 == e.1.offset / u2
    }

    fn bit_group(
        &mut self,
        owner: &str,
        gi: usize,
        begin: u64,
        end: u64,
        g: &[BitEntry<'m>],
    ) -> String {
        let name = self.fresh(format!("{owner}__bits{gi}"));
        let total = (end - begin) * 8;
        let pad = |n: u64| BitfieldMember {
            name: None,
            bits: Expr::int(i128::from(n)),
            ty: None,
            attrs: Vec::new(),
        };
        let mut members = Vec::new();
        let mut pos = 0u64;
        for (m, b) in g {
            let p = b.offset - begin * 8;
            if p < pos {
                continue; // overlapping bit-fields cannot be expressed
            }
            if p > pos {
                members.push(pad(p - pos));
            }
            members.push(BitfieldMember {
                name: m.name.as_deref().map(sanitize),
                bits: Expr::int(i128::from(b.width)),
                ty: self.bit_type(&m.ty),
                attrs: Vec::new(),
            });
            pos = p + u64::from(b.width);
        }
        if pos < total {
            members.push(pad(total - pos));
        }
        self.push(TypeDef::Bitfield(BitfieldDef {
            name: name.clone(),
            members,
            attrs: Vec::new(),
        }));
        name
    }

    /// Member type of a bit-field: the underlying primitive (for signedness
    /// and `bool`) or enum (for variant names).
    fn bit_type(&self, ty: &Ty) -> Option<TypeRef> {
        let mut t = self.canon(ty);
        for _ in 0..64 {
            match t {
                Ty::Prim(p) => return Some(TypeRef::named(p.clone())),
                Ty::Ref(id) => match &self.model.types[*id] {
                    ModelType::Enum(_) => return Some(TypeRef::named(self.names[*id].clone())),
                    ModelType::Alias(a) => t = self.canon(&a.target),
                    ModelType::Record(_) => return None,
                },
                _ => return None,
            }
        }
        None
    }

    fn struct_body(&mut self, owner: &str, mut items: Vec<Item>, size: u64) -> Vec<Stmt> {
        items.sort_by_key(|i| (i.begin, i.end));
        let mut body = Vec::new();
        let mut cursor = 0u64;
        let mut unions = 0usize;
        let mut iter = items.into_iter().peekable();
        while let Some(first) = iter.next() {
            let begin = first.begin;
            let mut end = first.end;
            let mut cluster = vec![first];
            while let Some(next) = iter.next_if(|n| n.begin < end) {
                end = end.max(next.end);
                cluster.push(next);
            }
            if begin > cursor {
                body.push(padding(begin - cursor));
            }
            if cluster.len() == 1 {
                body.extend(cluster.into_iter().map(|i| i.stmt));
            } else {
                body.push(self.overlap(owner, unions, cluster));
                unions += 1;
            }
            cursor = cursor.max(end);
        }
        if size > cursor {
            body.push(padding(size - cursor));
        }
        body
    }

    /// Overlapping members become a union whose alternatives are single
    /// members or structs of non-overlapping members.
    fn overlap(&mut self, owner: &str, k: usize, cluster: Vec<Item>) -> Stmt {
        let base = cluster[0].begin;
        let uname = self.fresh(format!("{owner}__u{k}"));
        let mut lanes: Vec<(u64, Vec<Item>)> = Vec::new();
        for it in cluster {
            match lanes.iter_mut().find(|(e, _)| *e <= it.begin) {
                Some(lane) => {
                    lane.0 = it.end;
                    lane.1.push(it);
                }
                None => lanes.push((it.end, vec![it])),
            }
        }
        let mut body = Vec::new();
        for (li, (lane_end, mut lane)) in lanes.into_iter().enumerate() {
            if lane.len() == 1 && lane[0].begin == base {
                body.push(lane.remove(0).stmt);
                continue;
            }
            let sname = self.fresh(format!("{uname}_s{li}"));
            let shifted = lane
                .into_iter()
                .map(|mut i| {
                    i.begin -= base;
                    i.end -= base;
                    i
                })
                .collect();
            let sbody = self.struct_body(&sname, shifted, lane_end - base);
            self.push(struct_def(sname.clone(), false, sbody));
            body.push(field_stmt(plain_field(&sname, &format!("__anon_s{li}"))));
        }
        self.push(struct_def(uname.clone(), true, body));
        field_stmt(plain_field(&uname, &format!("__anon_u{k}")))
    }

    fn union_body(&mut self, owner: &str, items: Vec<Item>, size: u64) -> Vec<Stmt> {
        let mut body = Vec::new();
        let mut max_end = 0u64;
        for (li, it) in items.into_iter().enumerate() {
            max_end = max_end.max(it.end);
            if it.begin == 0 {
                body.push(it.stmt);
                continue;
            }
            let sname = self.fresh(format!("{owner}__m{li}"));
            let sbody = vec![padding(it.begin), it.stmt];
            self.push(struct_def(sname.clone(), false, sbody));
            body.push(field_stmt(plain_field(&sname, &format!("__anon_m{li}"))));
        }
        if size > max_end {
            body.push(padding(size));
        }
        body
    }

    fn enumeration(&mut self, id: TypeId, e: &EnumType) {
        let base = int_prim(e.size, e.signed).unwrap_or_else(|| "u32".to_string());
        let variants = e
            .variants
            .iter()
            .map(|(name, v)| EnumVariant {
                name: sanitize(name),
                value: Some(Expr::int(*v)),
                end: None,
            })
            .collect();
        self.push(TypeDef::Enum(EnumDef {
            name: self.names[id].clone(),
            base: TypeRef::named(base),
            variants,
            attrs: Vec::new(),
        }));
    }

    fn alias(&mut self, id: TypeId, a: &Alias) {
        let (elem, count) = self.flatten(&a.target);
        let mut attrs = Vec::new();
        let (target, count) = match elem {
            Ty::Prim(p) => (p.clone(), count),
            Ty::Ref(t) => (self.names[*t].clone(), count),
            Ty::Pointer { size, .. } => match int_prim(*size, false) {
                Some(p) => {
                    attrs.push(Attr::new("format", vec![Expr::ident("hex")]));
                    (p, count)
                }
                None => (
                    "u8".to_string(),
                    Some(count.unwrap_or(1).saturating_mul(*size)),
                ),
            },
            Ty::Bytes(n) => (
                "u8".to_string(),
                Some(count.unwrap_or(1).saturating_mul(*n)),
            ),
            Ty::Array(..) => unreachable!("flatten strips arrays"),
        };
        self.push(TypeDef::Alias(AliasDef {
            name: self.names[id].clone(),
            target: TypeRef::named(target),
            array: count.map(|c| Expr::int(i128::from(c))),
            attrs,
        }));
    }
}

// ===========================================================================
// DWARF front-end
// ===========================================================================

type Rd<'a> = gimli::EndianSlice<'a, gimli::RunTimeEndian>;

fn dwarf_err(e: gimli::Error) -> Error {
    Error::Invalid(format!("malformed DWARF: {e}"))
}

/// Imports every type defined in the DWARF debug information of an ELF,
/// Mach-O, PE or COFF object (or a split `.dwo` file).
pub fn from_dwarf(object_file: &[u8]) -> Result<Program> {
    emit(&dwarf_model(object_file)?)
}

/// DWARF front-end: reads the debug information into a [`TypeModel`].
pub fn dwarf_model(object_file: &[u8]) -> Result<TypeModel> {
    use object::Object;

    let file = object::File::parse(object_file)
        .map_err(|e| Error::Invalid(format!("not a supported object file: {e}")))?;
    let little = file.is_little_endian();
    let has = |name: &str| file.section_by_name(name).is_some();
    let dwo = !has(".debug_info") && has(".debug_info.dwo");
    if !has(".debug_info") && !dwo && !has(".debug_types") {
        return Err(Error::Invalid(
            "the object file has no DWARF debug information (no .debug_info section)".into(),
        ));
    }
    let sections = gimli::DwarfSections::load(|id| -> Result<Vec<u8>> {
        let name = if dwo { id.dwo_name() } else { Some(id.name()) };
        match name.and_then(|n| file.section_by_name(n)) {
            Some(s) => section_bytes(&file, &s, little),
            None => Ok(Vec::new()),
        }
    })?;
    let endian = if little {
        gimli::RunTimeEndian::Little
    } else {
        gimli::RunTimeEndian::Big
    };
    let mut dwarf = sections.borrow(|s| gimli::EndianSlice::new(s, endian));
    if dwo {
        dwarf.file_type = gimli::DwarfFileType::Dwo;
    }
    let dies = collect_dies(&dwarf)?;
    let endian = if little { Endian::Little } else { Endian::Big };
    Ok(DwarfConv::new(&dies, endian).run())
}

/// Section contents, with absolute relocations applied for relocatable ELF
/// objects (where `.debug_*` cross-section offsets live in relocations).
fn section_bytes(
    file: &object::File<'_>,
    section: &object::Section<'_, '_>,
    little: bool,
) -> Result<Vec<u8>> {
    use object::{Object, ObjectSection, ObjectSymbol, RelocationTarget};

    let mut data = section
        .uncompressed_data()
        .map_err(|e| {
            Error::Invalid(format!(
                "cannot read section {}: {e}",
                section.name().unwrap_or("?")
            ))
        })?
        .into_owned();
    if file.kind() != object::ObjectKind::Relocatable || file.format() != object::BinaryFormat::Elf
    {
        return Ok(data);
    }
    for (offset, reloc) in section.relocations() {
        if reloc.kind() != object::RelocationKind::Absolute {
            continue;
        }
        let width = match reloc.size() {
            32 => 4,
            64 => 8,
            _ => continue,
        };
        let Ok(start) = usize::try_from(offset) else {
            continue;
        };
        let Some(slot) = data.get_mut(start..start + width) else {
            continue;
        };
        let base = match reloc.target() {
            RelocationTarget::Symbol(i) => file.symbol_by_index(i).map_or(0, |s| s.address()),
            RelocationTarget::Section(i) => file.section_by_index(i).map_or(0, |s| s.address()),
            _ => 0,
        };
        let mut value = base.wrapping_add_signed(reloc.addend());
        if reloc.has_implicit_addend() {
            let endian = if little { Endian::Little } else { Endian::Big };
            value = value.wrapping_add(crate::prim::decode_uint(slot, endian) as u64);
        }
        for (i, b) in slot.iter_mut().enumerate() {
            let shift = if little { i } else { width - 1 - i } * 8;
            *b = (value >> shift) as u8;
        }
    }
    Ok(data)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DieKey {
    /// In `.debug_types` rather than `.debug_info`.
    types: bool,
    offset: usize,
}

#[derive(Debug, Clone, Copy)]
enum DRef {
    Die(DieKey),
    Sig(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Location {
    Absent,
    Const(u64),
    /// A location expression this importer cannot evaluate (virtual bases).
    Unsupported,
}

/// Array bound attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bound {
    Absent,
    Const(i64),
    /// An expression or reference (variable-length array).
    Dynamic,
}

impl Bound {
    fn of(v: Option<i64>) -> Bound {
        v.map_or(Bound::Dynamic, Bound::Const)
    }
}

#[derive(Debug, Clone, Copy)]
enum ConstValue {
    Signed(i64),
    Unsigned(u64),
    /// Fixed-size data form whose signedness depends on the type.
    Data(u64, u32),
    Wide(u128),
}

#[derive(Debug, Clone)]
struct Die {
    tag: gimli::DwTag,
    name: Option<String>,
    /// Name with enclosing namespaces and classes (`ns::Outer::Inner`).
    qualified: Option<String>,
    children: Vec<usize>,
    ty: Option<DRef>,
    byte_size: Option<u64>,
    bit_size: Option<u64>,
    data_bit_offset: Option<u64>,
    bit_offset: Option<i64>,
    location: Location,
    encoding: Option<gimli::DwAte>,
    declaration: bool,
    external: bool,
    count: Bound,
    upper: Bound,
    lower: Option<i64>,
    const_value: Option<ConstValue>,
    signature: Option<u64>,
    address_size: u8,
}

#[derive(Debug, Default)]
struct Dies {
    list: Vec<Die>,
    index: HashMap<DieKey, usize>,
    sigs: HashMap<u64, DieKey>,
}

fn is_type_tag(tag: gimli::DwTag) -> bool {
    matches!(
        tag,
        gimli::DW_TAG_base_type
            | gimli::DW_TAG_structure_type
            | gimli::DW_TAG_class_type
            | gimli::DW_TAG_interface_type
            | gimli::DW_TAG_union_type
            | gimli::DW_TAG_enumeration_type
            | gimli::DW_TAG_enumerator
            | gimli::DW_TAG_member
            | gimli::DW_TAG_inheritance
            | gimli::DW_TAG_typedef
            | gimli::DW_TAG_array_type
            | gimli::DW_TAG_subrange_type
            | gimli::DW_TAG_pointer_type
            | gimli::DW_TAG_reference_type
            | gimli::DW_TAG_rvalue_reference_type
            | gimli::DW_TAG_const_type
            | gimli::DW_TAG_volatile_type
            | gimli::DW_TAG_restrict_type
            | gimli::DW_TAG_atomic_type
            | gimli::DW_TAG_packed_type
            | gimli::DW_TAG_immutable_type
            | gimli::DW_TAG_shared_type
            | gimli::DW_TAG_subroutine_type
            | gimli::DW_TAG_ptr_to_member_type
            | gimli::DW_TAG_unspecified_type
            | gimli::DW_TAG_string_type
    )
}

fn is_record_tag(tag: gimli::DwTag) -> bool {
    matches!(
        tag,
        gimli::DW_TAG_structure_type
            | gimli::DW_TAG_class_type
            | gimli::DW_TAG_interface_type
            | gimli::DW_TAG_union_type
    )
}

/// Tag namespace for resolving declarations by name.
fn kind_key(tag: gimli::DwTag) -> Option<u8> {
    match tag {
        gimli::DW_TAG_structure_type | gimli::DW_TAG_class_type | gimli::DW_TAG_interface_type => {
            Some(0)
        }
        gimli::DW_TAG_union_type => Some(1),
        gimli::DW_TAG_enumeration_type => Some(2),
        _ => None,
    }
}

fn collect_dies(dwarf: &gimli::Dwarf<Rd<'_>>) -> Result<Dies> {
    let mut headers = Vec::new();
    let mut units = dwarf.units();
    while let Some(h) = units.next().map_err(dwarf_err)? {
        headers.push(h);
    }
    let mut units = dwarf.type_units();
    while let Some(h) = units.next().map_err(dwarf_err)? {
        headers.push(h);
    }
    let mut dies = Dies::default();
    for header in headers {
        let types = header.section() == gimli::SectionId::DebugTypes;
        let base = header.offset().0;
        if let gimli::UnitType::Type {
            type_signature,
            type_offset,
        }
        | gimli::UnitType::SplitType {
            type_signature,
            type_offset,
        } = header.type_()
        {
            dies.sigs.insert(
                type_signature.0,
                DieKey {
                    types,
                    offset: base + type_offset.0,
                },
            );
        }
        let unit = dwarf.unit(header).map_err(dwarf_err)?;
        let mut cursor = unit.entries();
        // (stored DIE index, scope qualifier) per depth.
        let mut stack: Vec<(Option<usize>, Option<String>)> = Vec::new();
        while let Some(entry) = cursor.next_dfs().map_err(dwarf_err)? {
            let depth = usize::try_from(entry.depth()).unwrap_or(0);
            stack.truncate(depth);
            let parent = stack.last().and_then(|s| s.0);
            let tag = entry.tag();
            let name = entry
                .attr_value(gimli::DW_AT_name)
                .and_then(|v| dwarf.attr_string(&unit, v).ok())
                .map(|s| s.to_string_lossy().into_owned());
            let scope: Vec<&str> = stack.iter().filter_map(|s| s.1.as_deref()).collect();
            let qualified = name.as_ref().map(|n| {
                if scope.is_empty() {
                    n.clone()
                } else {
                    format!("{}::{n}", scope.join("::"))
                }
            });
            let qualifier = match tag {
                gimli::DW_TAG_namespace => name.clone(),
                t if is_record_tag(t) || t == gimli::DW_TAG_enumeration_type => name.clone(),
                _ => None,
            };
            let mut stored = None;
            if is_type_tag(tag) {
                let key = DieKey {
                    types,
                    offset: base + entry.offset().0,
                };
                let die = read_die(&unit, entry, types, name, qualified);
                let i = dies.list.len();
                dies.list.push(die);
                dies.index.insert(key, i);
                if let Some(p) = parent {
                    dies.list[p].children.push(i);
                }
                stored = Some(i);
            }
            stack.push((stored, qualifier));
        }
    }
    Ok(dies)
}

fn read_die(
    unit: &gimli::Unit<Rd<'_>>,
    entry: &gimli::DebuggingInformationEntry<Rd<'_>>,
    types: bool,
    name: Option<String>,
    qualified: Option<String>,
) -> Die {
    use gimli::AttributeValue as V;

    let mut die = Die {
        tag: entry.tag(),
        name,
        qualified,
        children: Vec::new(),
        ty: None,
        byte_size: None,
        bit_size: None,
        data_bit_offset: None,
        bit_offset: None,
        location: Location::Absent,
        encoding: None,
        declaration: false,
        external: false,
        count: Bound::Absent,
        upper: Bound::Absent,
        lower: None,
        const_value: None,
        signature: None,
        address_size: unit.header.address_size(),
    };
    for attr in entry.attrs() {
        let value = attr.value();
        match attr.name() {
            gimli::DW_AT_type => {
                die.ty = match value {
                    V::UnitRef(o) => Some(DRef::Die(DieKey {
                        types,
                        offset: unit.header.offset().0 + o.0,
                    })),
                    V::DebugInfoRef(o) => Some(DRef::Die(DieKey {
                        types: false,
                        offset: o.0,
                    })),
                    V::DebugTypesRef(s) => Some(DRef::Sig(s.0)),
                    _ => None,
                };
            }
            gimli::DW_AT_byte_size => die.byte_size = value.udata_value(),
            gimli::DW_AT_bit_size => die.bit_size = value.udata_value(),
            gimli::DW_AT_data_bit_offset => die.data_bit_offset = value.udata_value(),
            gimli::DW_AT_bit_offset => die.bit_offset = constant(&value),
            gimli::DW_AT_data_member_location => {
                die.location = location(&value, unit.encoding());
            }
            gimli::DW_AT_encoding => {
                if let V::Encoding(e) = value {
                    die.encoding = Some(e);
                }
            }
            gimli::DW_AT_declaration => die.declaration = matches!(value, V::Flag(true)),
            gimli::DW_AT_external => die.external = matches!(value, V::Flag(true)),
            gimli::DW_AT_count => die.count = Bound::of(constant(&value)),
            gimli::DW_AT_upper_bound => die.upper = Bound::of(constant(&value)),
            gimli::DW_AT_lower_bound => die.lower = constant(&value),
            gimli::DW_AT_const_value => {
                die.const_value = match value {
                    V::Sdata(v) => Some(ConstValue::Signed(v)),
                    V::Udata(v) => Some(ConstValue::Unsigned(v)),
                    V::Data1(v) => Some(ConstValue::Data(u64::from(v), 8)),
                    V::Data2(v) => Some(ConstValue::Data(u64::from(v), 16)),
                    V::Data4(v) => Some(ConstValue::Data(u64::from(v), 32)),
                    V::Data8(v) => Some(ConstValue::Data(v, 64)),
                    V::Data16(v) => Some(ConstValue::Wide(v)),
                    _ => None,
                };
            }
            gimli::DW_AT_signature => {
                if let V::DebugTypesRef(s) = value {
                    die.signature = Some(s.0);
                }
            }
            _ => {}
        }
    }
    die
}

/// Integer constant of a bound or offset attribute (`None` for expressions
/// and references).
fn constant(value: &gimli::AttributeValue<Rd<'_>>) -> Option<i64> {
    match *value {
        gimli::AttributeValue::Sdata(v) => Some(v),
        _ => value.udata_value().map(|v| v as i64),
    }
}

fn location(value: &gimli::AttributeValue<Rd<'_>>, encoding: gimli::Encoding) -> Location {
    use gimli::AttributeValue as V;

    let expr = match value {
        V::Udata(v) => return Location::Const(*v),
        V::Exprloc(e) => *e,
        V::Block(b) => gimli::Expression(*b),
        _ => {
            return value
                .udata_value()
                .map_or(Location::Unsupported, Location::Const);
        }
    };
    let mut ops = expr.operations(encoding);
    let Ok(Some(
        gimli::Operation::PlusConstant { value: first }
        | gimli::Operation::UnsignedConstant { value: first },
    )) = ops.next()
    else {
        return Location::Unsupported;
    };
    match ops.next() {
        Ok(None) => Location::Const(first),
        _ => Location::Unsupported,
    }
}

fn dwarf_base_type(name: Option<&str>, encoding: Option<gimli::DwAte>, size: u64) -> Ty {
    let name = name.unwrap_or("");
    let int = |signed: bool| int_prim(size, signed).map_or(Ty::Bytes(size), Ty::Prim);
    let wide_char = size == 2 && (name.contains("wchar") || name.contains("char16"));
    match encoding {
        Some(gimli::DW_ATE_boolean) if size == 1 => Ty::prim("bool"),
        Some(gimli::DW_ATE_float) => match size {
            2 => Ty::prim("f16"),
            4 => Ty::prim("float"),
            8 => Ty::prim("double"),
            _ => Ty::Bytes(size),
        },
        Some(gimli::DW_ATE_signed_char | gimli::DW_ATE_unsigned_char) if size == 1 => {
            if name == "char" {
                Ty::prim("char")
            } else {
                int(encoding == Some(gimli::DW_ATE_signed_char))
            }
        }
        Some(gimli::DW_ATE_UTF) if size == 1 => Ty::prim("char"),
        Some(gimli::DW_ATE_UTF) if size == 2 => Ty::prim("char16"),
        Some(gimli::DW_ATE_signed | gimli::DW_ATE_unsigned) if wide_char => Ty::prim("char16"),
        Some(gimli::DW_ATE_signed | gimli::DW_ATE_signed_fixed) => int(true),
        Some(
            gimli::DW_ATE_unsigned
            | gimli::DW_ATE_unsigned_fixed
            | gimli::DW_ATE_address
            | gimli::DW_ATE_boolean
            | gimli::DW_ATE_UTF,
        )
        | None => int(false),
        Some(_) => Ty::Bytes(size),
    }
}

struct DwarfConv<'d> {
    dies: &'d Dies,
    model: TypeModel,
    done: HashMap<usize, Ty>,
    visiting: HashSet<usize>,
    defs: HashMap<(u8, &'d str), usize>,
}

impl<'d> DwarfConv<'d> {
    fn new(dies: &'d Dies, endian: Endian) -> Self {
        let mut defs = HashMap::new();
        for (i, d) in dies.list.iter().enumerate() {
            if !d.declaration
                && let Some(k) = kind_key(d.tag)
                && let Some(q) = d.qualified.as_deref()
            {
                defs.entry((k, q)).or_insert(i);
            }
        }
        Self {
            dies,
            model: TypeModel::new(endian),
            done: HashMap::new(),
            visiting: HashSet::new(),
            defs,
        }
    }

    fn run(mut self) -> TypeModel {
        let dies = self.dies;
        for (i, d) in dies.list.iter().enumerate() {
            let wanted =
                (kind_key(d.tag).is_some() && !d.declaration) || d.tag == gimli::DW_TAG_typedef;
            if wanted {
                self.convert(i);
            }
        }
        self.model
    }

    fn resolve(&self, r: Option<DRef>) -> Option<usize> {
        match r? {
            DRef::Die(k) => self.dies.index.get(&k).copied(),
            DRef::Sig(s) => self
                .dies
                .sigs
                .get(&s)
                .and_then(|k| self.dies.index.get(k))
                .copied(),
        }
    }

    fn ty_of(&mut self, r: Option<DRef>) -> Ty {
        match self.resolve(r) {
            Some(i) => self.convert(i),
            None => Ty::Bytes(0),
        }
    }

    fn convert(&mut self, idx: usize) -> Ty {
        if let Some(t) = self.done.get(&idx) {
            return t.clone();
        }
        if !self.visiting.insert(idx) {
            return Ty::Bytes(0); // cyclic modifiers / typedefs
        }
        let ty = self.convert_inner(idx);
        self.visiting.remove(&idx);
        self.done.insert(idx, ty.clone());
        ty
    }

    fn convert_inner(&mut self, idx: usize) -> Ty {
        let dies = self.dies;
        let die = &dies.list[idx];
        let address = u64::from(die.address_size);
        match die.tag {
            gimli::DW_TAG_base_type => dwarf_base_type(
                die.name.as_deref(),
                die.encoding,
                die.byte_size.unwrap_or(0),
            ),
            t if is_record_tag(t) => {
                if die.declaration {
                    self.definition_of(idx)
                } else {
                    self.record(idx)
                }
            }
            gimli::DW_TAG_enumeration_type => {
                if die.declaration {
                    self.definition_of(idx)
                } else {
                    self.enumeration(idx)
                }
            }
            gimli::DW_TAG_typedef => {
                let target = self.ty_of(die.ty);
                match &die.qualified {
                    Some(name) => Ty::Ref(self.model.add(ModelType::Alias(Alias {
                        name: name.clone(),
                        target,
                    }))),
                    None => target,
                }
            }
            gimli::DW_TAG_array_type => self.array(idx),
            gimli::DW_TAG_pointer_type
            | gimli::DW_TAG_reference_type
            | gimli::DW_TAG_rvalue_reference_type => Ty::Pointer {
                pointee: self.display(die.ty, 0),
                size: die.byte_size.unwrap_or(address),
            },
            gimli::DW_TAG_const_type
            | gimli::DW_TAG_volatile_type
            | gimli::DW_TAG_restrict_type
            | gimli::DW_TAG_atomic_type
            | gimli::DW_TAG_packed_type
            | gimli::DW_TAG_immutable_type
            | gimli::DW_TAG_shared_type => self.ty_of(die.ty),
            gimli::DW_TAG_ptr_to_member_type => Ty::Bytes(die.byte_size.unwrap_or(address)),
            _ => Ty::Bytes(die.byte_size.unwrap_or(0)),
        }
    }

    /// Definition for a declaration-only struct/union/enum: via its type
    /// signature, or by qualified name in any unit.
    fn definition_of(&mut self, idx: usize) -> Ty {
        let dies = self.dies;
        let die = &dies.list[idx];
        if let Some(sig) = die.signature
            && let Some(i) = self.resolve(Some(DRef::Sig(sig)))
            && i != idx
        {
            return self.convert(i);
        }
        if let Some(k) = kind_key(die.tag)
            && let Some(q) = die.qualified.as_deref()
            && let Some(&def) = self.defs.get(&(k, q))
        {
            return self.convert(def);
        }
        Ty::Bytes(die.byte_size.unwrap_or(0)) // incomplete type
    }

    fn record(&mut self, idx: usize) -> Ty {
        let dies = self.dies;
        let die = &dies.list[idx];
        let id = self.model.add(ModelType::Record(Record {
            name: die.qualified.clone(),
            union: die.tag == gimli::DW_TAG_union_type,
            size: 0,
            members: Vec::new(),
        }));
        self.done.insert(idx, Ty::Ref(id));
        let mut members = Vec::new();
        let mut bases = 0;
        for &c in &die.children {
            let ch = &dies.list[c];
            match ch.tag {
                gimli::DW_TAG_member => {
                    if let Some(m) = self.member(ch) {
                        members.push(m);
                    }
                }
                gimli::DW_TAG_inheritance => {
                    let offset = match ch.location {
                        Location::Absent => 0,
                        Location::Const(v) => v,
                        Location::Unsupported => continue, // virtual base
                    };
                    let ty = self.ty_of(ch.ty);
                    let name = format!("__anon_base{bases}");
                    members.push(Member::new(Some(&name), ty, offset));
                    bases += 1;
                }
                _ => {}
            }
        }
        let computed = members
            .iter()
            .map(|m| match m.bit_field {
                Some(b) => bit_range(b).1,
                None => m.offset + self.model.size_of(&m.ty),
            })
            .max()
            .unwrap_or(0);
        if let Some(ModelType::Record(r)) = self.model.types.get_mut(id) {
            r.size = die.byte_size.unwrap_or(computed);
            r.members = members;
        }
        Ty::Ref(id)
    }

    fn member(&mut self, ch: &Die) -> Option<Member> {
        if ch.external || ch.declaration {
            return None; // static data member
        }
        let offset = match ch.location {
            Location::Absent => 0,
            Location::Const(v) => v,
            Location::Unsupported => return None,
        };
        let ty = self.ty_of(ch.ty);
        let name = ch.name.as_deref();
        let Some(width) = ch.bit_size else {
            return Some(Member::new(name, ty, offset));
        };
        let bit_offset = if let Some(d) = ch.data_bit_offset {
            d
        } else if let Some(legacy) = ch.bit_offset {
            // DWARF 2/3: bits from the most significant bit of the storage
            // unit at `offset` to the most significant bit of the member.
            let storage = ch.byte_size.unwrap_or_else(|| self.model.size_of(&ty));
            let unit = i128::from(offset) * 8;
            let v = if self.model.endian == Endian::Big {
                unit + i128::from(legacy)
            } else {
                unit + i128::from(storage) * 8 - i128::from(legacy) - i128::from(width)
            };
            u64::try_from(v).ok()?
        } else {
            offset * 8
        };
        let tsize = self.model.size_of(&ty);
        if width % 8 == 0 && bit_offset % 8 == 0 && width == tsize * 8 {
            return Some(Member::new(name, ty, bit_offset / 8));
        }
        let width = u32::try_from(width)
            .ok()
            .filter(|w| (1..=128).contains(w))?;
        Some(Member::bits(name, ty, bit_offset, width))
    }

    fn enumeration(&mut self, idx: usize) -> Ty {
        let dies = self.dies;
        let die = &dies.list[idx];
        let under = die.ty.map(|r| self.ty_of(Some(r)));
        let under_signed = match &under {
            Some(Ty::Prim(p)) => Prim::from_name(p).map(Prim::is_signed),
            _ => None,
        };
        let size = die
            .byte_size
            .or_else(|| under.as_ref().map(|u| self.model.size_of(u)))
            .filter(|s| *s > 0)
            .unwrap_or(4);
        let values: Vec<(&str, ConstValue)> = die
            .children
            .iter()
            .map(|&c| &dies.list[c])
            .filter(|c| c.tag == gimli::DW_TAG_enumerator)
            .filter_map(|c| Some((c.name.as_deref()?, c.const_value?)))
            .collect();
        let signed = under_signed.unwrap_or_else(|| {
            values
                .iter()
                .any(|(_, v)| matches!(v, ConstValue::Signed(x) if *x < 0))
        });
        let variants = values
            .into_iter()
            .map(|(n, v)| {
                let v = match v {
                    ConstValue::Signed(x) => i128::from(x),
                    ConstValue::Data(x, bits) if signed => sign_extend(u128::from(x), bits),
                    ConstValue::Unsigned(x) | ConstValue::Data(x, _) => i128::from(x),
                    ConstValue::Wide(x) => x as i128,
                };
                (n.to_string(), v)
            })
            .collect();
        Ty::Ref(self.model.add(ModelType::Enum(EnumType {
            name: die.qualified.clone(),
            size,
            signed,
            variants,
        })))
    }

    fn array(&mut self, idx: usize) -> Ty {
        let dies = self.dies;
        let die = &dies.list[idx];
        let elem = self.ty_of(die.ty);
        let mut dims: Vec<u64> = die
            .children
            .iter()
            .map(|&c| &dies.list[c])
            .filter(|c| c.tag == gimli::DW_TAG_subrange_type)
            .map(|c| {
                let n = match (c.count, c.upper) {
                    (Bound::Const(count), _) => count,
                    (Bound::Absent, Bound::Const(upper)) => upper - c.lower.unwrap_or(0) + 1,
                    _ => 0, // unknown or variable bound
                };
                u64::try_from(n).unwrap_or(0)
            })
            .collect();
        if dims.is_empty() {
            dims.push(0);
        }
        dims.iter().rev().fold(elem, |acc, &n| Ty::array(acc, n))
    }

    /// C-like spelling of a type for pointer display names.
    fn display(&self, r: Option<DRef>, depth: u32) -> String {
        if depth > 16 {
            return "?".into();
        }
        let Some(i) = self.resolve(r) else {
            return "void".into();
        };
        let die = &self.dies.list[i];
        match die.tag {
            gimli::DW_TAG_pointer_type
            | gimli::DW_TAG_reference_type
            | gimli::DW_TAG_rvalue_reference_type => {
                format!("{}*", self.display(die.ty, depth + 1))
            }
            gimli::DW_TAG_const_type
            | gimli::DW_TAG_volatile_type
            | gimli::DW_TAG_restrict_type
            | gimli::DW_TAG_atomic_type
            | gimli::DW_TAG_packed_type
            | gimli::DW_TAG_immutable_type
            | gimli::DW_TAG_shared_type => self.display(die.ty, depth + 1),
            gimli::DW_TAG_array_type => format!("{}[]", self.display(die.ty, depth + 1)),
            gimli::DW_TAG_subroutine_type => "fn".into(),
            _ => die.qualified.clone().unwrap_or_else(|| "anonymous".into()),
        }
    }
}

// ===========================================================================
// PDB front-end
// ===========================================================================

fn pdb_err(e: &pdb::Error) -> Error {
    Error::Invalid(format!("not a valid PDB file: {e}"))
}

/// Imports the user-defined types of a PDB's type stream (TPI).
pub fn from_pdb(pdb_file: &[u8]) -> Result<Program> {
    emit(&pdb_model(pdb_file)?)
}

/// PDB front-end: reads the TPI stream into a [`TypeModel`].
pub fn pdb_model(pdb_file: &[u8]) -> Result<TypeModel> {
    use pdb::FallibleIterator;

    let mut file = pdb::PDB::open(std::io::Cursor::new(pdb_file)).map_err(|e| pdb_err(&e))?;
    let info = file.type_information().map_err(|e| pdb_err(&e))?;
    let mut finder = info.finder();
    let mut iter = info.iter();
    let mut all = Vec::new();
    while let Some(item) = iter.next().map_err(|e| pdb_err(&e))? {
        finder.update(&iter);
        all.push(item.index());
    }
    let mut conv = PdbConv {
        finder: &finder,
        model: TypeModel::new(Endian::Little),
        done: HashMap::new(),
        visiting: HashSet::new(),
        defs: HashMap::new(),
    };
    let mut roots = Vec::new();
    for &ti in &all {
        let Ok(data) = conv.parse(ti) else { continue };
        let def = match &data {
            pdb::TypeData::Class(c) if !c.properties.forward_reference() => {
                Some((0, c.name.to_string().into_owned()))
            }
            pdb::TypeData::Union(u) if !u.properties.forward_reference() => {
                Some((1, u.name.to_string().into_owned()))
            }
            pdb::TypeData::Enumeration(e) if !e.properties.forward_reference() => {
                Some((2, e.name.to_string().into_owned()))
            }
            _ => None,
        };
        if let Some(key) = def {
            conv.defs.entry(key).or_insert(ti);
            roots.push(ti);
        }
    }
    for ti in roots {
        conv.convert(ti);
    }
    Ok(conv.model)
}

/// Whether a PDB type name denotes an anonymous type.
fn pdb_anonymous(name: &str) -> bool {
    name.is_empty()
        || name.starts_with('<')
        || name.starts_with("__unnamed")
        || name.contains("<unnamed")
        || name.contains("<anonymous")
}

/// Template type of a PDB primitive (non-pointer) kind.
pub(crate) fn pdb_prim(kind: pdb::PrimitiveKind) -> Ty {
    use pdb::PrimitiveKind as K;

    match kind {
        K::Char | K::RChar => Ty::prim("char"),
        K::UChar | K::U8 => Ty::prim("u8"),
        K::I8 => Ty::prim("s8"),
        K::WChar | K::RChar16 => Ty::prim("char16"),
        K::RChar32 | K::ULong | K::U32 | K::Bool32 => Ty::prim("u32"),
        K::Short | K::I16 => Ty::prim("s16"),
        K::UShort | K::U16 | K::Bool16 => Ty::prim("u16"),
        K::Long | K::I32 | K::HRESULT => Ty::prim("s32"),
        K::Quad | K::I64 => Ty::prim("s64"),
        K::UQuad | K::U64 | K::Bool64 => Ty::prim("u64"),
        K::Octa | K::I128 => Ty::prim("s128"),
        K::UOcta | K::U128 => Ty::prim("u128"),
        K::F16 => Ty::prim("f16"),
        K::F32 | K::F32PP => Ty::prim("float"),
        K::F64 => Ty::prim("double"),
        K::F48 => Ty::Bytes(6),
        K::F80 => Ty::Bytes(10),
        K::F128 | K::Complex64 | K::Complex128 => Ty::Bytes(16),
        K::Complex32 => Ty::Bytes(8),
        K::Complex80 => Ty::Bytes(20),
        K::Bool8 => Ty::prim("bool"),
        _ => Ty::Bytes(0), // void, no type
    }
}

/// Element counts (innermost first) of a PDB array whose `dims` are byte
/// sizes of each dimension (`float[4][4]` → `[16, 64]`).
pub(crate) fn pdb_array_counts(elem_size: u64, dims: &[u32]) -> Vec<u64> {
    let mut prev = elem_size;
    dims.iter()
        .map(|&d| {
            let d = u64::from(d);
            let c = d.checked_div(prev).unwrap_or(0);
            prev = d;
            c
        })
        .collect()
}

fn pdb_variant(v: &pdb::Variant) -> i128 {
    match *v {
        pdb::Variant::U8(x) => i128::from(x),
        pdb::Variant::U16(x) => i128::from(x),
        pdb::Variant::U32(x) => i128::from(x),
        pdb::Variant::U64(x) => i128::from(x),
        pdb::Variant::I8(x) => i128::from(x),
        pdb::Variant::I16(x) => i128::from(x),
        pdb::Variant::I32(x) => i128::from(x),
        pdb::Variant::I64(x) => i128::from(x),
    }
}

fn indirection_size(i: pdb::Indirection) -> u64 {
    match i {
        pdb::Indirection::Near16 => 2,
        pdb::Indirection::Far16 | pdb::Indirection::Huge16 | pdb::Indirection::Near32 => 4,
        pdb::Indirection::Far32 => 6,
        pdb::Indirection::Near64 => 8,
        pdb::Indirection::Near128 => 16,
    }
}

struct PdbConv<'a, 't> {
    finder: &'a pdb::TypeFinder<'t>,
    model: TypeModel,
    done: HashMap<u32, Ty>,
    visiting: HashSet<u32>,
    defs: HashMap<(u8, String), pdb::TypeIndex>,
}

impl<'t> PdbConv<'_, 't> {
    fn parse(&self, ti: pdb::TypeIndex) -> pdb::Result<pdb::TypeData<'t>> {
        self.finder.find(ti)?.parse()
    }

    fn convert(&mut self, ti: pdb::TypeIndex) -> Ty {
        if let Some(t) = self.done.get(&ti.0) {
            return t.clone();
        }
        if !self.visiting.insert(ti.0) {
            return Ty::Bytes(0);
        }
        let ty = self.convert_inner(ti);
        self.visiting.remove(&ti.0);
        self.done.insert(ti.0, ty.clone());
        ty
    }

    fn forward(&mut self, kind: u8, name: &str) -> Ty {
        match self.defs.get(&(kind, name.to_string())) {
            Some(&def) => self.convert(def),
            None => Ty::Bytes(0),
        }
    }

    fn convert_inner(&mut self, ti: pdb::TypeIndex) -> Ty {
        let Ok(data) = self.parse(ti) else {
            return Ty::Bytes(0);
        };
        match data {
            pdb::TypeData::Primitive(p) => match p.indirection {
                Some(i) => Ty::Pointer {
                    pointee: match pdb_prim(p.kind) {
                        Ty::Prim(n) => n,
                        _ => "void".into(),
                    },
                    size: indirection_size(i),
                },
                None => pdb_prim(p.kind),
            },
            pdb::TypeData::Class(c) => {
                let name = c.name.to_string();
                if c.properties.forward_reference() {
                    self.forward(0, &name)
                } else {
                    self.record(ti, &name, false, c.size, c.fields)
                }
            }
            pdb::TypeData::Union(u) => {
                let name = u.name.to_string();
                if u.properties.forward_reference() {
                    self.forward(1, &name)
                } else {
                    self.record(ti, &name, true, u.size, Some(u.fields))
                }
            }
            pdb::TypeData::Enumeration(e) => {
                let name = e.name.to_string();
                if e.properties.forward_reference() {
                    self.forward(2, &name)
                } else {
                    self.enumeration(&name, e.underlying_type, e.fields)
                }
            }
            pdb::TypeData::Pointer(p) => {
                let size = match p.attributes.size() {
                    0 => 8,
                    s => u64::from(s),
                };
                if p.attributes.pointer_to_member() {
                    Ty::Bytes(size)
                } else {
                    Ty::Pointer {
                        pointee: self.display(p.underlying_type, 0),
                        size,
                    }
                }
            }
            pdb::TypeData::Modifier(m) => self.convert(m.underlying_type),
            pdb::TypeData::Bitfield(b) => self.convert(b.underlying_type),
            pdb::TypeData::Array(a) => {
                let elem = self.convert(a.element_type);
                let esize = self.model.size_of(&elem);
                if esize == 0 {
                    return Ty::Bytes(a.dimensions.last().map_or(0, |&d| u64::from(d)));
                }
                pdb_array_counts(esize, &a.dimensions)
                    .into_iter()
                    .fold(elem, Ty::array)
            }
            _ => Ty::Bytes(0),
        }
    }

    /// Field list records, following `LF_INDEX` continuations.
    fn fields(&self, list: Option<pdb::TypeIndex>) -> Vec<pdb::TypeData<'t>> {
        let mut out = Vec::new();
        let mut next = list;
        let mut seen = HashSet::new();
        while let Some(ti) = next.take() {
            if !seen.insert(ti.0) {
                break;
            }
            let Ok(pdb::TypeData::FieldList(fl)) = self.parse(ti) else {
                break;
            };
            out.extend(fl.fields);
            next = fl.continuation;
        }
        out
    }

    fn record(
        &mut self,
        ti: pdb::TypeIndex,
        name: &str,
        union: bool,
        size: u64,
        fields: Option<pdb::TypeIndex>,
    ) -> Ty {
        let id = self.model.add(ModelType::Record(Record {
            name: (!pdb_anonymous(name)).then(|| name.to_string()),
            union,
            size,
            members: Vec::new(),
        }));
        self.done.insert(ti.0, Ty::Ref(id));
        let mut members = Vec::new();
        let mut bases = 0;
        for f in self.fields(fields) {
            match f {
                pdb::TypeData::Member(m) => {
                    let mname = m.name.to_string();
                    let mname = (!mname.is_empty()).then_some(mname.as_ref());
                    if let Ok(pdb::TypeData::Bitfield(b)) = self.parse(m.field_type) {
                        let ty = self.convert(b.underlying_type);
                        if b.length > 0 {
                            members.push(Member::bits(
                                mname,
                                ty,
                                m.offset * 8 + u64::from(b.position),
                                u32::from(b.length),
                            ));
                        }
                    } else {
                        let ty = self.convert(m.field_type);
                        members.push(Member::new(mname, ty, m.offset));
                    }
                }
                pdb::TypeData::BaseClass(b) => {
                    let ty = self.convert(b.base_class);
                    let bname = format!("__anon_base{bases}");
                    members.push(Member::new(Some(&bname), ty, u64::from(b.offset)));
                    bases += 1;
                }
                _ => {}
            }
        }
        if let Some(ModelType::Record(r)) = self.model.types.get_mut(id) {
            r.members = members;
        }
        Ty::Ref(id)
    }

    fn enumeration(&mut self, name: &str, under: pdb::TypeIndex, list: pdb::TypeIndex) -> Ty {
        let under = self.convert(under);
        let (size, signed) = match &under {
            Ty::Prim(p) => (
                self.model.size_of(&under),
                Prim::from_name(p).is_some_and(Prim::is_signed),
            ),
            _ => (4, true),
        };
        let variants = self
            .fields(Some(list))
            .into_iter()
            .filter_map(|f| match f {
                pdb::TypeData::Enumerate(e) => {
                    Some((e.name.to_string().into_owned(), pdb_variant(&e.value)))
                }
                _ => None,
            })
            .collect();
        Ty::Ref(self.model.add(ModelType::Enum(EnumType {
            name: (!pdb_anonymous(name)).then(|| name.to_string()),
            size: if size == 0 { 4 } else { size },
            signed,
            variants,
        })))
    }

    fn display(&self, ti: pdb::TypeIndex, depth: u32) -> String {
        if depth > 16 {
            return "?".into();
        }
        match self.parse(ti) {
            Ok(pdb::TypeData::Primitive(p)) => {
                let base = match pdb_prim(p.kind) {
                    Ty::Prim(n) => n,
                    _ => "void".into(),
                };
                if p.indirection.is_some() {
                    format!("{base}*")
                } else {
                    base
                }
            }
            Ok(pdb::TypeData::Class(c)) => c.name.to_string().into_owned(),
            Ok(pdb::TypeData::Union(u)) => u.name.to_string().into_owned(),
            Ok(pdb::TypeData::Enumeration(e)) => e.name.to_string().into_owned(),
            Ok(pdb::TypeData::Pointer(p)) => {
                format!("{}*", self.display(p.underlying_type, depth + 1))
            }
            Ok(pdb::TypeData::Modifier(m)) => self.display(m.underlying_type, depth + 1),
            Ok(pdb::TypeData::Array(a)) => format!("{}[]", self.display(a.element_type, depth + 1)),
            Ok(pdb::TypeData::Procedure(_) | pdb::TypeData::MemberFunction(_)) => "fn".into(),
            _ => "?".into(),
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EvalOptions, TemplateResult, evaluate};
    use hexa_core::piece::Piece;
    use hexa_core::{Cancel, Tree};

    fn run(p: &Program, root: &str, bytes: &[u8]) -> TemplateResult {
        let tree = Tree::from_piece(Piece::bytes(bytes.to_vec()));
        let opts = EvalOptions {
            root_type: Some(root.to_string()),
            ..EvalOptions::default()
        };
        let r = evaluate(p, &tree, &opts, &Cancel::new()).expect("evaluate");
        assert!(r.is_ok(), "evaluation errors: {:#?}", r.errors);
        r
    }

    fn int(r: &TemplateResult, path: &str) -> i128 {
        r.find(path)
            .unwrap_or_else(|| panic!("no node `{path}`"))
            .raw
            .as_ref()
            .and_then(crate::Value::as_int)
            .unwrap_or_else(|| panic!("`{path}` has no integer value"))
    }

    fn record(name: &str, size: u64, members: Vec<Member>) -> ModelType {
        ModelType::Record(Record {
            name: Some(name.into()),
            union: false,
            size,
            members,
        })
    }

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize("Foo"), "Foo");
        assert_eq!(sanitize("std::vector<int>"), "std::vector_int_");
        assert_eq!(sanitize("1x"), "_1x");
        assert_eq!(sanitize(""), "_");
        assert!(pdb_anonymous("<unnamed-tag>"));
        assert!(pdb_anonymous("__unnamed"));
        assert!(!pdb_anonymous("Foo"));
    }

    #[test]
    fn pdb_primitives_and_arrays() {
        use pdb::PrimitiveKind as K;
        assert_eq!(pdb_prim(K::Long), Ty::prim("s32"));
        assert_eq!(pdb_prim(K::ULong), Ty::prim("u32"));
        assert_eq!(pdb_prim(K::Char), Ty::prim("char"));
        assert_eq!(pdb_prim(K::UChar), Ty::prim("u8"));
        assert_eq!(pdb_prim(K::WChar), Ty::prim("char16"));
        assert_eq!(pdb_prim(K::Quad), Ty::prim("s64"));
        assert_eq!(pdb_prim(K::F64), Ty::prim("double"));
        assert_eq!(pdb_prim(K::Bool8), Ty::prim("bool"));
        assert_eq!(pdb_prim(K::Void), Ty::Bytes(0));
        assert_eq!(pdb_prim(K::F80), Ty::Bytes(10));
        // float[4][4]
        assert_eq!(pdb_array_counts(4, &[16, 64]), vec![4, 4]);
        // int[3]
        assert_eq!(pdb_array_counts(4, &[12]), vec![3]);
        assert_eq!(pdb_array_counts(0, &[12]), vec![0]);
        assert_eq!(pdb_variant(&pdb::Variant::I8(-2)), -2);
        assert_eq!(
            pdb_variant(&pdb::Variant::U64(u64::MAX)),
            i128::from(u64::MAX)
        );
    }

    #[test]
    fn dwarf_base_types() {
        let t = |n: &str, e, s| dwarf_base_type(Some(n), Some(e), s);
        assert_eq!(t("char", gimli::DW_ATE_signed_char, 1), Ty::prim("char"));
        assert_eq!(
            t("unsigned char", gimli::DW_ATE_unsigned_char, 1),
            Ty::prim("u8")
        );
        assert_eq!(
            t("signed char", gimli::DW_ATE_signed_char, 1),
            Ty::prim("s8")
        );
        assert_eq!(t("long", gimli::DW_ATE_signed, 8), Ty::prim("s64"));
        assert_eq!(t("_Bool", gimli::DW_ATE_boolean, 1), Ty::prim("bool"));
        assert_eq!(t("double", gimli::DW_ATE_float, 8), Ty::prim("double"));
        assert_eq!(t("long double", gimli::DW_ATE_float, 16), Ty::Bytes(16));
        assert_eq!(t("wchar_t", gimli::DW_ATE_signed, 4), Ty::prim("s32"));
        assert_eq!(t("wchar_t", gimli::DW_ATE_unsigned, 2), Ty::prim("char16"));
        assert_eq!(t("char16_t", gimli::DW_ATE_UTF, 2), Ty::prim("char16"));
        assert_eq!(t("__int128", gimli::DW_ATE_signed, 16), Ty::prim("s128"));
        assert_eq!(
            t("_Complex float", gimli::DW_ATE_complex_float, 8),
            Ty::Bytes(8)
        );
    }

    #[test]
    fn names_merge_suffix_adopt_and_hide_aliases() {
        let mut m = TypeModel::new(Endian::Little);
        let a = m.add(record(
            "Point",
            4,
            vec![Member::new(Some("x"), Ty::prim("u32"), 0)],
        ));
        let b = m.add(record(
            "Point",
            4,
            vec![Member::new(Some("x"), Ty::prim("u32"), 0)],
        ));
        let c = m.add(record(
            "Point",
            1,
            vec![Member::new(Some("x"), Ty::prim("u8"), 0)],
        ));
        let anon = m.add(ModelType::Record(Record {
            name: None,
            union: false,
            size: 2,
            members: vec![Member::new(Some("v"), Ty::prim("u16"), 0)],
        }));
        let td = m.add(ModelType::Alias(Alias {
            name: "Anon".into(),
            target: Ty::Ref(anon),
        }));
        let self_named = m.add(ModelType::Alias(Alias {
            name: "Point".into(),
            target: Ty::Ref(a),
        }));
        let shadow = m.add(ModelType::Alias(Alias {
            name: "u8".into(),
            target: Ty::prim("char"),
        }));
        let same = m.add(ModelType::Alias(Alias {
            name: "uint32_t".into(),
            target: Ty::prim("u32"),
        }));
        let prim_named = m.add(record("u16", 2, vec![]));
        let n = Naming::compute(&m);
        assert_eq!(n.names[a], "Point");
        assert_eq!(n.names[b], "Point");
        assert!(n.emit[a] && !n.emit[b]);
        assert_eq!(n.names[c], "Point_2");
        assert_eq!(n.names[anon], "Anon");
        assert!(n.transparent[td] && n.transparent[self_named] && n.transparent[shadow]);
        assert!(!n.transparent[same]);
        assert_eq!(n.names[prim_named], "u16_");
        let p = emit(&m).unwrap();
        let names: Vec<&str> = p.type_defs().map(TypeDef::name).collect();
        assert_eq!(names, ["Point", "Point_2", "Anon", "uint32_t", "u16_"]);
    }

    #[test]
    fn anonymous_members_named_after_owner() {
        let mut m = TypeModel::new(Endian::Little);
        let inner = m.add(ModelType::Record(Record {
            name: None,
            union: true,
            size: 2,
            members: vec![
                Member::new(Some("h"), Ty::prim("u16"), 0),
                Member::new(Some("b"), Ty::prim("u8"), 0),
            ],
        }));
        m.add(record(
            "W",
            4,
            vec![
                Member::new(Some("tag"), Ty::prim("u8"), 0),
                Member::new(None, Ty::Ref(inner), 2),
            ],
        ));
        let p = emit(&m).unwrap();
        assert!(p.type_def("W__anon1").is_some());
        let r = run(&p, "W", &[1, 0xEE, 0x34, 0x12]);
        assert_eq!(int(&r, "w.tag"), 1);
        assert_eq!(int(&r, "w.__anon1.h"), 0x1234);
        assert_eq!(int(&r, "w.__anon1.b"), 0x34);
        assert_eq!(r.find("w").unwrap().size, 4);
    }

    #[test]
    fn union_padding_and_offset_members() {
        let mut m = TypeModel::new(Endian::Little);
        m.add(ModelType::Record(Record {
            name: Some("U".into()),
            union: true,
            size: 8,
            members: vec![
                Member::new(Some("a"), Ty::prim("u16"), 0),
                Member::new(Some("b"), Ty::prim("u8"), 3),
                Member::bits(Some("c"), Ty::prim("u8"), 4, 4),
            ],
        }));
        let p = emit(&m).unwrap();
        let r = run(&p, "U", &[0x34, 0x12, 0, 0x77, 0, 0, 0, 0, 0xFF]);
        assert_eq!(r.find("u").unwrap().size, 8);
        assert_eq!(int(&r, "u.a"), 0x1234);
        assert_eq!(int(&r, "u.__anon_m1.b"), 0x77);
        assert_eq!(int(&r, "u.__anon_bits0.c"), 3);
    }

    #[test]
    fn invalid_models_are_rejected() {
        let mut m = TypeModel::new(Endian::Little);
        m.add(record("A", 4, vec![Member::new(Some("x"), Ty::Ref(7), 0)]));
        assert!(matches!(emit(&m), Err(Error::Invalid(_))));
        let mut m = TypeModel::new(Endian::Little);
        m.add(record(
            "A",
            4,
            vec![Member::new(Some("x"), Ty::prim("string"), 0)],
        ));
        assert!(matches!(emit(&m), Err(Error::Invalid(_))));
        let mut m = TypeModel::new(Endian::Little);
        m.add(record(
            "A",
            32,
            vec![Member::bits(Some("x"), Ty::prim("u8"), 0, 200)],
        ));
        assert!(matches!(emit(&m), Err(Error::Invalid(_))));
    }

    #[test]
    fn bad_inputs_give_clean_errors() {
        assert!(matches!(
            from_pdb(b"definitely not a pdb"),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(from_pdb(&[]), Err(Error::Invalid(_))));
        assert!(matches!(
            from_dwarf(b"\x7fELF garbage"),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(from_dwarf(&[]), Err(Error::Invalid(_))));
    }

    #[test]
    fn location_expressions() {
        let enc = gimli::Encoding {
            format: gimli::Format::Dwarf32,
            version: 4,
            address_size: 8,
        };
        let le = gimli::RunTimeEndian::Little;
        let expr = |b: &'static [u8]| {
            gimli::AttributeValue::Exprloc(gimli::Expression(gimli::EndianSlice::new(b, le)))
        };
        // DW_OP_plus_uconst 12
        assert_eq!(location(&expr(&[0x23, 12]), enc), Location::Const(12));
        // DW_OP_constu 5
        assert_eq!(location(&expr(&[0x10, 5]), enc), Location::Const(5));
        // DW_OP_dup; DW_OP_deref (virtual base)
        assert_eq!(location(&expr(&[0x12, 0x06]), enc), Location::Unsupported);
        assert_eq!(
            location(&gimli::AttributeValue::Udata(8), enc),
            Location::Const(8)
        );
    }
}
