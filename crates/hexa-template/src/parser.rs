//! Recursive-descent parser for the template language (STRC-01/02) and its
//! dialects: `ImHex` `.hexpat` (STRC-04), 010 Editor `.bt` (STRC-05) and C
//! headers (STRC-06). The parser recovers from errors at statement
//! boundaries so editors get every diagnostic and a partial AST.

use crate::ast::Endian;
use crate::ast::{
    AliasDef, ArraySpec, Attr, BinOp, BitfieldDef, BitfieldMember, EnumDef, EnumVariant, Expr,
    ExprKind, FieldDecl, FnDef, LetDecl, MatchArm, Param, Pattern, Program, SizeOfArg, Span, Stmt,
    StmtKind, StructDef, StructKind, TypeDef, TypeRef, UnOp,
};
use crate::diag::{Diagnostic, Error, Result};
use crate::lexer::{Resolver, Tok, Token, preprocess};

/// Source dialect accepted by the parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dialect {
    /// Native / `ImHex` pattern language (STRC-01, STRC-04).
    #[default]
    Hexpat,
    /// 010 Editor binary template (STRC-05).
    Bt,
    /// C header (STRC-06).
    C,
    /// Kaitai Struct expression language (STRC-03); expressions only.
    Ksy,
}

/// Parser configuration.
#[derive(Clone, Copy)]
pub struct ParseOptions<'a> {
    pub dialect: Dialect,
    /// Resolves `#include` / `import` (STRC-02). Templates cannot reach the
    /// file system any other way.
    pub resolver: Option<Resolver<'a>>,
    /// Size of C `long` in bytes (C headers only; 8 = LP64, 4 = LLP64).
    pub c_long_size: u8,
    /// Size of C pointers in bytes (C headers only).
    pub c_pointer_size: u8,
}

impl Default for ParseOptions<'_> {
    fn default() -> Self {
        Self {
            dialect: Dialect::Hexpat,
            resolver: None,
            c_long_size: 8,
            c_pointer_size: 8,
        }
    }
}

impl std::fmt::Debug for ParseOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParseOptions")
            .field("dialect", &self.dialect)
            .field("resolver", &self.resolver.is_some())
            .field("c_long_size", &self.c_long_size)
            .field("c_pointer_size", &self.c_pointer_size)
            .finish()
    }
}

/// Parses native template source; any error diagnostic fails the parse.
pub fn parse(src: &str) -> Result<Program> {
    parse_strict(src, &ParseOptions::default())
}

/// Parses with options; any error diagnostic fails the parse.
pub fn parse_strict(src: &str, opts: &ParseOptions<'_>) -> Result<Program> {
    let (p, d) = parse_with(src, opts);
    if d.iter().any(|d| d.severity == crate::diag::Severity::Error) {
        Err(Error::Parse(d))
    } else {
        Ok(p)
    }
}

/// Parses with error recovery, returning a (possibly partial) program and
/// all diagnostics.
pub fn parse_with(src: &str, opts: &ParseOptions<'_>) -> (Program, Vec<Diagnostic>) {
    let (toks, diags) = preprocess(src, opts.resolver);
    let mut p = Parser::new(toks, *opts);
    p.diags = diags;
    let mut items = p.items_until(None, Ctx::Top);
    let mut all = std::mem::take(&mut p.hoisted);
    all.append(&mut items);
    (
        Program {
            items: all,
            pragmas: p.pragmas,
        },
        p.diags,
    )
}

/// Parses a single expression (used by importers, e.g. Kaitai `if:`).
pub fn parse_expr(src: &str, dialect: Dialect) -> Result<Expr, Diagnostic> {
    let (toks, diags) = crate::lexer::tokenize(src);
    if let Some(d) = diags.into_iter().next() {
        return Err(d);
    }
    let mut p = Parser::new(
        toks,
        ParseOptions {
            dialect,
            ..ParseOptions::default()
        },
    );
    let e = p.expr()?;
    if !p.at_eof() {
        return Err(p.err("unexpected tokens after expression"));
    }
    Ok(e)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Top,
    Struct,
    Func,
}

type PResult<T> = Result<T, Diagnostic>;

const KEYWORDS: &[&str] = &[
    "if",
    "else",
    "while",
    "for",
    "do",
    "return",
    "break",
    "continue",
    "match",
    "switch",
    "case",
    "default",
    "fn",
    "struct",
    "union",
    "enum",
    "bitfield",
    "using",
    "typedef",
    "namespace",
    "let",
    "local",
    "true",
    "false",
    "this",
    "parent",
    "sizeof",
    "addressof",
    "import",
];

/// Canonical primitive for C / 010 multi-word type spellings.
fn c_multiword(words: &[String], long_size: u8) -> Option<&'static str> {
    let mut unsigned = false;
    let mut signed = false;
    let mut longs = 0;
    let mut short = false;
    let mut base: Option<&str> = None;
    for w in words {
        match w.as_str() {
            "unsigned" => unsigned = true,
            "signed" => signed = true,
            "long" => longs += 1,
            "short" => short = true,
            "int" | "char" | "double" | "float" => base = Some(w.as_str()),
            _ => return None,
        }
    }
    Some(match (base, short, longs, unsigned) {
        (Some("char"), _, _, false) if signed => "s8",
        (Some("char"), _, _, false) => "char",
        (Some("char"), _, _, true) => "u8",
        (Some("double"), ..) => "double",
        (Some("float"), ..) => "float",
        (_, true, _, false) => "s16",
        (_, true, _, true) => "u16",
        (_, _, 0, false) => "s32",
        (_, _, 0, true) => "u32",
        (_, _, 1, u) if long_size != 8 => {
            if u {
                "u32"
            } else {
                "s32"
            }
        }
        (_, _, _, false) => "s64",
        (_, _, _, true) => "u64",
    })
}

const C_TYPE_WORDS: &[&str] = &[
    "unsigned", "signed", "long", "short", "int", "char", "double", "float",
];

/// Primitive spellings recognized for casts like `(uint)x`.
pub(crate) fn is_cast_type_name(name: &str) -> bool {
    crate::prim::Prim::from_name(name).is_some() || C_TYPE_WORDS.contains(&name)
}

struct Parser<'a> {
    toks: Vec<Token>,
    pos: usize,
    opts: ParseOptions<'a>,
    diags: Vec<Diagnostic>,
    hoisted: Vec<Stmt>,
    pragmas: Vec<(String, String)>,
    anon: u32,
    pack: Vec<Option<u32>>,
    ns: Vec<String>,
    single_decl: bool,
}

impl<'a> Parser<'a> {
    fn new(toks: Vec<Token>, opts: ParseOptions<'a>) -> Self {
        Self {
            toks,
            pos: 0,
            opts,
            diags: Vec::new(),
            hoisted: Vec::new(),
            pragmas: Vec::new(),
            anon: 0,
            pack: vec![None],
            ns: Vec::new(),
            single_decl: false,
        }
    }

    // ----- token helpers -------------------------------------------------

    fn tok(&self) -> &Tok {
        self.toks.get(self.pos).map_or(&Tok::Eof, |t| &t.tok)
    }

    fn tok_at(&self, n: usize) -> &Tok {
        self.toks.get(self.pos + n).map_or(&Tok::Eof, |t| &t.tok)
    }

    fn span(&self) -> Span {
        self.toks
            .get(self.pos)
            .or(self.toks.last())
            .map_or_else(Span::default, |t| t.span)
    }

    fn prev_span(&self) -> Span {
        if self.pos == 0 {
            return self.span();
        }
        self.toks
            .get(self.pos - 1)
            .map_or_else(Span::default, |t| t.span)
    }

    fn at_eof(&self) -> bool {
        matches!(self.tok(), Tok::Eof)
    }

    fn bump(&mut self) -> Tok {
        let t = self.tok().clone();
        if self.pos < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    fn at(&self, p: &str) -> bool {
        matches!(self.tok(), Tok::Punct(q) if *q == p)
    }

    fn at_n(&self, n: usize, p: &str) -> bool {
        matches!(self.tok_at(n), Tok::Punct(q) if *q == p)
    }

    fn eat(&mut self, p: &str) -> bool {
        if self.at(p) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn err(&self, msg: impl Into<String>) -> Diagnostic {
        Diagnostic::error(self.span(), msg)
    }

    fn describe(&self) -> String {
        match self.tok() {
            Tok::Ident(s) => format!("`{s}`"),
            Tok::Int(v) | Tok::Char(v) => format!("`{v}`"),
            Tok::Float(v) => format!("`{v}`"),
            Tok::Str(s) => format!("\"{s}\""),
            Tok::Punct(p) => format!("`{p}`"),
            Tok::Directive(n, _) => format!("`#{n}`"),
            Tok::Eof => "end of file".into(),
        }
    }

    fn expect(&mut self, p: &str) -> PResult<Span> {
        // Split compound closers inside template argument lists.
        if p == ">" {
            let rest = match self.tok() {
                Tok::Punct(">>") => Some(">"),
                Tok::Punct(">=") => Some("="),
                Tok::Punct(">>=") => Some(">="),
                _ => None,
            };
            if let Some(rest) = rest {
                let s = self.span();
                self.toks[self.pos].tok = Tok::Punct(rest);
                self.toks[self.pos].span.col += 1;
                return Ok(s);
            }
        }
        if self.at(p) {
            let s = self.span();
            self.bump();
            Ok(s)
        } else {
            Err(self.err(format!("expected `{p}`, found {}", self.describe())))
        }
    }

    fn is_ident(&self, name: &str) -> bool {
        matches!(self.tok(), Tok::Ident(s) if s == name)
    }

    fn is_ident_at(&self, n: usize, name: &str) -> bool {
        matches!(self.tok_at(n), Tok::Ident(s) if s == name)
    }

    fn eat_ident(&mut self, name: &str) -> bool {
        if self.is_ident(name) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn ident(&mut self) -> PResult<String> {
        match self.tok().clone() {
            Tok::Ident(s) => {
                self.bump();
                Ok(s)
            }
            _ => Err(self.err(format!("expected identifier, found {}", self.describe()))),
        }
    }

    /// Identifier with optional `::` path segments.
    fn path(&mut self) -> PResult<String> {
        let mut s = self.ident()?;
        while self.at("::") && matches!(self.tok_at(1), Tok::Ident(_)) {
            self.bump();
            s.push_str("::");
            s.push_str(&self.ident()?);
        }
        Ok(s)
    }

    fn bt(&self) -> bool {
        self.opts.dialect == Dialect::Bt
    }

    fn c(&self) -> bool {
        self.opts.dialect == Dialect::C
    }

    fn ksy(&self) -> bool {
        self.opts.dialect == Dialect::Ksy
    }

    fn qualify(&self, name: &str) -> String {
        if self.ns.is_empty() {
            name.to_string()
        } else {
            format!("{}::{name}", self.ns.join("::"))
        }
    }

    fn anon_name(&mut self, what: &str) -> String {
        self.anon += 1;
        format!("__anon_{what}_{}", self.anon)
    }

    // ----- recovery ------------------------------------------------------

    fn recover(&mut self) {
        let mut depth = 0i32;
        loop {
            match self.tok() {
                Tok::Eof => return,
                Tok::Punct("{") => depth += 1,
                Tok::Punct("}") => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                    if depth == 0 {
                        self.bump();
                        self.eat(";");
                        return;
                    }
                }
                Tok::Punct(";") if depth == 0 => {
                    self.bump();
                    return;
                }
                _ => {}
            }
            self.bump();
        }
    }

    fn items_until(&mut self, close: Option<&str>, ctx: Ctx) -> Vec<Stmt> {
        let mut out = Vec::new();
        loop {
            if self.at_eof() {
                if let Some(c) = close {
                    self.diags
                        .push(self.err(format!("expected `{c}` before end of file")));
                }
                return out;
            }
            if let Some(c) = close
                && self.at(c)
            {
                self.bump();
                return out;
            }
            if close.is_none() && self.at("}") {
                self.diags.push(self.err("unmatched `}`"));
                self.bump();
                continue;
            }
            let start = self.pos;
            match self.stmt(ctx) {
                Ok(mut s) => out.append(&mut s),
                Err(d) => {
                    self.diags.push(d);
                    self.recover();
                    if self.pos == start {
                        self.bump();
                    }
                }
            }
        }
    }

    fn block(&mut self, ctx: Ctx) -> PResult<Vec<Stmt>> {
        self.expect("{")?;
        Ok(self.items_until(Some("}"), ctx))
    }

    fn block_or_stmt(&mut self, ctx: Ctx) -> PResult<Vec<Stmt>> {
        if self.at("{") {
            self.block(ctx)
        } else {
            self.stmt(ctx)
        }
    }

    // ----- statements ----------------------------------------------------

    fn stmt(&mut self, ctx: Ctx) -> PResult<Vec<Stmt>> {
        let start = self.span();
        let one = |k: StmtKind, p: &Self| Ok(vec![Stmt::new(k, start.to(p.prev_span()))]);
        match self.tok().clone() {
            Tok::Directive(name, rest) => {
                self.bump();
                self.directive(&name, &rest, start);
                Ok(vec![])
            }
            Tok::Punct(";") => {
                self.bump();
                Ok(vec![])
            }
            Tok::Punct("{") => {
                let b = self.block(ctx)?;
                one(StmtKind::Block(b), self)
            }
            Tok::Punct("[") if self.at_n(1, "[") => {
                // Stray attribute list; ignore.
                self.attrs()?;
                Ok(vec![])
            }
            Tok::Ident(kw) => match kw.as_str() {
                "struct" | "union" if self.is_type_definition() => self.struct_def(ctx, start),
                "enum" if self.is_type_definition() => self.enum_def(ctx, start),
                "bitfield" => self.bitfield_def(ctx, start),
                "using" => self.using(ctx, start),
                "typedef" => self.typedef(ctx, start),
                "fn" => self.fn_def(start),
                "namespace" => {
                    self.bump();
                    let mut n = self.path()?;
                    if n == "auto" {
                        n.clear();
                    }
                    let pushed = !n.is_empty();
                    if pushed {
                        self.ns.push(n);
                    }
                    let body = self.block(Ctx::Top);
                    if pushed {
                        self.ns.pop();
                    }
                    body
                }
                "if" => {
                    self.bump();
                    self.expect("(")?;
                    let cond = self.expr()?;
                    self.expect(")")?;
                    let then = self.block_or_stmt(ctx)?;
                    let els = if self.eat_ident("else") {
                        self.block_or_stmt(ctx)?
                    } else {
                        vec![]
                    };
                    one(StmtKind::If { cond, then, els }, self)
                }
                "match" => self.match_stmt(ctx, start),
                "switch" => self.switch_stmt(ctx, start),
                "while" => {
                    self.bump();
                    self.expect("(")?;
                    let cond = self.expr()?;
                    self.expect(")")?;
                    let body = self.block_or_stmt(ctx)?;
                    one(StmtKind::While { cond, body }, self)
                }
                "do" => {
                    self.bump();
                    let body = self.block_or_stmt(ctx)?;
                    if !self.eat_ident("while") {
                        return Err(self.err("expected `while` after `do` body"));
                    }
                    self.expect("(")?;
                    let cond = self.expr()?;
                    self.expect(")")?;
                    self.eat(";");
                    one(StmtKind::DoWhile { body, cond }, self)
                }
                "for" => self.for_stmt(ctx, start),
                "break" => {
                    self.bump();
                    self.expect(";")?;
                    one(StmtKind::Break, self)
                }
                "continue" => {
                    self.bump();
                    self.expect(";")?;
                    one(StmtKind::Continue, self)
                }
                "return" => {
                    self.bump();
                    let v = if self.at(";") {
                        None
                    } else {
                        Some(self.expr()?)
                    };
                    self.expect(";")?;
                    one(StmtKind::Return(v), self)
                }
                "assert" | "std::assert" | "std::assert_warn" | "Assert" => self.assert_stmt(start),
                "std"
                    if self.at_n(1, "::")
                        && (self.is_ident_at(2, "assert")
                            || self.is_ident_at(2, "assert_warn"))
                        && self.at_n(3, "(") =>
                {
                    self.assert_stmt(start)
                }
                "auto" | "let" if matches!(self.tok_at(1), Tok::Ident(_)) => {
                    self.bump();
                    let name = self.ident()?;
                    let init = if self.eat("=") {
                        Some(self.expr()?)
                    } else {
                        None
                    };
                    let attrs = self.attrs_opt()?;
                    self.expect(";")?;
                    one(
                        StmtKind::Let(LetDecl {
                            name,
                            ty: None,
                            init,
                            array: None,
                            visible: ctx != Ctx::Func,
                            attrs,
                        }),
                        self,
                    )
                }
                "local" | "const" | "static" | "volatile" => {
                    // `const` at top level of a C header / bt is a local too.
                    self.bump();
                    if kw == "static" && !self.c() {
                        return self.stmt(ctx);
                    }
                    let ty = self.type_ref()?;
                    let mut out = Vec::new();
                    loop {
                        let s = self.span();
                        let name = self.ident()?;
                        let array = if self.eat("[") {
                            let e = if self.at("]") {
                                None
                            } else {
                                Some(self.expr()?)
                            };
                            self.expect("]")?;
                            e
                        } else {
                            None
                        };
                        let init = if self.eat("=") {
                            Some(self.initializer()?)
                        } else {
                            None
                        };
                        let attrs = self.attrs_opt()?;
                        out.push(Stmt::new(
                            StmtKind::Let(LetDecl {
                                name,
                                ty: Some(ty.clone()),
                                init,
                                array,
                                visible: false,
                                attrs,
                            }),
                            s.to(self.prev_span()),
                        ));
                        if !self.eat(",") {
                            break;
                        }
                    }
                    self.expect(";")?;
                    Ok(out)
                }
                "extern" | "inline" if self.c() || self.bt() => {
                    self.recover();
                    Ok(vec![])
                }
                "else" => Err(self.err("`else` without `if`")),
                _ => self.decl_or_expr(ctx, true),
            },
            _ => self.decl_or_expr(ctx, true),
        }
    }

    fn directive(&mut self, name: &str, rest: &str, span: Span) {
        if name != "pragma" {
            self.diags.push(Diagnostic::warning(
                span,
                format!("unsupported directive `#{name}` ignored"),
            ));
            return;
        }
        let rest = rest.trim();
        let (key, value) = rest
            .split_once(|c: char| c.is_whitespace() || c == '(')
            .map_or((rest, ""), |(k, v)| (k, v));
        let value = if rest[key.len()..].starts_with('(') {
            format!("({value}")
        } else {
            value.trim().to_string()
        };
        if key == "pack" {
            let inner = value
                .trim()
                .trim_start_matches('(')
                .trim_end_matches(')')
                .to_string();
            let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
            match parts.as_slice() {
                [""] => *self.pack.last_mut().unwrap_or(&mut None) = None,
                ["push"] => {
                    let cur = self.pack.last().copied().flatten();
                    self.pack.push(cur);
                }
                ["push", n, ..] => self.pack.push(n.parse().ok()),
                ["pop", ..] => {
                    if self.pack.len() > 1 {
                        self.pack.pop();
                    }
                }
                [n] => *self.pack.last_mut().unwrap_or(&mut None) = n.parse().ok(),
                _ => {}
            }
        }
        self.pragmas.push((key.to_string(), value));
    }

    fn assert_stmt(&mut self, start: Span) -> PResult<Vec<Stmt>> {
        let name = self.path()?;
        self.expect("(")?;
        let cond = self.expr()?;
        let msg = if self.eat(",") {
            Some(self.expr()?)
        } else {
            None
        };
        self.expect(")")?;
        self.expect(";")?;
        Ok(vec![Stmt::new(
            StmtKind::Assert {
                cond,
                msg,
                warn: name.ends_with("assert_warn"),
            },
            start.to(self.prev_span()),
        )])
    }

    /// `struct X {`, `struct {`, `struct X : P {`, `struct X<T> {`,
    /// `struct X (params) {` — as opposed to `struct X var;` in C.
    fn is_type_definition(&self) -> bool {
        let mut n = 1;
        if matches!(self.tok_at(1), Tok::Ident(_)) {
            n = 2;
            if self.at_n(2, "<") && self.is_ident("enum") {
                // bt `enum <uchar> NAME {`
                return true;
            }
        } else if self.at_n(1, "<") {
            return true; // bt `enum <uchar> {`
        }
        while self.at_n(n, "::") {
            n += 2;
        }
        matches!(self.tok_at(n), Tok::Punct("{" | ":" | "<" | "("))
            || (self.c() && self.is_ident_at(1, "__attribute__"))
    }

    fn gnu_attribute(&mut self) -> PResult<bool> {
        let mut packed = false;
        while self.eat_ident("__attribute__") || self.eat_ident("__attribute") {
            self.expect("(")?;
            let mut depth = 1;
            while depth > 0 && !self.at_eof() {
                match self.bump() {
                    Tok::Punct("(") => depth += 1,
                    Tok::Punct(")") => depth -= 1,
                    Tok::Ident(s) if s == "packed" || s == "__packed__" => packed = true,
                    _ => {}
                }
            }
        }
        Ok(packed)
    }

    fn struct_def(&mut self, ctx: Ctx, start: Span) -> PResult<Vec<Stmt>> {
        let kind = if self.ident()? == "union" {
            StructKind::Union
        } else {
            StructKind::Struct
        };
        let mut packed = self.gnu_attribute()?;
        let name = if matches!(self.tok(), Tok::Ident(_)) {
            self.path()?
        } else {
            self.anon_name(if kind == StructKind::Union {
                "union"
            } else {
                "struct"
            })
        };
        let def = self.struct_rest(kind, &name, &mut packed)?;
        self.finish_type_def(ctx, start, TypeDef::Struct(def), &name)
    }

    fn struct_rest(
        &mut self,
        kind: StructKind,
        name: &str,
        packed: &mut bool,
    ) -> PResult<StructDef> {
        let mut params = Vec::new();
        if self.at("<") {
            self.bump();
            while !self.at(">") {
                let p = if self.eat_ident("auto") {
                    Param {
                        name: self.ident()?,
                        ty: Some(TypeRef::named("auto")),
                    }
                } else if matches!(self.tok_at(1), Tok::Ident(_)) {
                    let ty = self.type_ref()?;
                    Param {
                        name: self.ident()?,
                        ty: Some(ty),
                    }
                } else {
                    Param {
                        name: self.ident()?,
                        ty: None,
                    }
                };
                params.push(p);
                if !self.eat(",") {
                    break;
                }
            }
            self.expect(">")?;
        }
        if self.at("(") {
            params.extend(self.fn_params()?);
        }
        let inherits = if self.eat(":") {
            Some(self.type_ref()?)
        } else {
            None
        };
        let body = {
            self.expect("{")?;
            self.items_until(Some("}"), Ctx::Struct)
        };
        *packed |= self.gnu_attribute()?;
        let mut attrs = self.attrs_opt()?;
        if self.c() {
            let pack = if *packed {
                Some(1)
            } else {
                self.pack.last().copied().flatten()
            };
            attrs.push(Attr::new(
                "c_layout",
                vec![Expr::int(i128::from(pack.unwrap_or(0)))],
            ));
        }
        Ok(StructDef {
            name: self.qualify(name),
            kind,
            params,
            inherits,
            body,
            attrs,
        })
    }

    /// After a definition: optional declarators (`} a, *b;` in C/bt) or `;`.
    fn finish_type_def(
        &mut self,
        ctx: Ctx,
        start: Span,
        def: TypeDef,
        name: &str,
    ) -> PResult<Vec<Stmt>> {
        let full = def.name().to_string();
        let span = start.to(self.prev_span());
        let def_stmt = Stmt::new(StmtKind::TypeDef(def), span);
        let mut out = Vec::new();
        if ctx == Ctx::Top {
            out.push(def_stmt);
        } else {
            self.hoisted.push(def_stmt);
        }
        let _ = name;
        if self.eat(";") {
            return Ok(out);
        }
        if matches!(self.tok(), Tok::Ident(_)) || self.at("*") {
            let ty = TypeRef::named(full);
            out.extend(self.declarators(ctx, &ty, start)?);
            return Ok(out);
        }
        // ImHex allows omitting the `;` after `}`.
        if !self.opts.dialect.eq(&Dialect::Hexpat) {
            self.expect(";")?;
        }
        Ok(out)
    }

    fn enum_def(&mut self, ctx: Ctx, start: Span) -> PResult<Vec<Stmt>> {
        self.bump(); // enum
        let mut base = None;
        if self.eat("<") {
            base = Some(self.type_ref()?);
            self.expect(">")?;
        }
        let name = if matches!(self.tok(), Tok::Ident(_)) {
            self.path()?
        } else {
            self.anon_name("enum")
        };
        if self.eat(":") {
            base = Some(self.type_ref()?);
        }
        let variants = self.enum_body()?;
        let attrs = self.attrs_opt()?;
        let base = base
            .unwrap_or_else(|| TypeRef::named(if self.c() || self.bt() { "s32" } else { "u32" }));
        let def = TypeDef::Enum(EnumDef {
            name: self.qualify(&name),
            base,
            variants,
            attrs,
        });
        self.finish_type_def(ctx, start, def, &name)
    }

    fn enum_body(&mut self) -> PResult<Vec<EnumVariant>> {
        self.expect("{")?;
        let mut variants = Vec::new();
        while !self.at("}") {
            let name = self.ident()?;
            let mut value = None;
            let mut end = None;
            if self.eat("=") {
                value = Some(self.expr()?);
                if self.eat("...") {
                    end = Some(self.expr()?);
                }
            }
            variants.push(EnumVariant { name, value, end });
            if !self.eat(",") {
                break;
            }
        }
        self.expect("}")?;
        Ok(variants)
    }

    fn bitfield_def(&mut self, ctx: Ctx, start: Span) -> PResult<Vec<Stmt>> {
        self.bump();
        let name = self.path()?;
        self.expect("{")?;
        let mut members = Vec::new();
        while !self.at("}") && !self.at_eof() {
            if self.eat(";") {
                continue;
            }
            let (ty, mname) = if matches!(self.tok_at(1), Tok::Punct(":")) {
                let n = self.ident()?;
                (None, n)
            } else {
                let ty = self.type_ref()?;
                (Some(ty), self.ident()?)
            };
            self.expect(":")?;
            let bits = self.expr()?;
            let attrs = self.attrs_opt()?;
            self.expect(";")?;
            members.push(BitfieldMember {
                name: (mname != "padding").then_some(mname),
                bits,
                ty,
                attrs,
            });
        }
        self.expect("}")?;
        let attrs = self.attrs_opt()?;
        let def = TypeDef::Bitfield(BitfieldDef {
            name: self.qualify(&name),
            members,
            attrs,
        });
        self.finish_type_def(ctx, start, def, &name)
    }

    fn using(&mut self, ctx: Ctx, start: Span) -> PResult<Vec<Stmt>> {
        self.bump();
        let name = self.path()?;
        if self.at("<") {
            // Generic alias parameters are accepted but not used.
            while !self.at(">") && !self.at_eof() {
                self.bump();
            }
            self.expect(">")?;
        }
        if self.eat(";") {
            return Ok(vec![]); // forward declaration
        }
        self.expect("=")?;
        let target = self.type_ref()?;
        let array = if self.eat("[") {
            let e = self.expr()?;
            self.expect("]")?;
            Some(e)
        } else {
            None
        };
        let attrs = self.attrs_opt()?;
        self.expect(";")?;
        let def = Stmt::new(
            StmtKind::TypeDef(TypeDef::Alias(AliasDef {
                name: self.qualify(&name),
                target,
                array,
                attrs,
            })),
            start.to(self.prev_span()),
        );
        Ok(self.place_def(ctx, def))
    }

    fn place_def(&mut self, ctx: Ctx, def: Stmt) -> Vec<Stmt> {
        if ctx == Ctx::Top {
            vec![def]
        } else {
            self.hoisted.push(def);
            vec![]
        }
    }

    fn typedef(&mut self, ctx: Ctx, start: Span) -> PResult<Vec<Stmt>> {
        self.bump();
        let mut out = Vec::new();
        let target: TypeRef;
        if (self.is_ident("struct") || self.is_ident("union")) && self.is_type_definition() {
            let kind = if self.ident()? == "union" {
                StructKind::Union
            } else {
                StructKind::Struct
            };
            let mut packed = self.gnu_attribute()?;
            let tag = if matches!(self.tok(), Tok::Ident(_)) {
                Some(self.path()?)
            } else {
                None
            };
            // The struct is named after the first declarator; parse body first.
            let placeholder = tag.clone().unwrap_or_else(|| self.anon_name("struct"));
            let mut def = self.struct_rest(kind, &placeholder, &mut packed)?;
            let alias_name = if matches!(self.tok(), Tok::Ident(_)) {
                Some(self.ident()?)
            } else {
                None
            };
            if let Some(a) = &alias_name {
                def.name = self.qualify(a);
                if let Some(t) = &tag
                    && t != a
                {
                    out.push(Stmt::new(
                        StmtKind::TypeDef(TypeDef::Alias(AliasDef {
                            name: self.qualify(t),
                            target: TypeRef::named(def.name.clone()),
                            array: None,
                            attrs: vec![],
                        })),
                        start,
                    ));
                }
            }
            let mut extra = self.attrs_opt()?;
            def.attrs.append(&mut extra);
            target = TypeRef::named(def.name.clone());
            out.insert(0, Stmt::new(StmtKind::TypeDef(TypeDef::Struct(def)), start));
            if alias_name.is_none() {
                self.expect(";")?;
                return Ok(self.place_all(ctx, out));
            }
        } else if self.is_ident("enum") && self.is_type_definition() {
            self.bump();
            let mut base = None;
            if self.eat("<") {
                base = Some(self.type_ref()?);
                self.expect(">")?;
            }
            let tag = if matches!(self.tok(), Tok::Ident(_)) {
                Some(self.ident()?)
            } else {
                None
            };
            if self.eat(":") {
                base = Some(self.type_ref()?);
            }
            let variants = self.enum_body()?;
            let name = self.ident()?;
            let attrs = self.attrs_opt()?;
            let def = EnumDef {
                name: self.qualify(&name),
                base: base.unwrap_or_else(|| TypeRef::named("s32")),
                variants,
                attrs,
            };
            target = TypeRef::named(def.name.clone());
            out.push(Stmt::new(StmtKind::TypeDef(TypeDef::Enum(def)), start));
            if let Some(t) = tag
                && t != name
            {
                out.push(self.alias_stmt(&t, target.clone(), None, start));
            }
        } else {
            let ty = self.type_ref()?;
            if self.at("(") {
                // Function pointer typedef: `typedef int (*fn)(int);`
                self.bump();
                self.eat("*");
                let name = self.ident()?;
                self.recover();
                let ptr = TypeRef::named(self.pointer_int());
                return Ok(self.place_all(ctx, vec![self.alias_stmt(&name, ptr, None, start)]));
            }
            let mut ty = ty;
            if self.eat("*") {
                while self.eat("*") {}
                ty = TypeRef::named(self.pointer_int());
            }
            let name = self.ident()?;
            let array = self.dims()?.map(|a| match a {
                ArraySpec::Fixed(e) => e,
                _ => Expr::int(0),
            });
            let attrs = self.attrs_opt()?;
            let mut s = self.alias_stmt(&name, ty.clone(), array, start);
            if let StmtKind::TypeDef(TypeDef::Alias(a)) = &mut s.kind {
                a.attrs = attrs;
            }
            out.push(s);
            target = TypeRef::named(self.qualify(&name));
            let _ = target;
            while self.eat(",") {
                let ptr = self.eat("*");
                let n = self.ident()?;
                let t = if ptr {
                    TypeRef::named(self.pointer_int())
                } else {
                    ty.clone()
                };
                out.push(self.alias_stmt(&n, t, None, start));
            }
            self.expect(";")?;
            return Ok(self.place_all(ctx, out));
        }
        // Additional declarators after a struct/enum typedef: `, *PNAME`.
        while self.eat(",") {
            let ptr = self.eat("*");
            let n = self.ident()?;
            let t = if ptr {
                TypeRef::named(self.pointer_int())
            } else {
                target.clone()
            };
            out.push(self.alias_stmt(&n, t, None, start));
        }
        self.expect(";")?;
        Ok(self.place_all(ctx, out))
    }

    fn place_all(&mut self, ctx: Ctx, stmts: Vec<Stmt>) -> Vec<Stmt> {
        if ctx == Ctx::Top {
            stmts
        } else {
            self.hoisted.extend(stmts);
            vec![]
        }
    }

    fn alias_stmt(&self, name: &str, target: TypeRef, array: Option<Expr>, span: Span) -> Stmt {
        Stmt::new(
            StmtKind::TypeDef(TypeDef::Alias(AliasDef {
                name: self.qualify(name),
                target,
                array,
                attrs: vec![],
            })),
            span,
        )
    }

    fn pointer_int(&self) -> &'static str {
        if self.opts.c_pointer_size == 4 {
            "u32"
        } else {
            "u64"
        }
    }

    fn fn_params(&mut self) -> PResult<Vec<Param>> {
        self.expect("(")?;
        let mut params = Vec::new();
        while !self.at(")") {
            if self.is_ident("void") && self.at_n(1, ")") {
                self.bump();
                break;
            }
            self.eat_ident("ref");
            self.eat_ident("const");
            self.eat_ident("local");
            if self.eat_ident("auto") {
                self.eat("...");
                params.push(Param {
                    name: self.ident()?,
                    ty: Some(TypeRef::named("auto")),
                });
            } else {
                let ty = self.type_ref()?;
                self.eat("&");
                self.eat("*");
                self.eat("...");
                let name = self.ident()?;
                if self.eat("[") {
                    self.expect("]")?;
                }
                params.push(Param { name, ty: Some(ty) });
            }
            if self.eat("=") {
                self.expr()?; // default values are accepted and ignored
            }
            if !self.eat(",") {
                break;
            }
        }
        self.expect(")")?;
        Ok(params)
    }

    fn fn_def(&mut self, start: Span) -> PResult<Vec<Stmt>> {
        self.bump();
        let name = self.path()?;
        let params = self.fn_params()?;
        let body = self.block(Ctx::Func)?;
        self.eat(";");
        let span = start.to(self.prev_span());
        let f = FnDef {
            name: self.qualify(&name),
            params,
            body,
            span,
        };
        Ok(vec![Stmt::new(StmtKind::Fn(f), span)])
    }

    fn match_stmt(&mut self, ctx: Ctx, start: Span) -> PResult<Vec<Stmt>> {
        self.bump();
        self.expect("(")?;
        let mut scrutinee = vec![self.expr()?];
        while self.eat(",") {
            scrutinee.push(self.expr()?);
        }
        self.expect(")")?;
        self.expect("{")?;
        let mut arms = Vec::new();
        while !self.at("}") && !self.at_eof() {
            self.expect("(")?;
            let mut patterns = Vec::new();
            loop {
                patterns.push(self.pattern()?);
                if !self.eat(",") {
                    break;
                }
            }
            self.expect(")")?;
            self.expect(":")?;
            let body = self.block_or_stmt(ctx)?;
            arms.push(MatchArm { patterns, body });
        }
        self.expect("}")?;
        Ok(vec![Stmt::new(
            StmtKind::Match { scrutinee, arms },
            start.to(self.prev_span()),
        )])
    }

    fn pattern(&mut self) -> PResult<Pattern> {
        let mut alts = Vec::new();
        loop {
            if self.is_ident("_") {
                self.bump();
                alts.push(Pattern::Wildcard);
            } else {
                let a = self.binary(5, false)?;
                if self.eat("...") {
                    let b = self.binary(5, false)?;
                    alts.push(Pattern::Range(a, b));
                } else {
                    alts.push(Pattern::Value(a));
                }
            }
            if !self.eat("|") {
                break;
            }
        }
        Ok(if alts.len() == 1 {
            alts.pop().unwrap_or(Pattern::Wildcard)
        } else {
            Pattern::Or(alts)
        })
    }

    fn switch_stmt(&mut self, ctx: Ctx, start: Span) -> PResult<Vec<Stmt>> {
        self.bump();
        self.expect("(")?;
        let scrutinee = self.expr()?;
        self.expect(")")?;
        self.expect("{")?;
        let mut arms: Vec<MatchArm> = Vec::new();
        let mut labels: Vec<Pattern> = Vec::new();
        let mut body: Vec<Stmt> = Vec::new();
        let mut fallthrough = false;
        let flush = |labels: &mut Vec<Pattern>, body: &mut Vec<Stmt>, arms: &mut Vec<MatchArm>| {
            if labels.is_empty() {
                return;
            }
            if matches!(body.last().map(|s| &s.kind), Some(StmtKind::Break)) {
                body.pop();
            }
            let pat = if labels.len() == 1 {
                labels.pop().unwrap_or(Pattern::Wildcard)
            } else {
                Pattern::Or(std::mem::take(labels))
            };
            labels.clear();
            arms.push(MatchArm {
                patterns: vec![pat],
                body: std::mem::take(body),
            });
        };
        while !self.at("}") && !self.at_eof() {
            if self.is_ident("case") || self.is_ident("default") {
                if !body.is_empty() || fallthrough {
                    flush(&mut labels, &mut body, &mut arms);
                    fallthrough = false;
                }
                if self.eat_ident("default") {
                    labels.push(Pattern::Wildcard);
                } else {
                    self.bump();
                    let a = self.binary(1, false)?;
                    if self.eat("...") {
                        let b = self.binary(1, false)?;
                        labels.push(Pattern::Range(a, b));
                    } else {
                        labels.push(Pattern::Value(a));
                    }
                }
                self.expect(":")?;
                continue;
            }
            let s = self.stmt(ctx)?;
            fallthrough = true;
            body.extend(s);
        }
        flush(&mut labels, &mut body, &mut arms);
        self.expect("}")?;
        // The default arm must be tried last.
        arms.sort_by_key(|a| a.patterns.iter().any(|p| matches!(p, Pattern::Wildcard)));
        Ok(vec![Stmt::new(
            StmtKind::Match {
                scrutinee: vec![scrutinee],
                arms,
            },
            start.to(self.prev_span()),
        )])
    }

    fn for_stmt(&mut self, ctx: Ctx, start: Span) -> PResult<Vec<Stmt>> {
        self.bump();
        self.expect("(")?;
        let sep = |p: &mut Self| p.eat(";") || p.eat(",");
        let init = if sep(self) {
            None
        } else {
            self.single_decl = true;
            let s = self.decl_or_expr(Ctx::Func, false);
            self.single_decl = false;
            let s = s?;
            if !sep(self) {
                return Err(self.err("expected `;` or `,` in `for`"));
            }
            s.into_iter().next().map(Box::new)
        };
        let cond = if sep(self) {
            None
        } else {
            let c = self.expr()?;
            if !sep(self) {
                return Err(self.err("expected `;` or `,` in `for`"));
            }
            Some(c)
        };
        let step = if self.at(")") {
            None
        } else {
            self.decl_or_expr(Ctx::Func, false)?
                .into_iter()
                .next()
                .map(Box::new)
        };
        self.expect(")")?;
        let body = self.block_or_stmt(ctx)?;
        Ok(vec![Stmt::new(
            StmtKind::For {
                init,
                cond,
                step,
                body,
            },
            start.to(self.prev_span()),
        )])
    }

    // ----- declarations --------------------------------------------------

    /// Tries a declaration, falling back to an expression / assignment.
    fn decl_or_expr(&mut self, ctx: Ctx, semi: bool) -> PResult<Vec<Stmt>> {
        let save = self.pos;
        let start = self.span();
        if let Some(r) = self.try_decl(ctx, semi, start) {
            return r;
        }
        self.pos = save;
        // Expression statement / assignment.
        if self.at("++") || self.at("--") {
            let op = if self.bump() == Tok::Punct("++") {
                BinOp::Add
            } else {
                BinOp::Sub
            };
            let target = self.unary()?;
            if semi {
                self.expect(";")?;
            }
            return Ok(vec![Stmt::new(
                StmtKind::Assign {
                    target,
                    op: Some(op),
                    value: Expr::int(1),
                },
                start.to(self.prev_span()),
            )]);
        }
        let e = self.expr()?;
        let assign_op = match self.tok() {
            Tok::Punct("=") => Some(None),
            Tok::Punct("+=") => Some(Some(BinOp::Add)),
            Tok::Punct("-=") => Some(Some(BinOp::Sub)),
            Tok::Punct("*=") => Some(Some(BinOp::Mul)),
            Tok::Punct("/=") => Some(Some(BinOp::Div)),
            Tok::Punct("%=") => Some(Some(BinOp::Rem)),
            Tok::Punct("&=") => Some(Some(BinOp::BitAnd)),
            Tok::Punct("|=") => Some(Some(BinOp::BitOr)),
            Tok::Punct("^=") => Some(Some(BinOp::BitXor)),
            Tok::Punct("<<=") => Some(Some(BinOp::Shl)),
            Tok::Punct(">>=") => Some(Some(BinOp::Shr)),
            _ => None,
        };
        let kind = if let Some(op) = assign_op {
            self.bump();
            let value = self.expr()?;
            StmtKind::Assign {
                target: e,
                op,
                value,
            }
        } else if self.at("++") || self.at("--") {
            let op = if self.bump() == Tok::Punct("++") {
                BinOp::Add
            } else {
                BinOp::Sub
            };
            StmtKind::Assign {
                target: e,
                op: Some(op),
                value: Expr::int(1),
            }
        } else {
            StmtKind::Expr(e)
        };
        if semi {
            self.expect(";")?;
        }
        Ok(vec![Stmt::new(kind, start.to(self.prev_span()))])
    }

    /// Returns `None` when the tokens do not form a declaration.
    fn try_decl(&mut self, ctx: Ctx, semi: bool, start: Span) -> Option<PResult<Vec<Stmt>>> {
        if !matches!(self.tok(), Tok::Ident(_)) {
            return None;
        }
        if let Tok::Ident(s) = self.tok()
            && KEYWORDS.contains(&s.as_str())
            && s != "struct"
            && s != "union"
            && s != "enum"
        {
            return None;
        }
        // `padding[n];`
        if self.is_ident("padding") && self.at_n(1, "[") {
            self.bump();
            let dims = match self.dims() {
                Ok(d) => d,
                Err(e) => return Some(Err(e)),
            };
            let attrs = match self.attrs_opt() {
                Ok(a) => a,
                Err(e) => return Some(Err(e)),
            };
            if semi && let Err(e) = self.expect(";") {
                return Some(Err(e));
            }
            return Some(Ok(vec![Stmt::new(
                StmtKind::Field(FieldDecl {
                    ty: TypeRef::named("padding"),
                    name: "padding".into(),
                    array: dims,
                    pointer: None,
                    placement: None,
                    bits: None,
                    attrs,
                }),
                start.to(self.prev_span()),
            )]));
        }
        let ty = self.type_ref().ok()?;
        let is_decl = match self.tok() {
            Tok::Ident(s) => !KEYWORDS.contains(&s.as_str()) || s == "parent",
            Tok::Punct("*") => matches!(self.tok_at(1), Tok::Ident(_) | Tok::Punct("*")),
            Tok::Punct("&") => (self.bt() || self.c()) && matches!(self.tok_at(1), Tok::Ident(_)),
            _ => false,
        };
        if !is_decl {
            return None;
        }
        // C / 010 function definitions and prototypes.
        if (self.bt() || self.c())
            && ctx == Ctx::Top
            && matches!(self.tok(), Tok::Ident(_))
            && self.at_n(1, "(")
        {
            let save = self.pos;
            let name = self.ident().ok()?;
            if let Ok(params) = self.fn_params() {
                if self.at("{") {
                    return Some(self.block(Ctx::Func).map(|body| {
                        let span = start.to(self.prev_span());
                        vec![Stmt::new(
                            StmtKind::Fn(FnDef {
                                name: self.qualify(&name),
                                params,
                                body,
                                span,
                            }),
                            span,
                        )]
                    }));
                }
                if self.at(";") && self.c() {
                    self.bump();
                    return Some(Ok(vec![])); // prototype
                }
            }
            self.pos = save;
        }
        let r = self.declarators(ctx, &ty, start);
        Some(self.finish_decl(r, semi))
    }

    fn finish_decl(&mut self, r: PResult<Vec<Stmt>>, semi: bool) -> PResult<Vec<Stmt>> {
        let v = r?;
        if semi {
            self.expect(";")?;
        }
        Ok(v)
    }

    /// One or more declarators after a type; does not consume `;`.
    fn declarators(&mut self, ctx: Ctx, ty: &TypeRef, start: Span) -> PResult<Vec<Stmt>> {
        let mut out = Vec::new();
        loop {
            let s = if out.is_empty() { start } else { self.span() };
            out.push(self.declarator(ctx, ty.clone(), s)?);
            if self.single_decl || !self.eat(",") {
                break;
            }
        }
        Ok(out)
    }

    fn declarator(&mut self, ctx: Ctx, mut ty: TypeRef, start: Span) -> PResult<Stmt> {
        let mut pointer = None;
        let mut c_pointer = false;
        if self.eat("*") {
            while self.eat("*") {}
            c_pointer = true;
        }
        self.eat("&");
        let name = self.ident()?;
        if c_pointer && self.eat(":") {
            // ImHex pointer: `T *p : u32;`
            pointer = Some(self.type_ref()?);
            c_pointer = false;
        }
        // 010 struct arguments after the variable name: `REC r(5);`
        if self.bt() && self.at("(") {
            self.bump();
            while !self.at(")") {
                ty.args.push(self.expr()?);
                if !self.eat(",") {
                    break;
                }
            }
            self.expect(")")?;
        }
        let array = self.dims()?;
        let bits = if pointer.is_none() && self.eat(":") {
            Some(self.binary(1, false)?)
        } else {
            None
        };
        let placement = if self.eat("@") {
            Some(self.expr()?)
        } else {
            None
        };
        let init = if self.eat("=") {
            Some(self.initializer()?)
        } else {
            None
        };
        if self.eat_ident("in") || self.eat_ident("out") {
            // ImHex in/out variables are treated as locals.
        }
        let mut attrs = self.attrs_opt()?;
        if c_pointer {
            attrs.push(Attr::string("type_name", format!("{}*", ty.name)));
            ty = TypeRef::named(self.pointer_int());
        }
        let span = start.to(self.prev_span());
        if init.is_some() || ctx == Ctx::Func {
            let array = match array {
                Some(ArraySpec::Fixed(e)) => Some(e),
                _ => None,
            };
            return Ok(Stmt::new(
                StmtKind::Let(LetDecl {
                    name,
                    ty: Some(ty),
                    init,
                    array,
                    visible: false,
                    attrs,
                }),
                span,
            ));
        }
        Ok(Stmt::new(
            StmtKind::Field(FieldDecl {
                ty,
                name,
                array,
                pointer,
                placement,
                bits,
                attrs,
            }),
            span,
        ))
    }

    /// `= expr` or a C brace initializer (first element kept).
    fn initializer(&mut self) -> PResult<Expr> {
        if self.at("{") {
            self.bump();
            let first = if self.at("}") {
                Expr::int(0)
            } else {
                self.expr()?
            };
            while !self.at("}") && !self.at_eof() {
                self.bump();
            }
            self.expect("}")?;
            return Ok(first);
        }
        self.expr()
    }

    /// Array dimensions; several `[a][b]` are flattened to `[a*b]`.
    fn dims(&mut self) -> PResult<Option<ArraySpec>> {
        let mut spec: Option<ArraySpec> = None;
        while self.at("[") && !self.at_n(1, "[") {
            self.bump();
            let d = if self.at("]") {
                ArraySpec::Eof
            } else if self.is_ident("while") && self.at_n(1, "(") {
                self.bump();
                self.expect("(")?;
                let c = self.expr()?;
                self.expect(")")?;
                ArraySpec::While(c)
            } else if self.is_ident("until") && self.at_n(1, "(") {
                self.bump();
                self.expect("(")?;
                let c = self.expr()?;
                self.expect(")")?;
                ArraySpec::Until(c)
            } else {
                ArraySpec::Fixed(self.expr()?)
            };
            self.expect("]")?;
            spec = Some(match (spec, d) {
                (Some(ArraySpec::Fixed(a)), ArraySpec::Fixed(b)) => {
                    ArraySpec::Fixed(Expr::binary(BinOp::Mul, a, b))
                }
                (_, d) => d,
            });
        }
        Ok(spec)
    }

    // ----- types -----------------------------------------------------------

    fn type_ref(&mut self) -> PResult<TypeRef> {
        let mut endian = None;
        loop {
            if self.is_ident("le") && matches!(self.tok_at(1), Tok::Ident(_)) {
                self.bump();
                endian = Some(Endian::Little);
            } else if self.is_ident("be") && matches!(self.tok_at(1), Tok::Ident(_)) {
                self.bump();
                endian = Some(Endian::Big);
            } else if (self.is_ident("const")
                || self.is_ident("volatile")
                || self.is_ident("register")
                || self.is_ident("ref")
                || self.is_ident("struct")
                || self.is_ident("union")
                || self.is_ident("enum"))
                && matches!(self.tok_at(1), Tok::Ident(_))
            {
                self.bump();
            } else {
                break;
            }
        }
        // C multi-word types.
        if let Tok::Ident(w) = self.tok()
            && C_TYPE_WORDS.contains(&w.as_str())
            && (self.bt() || self.c() || matches!(w.as_str(), "unsigned" | "signed"))
        {
            let mut words = Vec::new();
            while let Tok::Ident(w) = self.tok() {
                if !C_TYPE_WORDS.contains(&w.as_str()) {
                    break;
                }
                // `long` alone in .bt is 32-bit.
                words.push(w.clone());
                self.bump();
            }
            let long = if self.bt() { 4 } else { self.opts.c_long_size };
            let name = c_multiword(&words, long)
                .ok_or_else(|| self.err(format!("invalid type `{}`", words.join(" "))))?;
            let mut t = TypeRef::named(name);
            t.endian = endian;
            while self.is_ident("const") {
                self.bump();
            }
            return Ok(t);
        }
        if let Tok::Ident(s) = self.tok()
            && KEYWORDS.contains(&s.as_str())
        {
            return Err(self.err(format!("expected type, found keyword `{s}`")));
        }
        let name = self.path()?;
        let mut t = TypeRef::named(name);
        t.endian = endian;
        if self.at("<") && !self.bt() && !self.c() {
            self.bump();
            while !self.at(">") && !self.at(">>") && !self.at_eof() {
                t.args.push(self.type_arg()?);
                if !self.eat(",") {
                    break;
                }
            }
            self.expect(">")?;
        }
        while self.is_ident("const") {
            self.bump();
        }
        Ok(t)
    }

    /// Template argument: a type (`B<u8>`, `be u16`) or a value expression.
    fn type_arg(&mut self) -> PResult<Expr> {
        if matches!(self.tok(), Tok::Ident(_)) {
            let save = self.pos;
            let start = self.span();
            if let Ok(t) = self.type_ref()
                && (self.at(",") || self.at(">") || self.at(">>"))
            {
                if t.args.is_empty() && t.endian.is_none() {
                    return Ok(Expr::at(ExprKind::Ident(t.name), start));
                }
                return Ok(Expr::at(ExprKind::TypeArg(t), start.to(self.prev_span())));
            }
            self.pos = save;
        }
        self.binary(1, true)
    }

    // ----- attributes ------------------------------------------------------

    fn attrs_opt(&mut self) -> PResult<Vec<Attr>> {
        if self.at("[") && self.at_n(1, "[") {
            return self.attrs();
        }
        if self.bt() && self.at("<") {
            return self.bt_attrs();
        }
        Ok(vec![])
    }

    fn attrs(&mut self) -> PResult<Vec<Attr>> {
        let mut out = Vec::new();
        while self.at("[") && self.at_n(1, "[") {
            self.bump();
            self.bump();
            while !self.at("]") {
                let name = self.path()?;
                let mut args = Vec::new();
                if self.eat("(") {
                    while !self.at(")") {
                        args.push(self.expr()?);
                        if !self.eat(",") {
                            break;
                        }
                    }
                    self.expect(")")?;
                }
                out.push(Attr { name, args });
                if !self.eat(",") {
                    break;
                }
            }
            self.expect("]")?;
            self.expect("]")?;
        }
        Ok(out)
    }

    /// 010 Editor attribute list `<format=hex, comment="x", bgcolor=cRed>`.
    fn bt_attrs(&mut self) -> PResult<Vec<Attr>> {
        self.expect("<")?;
        let mut out = Vec::new();
        while !self.at(">") && !self.at_eof() {
            let key = self.ident()?;
            self.expect("=")?;
            let value = match self.tok().clone() {
                Tok::Str(s) => {
                    self.bump();
                    Expr::str(s)
                }
                Tok::Ident(_) => Expr::ident(self.path()?),
                _ => self.binary(5, true)?,
            };
            let ident = match &value.kind {
                ExprKind::Ident(s) => Some(s.clone()),
                _ => None,
            };
            match key.as_str() {
                "format" => out.push(Attr::string(
                    "format",
                    match ident.as_deref() {
                        Some("hex") => "hex",
                        Some("binary") => "bin",
                        Some("octal") => "oct",
                        _ => "dec",
                    },
                )),
                "comment" => out.push(match ident {
                    Some(f) => Attr::string("comment_fn", f),
                    None => Attr::new("comment", vec![value]),
                }),
                "name" => out.push(match ident {
                    Some(f) => Attr::string("name_fn", f),
                    None => Attr::new("name", vec![value]),
                }),
                "read" => {
                    if let Some(f) = ident {
                        out.push(Attr::string("format", f));
                    }
                }
                "hidden" => {
                    if !matches!(value.kind, ExprKind::Ident(ref s) if s == "false") {
                        out.push(Attr::new("hidden", vec![]));
                    }
                }
                "bgcolor" | "fgcolor" => {
                    if let Some(c) = bt_color(&value) {
                        out.push(Attr::string("color", c));
                    }
                }
                "size" if ident.is_none() => out.push(Attr::new("size", vec![value])),
                _ => {} // open, optimize, style, ... have no effect here
            }
            if !self.eat(",") {
                break;
            }
        }
        self.expect(">")?;
        Ok(out)
    }

    // ----- expressions -----------------------------------------------------

    pub(crate) fn expr(&mut self) -> PResult<Expr> {
        self.ternary(false)
    }

    fn ternary(&mut self, no_gt: bool) -> PResult<Expr> {
        let c = self.binary(1, no_gt)?;
        if self.eat("?") {
            let a = self.ternary(no_gt)?;
            self.expect(":")?;
            let b = self.ternary(no_gt)?;
            let span = c.span.to(b.span);
            return Ok(Expr::at(
                ExprKind::Ternary(Box::new(c), Box::new(a), Box::new(b)),
                span,
            ));
        }
        Ok(c)
    }

    fn binop(&self, no_gt: bool) -> Option<(u8, BinOp)> {
        let p = match self.tok() {
            Tok::Punct(p) => *p,
            Tok::Ident(s) if self.ksy() && s == "and" => "&&",
            Tok::Ident(s) if self.ksy() && s == "or" => "||",
            _ => return None,
        };
        Some(match p {
            "||" => (1, BinOp::Or),
            "^^" => (2, BinOp::Xor),
            "&&" => (3, BinOp::And),
            "|" => (4, BinOp::BitOr),
            "^" => (5, BinOp::BitXor),
            "&" => (6, BinOp::BitAnd),
            "==" => (7, BinOp::Eq),
            "!=" => (7, BinOp::Ne),
            "<" => (8, BinOp::Lt),
            "<=" => (8, BinOp::Le),
            ">" if !no_gt => (8, BinOp::Gt),
            ">=" if !no_gt => (8, BinOp::Ge),
            "<<" => (9, BinOp::Shl),
            ">>" if !no_gt => (9, BinOp::Shr),
            "+" => (10, BinOp::Add),
            "-" => (10, BinOp::Sub),
            "*" => (11, BinOp::Mul),
            "/" => (11, BinOp::Div),
            "%" => (11, BinOp::Rem),
            _ => return None,
        })
    }

    fn binary(&mut self, min: u8, no_gt: bool) -> PResult<Expr> {
        let mut lhs = self.unary()?;
        while let Some((prec, op)) = self.binop(no_gt) {
            if prec < min {
                break;
            }
            self.bump();
            let rhs = self.binary(prec + 1, no_gt)?;
            let span = lhs.span.to(rhs.span);
            lhs = Expr::at(ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)), span);
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> PResult<Expr> {
        let start = self.span();
        let op = match self.tok() {
            Tok::Punct("-") => Some(UnOp::Neg),
            Tok::Punct("+") => Some(UnOp::Plus),
            Tok::Punct("!") => Some(UnOp::Not),
            Tok::Punct("~") => Some(UnOp::BitNot),
            Tok::Ident(s) if self.ksy() && s == "not" => Some(UnOp::Not),
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let e = self.unary()?;
            let span = start.to(e.span);
            return Ok(Expr::at(ExprKind::Unary(op, Box::new(e)), span));
        }
        // C-style cast `(uint)x`.
        if self.at("(")
            && let Tok::Ident(n) = self.tok_at(1)
            && is_cast_type_name(n)
        {
            let save = self.pos;
            self.bump();
            if let Ok(ty) = self.type_ref()
                && self.eat(")")
                && !matches!(self.tok(), Tok::Punct(")" | ";" | "," | "]") | Tok::Eof)
            {
                let e = self.unary()?;
                let span = start.to(e.span);
                return Ok(Expr::at(ExprKind::Cast(ty, Box::new(e)), span));
            }
            self.pos = save;
        }
        self.postfix()
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            if self.at(".") {
                self.bump();
                let name = self.ident()?;
                let span = e.span.to(self.prev_span());
                if self.ksy() {
                    // `_io.pos` etc.
                    if matches!(&e.kind, ExprKind::Ident(s) if s == "_io") {
                        e = match name.as_str() {
                            "pos" => Expr::at(ExprKind::Dollar, span),
                            "size" => {
                                Expr::at(ExprKind::Call("std::mem::size".into(), vec![]), span)
                            }
                            "eof" => Expr::at(ExprKind::Call("std::mem::eof".into(), vec![]), span),
                            _ => return Err(self.err(format!("unsupported `_io.{name}`"))),
                        };
                        continue;
                    }
                }
                if self.at("(") {
                    // Method call: `x.substring(a, b)`.
                    let mut args = vec![e];
                    args.extend(self.call_args()?);
                    let span = span.to(self.prev_span());
                    e = Expr::at(ExprKind::Call(format!(".{name}"), args), span);
                } else if self.ksy() && self.at("<") && name == "as" {
                    // `.as<type>` casts are transparent here.
                    self.bump();
                    self.type_ref()?;
                    self.expect(">")?;
                } else {
                    e = Expr::at(ExprKind::Member(Box::new(e), name), span);
                }
            } else if self.at("[") && !self.at_n(1, "[") {
                self.bump();
                let i = self.expr()?;
                self.expect("]")?;
                let span = e.span.to(self.prev_span());
                e = Expr::at(ExprKind::Index(Box::new(e), Box::new(i)), span);
            } else if self.at("->") && (self.c() || self.bt()) {
                self.bump();
                let name = self.ident()?;
                let span = e.span.to(self.prev_span());
                e = Expr::at(ExprKind::Member(Box::new(e), name), span);
            } else {
                return Ok(e);
            }
        }
    }

    fn call_args(&mut self) -> PResult<Vec<Expr>> {
        self.expect("(")?;
        let mut args = Vec::new();
        while !self.at(")") {
            args.push(self.expr()?);
            if !self.eat(",") {
                break;
            }
        }
        self.expect(")")?;
        Ok(args)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let start = self.span();
        let at = |k: ExprKind, p: &Self| Ok(Expr::at(k, start.to(p.prev_span())));
        match self.tok().clone() {
            Tok::Int(v) | Tok::Char(v) => {
                self.bump();
                at(ExprKind::Int(v), self)
            }
            Tok::Float(v) => {
                self.bump();
                at(ExprKind::Float(v), self)
            }
            Tok::Str(s) => {
                self.bump();
                let mut s = s;
                while let Tok::Str(t) = self.tok().clone() {
                    self.bump();
                    s.push_str(&t);
                }
                at(ExprKind::Str(s), self)
            }
            Tok::Punct("$") => {
                self.bump();
                at(ExprKind::Dollar, self)
            }
            Tok::Punct("(") => {
                self.bump();
                let e = self.expr()?;
                self.expect(")")?;
                Ok(e)
            }
            Tok::Punct("[") if self.ksy() => {
                // Kaitai byte array literal `[0x50, 0x4b]` → string of bytes.
                self.bump();
                let mut s = String::new();
                while !self.at("]") {
                    match self.bump() {
                        Tok::Int(v) => s.push(char::from(v as u8)),
                        _ => return Err(self.err("expected byte value")),
                    }
                    if !self.eat(",") {
                        break;
                    }
                }
                self.expect("]")?;
                at(ExprKind::Str(s), self)
            }
            Tok::Ident(name) => {
                match name.as_str() {
                    "true" | "false" => {
                        self.bump();
                        return at(ExprKind::Bool(name == "true"), self);
                    }
                    "this" => {
                        self.bump();
                        return at(ExprKind::This, self);
                    }
                    "parent" => {
                        self.bump();
                        return at(ExprKind::Parent, self);
                    }
                    "_parent" if self.ksy() => {
                        self.bump();
                        return at(ExprKind::Parent, self);
                    }
                    "_root" if self.ksy() => {
                        self.bump();
                        return at(ExprKind::Root, self);
                    }
                    "null" | "NULL" if !self.ksy() => {
                        self.bump();
                        return at(ExprKind::Int(0), self);
                    }
                    "sizeof" => {
                        self.bump();
                        self.expect("(")?;
                        let arg = if matches!(self.tok(), Tok::Ident(n) if C_TYPE_WORDS.contains(&n.as_str()) || n == "le" || n == "be")
                        {
                            SizeOfArg::Type(self.type_ref()?)
                        } else {
                            SizeOfArg::Expr(Box::new(self.expr()?))
                        };
                        self.expect(")")?;
                        return at(ExprKind::SizeOf(arg), self);
                    }
                    "addressof" | "startof" => {
                        self.bump();
                        self.expect("(")?;
                        let e = self.expr()?;
                        self.expect(")")?;
                        return at(ExprKind::AddressOf(Box::new(e)), self);
                    }
                    _ => {}
                }
                let path = self.path()?;
                if self.at("(") {
                    let args = self.call_args()?;
                    return at(ExprKind::Call(path, args), self);
                }
                if self.ksy() && path.contains("::") {
                    return at(ExprKind::Ident(path), self);
                }
                at(ExprKind::Ident(path), self)
            }
            _ => Err(self.err(format!("expected expression, found {}", self.describe()))),
        }
    }
}

/// Converts a 010 colour (`cRed`, `0xBBGGRR`) to `rrggbb`.
fn bt_color(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Ident(n) => Some(
            match n.as_str() {
                "cBlack" => "000000",
                "cRed" => "ff0000",
                "cDkRed" => "800000",
                "cLtRed" => "ff8080",
                "cGreen" => "00ff00",
                "cDkGreen" => "008000",
                "cLtGreen" => "80ff80",
                "cBlue" => "0000ff",
                "cDkBlue" => "000080",
                "cLtBlue" => "8080ff",
                "cPurple" => "ff00ff",
                "cDkPurple" => "800080",
                "cLtPurple" => "ffe0ff",
                "cAqua" => "00ffff",
                "cDkAqua" => "008080",
                "cLtAqua" => "e0ffff",
                "cYellow" => "ffff00",
                "cDkYellow" => "808000",
                "cLtYellow" => "ffff80",
                "cDkGray" => "404040",
                "cGray" => "808080",
                "cSilver" => "c0c0c0",
                "cLtGray" => "e0e0e0",
                "cWhite" => "ffffff",
                _ => return None,
            }
            .to_string(),
        ),
        ExprKind::Int(v) => {
            let v = *v as u32;
            Some(format!(
                "{:02x}{:02x}{:02x}",
                v & 0xFF,
                (v >> 8) & 0xFF,
                (v >> 16) & 0xFF
            ))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(src: &str) -> Program {
        match parse(src) {
            Ok(p) => p,
            Err(Error::Parse(d)) => panic!("{d:#?}"),
            Err(e) => panic!("{e}"),
        }
    }

    fn ok_dialect(src: &str, dialect: Dialect) -> Program {
        let (p, d) = parse_with(
            src,
            &ParseOptions {
                dialect,
                ..ParseOptions::default()
            },
        );
        assert!(
            d.iter().all(|d| d.severity != crate::diag::Severity::Error),
            "{d:#?}"
        );
        p
    }

    #[test]
    fn struct_enum_bitfield_using() {
        let p = ok(r#"
            #pragma endian big
            enum Kind : u16 { A = 1, B, C = 0x10, R = 20 ... 30 };
            bitfield Flags { a : 3; padding : 2; Kind k : 3; } [[color("ff0000")]];
            using Word = be u32;
            struct Header<T, auto N> : Base {
                le u32 magic [[name("Magic"), format("hex")]];
                T values[N];
                Flags f;
                if (magic == 5) u8 x; else { u16 y; }
                padding[2];
                u32 *ptr : u16;
                u8 data[while($ < 10)] @ 0x20;
                char name[];
                auto computed = magic * 2;
            };
            Header<u8, 4> h @ 0;
        "#);
        assert_eq!(p.pragma("endian"), Some("big"));
        assert_eq!(p.type_defs().count(), 4);
        let Some(TypeDef::Struct(s)) = p.type_def("Header") else {
            panic!()
        };
        assert_eq!(s.params.len(), 2);
        assert!(s.inherits.is_some());
        assert_eq!(s.body.len(), 9);
        let Some(TypeDef::Enum(e)) = p.type_def("Kind") else {
            panic!()
        };
        assert_eq!(e.variants.len(), 4);
        assert!(e.variants[3].end.is_some());
        let last = p.items.last().unwrap();
        let StmtKind::Field(f) = &last.kind else {
            panic!()
        };
        assert_eq!(f.ty.args.len(), 2);
        assert!(f.placement.is_some());
    }

    #[test]
    fn statements_and_functions() {
        let p = ok(r#"
            fn sum(u32 a, u32 b) { u32 r = a + b; return r; };
            struct S {
                u8 count;
                for (u8 i = 0, i < count, i += 1) { u8 x; }
                while ($ < 4) { u8 y; }
                match (count, 1) {
                    (0 | 1, _): u8 a;
                    (2 ... 5, 1): { u16 b; }
                    (_, _): u32 c;
                }
                assert(count < 10, "too many");
                $ += 2;
                std::print("{}", count);
            };
        "#);
        assert_eq!(p.functions().count(), 1);
        let f = p.functions().next().unwrap();
        assert!(matches!(f.body[0].kind, StmtKind::Let(_)));
    }

    #[test]
    fn expressions_precedence() {
        let e = parse_expr(
            "1 + 2 * 3 == 7 && !false ? a.b[2] : sizeof(u32)",
            Dialect::Hexpat,
        )
        .unwrap();
        assert!(matches!(e.kind, ExprKind::Ternary(..)));
        let e = parse_expr("a | b ^ c & d", Dialect::Hexpat).unwrap();
        assert!(matches!(e.kind, ExprKind::Binary(BinOp::BitOr, ..)));
        let e = parse_expr("x and not y or _io.pos > 3", Dialect::Ksy).unwrap();
        assert!(matches!(e.kind, ExprKind::Binary(BinOp::Or, ..)));
        assert!(parse_expr("1 +", Dialect::Hexpat).is_err());
    }

    #[test]
    fn nested_template_args_split_shift() {
        let p = ok("struct A<T> { T x; }; struct B<T> { T y; }; A<B<u8>> v @ 0;");
        let StmtKind::Field(f) = &p.items.last().unwrap().kind else {
            panic!()
        };
        assert_eq!(f.ty.name, "A");
    }

    #[test]
    fn error_recovery_reports_multiple() {
        let (p, d) = parse_with(
            "struct A { u8 x u8 y; u16 z; }; struct B { u8 = ; }; u8 ok @ 0;",
            &ParseOptions::default(),
        );
        assert!(d.len() >= 2, "{d:#?}");
        assert!(p.type_def("A").is_some());
        assert!(
            p.items
                .iter()
                .any(|s| matches!(&s.kind, StmtKind::Field(f) if f.name == "ok"))
        );
    }

    #[test]
    fn bt_dialect() {
        let p = ok_dialect(
            r#"
            LittleEndian();
            typedef unsigned int UINT32;
            typedef struct {
                char magic[4] <comment="Magic", bgcolor=cRed>;
                uint size <format=hex>;
                local int i;
                for (i = 0; i < 2; i++) { ushort v; }
                if (size > 0) uchar data[size];
                int flags : 3;
                int rest : 29;
            } HEADER;
            typedef enum <uchar> { A = 1, B } KIND;
            int Twice(int v) { return v * 2; }
            HEADER hdr;
            FSeek(0x10);
            KIND k;
            "#,
            Dialect::Bt,
        );
        let Some(TypeDef::Struct(s)) = p.type_def("HEADER") else {
            panic!()
        };
        let StmtKind::Field(f) = &s.body[0].kind else {
            panic!()
        };
        assert_eq!(f.attrs.len(), 2);
        assert!(p.functions().any(|f| f.name == "Twice"));
        assert!(p.type_def("KIND").is_some());
        assert!(p.type_def("UINT32").is_some());
    }

    #[test]
    fn c_dialect() {
        let p = ok_dialect(
            r"
            #include <stdint.h>
            #pragma pack(push, 1)
            typedef struct tag_s {
                uint32_t a;
                unsigned long long b;
                char *name;
                struct { int x; } inner;
                int grid[2][3];
                unsigned f : 4;
            } S, *PS;
            #pragma pack(pop)
            struct T { short s; };
            enum Color { RED, GREEN = 5 };
            int proto(int a);
            ",
            Dialect::C,
        );
        let Some(TypeDef::Struct(s)) = p.type_def("S") else {
            panic!()
        };
        let c = crate::ast::find_attr(&s.attrs, "c_layout").unwrap();
        assert_eq!(c.args[0].kind, ExprKind::Int(1));
        assert!(p.type_def("tag_s").is_some());
        assert!(p.type_def("PS").is_some());
        let Some(TypeDef::Struct(t)) = p.type_def("T") else {
            panic!()
        };
        assert_eq!(
            crate::ast::find_attr(&t.attrs, "c_layout").unwrap().args[0].kind,
            ExprKind::Int(0)
        );
        assert!(p.type_defs().any(|t| t.name().starts_with("__anon_struct")));
    }

    #[test]
    fn switch_to_match() {
        let p = ok_dialect(
            "struct S { uchar t; switch (t) { case 1: case 2: ushort a; break; default: uint b; break; case 3: uchar c; } };",
            Dialect::Bt,
        );
        let Some(TypeDef::Struct(s)) = p.type_def("S") else {
            panic!()
        };
        let StmtKind::Match { arms, .. } = &s.body[1].kind else {
            panic!()
        };
        assert_eq!(arms.len(), 3);
        assert!(matches!(arms[2].patterns[0], Pattern::Wildcard));
        assert!(matches!(arms[0].patterns[0], Pattern::Or(_)));
    }

    #[test]
    fn namespaces_qualify_names() {
        let p = ok("namespace fmt { struct H { u8 a; }; fn f() { return 1; }; } fmt::H h @ 0;");
        assert!(p.type_def("fmt::H").is_some());
        assert!(p.functions().any(|f| f.name == "fmt::f"));
    }
}
