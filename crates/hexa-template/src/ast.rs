//! Abstract syntax tree shared by every template front-end (STRC-01..07).
//!
//! The native `.hexpat`-style parser, the Kaitai importer (STRC-03), the 010
//! Editor dialect (STRC-05), C headers (STRC-06) and debug-info importers
//! (STRC-07) all produce a [`Program`], which the evaluator runs.

use serde::{Deserialize, Serialize};

/// Source location (1-based lines and columns, end exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

impl Span {
    pub fn new(line: u32, col: u32, end_line: u32, end_col: u32) -> Self {
        Self {
            line,
            col,
            end_line,
            end_col,
        }
    }

    /// Smallest span covering both `self` and `other`.
    pub fn to(self, other: Span) -> Span {
        Span {
            line: self.line,
            col: self.col,
            end_line: other.end_line,
            end_col: other.end_col,
        }
    }

    /// Whether the 1-based position `(line, col)` lies inside the span.
    pub fn contains(&self, line: u32, col: u32) -> bool {
        (line, col) >= (self.line, self.col) && (line, col) <= (self.end_line, self.end_col)
    }
}

/// Byte order of multi-byte values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Endian {
    #[default]
    Little,
    Big,
}

/// A parsed template.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Program {
    /// Top-level statements: type definitions, functions and placed fields.
    pub items: Vec<Stmt>,
    /// `#pragma` lines (`endian`, `description`, `magic`, ...), in order.
    pub pragmas: Vec<(String, String)>,
}

impl Program {
    /// Iterates over every type definition, including nested namespaces.
    pub fn type_defs(&self) -> impl Iterator<Item = &TypeDef> {
        self.items.iter().filter_map(|s| match &s.kind {
            StmtKind::TypeDef(t) => Some(t),
            _ => None,
        })
    }

    /// Looks up a type definition by name.
    pub fn type_def(&self, name: &str) -> Option<&TypeDef> {
        self.type_defs().find(|t| t.name() == name)
    }

    /// Iterates over every function definition.
    pub fn functions(&self) -> impl Iterator<Item = &FnDef> {
        self.items.iter().filter_map(|s| match &s.kind {
            StmtKind::Fn(f) => Some(f),
            _ => None,
        })
    }

    /// Value of the last `#pragma name ...`.
    pub fn pragma(&self, name: &str) -> Option<&str> {
        self.pragmas
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Reference to a type at a use site, e.g. `be u32`, `Foo<4>`, `Bar(n)`.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeRef {
    pub name: String,
    /// Per-use endian prefix (`le`/`be`).
    pub endian: Option<Endian>,
    /// Template or parameter arguments.
    pub args: Vec<Expr>,
}

impl TypeRef {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            endian: None,
            args: Vec::new(),
        }
    }

    pub fn with_endian(mut self, endian: Option<Endian>) -> Self {
        self.endian = endian;
        self
    }
}

/// `[[name(args)]]` attribute (also produced from 010 `<key=value>` lists).
#[derive(Debug, Clone, PartialEq)]
pub struct Attr {
    pub name: String,
    pub args: Vec<Expr>,
}

impl Attr {
    pub fn new(name: impl Into<String>, args: Vec<Expr>) -> Self {
        Self {
            name: name.into(),
            args,
        }
    }

    /// Attribute with a single string argument.
    pub fn string(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self::new(name, vec![Expr::new(ExprKind::Str(value.into()))])
    }
}

/// Finds an attribute by name.
pub fn find_attr<'a>(attrs: &'a [Attr], name: &str) -> Option<&'a Attr> {
    attrs.iter().find(|a| a.name == name)
}

/// Array dimension of a field declaration.
#[derive(Debug, Clone, PartialEq)]
pub enum ArraySpec {
    /// `T x[expr];`
    Fixed(Expr),
    /// `T x[while(cond)];` — condition checked before every element.
    While(Expr),
    /// `T x[until(cond)];` — condition checked after every element (`_` is
    /// the last element), Kaitai `repeat-until`.
    Until(Expr),
    /// `T x[];` — until the end of the data (or the current sized region).
    Eof,
}

/// Field declaration inside a struct, union or at the top level.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDecl {
    pub ty: TypeRef,
    pub name: String,
    pub array: Option<ArraySpec>,
    /// Pointer size type for `T *name : u32;`.
    pub pointer: Option<TypeRef>,
    /// Placement `@ expr` (absolute offset; does not move the cursor).
    pub placement: Option<Expr>,
    /// In-struct bit-field width, `u32 flag : 3;` (010/C style).
    pub bits: Option<Expr>,
    pub attrs: Vec<Attr>,
}

/// Local or computed variable (`auto x = e;`, `let x = e;`, `local int x;`).
#[derive(Debug, Clone, PartialEq)]
pub struct LetDecl {
    pub name: String,
    pub ty: Option<TypeRef>,
    pub init: Option<Expr>,
    /// Local array length (`local int a[4];`).
    pub array: Option<Expr>,
    /// Shown as a computed node in the result tree (STRC-02).
    pub visible: bool,
    pub attrs: Vec<Attr>,
}

/// One arm of a `match`/`switch`.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    /// One pattern per scrutinee; several alternatives per arm via `Pattern::Or`.
    pub patterns: Vec<Pattern>,
    pub body: Vec<Stmt>,
}

/// Match pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Wildcard,
    Value(Expr),
    Range(Expr, Expr),
    Or(Vec<Pattern>),
}

/// Statement with its location.
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

impl Stmt {
    pub fn new(kind: StmtKind, span: Span) -> Self {
        Self { kind, span }
    }

    /// Statement without a meaningful location (generated by importers).
    pub fn synthetic(kind: StmtKind) -> Self {
        Self {
            kind,
            span: Span::default(),
        }
    }
}

/// Statement kinds.
#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    Field(FieldDecl),
    TypeDef(TypeDef),
    Fn(FnDef),
    Let(LetDecl),
    /// `target op= value` (`op` is `None` for plain `=`).
    Assign {
        target: Expr,
        op: Option<BinOp>,
        value: Expr,
    },
    Expr(Expr),
    If {
        cond: Expr,
        then: Vec<Stmt>,
        els: Vec<Stmt>,
    },
    Match {
        scrutinee: Vec<Expr>,
        arms: Vec<MatchArm>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    DoWhile {
        body: Vec<Stmt>,
        cond: Expr,
    },
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        step: Option<Box<Stmt>>,
        body: Vec<Stmt>,
    },
    Break,
    Continue,
    Return(Option<Expr>),
    Assert {
        cond: Expr,
        msg: Option<Expr>,
        warn: bool,
    },
    Block(Vec<Stmt>),
}

/// Struct or union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructKind {
    Struct,
    Union,
}

/// Template / struct parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    /// `None` for a type parameter (`struct Foo<T>`).
    pub ty: Option<TypeRef>,
}

/// A named type definition.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeDef {
    Struct(StructDef),
    Enum(EnumDef),
    Bitfield(BitfieldDef),
    Alias(AliasDef),
}

impl TypeDef {
    pub fn name(&self) -> &str {
        match self {
            TypeDef::Struct(s) => &s.name,
            TypeDef::Enum(e) => &e.name,
            TypeDef::Bitfield(b) => &b.name,
            TypeDef::Alias(a) => &a.name,
        }
    }

    pub fn attrs(&self) -> &[Attr] {
        match self {
            TypeDef::Struct(s) => &s.attrs,
            TypeDef::Enum(e) => &e.attrs,
            TypeDef::Bitfield(b) => &b.attrs,
            TypeDef::Alias(a) => &a.attrs,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructDef {
    pub name: String,
    pub kind: StructKind,
    pub params: Vec<Param>,
    /// Parent struct for `struct A : B { ... }`.
    pub inherits: Option<TypeRef>,
    pub body: Vec<Stmt>,
    pub attrs: Vec<Attr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
    pub name: String,
    pub value: Option<Expr>,
    /// Upper bound for range variants `A = 1 ... 5`.
    pub end: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumDef {
    pub name: String,
    pub base: TypeRef,
    pub variants: Vec<EnumVariant>,
    pub attrs: Vec<Attr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BitfieldMember {
    /// `None` for `padding : n;`.
    pub name: Option<String>,
    pub bits: Expr,
    /// Typed member (`Enum e : 3;`, `bool b : 1;`).
    pub ty: Option<TypeRef>,
    pub attrs: Vec<Attr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BitfieldDef {
    pub name: String,
    pub members: Vec<BitfieldMember>,
    pub attrs: Vec<Attr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AliasDef {
    pub name: String,
    pub target: TypeRef,
    /// Array dimension carried by the alias (`typedef char NAME[8];`).
    pub array: Option<Expr>,
    pub attrs: Vec<Attr>,
}

/// Function definition (`fn name(args) { ... }`).
#[derive(Debug, Clone, PartialEq)]
pub struct FnDef {
    pub name: String,
    pub params: Vec<Param>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

/// Expression with location.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

impl Expr {
    pub fn new(kind: ExprKind) -> Self {
        Self {
            kind,
            span: Span::default(),
        }
    }

    pub fn at(kind: ExprKind, span: Span) -> Self {
        Self { kind, span }
    }

    pub fn int(v: i128) -> Self {
        Self::new(ExprKind::Int(v))
    }

    pub fn ident(name: impl Into<String>) -> Self {
        Self::new(ExprKind::Ident(name.into()))
    }

    pub fn str(s: impl Into<String>) -> Self {
        Self::new(ExprKind::Str(s.into()))
    }

    pub fn binary(op: BinOp, l: Expr, r: Expr) -> Self {
        Self::new(ExprKind::Binary(op, Box::new(l), Box::new(r)))
    }

    pub fn call(name: impl Into<String>, args: Vec<Expr>) -> Self {
        Self::new(ExprKind::Call(name.into(), args))
    }

    pub fn member(base: Expr, name: impl Into<String>) -> Self {
        Self::new(ExprKind::Member(Box::new(base), name.into()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
    Plus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Xor,
}

impl BinOp {
    /// Source spelling of the operator.
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
            BinOp::Xor => "^^",
        }
    }
}

/// Argument of `sizeof`.
#[derive(Debug, Clone, PartialEq)]
pub enum SizeOfArg {
    Expr(Box<Expr>),
    Type(TypeRef),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(String),
    /// Identifier or `::`-path (`Enum::A`, `std::mem::size`).
    Ident(String),
    /// `$` — current offset.
    Dollar,
    This,
    Parent,
    /// Outermost structure being evaluated (Kaitai `_root`).
    Root,
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Member(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
    SizeOf(SizeOfArg),
    AddressOf(Box<Expr>),
    /// `(T)expr` or `T(expr)` with a primitive target.
    Cast(TypeRef, Box<Expr>),
    /// Type used as a template argument (`Foo<Bar<u8>>`, `Foo<be u16>`).
    TypeArg(TypeRef),
}
