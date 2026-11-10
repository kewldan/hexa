//! Editor support for template sources (STRC-17): diagnostics, completions
//! and hover. Positions are 1-based lines and columns (characters).

use std::collections::HashMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::ast::{
    ArraySpec, Expr, ExprKind, FnDef, Pattern, Program, SizeOfArg, Span, Stmt, StmtKind, TypeDef,
    TypeRef,
};
use crate::builtins;
use crate::diag::Diagnostic;
use crate::parser::{Dialect, ParseOptions, parse_with};
use crate::prim::Prim;

/// Diagnostics for native template source (syntax and semantic checks).
pub fn diagnostics(source: &str) -> Vec<Diagnostic> {
    diagnostics_with(source, &ParseOptions::default())
}

/// Diagnostics with explicit parse options (dialect, include resolver).
pub fn diagnostics_with(source: &str, opts: &ParseOptions<'_>) -> Vec<Diagnostic> {
    let (program, mut diags) = parse_with(source, opts);
    if opts.dialect != Dialect::Ksy {
        Checker::new(&program).run(&mut diags);
    }
    diags.sort_by_key(|d| (d.line, d.col));
    diags.dedup();
    diags
}

struct Checker<'a> {
    program: &'a Program,
    types: HashMap<&'a str, usize>,
    short: HashMap<&'a str, &'a str>,
    fns: HashMap<&'a str, &'a FnDef>,
}

impl<'a> Checker<'a> {
    fn new(program: &'a Program) -> Self {
        let mut types = HashMap::new();
        let mut short = HashMap::new();
        for t in program.type_defs() {
            *types.entry(t.name()).or_insert(0) += 1;
            if let Some((_, s)) = t.name().rsplit_once("::") {
                short.entry(s).or_insert(t.name());
            }
        }
        let mut fns = HashMap::new();
        for f in program.functions() {
            fns.insert(f.name.as_str(), f);
            if let Some((_, s)) = f.name.rsplit_once("::") {
                fns.entry(s).or_insert(f);
            }
        }
        Self {
            program,
            types,
            short,
            fns,
        }
    }

    fn known_type(&self, name: &str, params: &[String]) -> bool {
        params.iter().any(|p| p == name)
            || name == "auto"
            || Prim::from_name(name).is_some()
            || self.types.contains_key(name)
            || self.short.contains_key(name)
            || name
                .rsplit("::")
                .next()
                .is_some_and(|s| self.short.contains_key(s) || self.types.contains_key(s))
            || name.starts_with("type::")
            || name.starts_with("std::")
    }

    fn run(&self, diags: &mut Vec<Diagnostic>) {
        for (name, n) in &self.types {
            if *n > 1 {
                let span = self
                    .program
                    .items
                    .iter()
                    .rev()
                    .find(|s| matches!(&s.kind, StmtKind::TypeDef(t) if t.name() == *name))
                    .map_or_else(Span::default, |s| s.span);
                diags.push(Diagnostic::warning(
                    span,
                    format!("type `{name}` is defined {n} times; the first definition is used"),
                ));
            }
        }
        for s in &self.program.items {
            self.stmt(s, &[], diags);
        }
    }

    fn ty(&self, t: &TypeRef, span: Span, params: &[String], diags: &mut Vec<Diagnostic>) {
        if !self.known_type(&t.name, params) {
            diags.push(Diagnostic::error(
                span,
                format!("unknown type `{}`", t.name),
            ));
        }
        for a in &t.args {
            if let ExprKind::TypeArg(inner) = &a.kind {
                self.ty(inner, span, params, diags);
            }
        }
    }

    fn stmts(&self, stmts: &[Stmt], params: &[String], diags: &mut Vec<Diagnostic>) {
        for s in stmts {
            self.stmt(s, params, diags);
        }
    }

    fn stmt(&self, s: &Stmt, params: &[String], diags: &mut Vec<Diagnostic>) {
        let span = s.span;
        match &s.kind {
            StmtKind::Field(f) => {
                if f.ty.name != "padding" {
                    self.ty(&f.ty, span, params, diags);
                }
                if let Some(p) = &f.pointer {
                    self.ty(p, span, params, diags);
                }
                if let Some(ArraySpec::Fixed(e) | ArraySpec::While(e) | ArraySpec::Until(e)) =
                    &f.array
                {
                    self.expr(e, diags);
                }
                for e in f.placement.iter().chain(&f.bits) {
                    self.expr(e, diags);
                }
                for a in &f.attrs {
                    a.args.iter().for_each(|e| self.expr(e, diags));
                }
            }
            StmtKind::TypeDef(TypeDef::Struct(st)) => {
                let ps: Vec<String> = st.params.iter().map(|p| p.name.clone()).collect();
                if let Some(i) = &st.inherits {
                    self.ty(i, span, &ps, diags);
                }
                self.stmts(&st.body, &ps, diags);
            }
            StmtKind::TypeDef(TypeDef::Enum(e)) => {
                self.ty(&e.base, span, params, diags);
            }
            StmtKind::TypeDef(TypeDef::Alias(a)) => self.ty(&a.target, span, params, diags),
            StmtKind::TypeDef(TypeDef::Bitfield(b)) => {
                for m in &b.members {
                    if let Some(t) = &m.ty {
                        self.ty(t, span, params, diags);
                    }
                }
            }
            StmtKind::Fn(f) => {
                for p in &f.params {
                    if let Some(t) = &p.ty {
                        self.ty(t, f.span, params, diags);
                    }
                }
                self.stmts(&f.body, params, diags);
            }
            StmtKind::Let(l) => {
                if let Some(t) = &l.ty {
                    self.ty(t, span, params, diags);
                }
                for e in l.init.iter().chain(&l.array) {
                    self.expr(e, diags);
                }
            }
            StmtKind::Assign { target, value, .. } => {
                self.expr(target, diags);
                self.expr(value, diags);
            }
            StmtKind::Expr(e) => self.expr(e, diags),
            StmtKind::If { cond, then, els } => {
                self.expr(cond, diags);
                self.stmts(then, params, diags);
                self.stmts(els, params, diags);
            }
            StmtKind::Match { scrutinee, arms } => {
                for e in scrutinee {
                    self.expr(e, diags);
                }
                for arm in arms {
                    if arm.patterns.len() != scrutinee.len() {
                        diags.push(Diagnostic::error(
                            span,
                            format!(
                                "match arm has {} pattern(s) but {} value(s) are matched",
                                arm.patterns.len(),
                                scrutinee.len()
                            ),
                        ));
                    }
                    for p in &arm.patterns {
                        self.pattern(p, diags);
                    }
                    self.stmts(&arm.body, params, diags);
                }
            }
            StmtKind::While { cond, body } | StmtKind::DoWhile { body, cond } => {
                self.expr(cond, diags);
                self.stmts(body, params, diags);
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(i) = init {
                    self.stmt(i, params, diags);
                }
                if let Some(c) = cond {
                    self.expr(c, diags);
                }
                if let Some(st) = step {
                    self.stmt(st, params, diags);
                }
                self.stmts(body, params, diags);
            }
            StmtKind::Return(e) => {
                if let Some(e) = e {
                    self.expr(e, diags);
                }
            }
            StmtKind::Assert { cond, msg, .. } => {
                self.expr(cond, diags);
                if let Some(m) = msg {
                    self.expr(m, diags);
                }
            }
            StmtKind::Block(b) => self.stmts(b, params, diags),
            StmtKind::Break | StmtKind::Continue => {}
        }
    }

    fn pattern(&self, p: &Pattern, diags: &mut Vec<Diagnostic>) {
        match p {
            Pattern::Wildcard => {}
            Pattern::Value(e) => self.expr(e, diags),
            Pattern::Range(a, b) => {
                self.expr(a, diags);
                self.expr(b, diags);
            }
            Pattern::Or(ps) => ps.iter().for_each(|p| self.pattern(p, diags)),
        }
    }

    fn expr(&self, e: &Expr, diags: &mut Vec<Diagnostic>) {
        match &e.kind {
            ExprKind::Call(name, args) => {
                let known = name.starts_with('.')
                    || self.fns.contains_key(name.as_str())
                    || name
                        .rsplit("::")
                        .next()
                        .is_some_and(|s| self.fns.contains_key(s))
                    || builtins::is_function(name)
                    || Prim::from_name(name).is_some()
                    || self.types.contains_key(name.as_str());
                if !known {
                    diags.push(Diagnostic::warning(
                        e.span,
                        format!("unknown function `{name}`"),
                    ));
                } else if let Some(f) = self.fns.get(name.as_str()) {
                    let variadic = f
                        .params
                        .iter()
                        .any(|p| p.ty.as_ref().is_some_and(|t| t.name == "auto"));
                    if args.len() > f.params.len() && !variadic {
                        diags.push(Diagnostic::warning(
                            e.span,
                            format!(
                                "`{name}` takes {} argument(s), {} given",
                                f.params.len(),
                                args.len()
                            ),
                        ));
                    }
                }
                for a in args {
                    self.expr(a, diags);
                }
            }
            ExprKind::Unary(_, x)
            | ExprKind::Member(x, _)
            | ExprKind::AddressOf(x)
            | ExprKind::Cast(_, x) => {
                self.expr(x, diags);
            }
            ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) => {
                self.expr(a, diags);
                self.expr(b, diags);
            }
            ExprKind::Ternary(a, b, c) => {
                self.expr(a, diags);
                self.expr(b, diags);
                self.expr(c, diags);
            }
            ExprKind::SizeOf(SizeOfArg::Expr(x)) => self.expr(x, diags),
            _ => {}
        }
    }
}

// ----- completions ---------------------------------------------------------------

/// Kind of a completion item (maps to LSP `CompletionItemKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompletionKind {
    Keyword,
    Type,
    Struct,
    Enum,
    EnumMember,
    Function,
    Field,
    Variable,
    Attribute,
}

/// A completion proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Completion {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: Option<String>,
}

fn line_prefix(source: &str, line: u32, col: u32) -> String {
    let l = source
        .lines()
        .nth(line.saturating_sub(1) as usize)
        .unwrap_or("");
    l.chars().take(col.saturating_sub(1) as usize).collect()
}

/// Identifier (with `::` and `.` separators) ending at the cursor.
fn trailing_word(prefix: &str) -> &str {
    let start = prefix
        .char_indices()
        .rev()
        .find(|(_, c)| !(c.is_alphanumeric() || *c == '_' || *c == ':' || *c == '.' || *c == '$'))
        .map_or(0, |(i, c)| i + c.len_utf8());
    &prefix[start..]
}

/// Completions at a 1-based position (STRC-17).
pub fn completions(source: &str, line: u32, col: u32) -> Vec<Completion> {
    let (program, _) = parse_with(source, &ParseOptions::default());
    let prefix = line_prefix(source, line, col);
    let word = trailing_word(&prefix);
    let mut out = Vec::new();
    let push = |out: &mut Vec<Completion>, label: &str, kind, detail: Option<String>| {
        out.push(Completion {
            label: label.to_string(),
            kind,
            detail,
        });
    };

    // Attributes inside `[[ ... ]]`.
    let in_attr = prefix
        .rfind("[[")
        .is_some_and(|i| !prefix[i..].contains("]]"));
    if in_attr {
        let w = word.rsplit(',').next().unwrap_or(word).trim();
        for (a, d) in builtins::ATTRIBUTES {
            if a.starts_with(w) {
                push(
                    &mut out,
                    a,
                    CompletionKind::Attribute,
                    Some((*d).to_string()),
                );
            }
        }
        return out;
    }

    // Member access `a.b.` → fields of the type of `a.b`.
    if let Some((base, partial)) = word.rsplit_once('.') {
        if let Some(ty) = resolve_path_type(&program, base, line) {
            for (name, detail) in struct_members(&program, &ty) {
                if name.starts_with(partial) {
                    push(&mut out, &name, CompletionKind::Field, Some(detail));
                }
            }
        }
        return out;
    }

    // `Enum::` / `std::mem::` paths.
    if let Some((base, partial)) = word.rsplit_once("::") {
        if let Some(TypeDef::Enum(e)) = program.type_def(base) {
            for v in &e.variants {
                if v.name.starts_with(partial) {
                    push(
                        &mut out,
                        &v.name,
                        CompletionKind::EnumMember,
                        Some(format!("{}::{}", e.name, v.name)),
                    );
                }
            }
        }
        let full = format!("{base}::");
        for (n, sig, _) in builtins::FUNCTIONS {
            if let Some(rest) = n.strip_prefix(&full)
                && rest.starts_with(partial)
            {
                let label = rest.split("::").next().unwrap_or(rest);
                if !out.iter().any(|c| c.label == label) {
                    let kind = if rest.contains("::") {
                        CompletionKind::Keyword
                    } else {
                        CompletionKind::Function
                    };
                    push(&mut out, label, kind, Some((*sig).to_string()));
                }
            }
        }
        for t in program.type_defs() {
            if let Some(rest) = t.name().strip_prefix(&full)
                && rest.starts_with(partial)
            {
                push(&mut out, rest, kind_of(t), None);
            }
        }
        return out;
    }

    for (k, d) in builtins::KEYWORDS {
        if k.starts_with(word) {
            push(&mut out, k, CompletionKind::Keyword, Some((*d).to_string()));
        }
    }
    for (t, d) in builtins::TYPES {
        if t.starts_with(word) {
            push(&mut out, t, CompletionKind::Type, Some((*d).to_string()));
        }
    }
    for t in program.type_defs() {
        if t.name().starts_with(word) && !t.name().starts_with("__anon") {
            push(&mut out, t.name(), kind_of(t), Some(type_summary(t)));
        }
    }
    for f in program.functions() {
        if f.name.starts_with(word) {
            push(
                &mut out,
                &f.name,
                CompletionKind::Function,
                Some(fn_signature(f)),
            );
        }
    }
    for (n, sig, _) in builtins::FUNCTIONS {
        if n.starts_with(word) && (!n.contains("::") || word.starts_with("std")) {
            push(
                &mut out,
                n,
                CompletionKind::Function,
                Some((*sig).to_string()),
            );
        }
    }
    if "std".starts_with(word) {
        push(
            &mut out,
            "std",
            CompletionKind::Keyword,
            Some("standard library".into()),
        );
    }
    // Fields and locals visible at the cursor.
    for (name, detail) in visible_names(&program, line) {
        if name.starts_with(word) && !out.iter().any(|c| c.label == name) {
            push(&mut out, &name, CompletionKind::Variable, Some(detail));
        }
    }
    out
}

fn kind_of(t: &TypeDef) -> CompletionKind {
    match t {
        TypeDef::Enum(_) => CompletionKind::Enum,
        TypeDef::Alias(_) => CompletionKind::Type,
        TypeDef::Struct(_) | TypeDef::Bitfield(_) => CompletionKind::Struct,
    }
}

fn fn_signature(f: &FnDef) -> String {
    let params: Vec<String> = f
        .params
        .iter()
        .map(|p| match &p.ty {
            Some(t) => format!("{} {}", t.name, p.name),
            None => p.name.clone(),
        })
        .collect();
    format!("fn {}({})", f.name, params.join(", "))
}

fn type_summary(t: &TypeDef) -> String {
    match t {
        TypeDef::Struct(s) => {
            let fields = s
                .body
                .iter()
                .filter(|st| matches!(st.kind, StmtKind::Field(_)))
                .count();
            let kw = if s.kind == crate::ast::StructKind::Union {
                "union"
            } else {
                "struct"
            };
            format!("{kw} {} ({fields} field(s))", s.name)
        }
        TypeDef::Enum(e) => format!(
            "enum {} : {} ({} variant(s))",
            e.name,
            e.base.name,
            e.variants.len()
        ),
        TypeDef::Bitfield(b) => format!("bitfield {} ({} member(s))", b.name, b.members.len()),
        TypeDef::Alias(a) => format!("using {} = {}", a.name, a.target.name),
    }
}

/// Fields of a struct / bitfield type, following aliases.
fn struct_members(program: &Program, ty: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut seen = 0;
    let mut name = ty.to_string();
    while seen < 16 {
        seen += 1;
        match program.type_def(&name) {
            Some(TypeDef::Alias(a)) => name.clone_from(&a.target.name),
            Some(TypeDef::Struct(s)) => {
                if let Some(base) = &s.inherits {
                    out.extend(struct_members(program, &base.name));
                }
                collect_fields(&s.body, &mut out);
                return out;
            }
            Some(TypeDef::Bitfield(b)) => {
                for m in &b.members {
                    if let Some(n) = &m.name {
                        out.push((n.clone(), "bits".to_string()));
                    }
                }
                return out;
            }
            _ => return out,
        }
    }
    out
}

fn collect_fields(stmts: &[Stmt], out: &mut Vec<(String, String)>) {
    for s in stmts {
        match &s.kind {
            StmtKind::Field(f) if f.ty.name != "padding" => {
                if !out.iter().any(|(n, _)| *n == f.name) {
                    out.push((f.name.clone(), f.ty.name.clone()));
                }
            }
            StmtKind::Let(l) if l.visible => out.push((l.name.clone(), "computed".into())),
            StmtKind::If { then, els, .. } => {
                collect_fields(then, out);
                collect_fields(els, out);
            }
            StmtKind::Match { arms, .. } => arms.iter().for_each(|a| collect_fields(&a.body, out)),
            StmtKind::Block(b)
            | StmtKind::While { body: b, .. }
            | StmtKind::For { body: b, .. } => {
                collect_fields(b, out);
            }
            _ => {}
        }
    }
}

/// Declared type of field `name` inside `stmts` (searching nested blocks).
fn field_type(stmts: &[Stmt], name: &str) -> Option<String> {
    for s in stmts {
        match &s.kind {
            StmtKind::Field(f) if f.name == name => {
                return Some(f.ty.name.clone());
            }
            StmtKind::Let(l) if l.name == name => return l.ty.as_ref().map(|t| t.name.clone()),
            StmtKind::If { then, els, .. } => {
                if let Some(t) = field_type(then, name).or_else(|| field_type(els, name)) {
                    return Some(t);
                }
            }
            StmtKind::Match { arms, .. } => {
                if let Some(t) = arms.iter().find_map(|a| field_type(&a.body, name)) {
                    return Some(t);
                }
            }
            StmtKind::Block(b)
            | StmtKind::While { body: b, .. }
            | StmtKind::For { body: b, .. } => {
                if let Some(t) = field_type(b, name) {
                    return Some(t);
                }
            }
            _ => {}
        }
    }
    None
}

/// Innermost struct whose definition covers `line`.
fn struct_at(program: &Program, line: u32) -> Option<&crate::ast::StructDef> {
    program.items.iter().find_map(|s| match &s.kind {
        StmtKind::TypeDef(TypeDef::Struct(st))
            if s.span.line <= line && line <= s.span.end_line =>
        {
            Some(st)
        }
        _ => None,
    })
}

/// Type of an access path like `hdr.items` as seen from `line`.
fn resolve_path_type(program: &Program, path: &str, line: u32) -> Option<String> {
    let mut parts = path.split('.');
    let first = parts.next()?;
    let first = first.split('[').next().unwrap_or(first);
    let mut ty = match first {
        "this" => struct_at(program, line).map(|s| s.name.clone()),
        _ => struct_at(program, line)
            .and_then(|s| field_type(&s.body, first))
            .or_else(|| field_type(&program.items, first)),
    }?;
    for p in parts {
        let p = p.split('[').next().unwrap_or(p);
        let body = match program.type_def(&ty)? {
            TypeDef::Struct(s) => &s.body,
            _ => return None,
        };
        ty = field_type(body, p)?;
    }
    Some(ty)
}

fn visible_names(program: &Program, line: u32) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(s) = struct_at(program, line) {
        collect_fields(&s.body, &mut out);
        for p in &s.params {
            out.push((p.name.clone(), "parameter".into()));
        }
    }
    collect_fields(&program.items, &mut out);
    for s in &program.items {
        if let StmtKind::Let(l) = &s.kind {
            out.push((l.name.clone(), "global".into()));
        }
    }
    out
}

// ----- hover -------------------------------------------------------------------

/// Hover information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hover {
    /// Markdown text.
    pub contents: String,
    /// Range of the hovered word.
    pub line: u32,
    pub col: u32,
    pub end_col: u32,
}

/// Hover text for the word at a 1-based position (STRC-17).
pub fn hover(source: &str, line: u32, col: u32) -> Option<Hover> {
    let text = source.lines().nth(line.checked_sub(1)? as usize)?;
    let chars: Vec<char> = text.chars().collect();
    let idx = (col.checked_sub(1)? as usize).min(chars.len());
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == ':';
    let mut start = idx;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = idx;
    while end < chars.len() && is_word(chars[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    let word: String = chars[start..end]
        .iter()
        .collect::<String>()
        .trim_matches(':')
        .to_string();
    let (program, _) = parse_with(source, &ParseOptions::default());
    let contents = hover_text(&program, &word, line)?;
    Some(Hover {
        contents,
        line,
        col: start as u32 + 1,
        end_col: end as u32 + 1,
    })
}

fn hover_text(program: &Program, word: &str, line: u32) -> Option<String> {
    if let Some(t) = program.type_def(word) {
        let mut s = format!("```\n{}\n```", type_summary(t));
        if let TypeDef::Enum(e) = t {
            for v in e.variants.iter().take(32) {
                let _ = write!(s, "\n- `{}`", v.name);
            }
        }
        if let TypeDef::Struct(st) = t {
            let fields = struct_members(program, &st.name);
            for (n, ty) in fields.iter().take(32) {
                let _ = write!(s, "\n- `{ty} {n}`");
            }
        }
        return Some(s);
    }
    if let Some((ty, var)) = word.rsplit_once("::")
        && let Some(TypeDef::Enum(e)) = program.type_def(ty)
    {
        let v = e.variants.iter().find(|v| v.name == var)?;
        let value = v
            .value
            .as_ref()
            .map_or_else(|| "(implicit)".to_string(), expr_text);
        return Some(format!("```\n{}::{} = {value}\n```", e.name, v.name));
    }
    if let Some(f) = program.functions().find(|f| f.name == word) {
        return Some(format!("```\n{}\n```", fn_signature(f)));
    }
    if let Some((_, sig, doc)) = builtins::function(word) {
        return Some(format!("```\n{sig}\n```\n{doc}"));
    }
    if let Some((t, d)) = builtins::TYPES.iter().find(|(t, _)| *t == word) {
        return Some(format!("`{t}` — {d}"));
    }
    if let Some(p) = Prim::from_name(word) {
        return Some(format!(
            "`{word}` — alias of `{}` ({} bytes)",
            p.name(),
            p.size().map_or("variable".into(), |s| s.to_string())
        ));
    }
    if let Some((k, d)) = builtins::KEYWORDS.iter().find(|(k, _)| *k == word) {
        return Some(format!("`{k}` — {d}"));
    }
    if let Some((a, d)) = builtins::ATTRIBUTES.iter().find(|(a, _)| *a == word) {
        return Some(format!("attribute `{a}` — {d}"));
    }
    let ty = struct_at(program, line)
        .and_then(|s| field_type(&s.body, word))
        .or_else(|| field_type(&program.items, word))?;
    Some(format!("```\n{ty} {word}\n```"))
}

fn expr_text(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Int(v) => format!("{v} (0x{v:X})"),
        ExprKind::Ident(n) => n.clone(),
        _ => "expression".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Severity;

    const SRC: &str = "enum Kind : u8 { A = 1, B };\nstruct Inner { u8 x; u16 y; };\nstruct Outer {\n    Kind k;\n    Inner in;\n    \n};\nOuter o @ 0;\n";

    #[test]
    fn semantic_diagnostics() {
        let d = diagnostics(
            "struct S { Missing m; u8 a; }; fn f(u8 x) { return x; }; auto v = nofn(1) + f(1, 2);",
        );
        assert!(
            d.iter()
                .any(|d| d.message.contains("unknown type `Missing`")
                    && d.severity == Severity::Error)
        );
        assert!(
            d.iter()
                .any(|d| d.message.contains("unknown function `nofn`"))
        );
        assert!(d.iter().any(|d| d.message.contains("takes 1 argument")));
        let d = diagnostics("struct A { u8 x; }; struct A { u8 y; };");
        assert!(d.iter().any(|d| d.message.contains("defined 2 times")));
        assert!(diagnostics(SRC).is_empty(), "{:?}", diagnostics(SRC));
        let d = diagnostics("struct S { u8 a }");
        assert_eq!(d[0].line, 1);
        assert!(d[0].message.contains("expected `;`"));
    }

    #[test]
    fn completion_contexts() {
        // Inside Outer (line 6): members of `in`.
        let src = SRC.replace("    \n", "    auto z = in.\n");
        let c = completions(&src, 6, 20);
        let labels: Vec<_> = c.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["x", "y"]);
        let src = SRC.replace("    \n", "    auto z = Kind::\n");
        let c = completions(&src, 6, 23);
        assert_eq!(
            c.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(),
            ["A", "B"]
        );
        let c = completions(SRC, 6, 5);
        assert!(
            c.iter()
                .any(|c| c.label == "struct" && c.kind == CompletionKind::Keyword)
        );
        assert!(
            c.iter()
                .any(|c| c.label == "Inner" && c.kind == CompletionKind::Struct)
        );
        assert!(c.iter().any(|c| c.label == "u32"));
        assert!(
            c.iter()
                .any(|c| c.label == "k" && c.kind == CompletionKind::Variable)
        );
        let src = SRC.replace("    \n", "    std::mem::\n");
        let c = completions(&src, 6, 15);
        assert!(c.iter().any(|c| c.label == "read_unsigned"));
        let src = SRC.replace("    \n", "    u8 v [[co\n");
        let c = completions(&src, 6, 14);
        assert!(
            c.iter()
                .any(|c| c.label == "color" && c.kind == CompletionKind::Attribute)
        );
        assert!(c.iter().any(|c| c.label == "comment"));
    }

    #[test]
    fn hover_info() {
        let h = hover(SRC, 4, 6).unwrap();
        assert!(h.contents.contains("enum Kind : u8"), "{}", h.contents);
        assert_eq!((h.col, h.end_col), (5, 9));
        let h = hover(SRC, 2, 18).unwrap();
        assert!(h.contents.contains("u8"));
        let h = hover(SRC, 5, 11).unwrap();
        assert!(h.contents.contains("Inner in"), "{}", h.contents);
        let h = hover("auto x = std::mem::size();", 1, 16).unwrap();
        assert!(h.contents.contains("Size of the data"));
        assert!(hover(SRC, 6, 2).is_none());
        let h = hover(SRC, 3, 2).unwrap();
        assert!(h.contents.contains("Declares a structure"));
    }
}
