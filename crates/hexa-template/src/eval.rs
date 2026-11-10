//! Template evaluator (STRC-01/02, STRC-08, STRC-10, STRC-11).
//!
//! Evaluation is deterministic and sandboxed: templates only see the data
//! through a read cache over an immutable [`Tree`] snapshot, and every run is
//! bounded by node, depth, step and time limits plus a [`Cancel`] token.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hexa_core::{Cancel, Tree};
use serde::{Deserialize, Serialize};

use crate::ast::{
    AliasDef, ArraySpec, Attr, BinOp, BitfieldDef, Endian, EnumDef, Expr, ExprKind, FieldDecl,
    FnDef, LetDecl, Pattern, Program, SizeOfArg, Span, Stmt, StmtKind, StructDef, StructKind,
    TypeDef, TypeRef, UnOp, find_attr,
};
use crate::diag::{Error, Result};
use crate::prim::{Prim, decode_uint, f16_to_f64, sign_extend};
use crate::result::{
    ChecksumSpec, Color, EnumInfo, EvalMessage, LazyArray, LazyElems, Node, NodeId, NodeKind,
    TemplateResult, Value, enum_display,
};

/// Sandbox limits (STRC-10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub max_nodes: usize,
    pub max_depth: u32,
    pub max_steps: u64,
    /// Struct array elements kept as nodes; later ones are stored as
    /// offsets and expanded on demand.
    pub eager_elements: usize,
    /// Longest NUL-terminated string scanned.
    pub max_string: usize,
    pub time_limit_ms: Option<u64>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_nodes: 2_000_000,
            max_depth: 96,
            max_steps: 50_000_000,
            eager_elements: 10_000,
            max_string: 1 << 20,
            time_limit_ms: None,
        }
    }
}

/// Where and how to apply a template (STRC-08).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalOptions {
    /// Initial value of `$`.
    pub base_offset: u64,
    /// Restricts the data to a selection (`start..end`); `None` = whole file.
    pub range: Option<std::ops::Range<u64>>,
    /// Instead of running top-level statements, read this type at
    /// `base_offset` (apply a single structure, e.g. from a C header).
    pub root_type: Option<String>,
    pub root_name: Option<String>,
    /// Default byte order (overrides `#pragma endian`).
    pub endian: Option<Endian>,
    pub limits: Limits,
}

/// Evaluates `program` over `tree`. Evaluation errors are reported inside
/// the result (with the partial tree); only cancellation fails the call.
pub fn evaluate(
    program: &Program,
    tree: &Tree,
    opts: &EvalOptions,
    cancel: &Cancel,
) -> Result<TemplateResult> {
    run(program, tree, opts, cancel, None).map(|(r, _)| r)
}

/// Applies several templates to the same data (STRC-08).
pub fn evaluate_many(
    templates: &[(&Program, EvalOptions)],
    tree: &Tree,
    cancel: &Cancel,
) -> Result<Vec<TemplateResult>> {
    templates
        .iter()
        .map(|(p, o)| evaluate(p, tree, o, cancel))
        .collect()
}

/// Debugger hook: called before every statement; returning `true` pauses.
pub(crate) type Hook<'h> = &'h mut dyn FnMut(&Eval<'_, '_>, Span) -> bool;

/// Snapshot of evaluator state at a pause point (STRC-18).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugState {
    pub line: u32,
    pub col: u32,
    pub offset: u64,
    pub step: u64,
    pub call_stack: Vec<StackFrame>,
    pub locals: Vec<Variable>,
    /// Fields read so far in the innermost structure.
    pub fields: Vec<Variable>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackFrame {
    pub name: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub name: String,
    pub value: String,
}

pub(crate) fn run(
    program: &Program,
    tree: &Tree,
    opts: &EvalOptions,
    cancel: &Cancel,
    hook: Option<Hook<'_>>,
) -> Result<(TemplateResult, Option<DebugState>)> {
    let mut ev = Eval::new(program, tree, opts, cancel, hook);
    let outcome = ev.run_top(opts);
    let mut paused = None;
    if let Err(f) = outcome {
        match f.kind {
            FailKind::Cancelled => return Err(Error::Cancelled),
            FailKind::Pause => paused = ev.pause_state.take(),
            FailKind::Exit => {}
            FailKind::Limit => {
                ev.truncated = true;
                ev.record(&f, false);
            }
            FailKind::Error => ev.record(&f, false),
        }
    }
    Ok((ev.finish(program), paused))
}

// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailKind {
    Error,
    Cancelled,
    Limit,
    Pause,
    Exit,
}

#[derive(Debug)]
pub(crate) struct Fail {
    pub msg: String,
    pub span: Span,
    pub kind: FailKind,
    pub node: Option<NodeId>,
    pub offset: Option<u64>,
}

type R<T> = std::result::Result<T, Box<Fail>>;

fn fail<T>(span: Span, msg: impl Into<String>) -> R<T> {
    Err(Box::new(Fail {
        msg: msg.into(),
        span,
        kind: FailKind::Error,
        node: None,
        offset: None,
    }))
}

fn fail_kind<T>(span: Span, kind: FailKind, msg: impl Into<String>) -> R<T> {
    Err(Box::new(Fail {
        msg: msg.into(),
        span,
        kind,
        node: None,
        offset: None,
    }))
}

/// Runtime value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RVal {
    Void,
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(String),
    Node(NodeId),
    /// Element `i` of lazy array node.
    Elem(NodeId, u64),
    Array(Vec<RVal>),
    Type(TypeRef),
}

enum Flow {
    Normal,
    Break,
    Continue,
    Return(RVal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Global,
    Struct,
    Func,
}

struct Frame {
    kind: FrameKind,
    scopes: Vec<HashMap<String, RVal>>,
}

struct BitUnit {
    offset: u64,
    size: u64,
    used: u32,
}

struct SCtx {
    node: NodeId,
    start: u64,
    union: bool,
    max_end: u64,
    fields: HashMap<String, NodeId>,
    bits: Option<BitUnit>,
    /// End (in bits from `start`) of the last C bit-field.
    c_bit_end: Option<u64>,
    /// `Some(pack)` for C layout (0 = natural alignment).
    c_layout: Option<u64>,
    max_align: u64,
}

struct Data<'t> {
    tree: &'t Tree,
    len: u64,
    cstart: u64,
    cache: Vec<u8>,
}

const CHUNK: u64 = 64 * 1024;

impl Data<'_> {
    fn read(&mut self, off: u64, buf: &mut [u8]) -> std::io::Result<()> {
        let n = buf.len() as u64;
        let end = off
            .checked_add(n)
            .filter(|&e| e <= self.len)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "read past end")
            })?;
        if n > CHUNK {
            return self.tree.read(off, buf);
        }
        let cend = self.cstart + self.cache.len() as u64;
        if off < self.cstart || end > cend {
            let start = off & !0xFFF;
            self.cache = self.tree.read_vec(start, (CHUNK + 0x1000) as usize)?;
            self.cstart = start;
        }
        let s = (off - self.cstart) as usize;
        buf.copy_from_slice(&self.cache[s..s + buf.len()]);
        Ok(())
    }
}

enum Resolved<'p> {
    Prim(Prim),
    Struct(&'p StructDef),
    Enum(&'p EnumDef),
    Bitfield(&'p BitfieldDef),
    Alias(&'p AliasDef),
}

/// The evaluator. Exposed inside the crate for the debugger.
pub(crate) struct Eval<'p, 'h> {
    prog: &'p Program,
    types: HashMap<&'p str, &'p TypeDef>,
    short_types: HashMap<&'p str, &'p str>,
    fns: HashMap<&'p str, &'p FnDef>,
    data: Data<'p>,
    io_start: u64,
    io_end: u64,
    pub(crate) cursor: u64,
    endian: Endian,
    nodes: Vec<Node>,
    roots: Vec<NodeId>,
    lazy: HashMap<NodeId, LazyArray>,
    enums: HashMap<String, (EnumInfo, bool)>,
    enum_consts: Option<HashMap<String, i128>>,
    frames: Vec<Frame>,
    structs: Vec<SCtx>,
    limits: Limits,
    pub(crate) steps: u64,
    cancel: &'p Cancel,
    deadline: Option<Instant>,
    log: Vec<String>,
    errors: Vec<EvalMessage>,
    color_counter: u32,
    color_stack: Vec<Color>,
    array_index: Vec<u64>,
    break_array: bool,
    pub(crate) calls: Vec<(String, Span)>,
    depth: u32,
    truncated: bool,
    needs_program: bool,
    align_memo: HashMap<String, u64>,
    hook: Option<Hook<'h>>,
    pause_state: Option<DebugState>,
    /// Per-element size for the next array (`[[elem_size(n)]]`, Kaitai).
    pending_elem_size: Option<u64>,
}

impl<'p, 'h> Eval<'p, 'h> {
    fn new(
        prog: &'p Program,
        tree: &'p Tree,
        opts: &EvalOptions,
        cancel: &'p Cancel,
        hook: Option<Hook<'h>>,
    ) -> Self {
        let mut types = HashMap::new();
        let mut short_types = HashMap::new();
        let mut fns = HashMap::new();
        for s in &prog.items {
            match &s.kind {
                StmtKind::TypeDef(t) => {
                    let name = t.name();
                    types.entry(name).or_insert(t);
                    if let Some((_, short)) = name.rsplit_once("::") {
                        short_types.entry(short).or_insert(name);
                    }
                }
                StmtKind::Fn(f) => {
                    fns.entry(f.name.as_str()).or_insert(f);
                    if let Some((_, short)) = f.name.rsplit_once("::") {
                        fns.entry(short).or_insert(f);
                    }
                }
                _ => {}
            }
        }
        let len = tree.len();
        let (io_start, io_end) = match &opts.range {
            Some(r) => (r.start.min(len), r.end.min(len)),
            None => (0, len),
        };
        let endian = opts.endian.unwrap_or_else(|| match prog.pragma("endian") {
            Some("big") => Endian::Big,
            _ => Endian::Little,
        });
        let cursor = if opts.range.is_some() {
            opts.base_offset.max(io_start)
        } else {
            opts.base_offset
        };
        Self {
            prog,
            types,
            short_types,
            fns,
            data: Data {
                tree,
                len,
                cstart: 0,
                cache: Vec::new(),
            },
            io_start,
            io_end,
            cursor,
            endian,
            nodes: Vec::new(),
            roots: Vec::new(),
            lazy: HashMap::new(),
            enums: HashMap::new(),
            enum_consts: None,
            frames: vec![Frame {
                kind: FrameKind::Global,
                scopes: vec![HashMap::new()],
            }],
            structs: Vec::new(),
            limits: opts.limits.clone(),
            steps: 0,
            cancel,
            deadline: opts
                .limits
                .time_limit_ms
                .map(|ms| Instant::now() + Duration::from_millis(ms)),
            log: Vec::new(),
            errors: Vec::new(),
            color_counter: 0,
            color_stack: Vec::new(),
            array_index: Vec::new(),
            break_array: false,
            calls: Vec::new(),
            depth: 0,
            truncated: false,
            needs_program: false,
            align_memo: HashMap::new(),
            hook,
            pause_state: None,
            pending_elem_size: None,
        }
    }

    fn run_top(&mut self, opts: &EvalOptions) -> R<()> {
        if let Some(t) = &opts.root_type {
            let name = opts
                .root_name
                .clone()
                .unwrap_or_else(|| t.rsplit("::").next().unwrap_or(t).to_ascii_lowercase());
            self.read_type(&TypeRef::named(t.clone()), &name, None, Span::default())?;
            return Ok(());
        }
        let prog = self.prog;
        for s in &prog.items {
            if matches!(s.kind, StmtKind::TypeDef(_) | StmtKind::Fn(_)) {
                continue;
            }
            match self.exec(s)? {
                Flow::Normal | Flow::Continue => {}
                Flow::Break | Flow::Return(_) => break,
            }
        }
        Ok(())
    }

    fn record(&mut self, f: &Fail, warning: bool) {
        if let Some(n) = f.node
            && let Some(node) = self.nodes.get_mut(n as usize)
            && node.error.is_none()
            && !warning
        {
            node.error = Some(f.msg.clone());
        }
        self.errors.push(EvalMessage {
            message: f.msg.clone(),
            line: f.span.line,
            col: f.span.col,
            offset: f.offset.unwrap_or(self.cursor),
            node: f.node,
            warning,
        });
    }

    fn finish(self, program: &Program) -> TemplateResult {
        TemplateResult {
            nodes: self.nodes,
            roots: self.roots,
            log: self.log,
            errors: self.errors,
            steps: self.steps,
            truncated: self.truncated,
            tree: Some(self.data.tree.clone()),
            lazy: self.lazy,
            enums: self.enums,
            program: self.needs_program.then(|| Arc::new(program.clone())),
        }
    }

    // ----- bookkeeping -----------------------------------------------------

    fn step(&mut self, span: Span) -> R<()> {
        self.steps += 1;
        if self.steps > self.limits.max_steps {
            return fail_kind(span, FailKind::Limit, "step budget exhausted");
        }
        if self.steps.trailing_zeros() >= 12 {
            if self.cancel.is_cancelled() {
                return fail_kind(span, FailKind::Cancelled, "cancelled");
            }
            if let Some(d) = self.deadline
                && Instant::now() > d
            {
                return fail_kind(span, FailKind::Limit, "evaluation timed out");
            }
        }
        Ok(())
    }

    /// Debugger hook, called before every statement (STRC-18).
    fn debug_hook(&mut self, span: Span) -> R<()> {
        if let Some(hook) = self.hook.take() {
            let pause = hook(self, span);
            self.hook = Some(hook);
            if pause {
                self.pause_state = Some(self.snapshot(span));
                return fail_kind(span, FailKind::Pause, "paused");
            }
        }
        Ok(())
    }

    pub(crate) fn snapshot(&self, span: Span) -> DebugState {
        // Innermost frame first: each frame is at the line of the call it
        // made (the top frame at the current line).
        let names =
            std::iter::once("<global>".to_string()).chain(self.calls.iter().map(|c| c.0.clone()));
        let lines = self
            .calls
            .iter()
            .map(|c| c.1.line)
            .chain(std::iter::once(span.line));
        let mut call_stack: Vec<StackFrame> = names
            .zip(lines)
            .map(|(name, line)| StackFrame { name, line })
            .collect();
        call_stack.reverse();
        let mut locals = Vec::new();
        if let Some(f) = self.frames.last() {
            for scope in &f.scopes {
                let mut names: Vec<_> = scope.iter().collect();
                names.sort_by(|a, b| a.0.cmp(b.0));
                for (k, v) in names {
                    locals.push(Variable {
                        name: k.clone(),
                        value: self.display(v),
                    });
                }
            }
        }
        let fields = self
            .structs
            .last()
            .and_then(|s| self.nodes.get(s.node as usize))
            .map(|n| {
                n.children
                    .iter()
                    .filter_map(|c| self.nodes.get(*c as usize))
                    .map(|c| Variable {
                        name: c.name.clone(),
                        value: c.value.clone().unwrap_or_else(|| c.type_name.clone()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        DebugState {
            line: span.line,
            col: span.col,
            offset: self.cursor,
            step: self.steps,
            call_stack,
            locals,
            fields,
        }
    }

    pub(crate) fn call_depth(&self) -> usize {
        self.calls.len()
    }

    fn push_node(&mut self, mut n: Node, parent: Option<NodeId>, span: Span) -> R<NodeId> {
        if self.nodes.len() >= self.limits.max_nodes {
            return fail_kind(
                span,
                FailKind::Limit,
                "node limit reached; result truncated",
            );
        }
        let id = self.nodes.len() as NodeId;
        n.id = id;
        n.parent = parent;
        n.endian = self.endian;
        let name = n.name.clone();
        self.nodes.push(n);
        match parent {
            Some(p) => {
                self.nodes[p as usize].children.push(id);
                self.nodes[p as usize].len += u64::from(!self.nodes[p as usize].is_array);
                if let Some(s) = self.structs.last_mut()
                    && s.node == p
                {
                    s.fields.insert(name, id);
                }
            }
            None => self.roots.push(id),
        }
        Ok(id)
    }

    fn leaf_color(&mut self) -> Color {
        if let Some(c) = self.color_stack.last() {
            return c.clone();
        }
        let c = Color::Palette((self.color_counter % 6) as u8);
        self.color_counter += 1;
        c
    }

    fn cur_parent(&self) -> Option<NodeId> {
        self.structs.last().map(|s| s.node)
    }

    fn read_bytes(&mut self, off: u64, n: u64, span: Span) -> R<Vec<u8>> {
        let end = off.checked_add(n);
        if end.is_none_or(|e| e > self.io_end) || off < self.io_start {
            return Err(Box::new(Fail {
                msg: format!(
                    "read of {n} byte(s) at 0x{off:X} is outside of the data (end 0x{:X})",
                    self.io_end
                ),
                span,
                kind: FailKind::Error,
                node: None,
                offset: Some(off),
            }));
        }
        let mut buf = vec![0u8; n as usize];
        if let Err(e) = self.data.read(off, &mut buf) {
            return fail(span, format!("read error at 0x{off:X}: {e}"));
        }
        Ok(buf)
    }

    // ----- type resolution -------------------------------------------------

    fn subst(&self, ty: &TypeRef) -> TypeRef {
        if let Some(RVal::Type(t)) = self.lookup_local(&ty.name) {
            let mut t = t.clone();
            if ty.endian.is_some() {
                t.endian = ty.endian;
            }
            return t;
        }
        ty.clone()
    }

    fn resolve(&self, name: &str) -> Option<Resolved<'p>> {
        let def = self
            .types
            .get(name)
            .or_else(|| self.short_types.get(name).and_then(|f| self.types.get(f)))
            .or_else(|| {
                // Names used inside a namespace may omit the prefix.
                let short = name.rsplit("::").next()?;
                self.short_types.get(short).and_then(|f| self.types.get(f))
            });
        if let Some(def) = def {
            return Some(match def {
                TypeDef::Struct(s) => Resolved::Struct(s),
                TypeDef::Enum(e) => Resolved::Enum(e),
                TypeDef::Bitfield(b) => Resolved::Bitfield(b),
                TypeDef::Alias(a) => Resolved::Alias(a),
            });
        }
        Prim::from_name(name).map(Resolved::Prim)
    }

    /// Element description used to decide whether an array can be lazy.
    fn classify(&self, ty: &TypeRef) -> Option<(Prim, Option<String>, String)> {
        let ty = self.subst(ty);
        match self.resolve(&ty.name)? {
            Resolved::Prim(p) => Some((p, None, p.name())),
            Resolved::Enum(e) => {
                let base = self.subst(&e.base);
                match self.resolve(&base.name)? {
                    Resolved::Prim(p) if p.is_int() => {
                        Some((p, Some(e.name.clone()), e.name.clone()))
                    }
                    _ => None,
                }
            }
            Resolved::Alias(a) if a.array.is_none() && a.attrs.is_empty() => {
                let (p, e, _) = self.classify(&a.target)?;
                Some((p, e, a.name.clone()))
            }
            _ => None,
        }
    }

    fn endian_scope(&mut self, e: Option<Endian>) -> Endian {
        let saved = self.endian;
        if let Some(e) = e {
            self.endian = e;
        }
        saved
    }

    // ----- reading -----------------------------------------------------------

    pub(crate) fn read_type(
        &mut self,
        ty: &TypeRef,
        name: &str,
        parent: Option<NodeId>,
        span: Span,
    ) -> R<NodeId> {
        let ty = self.subst(ty);
        // ImHex `type::Hex<T>`-style wrappers.
        if (ty.name.starts_with("type::") || ty.name.starts_with("std::"))
            && self.resolve(&ty.name).is_none()
            && let Some(inner) = ty.args.first().and_then(|a| match &a.kind {
                ExprKind::Ident(n) => Some(TypeRef::named(n.clone())),
                ExprKind::TypeArg(t) => Some(t.clone()),
                _ => None,
            })
        {
            let mut inner = inner;
            if ty.endian.is_some() {
                inner.endian = ty.endian;
            }
            let id = self.read_type(&inner, name, parent, span)?;
            if ty.name.ends_with("Hex") {
                self.set_format(id, "hex");
            }
            return Ok(id);
        }
        let saved = self.endian_scope(ty.endian);
        let r = self.read_resolved(&ty, name, parent, span);
        self.endian = saved;
        r
    }

    fn read_resolved(
        &mut self,
        ty: &TypeRef,
        name: &str,
        parent: Option<NodeId>,
        span: Span,
    ) -> R<NodeId> {
        match self.resolve(&ty.name) {
            Some(Resolved::Prim(p)) => self.read_prim(p, name, parent, None, span),
            Some(Resolved::Enum(e)) => self.read_enum(e, name, parent, span),
            Some(Resolved::Bitfield(b)) => self.read_bitfield(b, name, parent, span),
            Some(Resolved::Struct(s)) => self.read_struct(s, &ty.args, name, parent, span),
            Some(Resolved::Alias(a)) => {
                let id = if let Some(len) = &a.array {
                    self.read_array(
                        &a.target,
                        name,
                        &ArraySpec::Fixed(len.clone()),
                        parent,
                        span,
                    )?
                } else {
                    let mut target = a.target.clone();
                    if target.args.is_empty() {
                        target.args.clone_from(&ty.args);
                    }
                    self.read_type(&target, name, parent, span)?
                };
                self.nodes[id as usize].type_name.clone_from(&a.name);
                if !a.attrs.is_empty() {
                    self.apply_attrs(id, &a.attrs, span)?;
                }
                Ok(id)
            }
            None => fail(span, format!("unknown type `{}`", ty.name)),
        }
    }

    fn set_format(&mut self, id: NodeId, fmt: &str) {
        let n = &mut self.nodes[id as usize];
        n.format = Some(fmt.to_string());
        if let (Some(raw), Some(p)) = (&n.raw, n.prim.as_deref().and_then(Prim::from_name)) {
            n.value = Some(format_value(raw, p, Some(fmt)));
        }
    }

    fn read_prim(
        &mut self,
        p: Prim,
        name: &str,
        parent: Option<NodeId>,
        type_name: Option<&str>,
        span: Span,
    ) -> R<NodeId> {
        let off = self.cursor;
        let tname = type_name.map_or_else(|| p.name(), str::to_string);
        match p {
            Prim::CStr | Prim::WStr => {
                let wide = p == Prim::WStr;
                let unit = if wide { 2 } else { 1 };
                let mut bytes = Vec::new();
                let mut pos = off;
                let mut terminated = false;
                while pos + unit <= self.io_end && bytes.len() < self.limits.max_string {
                    let b = self.read_bytes(pos, unit, span)?;
                    pos += unit;
                    if b.iter().all(|&x| x == 0) {
                        terminated = true;
                        break;
                    }
                    bytes.extend_from_slice(&b);
                }
                if !terminated && pos >= self.io_end && bytes.is_empty() {
                    return fail(
                        span,
                        format!("string at 0x{off:X} starts past the end of data"),
                    );
                }
                let s = if wide {
                    decode_utf16(&bytes, self.endian)
                } else {
                    decode_8bit(&bytes)
                };
                let mut n = Node::new(name, tname, NodeKind::String);
                n.offset = off;
                n.size = pos - off;
                n.value = Some(quote(&s));
                n.raw = Some(Value::Str(s));
                n.color = Some(self.leaf_color());
                self.cursor = pos;
                self.push_node(n, parent, span)
            }
            _ => {
                let size = p.size().unwrap_or(1);
                let bytes = self.read_bytes(off, size, span)?;
                let raw = decode_prim(p, &bytes, self.endian);
                let mut n = Node::new(
                    name,
                    tname,
                    if p == Prim::Padding {
                        NodeKind::Padding
                    } else {
                        NodeKind::Primitive
                    },
                );
                n.offset = off;
                n.size = size;
                n.value = Some(format_value(&raw, p, None));
                n.raw = Some(raw);
                n.prim = Some(p.name());
                n.color = Some(self.leaf_color());
                self.cursor = off + size;
                self.push_node(n, parent, span)
            }
        }
    }

    fn read_int_prim(&mut self, ty: &TypeRef, span: Span) -> R<(Prim, i128)> {
        let ty = self.subst(ty);
        let p = match self.resolve(&ty.name) {
            Some(Resolved::Prim(p)) if p.is_int() || p == Prim::Bool || p == Prim::Char => p,
            Some(Resolved::Enum(e)) => return self.read_int_prim(&e.base.clone(), span),
            Some(Resolved::Alias(a)) => return self.read_int_prim(&a.target.clone(), span),
            _ => return fail(span, format!("`{}` is not an integer type", ty.name)),
        };
        let saved = self.endian_scope(ty.endian);
        let size = p.size().unwrap_or(1);
        let bytes = self.read_bytes(self.cursor, size, span);
        let endian = self.endian;
        self.endian = saved;
        let bytes = bytes?;
        let v = decode_uint(&bytes, endian);
        let v = if p.is_signed() {
            sign_extend(v, (size * 8) as u32)
        } else {
            v as i128
        };
        Ok((p, v))
    }

    fn enum_info(&mut self, e: &'p EnumDef, span: Span) -> R<()> {
        if self.enums.contains_key(&e.name) {
            return Ok(());
        }
        let mut info: EnumInfo = Vec::new();
        let mut next: i128 = 0;
        if let Some(f) = self.frames.last_mut() {
            f.scopes.push(HashMap::new());
        }
        let r = (|| {
            for v in &e.variants {
                let lo = match &v.value {
                    Some(x) => self.eval_int(x)?,
                    None => next,
                };
                let hi = match &v.end {
                    Some(x) => self.eval_int(x)?,
                    None => lo,
                };
                next = hi.wrapping_add(1);
                if let Some(f) = self.frames.last_mut()
                    && let Some(s) = f.scopes.last_mut()
                {
                    s.insert(v.name.clone(), RVal::Int(lo));
                }
                info.push((v.name.clone(), lo, hi));
            }
            Ok::<(), Box<Fail>>(())
        })();
        if let Some(f) = self.frames.last_mut() {
            f.scopes.pop();
        }
        r.map_err(|mut f| {
            f.span = if f.span == Span::default() {
                span
            } else {
                f.span
            };
            f
        })?;
        let flags =
            find_attr(&e.attrs, "flags").is_some() || find_attr(&e.attrs, "bitflags").is_some();
        self.enums.insert(e.name.clone(), (info, flags));
        Ok(())
    }

    fn read_enum(
        &mut self,
        e: &'p EnumDef,
        name: &str,
        parent: Option<NodeId>,
        span: Span,
    ) -> R<NodeId> {
        self.enum_info(e, span)?;
        let off = self.cursor;
        let (p, v) = self.read_int_prim(&e.base, span)?;
        let size = p.size().unwrap_or(1);
        self.cursor = off + size;
        let mut n = Node::new(name, e.name.clone(), NodeKind::Enum);
        n.offset = off;
        n.size = size;
        let (vars, flags) = &self.enums[&e.name];
        n.enum_name = enum_display(vars, *flags, v);
        n.value = Some(enum_value_text(n.enum_name.as_deref(), v));
        n.raw = Some(Value::Int(v));
        n.prim = Some(p.name());
        n.color = Some(self.leaf_color());
        self.push_node(n, parent, span)
    }

    fn read_bitfield(
        &mut self,
        b: &'p BitfieldDef,
        name: &str,
        parent: Option<NodeId>,
        span: Span,
    ) -> R<NodeId> {
        let start = self.cursor;
        let mut widths = Vec::with_capacity(b.members.len());
        for m in &b.members {
            let w = self.eval_int(&m.bits)?;
            if !(0..=128).contains(&w) {
                return fail(span, format!("invalid bit width {w}"));
            }
            widths.push(w as u32);
        }
        let total: u32 = widths.iter().sum();
        if total > 128 {
            return fail(span, "bitfield wider than 128 bits");
        }
        let nbytes = u64::from(total.div_ceil(8));
        let bytes = self.read_bytes(start, nbytes, span)?;
        let value = decode_uint(&bytes, self.endian);
        let mut node = Node::new(name, b.name.clone(), NodeKind::Bitfield);
        node.offset = start;
        node.size = nbytes;
        node.raw = Some(Value::Int(value as i128));
        node.value = Some(format!("0x{value:0w$X}", w = (nbytes * 2) as usize));
        let color = self.leaf_color();
        node.color = Some(color.clone());
        let id = self.push_node(node, parent, span)?;
        self.cursor = start + nbytes;
        let mut pos = 0u32;
        let big = self.endian == Endian::Big;
        for (m, &w) in b.members.iter().zip(&widths) {
            let shift = if big { total - pos - w } else { pos };
            let mask = if w >= 128 {
                u128::MAX
            } else {
                (1u128 << w) - 1
            };
            let v = (value >> shift) & mask;
            if let Some(mname) = &m.name {
                let bit_index = if big { total - pos - w } else { pos };
                let byte_off = if big {
                    u64::from(pos / 8)
                } else {
                    u64::from(bit_index / 8)
                };
                let mut n = Node::new(mname.clone(), "bits", NodeKind::BitMember);
                n.offset = start + byte_off.min(nbytes.saturating_sub(1));
                n.size = u64::from((bit_index % 8 + w).div_ceil(8)).max(1);
                n.bit_offset = Some(bit_index);
                n.bit_size = Some(w);
                n.color = Some(color.clone());
                let mut raw = Value::Int(v as i128);
                let mut text = format!("{v} (0x{v:X})");
                if let Some(t) = &m.ty {
                    n.type_name.clone_from(&t.name);
                    match self.resolve(&self.subst(t).name) {
                        Some(Resolved::Prim(Prim::Bool)) => {
                            raw = Value::Bool(v != 0);
                            text = (v != 0).to_string();
                        }
                        Some(Resolved::Prim(p)) if p.is_signed() => {
                            let s = sign_extend(v, w);
                            raw = Value::Int(s);
                            text = s.to_string();
                        }
                        Some(Resolved::Enum(e)) => {
                            self.enum_info(e, span)?;
                            let (vars, flags) = &self.enums[&e.name];
                            n.kind = NodeKind::Enum;
                            n.enum_name = enum_display(vars, *flags, v as i128);
                            text = enum_value_text(n.enum_name.as_deref(), v as i128);
                        }
                        _ => {}
                    }
                }
                n.raw = Some(raw);
                n.value = Some(text);
                let mid = self.push_node(n, Some(id), span)?;
                if !m.attrs.is_empty() {
                    self.apply_attrs(mid, &m.attrs, span)?;
                }
            }
            pos += w;
        }
        Ok(id)
    }

    fn read_struct(
        &mut self,
        def: &'p StructDef,
        args: &[Expr],
        name: &str,
        parent: Option<NodeId>,
        span: Span,
    ) -> R<NodeId> {
        if self.depth >= self.limits.max_depth {
            return fail_kind(span, FailKind::Limit, "maximum nesting depth exceeded");
        }
        let mut bindings = HashMap::new();
        for (i, p) in def.params.iter().enumerate() {
            let Some(a) = args.get(i) else {
                return fail(
                    span,
                    format!("missing argument `{}` for `{}`", p.name, def.name),
                );
            };
            let v = if p.ty.is_none() {
                match &a.kind {
                    ExprKind::TypeArg(t) => RVal::Type(self.subst(t)),
                    ExprKind::Ident(n) => match self.lookup_local(n) {
                        Some(RVal::Type(t)) => RVal::Type(t.clone()),
                        _ => RVal::Type(TypeRef::named(n.clone())),
                    },
                    _ => return fail(a.span, "expected a type argument"),
                }
            } else {
                let v = self.eval(a)?;
                self.scalar_or_ref(v)
            };
            bindings.insert(p.name.clone(), v);
        }
        let start = self.cursor;
        let union = def.kind == StructKind::Union;
        let mut n = Node::new(
            name,
            def.name.clone(),
            if union {
                NodeKind::Union
            } else {
                NodeKind::Struct
            },
        );
        n.offset = start;
        let id = self.push_node(n, parent, span)?;
        let c_layout = match find_attr(&def.attrs, "c_layout") {
            Some(a) => Some(match a.args.first() {
                Some(e) => self.eval_int(e)? as u64,
                None => 0,
            }),
            None => None,
        };
        let pushed_color = match find_attr(&def.attrs, "color") {
            Some(a) => {
                let c = self.attr_color(a)?;
                self.color_stack.push(c);
                true
            }
            None => false,
        };
        self.frames.push(Frame {
            kind: FrameKind::Struct,
            scopes: vec![bindings],
        });
        self.structs.push(SCtx {
            node: id,
            start,
            union,
            max_end: start,
            fields: HashMap::new(),
            bits: None,
            c_bit_end: None,
            c_layout,
            max_align: 1,
        });
        self.calls.push((format!("{} {}", def.name, name), span));
        self.depth += 1;
        let res = self.struct_body(def, span);
        self.depth -= 1;
        self.calls.pop();
        let ctx = self.structs.pop();
        self.frames.pop();
        if pushed_color {
            self.color_stack.pop();
        }
        let ctx = ctx.expect("struct context");
        let mut end = if union {
            ctx.max_end.max(start)
        } else {
            self.cursor.max(start)
        };
        if let Some(pack) = ctx.c_layout {
            let a = if pack > 0 {
                ctx.max_align.min(pack)
            } else {
                ctx.max_align
            };
            end = start + (end - start).next_multiple_of(a.max(1));
            if let Some(s) = self.structs.last_mut() {
                s.max_align = s.max_align.max(a);
            }
        }
        self.nodes[id as usize].size = end - start;
        self.cursor = end;
        match res {
            Err(mut f) => {
                if f.node.is_none() && f.kind == FailKind::Error {
                    f.node = Some(id);
                    self.nodes[id as usize].error = Some(f.msg.clone());
                }
                Err(f)
            }
            Ok(flow) => {
                if matches!(flow, Flow::Break) {
                    self.break_array = true;
                }
                if let Some(a) = find_attr(&def.attrs, "size") {
                    let a = a.clone();
                    self.apply_size(id, &a, span)?;
                }
                let other: Vec<Attr> = def
                    .attrs
                    .iter()
                    .filter(|a| !matches!(a.name.as_str(), "c_layout" | "size" | "color"))
                    .cloned()
                    .collect();
                if !other.is_empty() {
                    self.apply_attrs(id, &other, span)?;
                }
                Ok(id)
            }
        }
    }

    fn struct_body(&mut self, def: &'p StructDef, span: Span) -> R<Flow> {
        if let Some(base) = &def.inherits {
            let base = self.subst(base);
            match self.resolve(&base.name) {
                Some(Resolved::Struct(b)) => match self.struct_body(b, span)? {
                    Flow::Normal => {}
                    f => return Ok(f),
                },
                _ => return fail(span, format!("cannot inherit from `{}`", base.name)),
            }
        }
        self.exec_stmts(&def.body)
    }

    /// Natural C alignment of a type (STRC-06).
    fn natural_align(&mut self, ty: &TypeRef) -> u64 {
        let ty = self.subst(ty);
        if let Some(a) = self.align_memo.get(&ty.name) {
            return *a;
        }
        let a = match self.resolve(&ty.name) {
            Some(Resolved::Prim(p)) => p.align(),
            Some(Resolved::Enum(e)) => self.natural_align(&e.base),
            Some(Resolved::Alias(a)) => self.natural_align(&a.target),
            Some(Resolved::Bitfield(_)) | None => 1,
            Some(Resolved::Struct(s)) => {
                self.align_memo.insert(ty.name.clone(), 1); // recursion guard
                let mut m = 1;
                for st in &s.body {
                    if let StmtKind::Field(f) = &st.kind {
                        m = m.max(self.natural_align(&f.ty));
                    }
                }
                let pack = find_attr(&s.attrs, "c_layout")
                    .and_then(|a| a.args.first())
                    .and_then(|e| match e.kind {
                        ExprKind::Int(v) => Some(v as u64),
                        _ => None,
                    })
                    .unwrap_or(0);
                if pack > 0 { m.min(pack) } else { m }
            }
        };
        self.align_memo.insert(ty.name.clone(), a);
        a
    }

    fn exec_field(&mut self, f: &'p FieldDecl, span: Span) -> R<()> {
        let parent = self.cur_parent();
        if let Some(s) = self.structs.last_mut() {
            if s.union {
                self.cursor = s.start;
            }
            if f.bits.is_none() {
                s.bits = None;
                s.c_bit_end = None;
            }
        }
        if f.bits.is_none()
            && let Some(pack) = self.structs.last().and_then(|s| s.c_layout)
        {
            let mut a = self.natural_align(&f.ty);
            if pack > 0 {
                a = a.min(pack);
            }
            let s = self.structs.last_mut().expect("struct");
            s.max_align = s.max_align.max(a);
            let rel = (self.cursor - s.start).next_multiple_of(a.max(1));
            self.cursor = s.start + rel;
        }
        if let Some(a) = find_attr(&f.attrs, "align") {
            let n = match a.args.first() {
                Some(e) => self.eval_int(e)?,
                None => 1,
            };
            if n > 1 {
                self.cursor = self.cursor.next_multiple_of(n as u64);
            }
        }
        let saved = self.cursor;
        if let Some(p) = &f.placement {
            let off = self.eval_int(p)?;
            if off < 0 {
                return fail(p.span, format!("negative placement offset {off}"));
            }
            self.cursor = off as u64;
        }
        let pushed_color = match find_attr(&f.attrs, "color") {
            Some(a) => {
                let c = self.attr_color(a)?;
                self.color_stack.push(c);
                true
            }
            None => false,
        };
        let size_attr = match find_attr(&f.attrs, "size") {
            Some(a) => Some(self.eval_int(a.args.first().unwrap_or(&Expr::int(0)))?),
            None => None,
        };
        if f.array.is_some()
            && let Some(a) = find_attr(&f.attrs, "elem_size")
        {
            let n = self.eval_int(a.args.first().unwrap_or(&Expr::int(0)))?;
            self.pending_elem_size = Some(n.max(0) as u64);
        }
        let field_start = self.cursor;
        let saved_end = self.io_end;
        if let Some(sz) = size_attr {
            let lim = field_start.saturating_add(sz.max(0) as u64);
            if lim > self.io_end {
                if pushed_color {
                    self.color_stack.pop();
                }
                return fail(
                    span,
                    format!("sized field `{}` ({sz} bytes) exceeds the data", f.name),
                );
            }
            self.io_end = lim;
        }
        let r = if f.pointer.is_some() {
            self.read_pointer(f, parent, span)
        } else if let Some(bits) = &f.bits {
            self.read_bit_member(f, bits, parent, span)
        } else if let Some(a) = &f.array {
            self.read_array(&f.ty, &f.name, a, parent, span)
        } else {
            self.read_type(&f.ty, &f.name, parent, span)
        };
        self.io_end = saved_end;
        if pushed_color {
            self.color_stack.pop();
        }
        self.break_array = false;
        let id = r?;
        if let Some(sz) = size_attr {
            let sz = sz.max(0) as u64;
            self.nodes[id as usize].size = sz;
            self.cursor = field_start + sz;
        }
        if !f.attrs.is_empty() {
            self.apply_attrs(id, &f.attrs, span)?;
        }
        if find_attr(&f.attrs, "no_unique_address").is_some() {
            self.cursor = field_start;
        }
        if f.placement.is_some() {
            self.cursor = saved;
        }
        if let Some(s) = self.structs.last_mut()
            && s.union
        {
            s.max_end = s.max_end.max(self.cursor);
            self.cursor = s.start;
        }
        Ok(())
    }

    fn apply_size(&mut self, id: NodeId, a: &Attr, _span: Span) -> R<()> {
        let sz = self
            .eval_int(a.args.first().unwrap_or(&Expr::int(0)))?
            .max(0) as u64;
        let off = self.nodes[id as usize].offset;
        self.nodes[id as usize].size = sz;
        self.cursor = off + sz;
        Ok(())
    }

    fn read_pointer(&mut self, f: &'p FieldDecl, parent: Option<NodeId>, span: Span) -> R<NodeId> {
        let pty = f.pointer.as_ref().expect("pointer");
        let off = self.cursor;
        let (p, addr) = self.read_int_prim(pty, span)?;
        let size = p.size().unwrap_or(1);
        self.cursor = off + size;
        let mut n = Node::new(&f.name, format!("{}*", f.ty.name), NodeKind::Pointer);
        n.offset = off;
        n.size = size;
        n.raw = Some(Value::Int(addr));
        n.value = Some(format!("0x{addr:X}"));
        n.prim = Some(p.name());
        n.color = Some(self.leaf_color());
        let id = self.push_node(n, parent, span)?;
        let mut target = addr;
        if let Some(a) = find_attr(&f.attrs, "pointer_base") {
            let base = match a.args.first() {
                Some(e) => self.eval(e)?,
                None => RVal::Int(0),
            };
            target = match base {
                RVal::Str(fname) => {
                    let r = self.call(&fname, &[], vec![RVal::Int(addr)], span)?;
                    self.int_of(&r, span)?
                }
                other => self.int_of(&other, span)?.wrapping_add(addr),
            };
        }
        if target < 0 || target as u64 >= self.io_end {
            self.nodes[id as usize].error = Some(format!(
                "pointer target 0x{target:X} is outside of the data"
            ));
            return Ok(id);
        }
        let after = self.cursor;
        self.cursor = target as u64;
        let pname = format!("*{}", f.name);
        let r = match &f.array {
            Some(a) => self.read_array(&f.ty, &pname, a, Some(id), span),
            None => self.read_type(&f.ty, &pname, Some(id), span),
        };
        self.cursor = after;
        r?;
        Ok(id)
    }

    fn read_bit_member(
        &mut self,
        f: &'p FieldDecl,
        bits: &Expr,
        parent: Option<NodeId>,
        span: Span,
    ) -> R<NodeId> {
        let w = self.eval_int(bits)?;
        let ty = self.subst(&f.ty);
        let Some((p, _, _)) = self
            .classify(&ty)
            .filter(|(p, _, _)| p.is_int() || *p == Prim::Bool || *p == Prim::Char)
        else {
            return fail(
                span,
                format!("bit-field type `{}` must be an integer", ty.name),
            );
        };
        let unit = p.size().unwrap_or(1);
        if !(0..=(i128::from(unit) * 8)).contains(&w) {
            return fail(span, format!("bit width {w} does not fit in `{}`", ty.name));
        }
        let w = w as u32;
        let big = self.endian == Endian::Big;
        let Some(ctx) = self.structs.last() else {
            return fail(span, "bit-fields are only allowed inside structures");
        };
        let (off, nbytes, rel) = if let Some(pack) = ctx.c_layout {
            // System V / GCC allocation: a bit-field shares storage with
            // whatever precedes it unless it would straddle a unit of its
            // declared type (packed structures never round up).
            let start = ctx.start;
            let unit_bits = unit * 8;
            let packed = pack == 1;
            let mut pos = ctx.c_bit_end.unwrap_or((self.cursor - start) * 8);
            let straddles = pos / unit_bits != (pos + u64::from(w).max(1) - 1) / unit_bits;
            if w == 0 || (!packed && straddles) {
                pos = pos.next_multiple_of(unit_bits);
            }
            let align = if pack > 0 {
                p.align().min(pack)
            } else {
                p.align()
            };
            let s = self.structs.last_mut().expect("struct");
            s.max_align = s.max_align.max(align);
            s.c_bit_end = Some(pos + u64::from(w));
            let unit_start = if packed {
                pos / 8 * 8
            } else {
                pos / unit_bits * unit_bits
            };
            let rel = pos - unit_start;
            let nbytes = if packed {
                (rel + u64::from(w)).div_ceil(8).max(1)
            } else {
                unit
            };
            self.cursor = self.cursor.max(start + (pos + u64::from(w)).div_ceil(8));
            (start + unit_start / 8, nbytes, rel as u32)
        } else {
            // 010 Editor style: consecutive bit-fields of the same type
            // share one storage unit read at the cursor.
            let need_new = match ctx.bits.as_ref() {
                Some(b) => b.size != unit || b.used + w > (unit * 8) as u32 || w == 0,
                None => true,
            };
            if need_new {
                let off = self.cursor;
                self.cursor = off + unit;
                if let Some(s) = self.structs.last_mut() {
                    s.bits = Some(BitUnit {
                        offset: off,
                        size: unit,
                        used: 0,
                    });
                }
            }
            let b = self
                .structs
                .last_mut()
                .and_then(|s| s.bits.as_mut())
                .expect("bit unit");
            let used = b.used;
            b.used += w;
            (b.offset, unit, used)
        };
        let bytes = self.read_bytes(off, nbytes, span)?;
        let value = decode_uint(&bytes, self.endian);
        let total = (nbytes * 8) as u32;
        let shift = if big {
            total.saturating_sub(rel + w)
        } else {
            rel
        };
        let mask = if w >= 128 {
            u128::MAX
        } else {
            (1u128 << w) - 1
        };
        let v = (value >> shift) & mask;
        let v = if p.is_signed() {
            sign_extend(v, w)
        } else {
            v as i128
        };
        let mut n = Node::new(&f.name, ty.name.clone(), NodeKind::BitMember);
        n.offset = off;
        n.size = nbytes;
        n.bit_offset = Some(rel);
        n.bit_size = Some(w);
        n.raw = Some(if p == Prim::Bool {
            Value::Bool(v != 0)
        } else {
            Value::Int(v)
        });
        n.value = Some(format_value(n.raw.as_ref().expect("raw"), p, None));
        n.color = Some(self.leaf_color());
        n.hidden = w == 0;
        self.push_node(n, parent, span)
    }

    fn array_count(&mut self, e: &Expr) -> R<u64> {
        let n = self.eval_int(e)?;
        if n < 0 {
            return fail(e.span, format!("negative array size {n}"));
        }
        u64::try_from(n).or_else(|_| fail(e.span, "array size too large"))
    }

    fn read_array(
        &mut self,
        elem: &TypeRef,
        name: &str,
        spec: &ArraySpec,
        parent: Option<NodeId>,
        span: Span,
    ) -> R<NodeId> {
        let elem = self.subst(elem);
        let saved = self.endian_scope(elem.endian);
        let r = self.read_array_inner(&elem, name, spec, parent, span);
        self.endian = saved;
        r
    }

    fn read_array_inner(
        &mut self,
        elem: &TypeRef,
        name: &str,
        spec: &ArraySpec,
        parent: Option<NodeId>,
        span: Span,
    ) -> R<NodeId> {
        let start = self.cursor;
        let elem_size = self.pending_elem_size.take();
        if elem_size.is_none()
            && let Some((p, enum_ty, tname)) = self.classify(elem)
            && let Some(esize) = p.size()
        {
            // Lazy contiguous array, string or padding.
            let count = match spec {
                ArraySpec::Fixed(e) => {
                    let n = self.array_count(e)?;
                    let bytes = n.checked_mul(esize);
                    if bytes.is_none_or(|b| start.checked_add(b).is_none_or(|e| e > self.io_end)) {
                        return Err(Box::new(Fail {
                            msg: format!(
                                "array `{name}` of {n} element(s) at 0x{start:X} extends past the end of data"
                            ),
                            span,
                            kind: FailKind::Error,
                            node: None,
                            offset: Some(start),
                        }));
                    }
                    n
                }
                ArraySpec::Eof => (self.io_end.saturating_sub(start)) / esize,
                ArraySpec::While(c) => {
                    let mut n = 0u64;
                    loop {
                        self.step(span)?;
                        self.cursor = start + n * esize;
                        if self.cursor + esize > self.io_end || !self.eval_bool(c)? {
                            break;
                        }
                        n += 1;
                    }
                    n
                }
                ArraySpec::Until(c) => {
                    let mut n = 0u64;
                    while start + (n + 1) * esize <= self.io_end {
                        self.step(span)?;
                        let off = start + n * esize;
                        let bytes = self.read_bytes(off, esize, span)?;
                        let v = value_to_rval(decode_prim(p, &bytes, self.endian));
                        n += 1;
                        self.cursor = off + esize;
                        self.push_scope_var("_", v);
                        let stop = self.eval_bool(c);
                        self.pop_scope();
                        if stop? {
                            break;
                        }
                    }
                    n
                }
            };
            let size = count * esize;
            self.cursor = start + size;
            if matches!(p, Prim::Char | Prim::Char16) && enum_ty.is_none() {
                let bytes = self.read_bytes(start, size, span)?;
                let s = if p == Prim::Char16 {
                    decode_utf16(&bytes, self.endian)
                } else {
                    decode_8bit(&bytes)
                };
                let s = s.split('\0').next().unwrap_or("").to_string();
                let mut n = Node::new(name, format!("{tname}[{count}]"), NodeKind::String);
                n.offset = start;
                n.size = size;
                n.len = count;
                n.value = Some(quote(&s));
                n.raw = Some(Value::Str(s));
                n.color = Some(self.leaf_color());
                return self.push_node(n, parent, span);
            }
            let kind = if p == Prim::Padding {
                NodeKind::Padding
            } else {
                NodeKind::Array
            };
            let mut n = Node::new(name, format!("{tname}[{count}]"), kind);
            n.offset = start;
            n.size = size;
            n.len = count;
            n.is_array = kind == NodeKind::Array;
            n.lazy = n.is_array;
            let color = self.leaf_color();
            n.color = Some(color.clone());
            if p == Prim::U(1) && count <= 64 {
                let bytes = self.read_bytes(start, size, span)?;
                n.value = Some(
                    bytes
                        .iter()
                        .map(|b| format!("{b:02X}"))
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
            let id = self.push_node(n, parent, span)?;
            if kind == NodeKind::Array {
                if let Some(et) = &enum_ty
                    && let Some(Resolved::Enum(e)) = self.resolve(et)
                {
                    self.enum_info(e, span)?;
                }
                self.lazy.insert(
                    id,
                    LazyArray {
                        elems: LazyElems::Prim {
                            prim: p,
                            endian: self.endian,
                            enum_ty,
                            format: None,
                            elem_type: tname,
                            color: Some(color),
                        },
                        elem_size: esize,
                    },
                );
            }
            return Ok(id);
        }
        // Materialized elements.
        let mut n = Node::new(name, format!("{}[]", elem.name), NodeKind::Array);
        n.offset = start;
        n.is_array = true;
        let id = self.push_node(n, parent, span)?;
        let fixed = match spec {
            ArraySpec::Fixed(e) => Some(self.array_count(e)?),
            _ => None,
        };
        let mut i: u64 = 0;
        let mut spans: Vec<(u64, u64)> = Vec::new();
        let eager = self.limits.eager_elements as u64;
        let res: R<()> = (|| {
            loop {
                if let Some(n) = fixed
                    && i >= n
                {
                    break;
                }
                match spec {
                    ArraySpec::Eof => {
                        if self.cursor >= self.io_end {
                            break;
                        }
                    }
                    ArraySpec::While(c) if !self.eval_bool(c)? => {
                        break;
                    }
                    _ => {}
                }
                self.step(span)?;
                let before_nodes = self.nodes.len();
                let before = self.cursor;
                self.array_index.push(i);
                let saved_end = self.io_end;
                if let Some(es) = elem_size {
                    if before.saturating_add(es) > self.io_end {
                        self.array_index.pop();
                        return fail(
                            span,
                            format!("element {i} of `{name}` ({es} bytes) exceeds the data"),
                        );
                    }
                    self.io_end = before + es;
                }
                let r = self.read_type(elem, &format!("[{i}]"), Some(id), span);
                self.io_end = saved_end;
                if let (Some(es), Ok(e)) = (elem_size, &r) {
                    self.cursor = before + es;
                    self.nodes[*e as usize].size = es;
                }
                self.array_index.pop();
                let eid = match r {
                    Ok(e) => e,
                    Err(f)
                        if matches!(spec, ArraySpec::Eof) && f.kind == FailKind::Error && i > 0 =>
                    {
                        // Trailing garbage after the last complete element.
                        self.discard_from(id, before_nodes);
                        self.cursor = before;
                        let mut f = f;
                        f.msg = format!("array `{name}` stopped at element {i}: {}", f.msg);
                        self.record(&f, true);
                        break;
                    }
                    Err(f) => return Err(f),
                };
                let consumed = self.cursor - before;
                i += 1;
                if i > eager {
                    let size = self.nodes[eid as usize].size;
                    spans.push((self.nodes[eid as usize].offset, size));
                    self.discard_from(id, before_nodes);
                }
                if self.break_array {
                    self.break_array = false;
                    break;
                }
                if let ArraySpec::Until(c) = spec {
                    let last = if i > eager {
                        RVal::Void
                    } else {
                        RVal::Node(eid)
                    };
                    self.push_scope_var("_", last);
                    let stop = self.eval_bool(c);
                    self.pop_scope();
                    if stop? {
                        break;
                    }
                }
                if consumed == 0 && fixed.is_none() {
                    break; // would loop forever
                }
            }
            Ok(())
        })();
        let node = &mut self.nodes[id as usize];
        node.len = i;
        node.size = self.cursor.saturating_sub(start);
        node.type_name = format!("{}[{i}]", elem.name);
        if !spans.is_empty() {
            node.lazy = true;
            self.needs_program = true;
            self.lazy.insert(
                id,
                LazyArray {
                    elems: LazyElems::Struct {
                        ty: elem.clone(),
                        endian: self.endian,
                        first: eager,
                        spans,
                    },
                    elem_size: 0,
                },
            );
        }
        res.map(|()| id)
    }

    fn discard_from(&mut self, array: NodeId, keep: usize) {
        self.nodes.truncate(keep);
        let a = &mut self.nodes[array as usize];
        a.children.retain(|c| (*c as usize) < keep);
        self.lazy.retain(|k, _| (*k as usize) < keep);
    }

    fn push_scope_var(&mut self, name: &str, v: RVal) {
        if let Some(f) = self.frames.last_mut() {
            let mut m = HashMap::new();
            m.insert(name.to_string(), v);
            f.scopes.push(m);
        }
    }

    fn pop_scope(&mut self) {
        if let Some(f) = self.frames.last_mut() {
            f.scopes.pop();
        }
    }

    fn attr_color(&mut self, a: &Attr) -> R<Color> {
        let v = match a.args.first() {
            Some(e) => self.eval(e)?,
            None => return fail(Span::default(), "color() needs an argument"),
        };
        Ok(match v {
            RVal::Str(s) => Color::Rgb(s.trim_start_matches('#').to_ascii_lowercase()),
            other => Color::Rgb(format!(
                "{:06x}",
                self.int_of(&other, Span::default())? & 0xFF_FFFF
            )),
        })
    }

    fn apply_attrs(&mut self, id: NodeId, attrs: &[Attr], span: Span) -> R<()> {
        for a in attrs {
            let arg = |i: usize| a.args.get(i);
            match a.name.as_str() {
                "name" => {
                    if let Some(e) = arg(0) {
                        let v = self.eval(e)?;
                        self.nodes[id as usize].display_name = Some(self.text_of(&v));
                    }
                }
                "comment" => {
                    if let Some(e) = arg(0) {
                        let v = self.eval(e)?;
                        self.nodes[id as usize].comment = Some(self.text_of(&v));
                    }
                }
                "color" => {
                    let c = self.attr_color(a)?;
                    self.nodes[id as usize].color = Some(c);
                }
                "hidden" | "hide" => self.nodes[id as usize].hidden = true,
                "type_name" => {
                    if let Some(e) = arg(0) {
                        let v = self.eval(e)?;
                        self.nodes[id as usize].type_name = self.text_of(&v);
                    }
                }
                "format" | "format_read" => {
                    if let Some(e) = arg(0) {
                        let f = if let ExprKind::Ident(s) = &e.kind {
                            s.clone()
                        } else {
                            let v = self.eval(e)?;
                            self.text_of(&v)
                        };
                        if matches!(f.as_str(), "hex" | "dec" | "bin" | "oct" | "str") {
                            self.set_format(id, &f);
                            if let Some(l) = self.lazy.get_mut(&id)
                                && let LazyElems::Prim { format, .. } = &mut l.elems
                            {
                                *format = Some(f.clone());
                            }
                        } else {
                            let r = self.call(&f, &[], vec![self.node_rval(id)], span)?;
                            self.nodes[id as usize].value = Some(self.text_of(&r));
                        }
                    }
                }
                "comment_fn" | "name_fn" => {
                    if let Some(ExprKind::Str(f)) = arg(0).map(|e| &e.kind) {
                        let f = f.clone();
                        let r = self.call(&f, &[], vec![self.node_rval(id)], span)?;
                        let t = self.text_of(&r);
                        if a.name == "comment_fn" {
                            self.nodes[id as usize].comment = Some(t);
                        } else {
                            self.nodes[id as usize].display_name = Some(t);
                        }
                    }
                }
                "checksum" => {
                    let alg = match arg(0) {
                        Some(e) => {
                            let v = self.eval(e)?;
                            self.text_of(&v)
                        }
                        None => return fail(span, "checksum(algorithm, start, end) expected"),
                    };
                    let (Some(s), Some(e)) = (arg(1), arg(2)) else {
                        return fail(span, "checksum(algorithm, start, end) expected");
                    };
                    let s = self.eval_int(s)?.max(0) as u64;
                    let e = self.eval_int(e)?.max(0) as u64;
                    self.nodes[id as usize].checksum = Some(ChecksumSpec {
                        algorithm: alg.to_ascii_lowercase(),
                        start: s,
                        end: e,
                    });
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn node_rval(&self, id: NodeId) -> RVal {
        let n = &self.nodes[id as usize];
        match (&n.kind, &n.raw) {
            (NodeKind::Struct | NodeKind::Union | NodeKind::Array, _) | (_, None) => RVal::Node(id),
            (_, Some(v)) => value_to_rval(v.clone()),
        }
    }

    // ----- statements --------------------------------------------------------

    fn exec_stmts(&mut self, stmts: &'p [Stmt]) -> R<Flow> {
        for s in stmts {
            match self.exec(s)? {
                Flow::Normal => {}
                f => return Ok(f),
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_block(&mut self, stmts: &'p [Stmt]) -> R<Flow> {
        if let Some(f) = self.frames.last_mut() {
            f.scopes.push(HashMap::new());
        }
        let r = self.exec_stmts(stmts);
        self.pop_scope();
        r
    }

    fn exec(&mut self, s: &'p Stmt) -> R<Flow> {
        self.step(s.span)?;
        self.debug_hook(s.span)?;
        let span = s.span;
        let r = self.exec_inner(s);
        r.map_err(|mut f| {
            if f.span == Span::default() {
                f.span = span;
            }
            f
        })
    }

    fn exec_inner(&mut self, s: &'p Stmt) -> R<Flow> {
        match &s.kind {
            StmtKind::Field(f) => {
                if self
                    .frames
                    .last()
                    .is_some_and(|f| f.kind == FrameKind::Func)
                {
                    return fail(s.span, "fields cannot be declared inside functions");
                }
                self.exec_field(f, s.span)?;
            }
            StmtKind::TypeDef(_) | StmtKind::Fn(_) => {}
            StmtKind::Let(l) => self.exec_let(l, s.span)?,
            StmtKind::Assign { target, op, value } => {
                let mut v = self.eval(value)?;
                if let Some(op) = op {
                    let cur = self.eval(target)?;
                    v = self.binop(*op, cur, v, s.span)?;
                }
                self.assign(target, v, s.span)?;
            }
            StmtKind::Expr(e) => {
                self.eval(e)?;
            }
            StmtKind::If { cond, then, els } => {
                return if self.eval_bool(cond)? {
                    self.exec_block(then)
                } else {
                    self.exec_block(els)
                };
            }
            StmtKind::Match { scrutinee, arms } => {
                let mut vals = Vec::with_capacity(scrutinee.len());
                for e in scrutinee {
                    let v = self.eval(e)?;
                    vals.push(self.scalar(v, e.span)?);
                }
                for arm in arms {
                    if arm.patterns.len() != vals.len() {
                        return fail(s.span, "match arm has the wrong number of patterns");
                    }
                    let mut all = true;
                    for (p, v) in arm.patterns.iter().zip(&vals) {
                        if !self.pattern_matches(p, v)? {
                            all = false;
                            break;
                        }
                    }
                    if all {
                        return self.exec_block(&arm.body);
                    }
                }
            }
            StmtKind::While { cond, body } => {
                while self.eval_bool(cond)? {
                    match self.exec_block(body)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Normal | Flow::Continue => {}
                    }
                    self.step(s.span)?;
                }
            }
            StmtKind::DoWhile { body, cond } => loop {
                match self.exec_block(body)? {
                    Flow::Break => break,
                    Flow::Return(v) => return Ok(Flow::Return(v)),
                    Flow::Normal | Flow::Continue => {}
                }
                if !self.eval_bool(cond)? {
                    break;
                }
                self.step(s.span)?;
            },
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(f) = self.frames.last_mut() {
                    f.scopes.push(HashMap::new());
                }
                let r = (|| {
                    if let Some(i) = init {
                        self.exec(i)?;
                    }
                    loop {
                        if let Some(c) = cond
                            && !self.eval_bool(c)?
                        {
                            break;
                        }
                        match self.exec_block(body)? {
                            Flow::Break => break,
                            Flow::Return(v) => return Ok(Flow::Return(v)),
                            Flow::Normal | Flow::Continue => {}
                        }
                        if let Some(st) = step {
                            self.exec(st)?;
                        } else {
                            self.step(s.span)?;
                        }
                    }
                    Ok(Flow::Normal)
                })();
                self.pop_scope();
                return r;
            }
            StmtKind::Break => return Ok(Flow::Break),
            StmtKind::Continue => return Ok(Flow::Continue),
            StmtKind::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e)?,
                    None => RVal::Void,
                };
                return Ok(Flow::Return(v));
            }
            StmtKind::Assert { cond, msg, warn } => {
                if !self.eval_bool(cond)? {
                    let m = match msg {
                        Some(m) => {
                            let v = self.eval(m)?;
                            self.text_of(&v)
                        }
                        None => "assertion failed".into(),
                    };
                    let f = Fail {
                        msg: m,
                        span: s.span,
                        kind: FailKind::Error,
                        node: None,
                        offset: None,
                    };
                    if *warn {
                        self.record(&f, true);
                    } else {
                        return Err(Box::new(f));
                    }
                }
            }
            StmtKind::Block(b) => return self.exec_block(b),
        }
        Ok(Flow::Normal)
    }

    fn exec_let(&mut self, l: &'p LetDecl, span: Span) -> R<()> {
        let mut v = match &l.init {
            Some(e) => {
                let v = self.eval(e)?;
                if l.visible { self.scalar_or_ref(v) } else { v }
            }
            None => match &l.array {
                Some(n) => {
                    let n = self.eval_int(n)?.clamp(0, 1 << 24) as usize;
                    RVal::Array(vec![self.default_value(l.ty.as_ref()); n])
                }
                None => self.default_value(l.ty.as_ref()),
            },
        };
        if let Some(ty) = &l.ty
            && let Some(p) = Prim::from_name(&self.subst(ty).name)
            && !matches!(v, RVal::Node(_) | RVal::Elem(..) | RVal::Array(_))
        {
            v = self.cast(p, v, span)?;
        }
        if let Some(f) = self.frames.last_mut()
            && let Some(scope) = f.scopes.last_mut()
        {
            scope.insert(l.name.clone(), v.clone());
        }
        let in_func = self
            .frames
            .last()
            .is_some_and(|f| f.kind == FrameKind::Func);
        if l.visible && !in_func {
            let mut n = Node::new(
                &l.name,
                l.ty.as_ref().map_or("auto", |t| t.name.as_str()),
                NodeKind::Computed,
            );
            n.offset = self.cursor;
            n.value = Some(self.display(&v));
            n.raw = rval_to_value(&self.scalar_or_ref(v));
            let parent = self.cur_parent();
            let id = self.push_node(n, parent, span)?;
            if !l.attrs.is_empty() {
                self.apply_attrs(id, &l.attrs, span)?;
            }
        }
        Ok(())
    }

    fn default_value(&self, ty: Option<&TypeRef>) -> RVal {
        match ty.and_then(|t| Prim::from_name(&self.subst(t).name)) {
            Some(Prim::CStr | Prim::WStr) => RVal::Str(String::new()),
            Some(p) if p.is_float() => RVal::Float(0.0),
            Some(Prim::Bool) => RVal::Bool(false),
            _ => RVal::Int(0),
        }
    }

    fn assign(&mut self, target: &Expr, v: RVal, span: Span) -> R<()> {
        match &target.kind {
            ExprKind::Dollar => {
                let off = self.int_of(&v, span)?;
                if off < 0 || off as u64 > self.io_end {
                    return fail(
                        span,
                        format!(
                            "cannot move `$` to 0x{off:X} (data ends at 0x{:X})",
                            self.io_end
                        ),
                    );
                }
                self.cursor = off as u64;
                Ok(())
            }
            ExprKind::Ident(name) => {
                let v = self.scalar_or_ref(v);
                if let Some(slot) = self.var_slot(name) {
                    *slot = v;
                    return Ok(());
                }
                if self.lookup(name).is_some() {
                    return fail(span, format!("cannot assign to field `{name}`"));
                }
                // Implicit global (010 Editor allows this).
                if let Some(s) = self.frames[0].scopes.first_mut() {
                    s.insert(name.clone(), v);
                }
                Ok(())
            }
            ExprKind::Index(base, idx) => {
                let ExprKind::Ident(name) = &base.kind else {
                    return fail(span, "only local arrays can be assigned by index");
                };
                let i = self.eval_int(idx)?;
                let v = self.scalar_or_ref(v);
                match self.var_slot(name) {
                    Some(RVal::Array(items)) => {
                        let Some(slot) = usize::try_from(i).ok().and_then(|i| items.get_mut(i))
                        else {
                            return fail(span, format!("index {i} out of bounds"));
                        };
                        *slot = v;
                        Ok(())
                    }
                    _ => fail(span, format!("`{name}` is not a local array")),
                }
            }
            _ => fail(span, "invalid assignment target"),
        }
    }

    fn var_slot(&mut self, name: &str) -> Option<&mut RVal> {
        let last = self.frames.len() - 1;
        let fi = if self.frames[last]
            .scopes
            .iter()
            .any(|s| s.contains_key(name))
        {
            last
        } else if self.frames[0].scopes.iter().any(|s| s.contains_key(name)) {
            0
        } else {
            return None;
        };
        self.frames[fi]
            .scopes
            .iter_mut()
            .rev()
            .find_map(|s| s.get_mut(name))
    }

    fn pattern_matches(&mut self, p: &Pattern, v: &RVal) -> R<bool> {
        Ok(match p {
            Pattern::Wildcard => true,
            Pattern::Value(e) => {
                let x = self.eval(e)?;
                let x = self.scalar(x, e.span)?;
                values_equal(&x, v)
            }
            Pattern::Range(a, b) => {
                let lo = self.eval(a)?;
                let hi = self.eval(b)?;
                let lo = self.scalar(lo, a.span)?;
                let hi = self.scalar(hi, b.span)?;
                compare(v, &lo).is_some_and(std::cmp::Ordering::is_ge)
                    && compare(v, &hi).is_some_and(std::cmp::Ordering::is_le)
            }
            Pattern::Or(ps) => {
                for p in ps {
                    if self.pattern_matches(p, v)? {
                        return Ok(true);
                    }
                }
                false
            }
        })
    }

    // ----- names -------------------------------------------------------------

    fn lookup_local(&self, name: &str) -> Option<&RVal> {
        let f = self.frames.last()?;
        f.scopes.iter().rev().find_map(|s| s.get(name))
    }

    fn lookup(&mut self, name: &str) -> Option<RVal> {
        if let Some(v) = self.lookup_local(name) {
            return Some(v.clone());
        }
        for s in self.structs.iter().rev() {
            if let Some(id) = s.fields.get(name) {
                return Some(RVal::Node(*id));
            }
        }
        if let Some(s) = self.structs.last()
            && let Some(id) = self.member_of(s.node, name)
        {
            return Some(RVal::Node(id));
        }
        if let Some(v) = self.frames[0].scopes.iter().rev().find_map(|s| s.get(name)) {
            return Some(v.clone());
        }
        if let Some(id) = self
            .roots
            .iter()
            .rev()
            .find(|&&r| self.nodes[r as usize].name == name)
        {
            return Some(RVal::Node(*id));
        }
        self.enum_const(name).map(RVal::Int)
    }

    fn enum_const(&mut self, name: &str) -> Option<i128> {
        if let Some((ty, var)) = name.rsplit_once("::")
            && let Some(Resolved::Enum(e)) = self.resolve(ty)
        {
            self.enum_info(e, Span::default()).ok()?;
            return self.enums[&e.name]
                .0
                .iter()
                .find(|(n, _, _)| n == var)
                .map(|(_, lo, _)| *lo);
        }
        if self.enum_consts.is_none() {
            let mut m = HashMap::new();
            let defs: Vec<&'p EnumDef> = self
                .types
                .values()
                .filter_map(|t| match t {
                    TypeDef::Enum(e) => Some(e),
                    _ => None,
                })
                .collect();
            self.enum_consts = Some(HashMap::new());
            for e in defs {
                if self.enum_info(e, Span::default()).is_ok() {
                    for (n, lo, _) in &self.enums[&e.name].0 {
                        m.entry(n.clone()).or_insert(*lo);
                    }
                }
            }
            self.enum_consts = Some(m);
        }
        self.enum_consts.as_ref()?.get(name).copied()
    }

    /// Child of a node by name (searching anonymous members and pointees).
    fn member_of(&self, id: NodeId, name: &str) -> Option<NodeId> {
        let n = self.nodes.get(id as usize)?;
        for &c in n.children.iter().rev() {
            let cn = &self.nodes[c as usize];
            if cn.name == name {
                return Some(c);
            }
        }
        for &c in &n.children {
            let cn = &self.nodes[c as usize];
            let anon = cn.name.is_empty() || cn.name.starts_with("__anon");
            if (anon || cn.kind == NodeKind::Pointer && cn.name.starts_with('*'))
                && let Some(x) = self.member_of(c, name)
            {
                return Some(x);
            }
        }
        if n.kind == NodeKind::Pointer {
            for &c in &n.children {
                if let Some(x) = self.member_of(c, name) {
                    return Some(x);
                }
            }
        }
        None
    }

    // ----- expressions -------------------------------------------------------

    fn eval_int(&mut self, e: &Expr) -> R<i128> {
        let v = self.eval(e)?;
        self.int_of(&v, e.span)
    }

    fn eval_bool(&mut self, e: &Expr) -> R<bool> {
        let v = self.eval(e)?;
        self.truthy(&v, e.span)
    }

    /// Converts node references to plain values where possible.
    fn scalar(&mut self, v: RVal, span: Span) -> R<RVal> {
        match v {
            RVal::Node(id) => {
                let n = &self.nodes[id as usize];
                match (&n.kind, &n.raw) {
                    (NodeKind::Struct | NodeKind::Union | NodeKind::Array, _) => fail(
                        span,
                        format!("`{}` is a {} and has no scalar value", n.name, n.type_name),
                    ),
                    (_, Some(v)) => Ok(value_to_rval(v.clone())),
                    (_, None) => fail(span, format!("`{}` has no value", n.name)),
                }
            }
            RVal::Elem(id, i) => self.elem_value(id, i, span),
            other => Ok(other),
        }
    }

    /// Like `scalar`, but keeps references to compound nodes.
    fn scalar_or_ref(&mut self, v: RVal) -> RVal {
        match &v {
            RVal::Node(id) => {
                let n = &self.nodes[*id as usize];
                match (&n.kind, &n.raw) {
                    (NodeKind::Struct | NodeKind::Union | NodeKind::Array, _) | (_, None) => v,
                    (_, Some(x)) => value_to_rval(x.clone()),
                }
            }
            RVal::Elem(id, i) => self.elem_value(*id, *i, Span::default()).unwrap_or(v),
            _ => v,
        }
    }

    fn elem_value(&mut self, id: NodeId, i: u64, span: Span) -> R<RVal> {
        let Some(l) = self.lazy.get(&id) else {
            return fail(span, "not a lazy array");
        };
        let (LazyElems::Prim { prim, endian, .. }, esize) = (&l.elems, l.elem_size) else {
            return fail(span, "element is not materialized");
        };
        let (prim, endian) = (*prim, *endian);
        let off = self.nodes[id as usize].offset + i * esize;
        let bytes = self.read_bytes(off, esize, span)?;
        Ok(value_to_rval(decode_prim(prim, &bytes, endian)))
    }

    fn int_of(&mut self, v: &RVal, span: Span) -> R<i128> {
        match self.scalar(v.clone(), span)? {
            RVal::Int(i) => Ok(i),
            RVal::Bool(b) => Ok(i128::from(b)),
            RVal::Float(f) => Ok(f as i128),
            RVal::Str(s) if s.chars().count() == 1 => {
                Ok(i128::from(u32::from(s.chars().next().unwrap_or('\0'))))
            }
            other => fail(
                span,
                format!("expected an integer, found {}", Self::kind_name(&other)),
            ),
        }
    }

    fn f64_of(&mut self, v: &RVal, span: Span) -> R<f64> {
        match self.scalar(v.clone(), span)? {
            RVal::Int(i) => Ok(i as f64),
            RVal::Float(f) => Ok(f),
            RVal::Bool(b) => Ok(f64::from(u8::from(b))),
            other => fail(
                span,
                format!("expected a number, found {}", Self::kind_name(&other)),
            ),
        }
    }

    fn truthy(&mut self, v: &RVal, span: Span) -> R<bool> {
        Ok(match self.scalar(v.clone(), span)? {
            RVal::Int(i) => i != 0,
            RVal::Bool(b) => b,
            RVal::Float(f) => f != 0.0,
            RVal::Str(s) => !s.is_empty(),
            RVal::Void => false,
            RVal::Node(_) | RVal::Elem(..) | RVal::Array(_) | RVal::Type(_) => true,
        })
    }

    fn kind_name(v: &RVal) -> &'static str {
        match v {
            RVal::Void => "nothing",
            RVal::Int(_) => "integer",
            RVal::Float(_) => "float",
            RVal::Bool(_) => "bool",
            RVal::Str(_) => "string",
            RVal::Node(_) | RVal::Elem(..) => "field",
            RVal::Array(_) => "array",
            RVal::Type(_) => "type",
        }
    }

    /// Text of a value for names, comments and `std::print`.
    fn text_of(&mut self, v: &RVal) -> String {
        match self.scalar(v.clone(), Span::default()) {
            Ok(RVal::Str(s)) => s,
            Ok(other) => self.display(&other),
            Err(_) => self.display(v),
        }
    }

    pub(crate) fn display(&self, v: &RVal) -> String {
        match v {
            RVal::Void => "void".into(),
            RVal::Int(i) => i.to_string(),
            RVal::Float(f) => format_float(*f),
            RVal::Bool(b) => b.to_string(),
            RVal::Str(s) => quote(s),
            RVal::Node(id) => {
                let n = &self.nodes[*id as usize];
                n.value
                    .clone()
                    .unwrap_or_else(|| format!("{} {}", n.type_name, n.name))
            }
            RVal::Elem(id, i) => format!("{}[{i}]", self.nodes[*id as usize].name),
            RVal::Array(a) => format!(
                "[{}]",
                a.iter()
                    .take(16)
                    .map(|x| self.display(x))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            RVal::Type(t) => t.name.clone(),
        }
    }

    fn cast(&mut self, p: Prim, v: RVal, span: Span) -> R<RVal> {
        Ok(match p {
            Prim::U(n) | Prim::S(n) => {
                let i = match self.scalar(v, span)? {
                    RVal::Float(f) => f as i128,
                    other => self.int_of(&other, span)?,
                };
                let bits = u32::from(n) * 8;
                let masked = if bits >= 128 {
                    i as u128
                } else {
                    (i as u128) & ((1u128 << bits) - 1)
                };
                RVal::Int(if p.is_signed() {
                    sign_extend(masked, bits)
                } else {
                    masked as i128
                })
            }
            Prim::F16 | Prim::F32 | Prim::F64 => {
                let f = self.f64_of(&v, span)?;
                RVal::Float(if p == Prim::F32 {
                    f64::from(f as f32)
                } else {
                    f
                })
            }
            Prim::Bool => RVal::Bool(self.truthy(&v, span)?),
            Prim::Char | Prim::Char16 => {
                let i = self.int_of(&v, span)?;
                RVal::Int(if p == Prim::Char {
                    i & 0xFF
                } else {
                    i & 0xFFFF
                })
            }
            Prim::CStr | Prim::WStr => RVal::Str(self.text_of(&v)),
            Prim::Padding => v,
        })
    }

    pub(crate) fn eval(&mut self, e: &Expr) -> R<RVal> {
        let span = e.span;
        Ok(match &e.kind {
            ExprKind::Int(v) => RVal::Int(*v),
            ExprKind::Float(v) => RVal::Float(*v),
            ExprKind::Bool(b) => RVal::Bool(*b),
            ExprKind::Str(s) => RVal::Str(s.clone()),
            ExprKind::Dollar => RVal::Int(i128::from(self.cursor)),
            ExprKind::Ident(name) => match self.lookup(name) {
                Some(v) => v,
                None => return fail(span, format!("unknown identifier `{name}`")),
            },
            ExprKind::This => match self.structs.last() {
                Some(s) => RVal::Node(s.node),
                None => return fail(span, "`this` used outside of a structure"),
            },
            ExprKind::Parent => {
                let n = self.structs.len();
                if n >= 2 {
                    RVal::Node(self.structs[n - 2].node)
                } else {
                    return fail(span, "`parent` used outside of a nested structure");
                }
            }
            ExprKind::Root => match self.structs.first() {
                Some(s) => RVal::Node(s.node),
                None => match self.roots.first() {
                    Some(r) => RVal::Node(*r),
                    None => return fail(span, "no root structure"),
                },
            },
            ExprKind::Unary(op, x) => {
                let v = self.eval(x)?;
                let v = self.scalar(v, x.span)?;
                match (op, v) {
                    (UnOp::Not, v) => RVal::Bool(!self.truthy(&v, span)?),
                    (UnOp::Neg, RVal::Float(f)) => RVal::Float(-f),
                    (UnOp::Plus, v) => v,
                    (UnOp::Neg, v) => RVal::Int(self.int_of(&v, span)?.wrapping_neg()),
                    (UnOp::BitNot, v) => RVal::Int(!self.int_of(&v, span)?),
                }
            }
            ExprKind::Binary(op, a, b) => {
                if matches!(op, BinOp::And | BinOp::Or) {
                    let l = self.eval_bool(a)?;
                    if (*op == BinOp::And && !l) || (*op == BinOp::Or && l) {
                        return Ok(RVal::Bool(l));
                    }
                    return Ok(RVal::Bool(self.eval_bool(b)?));
                }
                let l = self.eval(a)?;
                let r = self.eval(b)?;
                self.binop(*op, l, r, span)?
            }
            ExprKind::Ternary(c, a, b) => {
                if self.eval_bool(c)? {
                    self.eval(a)?
                } else {
                    self.eval(b)?
                }
            }
            ExprKind::Member(base, name) => {
                let b = self.eval(base)?;
                self.member(b, name, span)?
            }
            ExprKind::Index(base, idx) => {
                // 010-style duplicate fields: `chunk[3]` with repeated `chunk`.
                if let ExprKind::Ident(name) = &base.kind
                    && let Some(s) = self.structs.last().map(|s| s.node).or(None)
                {
                    let dups: Vec<NodeId> = self.nodes[s as usize]
                        .children
                        .iter()
                        .copied()
                        .filter(|c| self.nodes[*c as usize].name == *name)
                        .collect();
                    if dups.len() > 1
                        || (dups.len() == 1
                            && !self.nodes[dups[0] as usize].is_array
                            && self.nodes[dups[0] as usize].kind != NodeKind::String)
                    {
                        let i = self.eval_int(idx)?;
                        return match usize::try_from(i).ok().and_then(|i| dups.get(i)) {
                            Some(id) => Ok(RVal::Node(*id)),
                            None => fail(span, format!("index {i} out of bounds for `{name}`")),
                        };
                    }
                }
                if let ExprKind::Ident(name) = &base.kind
                    && self.structs.is_empty()
                {
                    let dups: Vec<NodeId> = self
                        .roots
                        .iter()
                        .copied()
                        .filter(|c| self.nodes[*c as usize].name == *name)
                        .collect();
                    if dups.len() > 1 {
                        let i = self.eval_int(idx)?;
                        return match usize::try_from(i).ok().and_then(|i| dups.get(i)) {
                            Some(id) => Ok(RVal::Node(*id)),
                            None => fail(span, format!("index {i} out of bounds for `{name}`")),
                        };
                    }
                }
                let b = self.eval(base)?;
                let i = self.eval_int(idx)?;
                self.index(b, i, span)?
            }
            ExprKind::Call(name, args) => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval(a)?);
                }
                self.call(name, args, vals, span)?
            }
            ExprKind::SizeOf(arg) => RVal::Int(i128::from(self.size_of(arg, span)?)),
            ExprKind::AddressOf(x) => {
                let v = self.eval(x)?;
                match v {
                    RVal::Node(id) => RVal::Int(i128::from(self.nodes[id as usize].offset)),
                    RVal::Elem(id, i) => {
                        let es = self.lazy.get(&id).map_or(0, |l| l.elem_size);
                        RVal::Int(i128::from(self.nodes[id as usize].offset + i * es))
                    }
                    _ => return fail(span, "addressof() needs a field"),
                }
            }
            ExprKind::TypeArg(t) => RVal::Type(self.subst(t)),
            ExprKind::Cast(ty, x) => {
                let v = self.eval(x)?;
                let t = self.subst(ty);
                match self.classify(&t) {
                    Some((p, _, _)) => self.cast(p, v, span)?,
                    None => return fail(span, format!("cannot cast to `{}`", t.name)),
                }
            }
        })
    }

    fn size_of(&mut self, arg: &SizeOfArg, span: Span) -> R<u64> {
        let ty = match arg {
            SizeOfArg::Type(t) => t.clone(),
            SizeOfArg::Expr(e) => {
                if let ExprKind::Ident(n) = &e.kind {
                    match self.lookup(n) {
                        Some(RVal::Type(t)) => t,
                        Some(v) => return self.size_of_value(&v, span),
                        None => TypeRef::named(n.clone()),
                    }
                } else {
                    let v = self.eval(e)?;
                    return self.size_of_value(&v, span);
                }
            }
        };
        if let Some(s) = self.static_size(&ty, 0) {
            return Ok(s);
        }
        // Trial read at `$`.
        if self.resolve(&self.subst(&ty).name).is_none() {
            return fail(span, format!("unknown type `{}`", ty.name));
        }
        let keep = self.nodes.len();
        let roots = self.roots.len();
        let cursor = self.cursor;
        let r = self.read_type(&ty, "sizeof", None, span);
        let size = r.as_ref().ok().map(|id| self.nodes[*id as usize].size);
        self.nodes.truncate(keep);
        self.roots.truncate(roots);
        self.lazy.retain(|k, _| (*k as usize) < keep);
        self.cursor = cursor;
        r?;
        Ok(size.unwrap_or(0))
    }

    fn size_of_value(&mut self, v: &RVal, span: Span) -> R<u64> {
        Ok(match v {
            RVal::Node(id) => match self.structs.iter().find(|s| s.node == *id) {
                // Structure still being read: its size so far.
                Some(s) if s.union => s.max_end.saturating_sub(s.start),
                Some(s) => self.cursor.saturating_sub(s.start),
                None => self.nodes[*id as usize].size,
            },
            RVal::Elem(id, _) => self.lazy.get(id).map_or(0, |l| l.elem_size),
            RVal::Str(s) => s.len() as u64,
            RVal::Array(a) => a.len() as u64,
            RVal::Type(t) => {
                let t = t.clone();
                return self.size_of(&SizeOfArg::Type(t), span);
            }
            _ => return fail(span, "sizeof() needs a field or type"),
        })
    }

    fn static_size(&mut self, ty: &TypeRef, depth: u32) -> Option<u64> {
        if depth > 32 {
            return None;
        }
        let ty = self.subst(ty);
        match self.resolve(&ty.name)? {
            Resolved::Prim(p) => p.size(),
            Resolved::Enum(e) => self.static_size(&e.base, depth + 1),
            Resolved::Alias(a) => {
                let s = self.static_size(&a.target, depth + 1)?;
                match &a.array {
                    Some(n) => match n.kind {
                        ExprKind::Int(n) => Some(s * n as u64),
                        _ => None,
                    },
                    None => Some(s),
                }
            }
            Resolved::Bitfield(b) => {
                let mut bits = 0u64;
                for m in &b.members {
                    match m.bits.kind {
                        ExprKind::Int(v) => bits += v as u64,
                        _ => return None,
                    }
                }
                Some(bits.div_ceil(8))
            }
            Resolved::Struct(s) => {
                if !s.params.is_empty()
                    || s.inherits.is_some()
                    || find_attr(&s.attrs, "c_layout").is_some()
                {
                    return None;
                }
                let mut total = 0u64;
                for st in &s.body {
                    let StmtKind::Field(f) = &st.kind else {
                        return None;
                    };
                    if f.placement.is_some()
                        || f.pointer.is_some()
                        || f.bits.is_some()
                        || !f.attrs.is_empty()
                    {
                        return None;
                    }
                    let es = self.static_size(&f.ty, depth + 1)?;
                    let n = match &f.array {
                        None => 1,
                        Some(ArraySpec::Fixed(e)) => match e.kind {
                            ExprKind::Int(n) => n as u64,
                            _ => return None,
                        },
                        Some(_) => return None,
                    };
                    if s.kind == StructKind::Union {
                        total = total.max(es * n);
                    } else {
                        total += es * n;
                    }
                }
                Some(total)
            }
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    fn member(&mut self, b: RVal, name: &str, span: Span) -> R<RVal> {
        if let RVal::Node(id) = b
            && let Some(c) = self.member_of(id, name)
        {
            return Ok(RVal::Node(c));
        }
        // Built-in properties (Kaitai and convenience).
        match name {
            "length" | "size" | "len" | "count" => {
                if let RVal::Node(id) = b {
                    let n = &self.nodes[id as usize];
                    if n.is_array {
                        return Ok(RVal::Int(i128::from(n.len)));
                    }
                    if n.kind == NodeKind::String
                        && let Some(Value::Str(s)) = &n.raw
                    {
                        return Ok(RVal::Int(s.len() as i128));
                    }
                    if name == "size" {
                        return Ok(RVal::Int(i128::from(n.size)));
                    }
                }
                match self.scalar(b.clone(), span)? {
                    RVal::Str(s) => return Ok(RVal::Int(s.len() as i128)),
                    RVal::Array(a) => return Ok(RVal::Int(a.len() as i128)),
                    _ => {}
                }
            }
            "to_i" => return Ok(RVal::Int(self.int_of(&b, span)?)),
            "to_s" => return Ok(RVal::Str(self.text_of(&b))),
            "offset" | "_offset" => {
                if let RVal::Node(id) = b {
                    return Ok(RVal::Int(i128::from(self.nodes[id as usize].offset)));
                }
            }
            "first" | "last" => {
                if let RVal::Node(id) = b {
                    let len = self.nodes[id as usize].len;
                    if len > 0 {
                        let i = if name == "first" { 0 } else { len - 1 };
                        return self.index(RVal::Node(id), i128::from(i), span);
                    }
                }
            }
            _ => {}
        }
        let what = match &b {
            RVal::Node(id) => format!("`{}`", self.nodes[*id as usize].name),
            other => Self::kind_name(other).to_string(),
        };
        fail(span, format!("{what} has no member `{name}`"))
    }

    fn index(&mut self, b: RVal, i: i128, span: Span) -> R<RVal> {
        match b {
            RVal::Node(id) => {
                let n = &self.nodes[id as usize];
                let Ok(iu) = u64::try_from(i) else {
                    return fail(span, format!("negative index {i}"));
                };
                if n.is_array {
                    if iu >= n.len {
                        return fail(
                            span,
                            format!(
                                "index {i} out of bounds for `{}` (length {})",
                                n.name, n.len
                            ),
                        );
                    }
                    if let Some(&c) = n.children.get(iu as usize) {
                        return Ok(RVal::Node(c));
                    }
                    if self
                        .lazy
                        .get(&id)
                        .is_some_and(|l| matches!(l.elems, LazyElems::Prim { .. }))
                    {
                        return Ok(RVal::Elem(id, iu));
                    }
                    return fail(
                        span,
                        format!("element {i} of `{}` is beyond the eager limit", n.name),
                    );
                }
                if let Some(Value::Str(s)) = &n.raw {
                    return s
                        .as_bytes()
                        .get(iu as usize)
                        .map(|b| RVal::Int(i128::from(*b)))
                        .map_or_else(|| fail(span, format!("index {i} out of bounds")), Ok);
                }
                fail(span, format!("`{}` is not an array", n.name))
            }
            RVal::Str(s) => s
                .as_bytes()
                .get(usize::try_from(i).unwrap_or(usize::MAX))
                .map(|b| RVal::Int(i128::from(*b)))
                .map_or_else(|| fail(span, format!("index {i} out of bounds")), Ok),
            RVal::Array(a) => a
                .get(usize::try_from(i).unwrap_or(usize::MAX))
                .cloned()
                .map_or_else(|| fail(span, format!("index {i} out of bounds")), Ok),
            other => fail(span, format!("cannot index a {}", Self::kind_name(&other))),
        }
    }

    #[allow(clippy::float_cmp)]
    fn binop(&mut self, op: BinOp, l: RVal, r: RVal, span: Span) -> R<RVal> {
        let l = self.scalar(l, span)?;
        let r = self.scalar(r, span)?;
        match op {
            BinOp::And => return Ok(RVal::Bool(self.truthy(&l, span)? && self.truthy(&r, span)?)),
            BinOp::Or => return Ok(RVal::Bool(self.truthy(&l, span)? || self.truthy(&r, span)?)),
            BinOp::Xor => return Ok(RVal::Bool(self.truthy(&l, span)? ^ self.truthy(&r, span)?)),
            _ => {}
        }
        if matches!(l, RVal::Str(_)) || matches!(r, RVal::Str(_)) {
            return match op {
                BinOp::Add => Ok(RVal::Str(format!(
                    "{}{}",
                    self.text_of(&l),
                    self.text_of(&r)
                ))),
                BinOp::Eq => Ok(RVal::Bool(values_equal(&l, &r))),
                BinOp::Ne => Ok(RVal::Bool(!values_equal(&l, &r))),
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    let o = compare(&l, &r);
                    Ok(RVal::Bool(o.is_some_and(|o| match op {
                        BinOp::Lt => o.is_lt(),
                        BinOp::Le => o.is_le(),
                        BinOp::Gt => o.is_gt(),
                        _ => o.is_ge(),
                    })))
                }
                BinOp::Mul => {
                    let (s, n) = match (&l, &r) {
                        (RVal::Str(s), o) | (o, RVal::Str(s)) => (s.clone(), self.int_of(o, span)?),
                        _ => unreachable!(),
                    };
                    Ok(RVal::Str(s.repeat(n.clamp(0, 1 << 16) as usize)))
                }
                _ => fail(
                    span,
                    format!("operator `{}` is not defined for strings", op.symbol()),
                ),
            };
        }
        if matches!(l, RVal::Float(_)) || matches!(r, RVal::Float(_)) {
            let a = self.f64_of(&l, span)?;
            let b = self.f64_of(&r, span)?;
            return Ok(match op {
                BinOp::Add => RVal::Float(a + b),
                BinOp::Sub => RVal::Float(a - b),
                BinOp::Mul => RVal::Float(a * b),
                BinOp::Div => RVal::Float(a / b),
                BinOp::Rem => RVal::Float(a % b),
                BinOp::Eq => RVal::Bool(a == b),
                BinOp::Ne => RVal::Bool(a != b),
                BinOp::Lt => RVal::Bool(a < b),
                BinOp::Le => RVal::Bool(a <= b),
                BinOp::Gt => RVal::Bool(a > b),
                BinOp::Ge => RVal::Bool(a >= b),
                _ => return fail(span, format!("operator `{}` needs integers", op.symbol())),
            });
        }
        let a = self.int_of(&l, span)?;
        let b = self.int_of(&r, span)?;
        Ok(match op {
            BinOp::Add => RVal::Int(a.wrapping_add(b)),
            BinOp::Sub => RVal::Int(a.wrapping_sub(b)),
            BinOp::Mul => RVal::Int(a.wrapping_mul(b)),
            BinOp::Div => {
                if b == 0 {
                    return fail(span, "division by zero");
                }
                RVal::Int(a.wrapping_div(b))
            }
            BinOp::Rem => {
                if b == 0 {
                    return fail(span, "division by zero");
                }
                RVal::Int(a.wrapping_rem(b))
            }
            BinOp::Shl => RVal::Int(if (0..128).contains(&b) {
                a.wrapping_shl(b as u32)
            } else {
                0
            }),
            BinOp::Shr => RVal::Int(if (0..128).contains(&b) {
                a >> b
            } else if a < 0 {
                -1
            } else {
                0
            }),
            BinOp::BitAnd => RVal::Int(a & b),
            BinOp::BitOr => RVal::Int(a | b),
            BinOp::BitXor => RVal::Int(a ^ b),
            BinOp::Eq => RVal::Bool(a == b),
            BinOp::Ne => RVal::Bool(a != b),
            BinOp::Lt => RVal::Bool(a < b),
            BinOp::Le => RVal::Bool(a <= b),
            BinOp::Gt => RVal::Bool(a > b),
            BinOp::Ge => RVal::Bool(a >= b),
            BinOp::And | BinOp::Or | BinOp::Xor => unreachable!(),
        })
    }

    // ----- calls -------------------------------------------------------------

    fn call(&mut self, name: &str, args: &[Expr], vals: Vec<RVal>, span: Span) -> R<RVal> {
        if let Some(f) = self.fns.get(name).copied() {
            return self.call_user(f, vals, span);
        }
        if let Some(r) = self.builtin(name, args, &vals, span)? {
            return Ok(r);
        }
        if let Some(short) = name.rsplit("::").next()
            && short != name
            && let Some(f) = self.fns.get(short).copied()
        {
            return self.call_user(f, vals, span);
        }
        if let Some(p) = Prim::from_name(name) {
            let v = vals.into_iter().next().unwrap_or(RVal::Int(0));
            return self.cast(p, v, span);
        }
        if let Some((p, _, _)) = self.classify(&TypeRef::named(name)) {
            let v = vals.into_iter().next().unwrap_or(RVal::Int(0));
            return self.cast(p, v, span);
        }
        fail(span, format!("unknown function `{name}`"))
    }

    #[allow(clippy::needless_pass_by_value)]
    fn call_user(&mut self, f: &'p FnDef, vals: Vec<RVal>, span: Span) -> R<RVal> {
        if self.depth >= self.limits.max_depth {
            return fail_kind(span, FailKind::Limit, "maximum call depth exceeded");
        }
        let mut scope = HashMap::new();
        for (i, p) in f.params.iter().enumerate() {
            let v = vals.get(i).cloned().unwrap_or(RVal::Void);
            let v = match &p.ty {
                Some(t) => match Prim::from_name(&t.name) {
                    Some(prim)
                        if !matches!(
                            v,
                            RVal::Node(_) | RVal::Elem(..) | RVal::Array(_) | RVal::Void
                        ) =>
                    {
                        self.cast(prim, v, span)?
                    }
                    _ => self.scalar_or_ref(v),
                },
                None => v,
            };
            scope.insert(p.name.clone(), v);
        }
        self.frames.push(Frame {
            kind: FrameKind::Func,
            scopes: vec![scope],
        });
        self.calls.push((format!("fn {}", f.name), span));
        self.depth += 1;
        let r = self.exec_stmts(&f.body);
        self.depth -= 1;
        self.calls.pop();
        self.frames.pop();
        Ok(match r? {
            Flow::Return(v) => v,
            _ => RVal::Void,
        })
    }

    fn arg_int(&mut self, vals: &[RVal], i: usize, span: Span) -> R<i128> {
        match vals.get(i) {
            Some(v) => self.int_of(v, span),
            None => fail(span, format!("missing argument {}", i + 1)),
        }
    }

    fn arg_text(&mut self, vals: &[RVal], i: usize) -> String {
        vals.get(i).map(|v| self.text_of(v)).unwrap_or_default()
    }

    fn read_at(
        &mut self,
        off: i128,
        size: u64,
        signed: bool,
        endian: Endian,
        span: Span,
    ) -> R<RVal> {
        if off < 0 {
            return fail(span, format!("negative offset {off}"));
        }
        let bytes = self.read_bytes(off as u64, size, span)?;
        let v = decode_uint(&bytes, endian);
        Ok(RVal::Int(if signed {
            sign_extend(v, (size * 8) as u32)
        } else {
            v as i128
        }))
    }

    fn builtin(&mut self, name: &str, args: &[Expr], vals: &[RVal], span: Span) -> R<Option<RVal>> {
        let n = name.strip_prefix("std::").unwrap_or(name);
        let endian_arg = |ev: &mut Self, i: usize| -> R<Endian> {
            Ok(match vals.get(i) {
                Some(v) => match ev.scalar(v.clone(), span)? {
                    RVal::Str(s) if s.eq_ignore_ascii_case("big") || s == "be" => Endian::Big,
                    RVal::Str(_) => Endian::Little,
                    other => {
                        if ev.int_of(&other, span)? == 1 {
                            Endian::Big
                        } else {
                            Endian::Little
                        }
                    }
                },
                None => ev.endian,
            })
        };
        let pos_arg = |ev: &mut Self, i: usize| -> R<i128> {
            match vals.get(i) {
                Some(v) => ev.int_of(v, span),
                None => Ok(i128::from(ev.cursor)),
            }
        };
        let v = match n {
            "mem::size" | "FileSize" => RVal::Int(i128::from(self.io_end - self.io_start)),
            "mem::base_address" => RVal::Int(i128::from(self.io_start)),
            "mem::eof" | "FEof" => RVal::Bool(self.cursor >= self.io_end),
            "mem::read_unsigned" | "mem::read_signed" => {
                let off = self.arg_int(vals, 0, span)?;
                let size = self.arg_int(vals, 1, span)?;
                if !(1..=16).contains(&size) {
                    return fail(span, format!("invalid read size {size}"));
                }
                let e = endian_arg(self, 2)?;
                self.read_at(
                    off,
                    size as u64,
                    n.ends_with("signed") && !n.ends_with("unsigned"),
                    e,
                    span,
                )?
            }
            "mem::read_string" | "ReadString" => {
                let off = pos_arg(self, 0)?.max(0) as u64;
                let max = match vals.get(1) {
                    Some(v) => self.int_of(v, span)?,
                    None => -1,
                };
                let limit = if max < 0 {
                    (self.limits.max_string as u64).min(self.io_end.saturating_sub(off))
                } else {
                    (max as u64).min(self.io_end.saturating_sub(off))
                };
                let bytes = self.read_bytes(off, limit, span)?;
                let bytes = if max < 0 || n == "ReadString" {
                    bytes.split(|b| *b == 0).next().unwrap_or(&[]).to_vec()
                } else {
                    bytes
                };
                RVal::Str(decode_8bit(&bytes))
            }
            "mem::align_to" => {
                let a = self.arg_int(vals, 0, span)?;
                let v = self.arg_int(vals, 1, span)?;
                RVal::Int(if a > 0 { (v + a - 1) / a * a } else { v })
            }
            "mem::find_sequence" | "mem::find_bytes" => {
                // find_sequence(occurrence, bytes...) → offset or -1
                let occ = self.arg_int(vals, 0, span)?;
                let mut pat = Vec::new();
                for v in &vals[1.min(vals.len())..] {
                    pat.push(self.int_of(v, span)? as u8);
                }
                RVal::Int(self.find_bytes(&pat, occ, span)?)
            }
            "align" | "FAlign" => {
                let a = self.arg_int(vals, 0, span)?;
                if a > 1 {
                    self.cursor = self.cursor.next_multiple_of(a as u64).min(self.io_end);
                }
                RVal::Void
            }
            "core::array_index" => {
                RVal::Int(self.array_index.last().map_or(-1, |i| i128::from(*i)))
            }
            "core::member_count" => match vals.first() {
                Some(RVal::Node(id)) => RVal::Int(self.nodes[*id as usize].children.len() as i128),
                _ => RVal::Int(0),
            },
            "core::has_member" => {
                let m = self.arg_text(vals, 1);
                match vals.first() {
                    Some(RVal::Node(id)) => RVal::Bool(self.member_of(*id, &m).is_some()),
                    _ => RVal::Bool(false),
                }
            }
            "string::length" | "Strlen" | "strlen" => {
                let s = self.arg_text(vals, 0);
                RVal::Int(s.len() as i128)
            }
            "string::at" => {
                let s = self.arg_text(vals, 0);
                let i = self.arg_int(vals, 1, span)?;
                RVal::Str(
                    s.chars()
                        .nth(i.max(0) as usize)
                        .map(String::from)
                        .unwrap_or_default(),
                )
            }
            "string::substr" | "SubStr" | ".substring" | ".substr" => {
                let s = self.arg_text(vals, 0);
                let a = self.arg_int(vals, 1, span)?.max(0) as usize;
                let b = match vals.get(2) {
                    Some(v) => self.int_of(v, span)?.max(0) as usize,
                    None => s.len(),
                };
                let len = if n == ".substring" {
                    b.saturating_sub(a)
                } else {
                    b
                };
                RVal::Str(s.chars().skip(a).take(len).collect())
            }
            "string::to_string" | ".to_s" => RVal::Str(self.arg_text(vals, 0)),
            "string::starts_with" => {
                RVal::Bool(self.arg_text(vals, 0).starts_with(&self.arg_text(vals, 1)))
            }
            "string::ends_with" => {
                RVal::Bool(self.arg_text(vals, 0).ends_with(&self.arg_text(vals, 1)))
            }
            "string::contains" => {
                RVal::Bool(self.arg_text(vals, 0).contains(&self.arg_text(vals, 1)))
            }
            "string::parse_int" | "Atoi" | ".to_i" => {
                let s = self.arg_text(vals, 0);
                let base = match vals.get(1) {
                    Some(v) => self.int_of(v, span)? as u32,
                    None => 10,
                };
                let t = s.trim();
                let (t, base) = match t.strip_prefix("0x") {
                    Some(r) if base == 16 || base == 10 => (r, 16),
                    _ => (t, base),
                };
                RVal::Int(i128::from_str_radix(t, base.clamp(2, 36)).unwrap_or(0))
            }
            ".length" | ".size" => {
                let v = vals.first().cloned().unwrap_or(RVal::Void);
                return self.member(v, "length", span).map(Some);
            }
            ".reverse" => RVal::Str(self.arg_text(vals, 0).chars().rev().collect()),
            "print" | "Printf" | "Warning" | "warning" => {
                let s = if n == "Printf" || n == "Warning" {
                    self.printf(vals, span)?
                } else {
                    self.fmt_braces(vals, span)?
                };
                let s = s.trim_end_matches('\n').to_string();
                if n.eq_ignore_ascii_case("warning") {
                    let f = Fail {
                        msg: s,
                        span,
                        kind: FailKind::Error,
                        node: None,
                        offset: None,
                    };
                    self.record(&f, true);
                } else {
                    self.log.push(s);
                }
                RVal::Void
            }
            "SPrintf" => {
                // SPrintf(dest, fmt, ...) writes into `dest`.
                let text = self.printf(&vals[1.min(vals.len())..], span)?;
                if let Some(target) = args.first() {
                    self.assign(target, RVal::Str(text.clone()), span)?;
                }
                RVal::Str(text)
            }
            "format" | "Str" => RVal::Str(if n == "format" {
                self.fmt_braces(vals, span)?
            } else {
                self.printf(vals, span)?
            }),
            "error" => {
                let m = self.fmt_braces(vals, span)?;
                return fail(span, m);
            }
            "assert" | "assert_warn" | "Assert" => {
                let ok = match vals.first() {
                    Some(v) => self.truthy(v, span)?,
                    None => true,
                };
                if !ok {
                    let m = if vals.len() > 1 {
                        self.arg_text(vals, 1)
                    } else {
                        "assertion failed".into()
                    };
                    let f = Fail {
                        msg: m,
                        span,
                        kind: FailKind::Error,
                        node: None,
                        offset: None,
                    };
                    if n == "assert_warn" {
                        self.record(&f, true);
                    } else {
                        return Err(Box::new(f));
                    }
                }
                RVal::Void
            }
            "Exit" => return fail_kind(span, FailKind::Exit, "exit"),
            "math::min" | "Min" | "min" => {
                let a = self.arg_int(vals, 0, span)?;
                let b = self.arg_int(vals, 1, span)?;
                RVal::Int(a.min(b))
            }
            "math::max" | "Max" | "max" => {
                let a = self.arg_int(vals, 0, span)?;
                let b = self.arg_int(vals, 1, span)?;
                RVal::Int(a.max(b))
            }
            "math::abs" | "Abs" | "abs" => match vals
                .first()
                .map(|v| self.scalar(v.clone(), span))
                .transpose()?
            {
                Some(RVal::Float(f)) => RVal::Float(f.abs()),
                Some(v) => RVal::Int(self.int_of(&v, span)?.wrapping_abs()),
                None => return fail(span, "abs() needs an argument"),
            },
            "math::floor" | "Floor" => RVal::Float(
                self.f64_of(vals.first().unwrap_or(&RVal::Int(0)), span)?
                    .floor(),
            ),
            "math::ceil" | "Ceil" => RVal::Float(
                self.f64_of(vals.first().unwrap_or(&RVal::Int(0)), span)?
                    .ceil(),
            ),
            "math::pow" | "Pow" => {
                let a = self.f64_of(vals.first().unwrap_or(&RVal::Int(0)), span)?;
                let b = self.f64_of(vals.get(1).unwrap_or(&RVal::Int(1)), span)?;
                RVal::Float(a.powf(b))
            }
            // 010 Editor I/O.
            "FTell" => RVal::Int(i128::from(self.cursor)),
            "FSeek" => {
                let p = self.arg_int(vals, 0, span)?;
                if p < 0 || p as u64 > self.io_end {
                    RVal::Int(-1)
                } else {
                    self.cursor = p as u64;
                    RVal::Int(0)
                }
            }
            "FSkip" => {
                let d = self.arg_int(vals, 0, span)?;
                let p = i128::from(self.cursor) + d;
                if p < 0 || p as u64 > self.io_end {
                    RVal::Int(-1)
                } else {
                    self.cursor = p as u64;
                    RVal::Int(0)
                }
            }
            "LittleEndian" => {
                self.endian = Endian::Little;
                RVal::Void
            }
            "BigEndian" => {
                self.endian = Endian::Big;
                RVal::Void
            }
            "IsBigEndian" => RVal::Bool(self.endian == Endian::Big),
            "IsLittleEndian" => RVal::Bool(self.endian == Endian::Little),
            "ReadByte" | "ReadUByte" | "ReadUChar" | "ReadChar" | "ReadShort" | "ReadUShort"
            | "ReadInt" | "ReadUInt" | "ReadInt64" | "ReadUInt64" | "ReadQuad" | "ReadUQuad"
            | "ReadLong" | "ReadULong" => {
                let p = pos_arg(self, 0)?;
                let (size, signed) = match n {
                    "ReadByte" | "ReadChar" => (1, true),
                    "ReadUByte" | "ReadUChar" => (1, false),
                    "ReadShort" => (2, true),
                    "ReadUShort" => (2, false),
                    "ReadInt" | "ReadLong" => (4, true),
                    "ReadUInt" | "ReadULong" => (4, false),
                    "ReadInt64" | "ReadQuad" => (8, true),
                    _ => (8, false),
                };
                let e = self.endian;
                self.read_at(p, size, signed, e, span)?
            }
            "ReadFloat" | "ReadDouble" | "ReadHFloat" => {
                let p = pos_arg(self, 0)?.max(0) as u64;
                let (prim, size) = match n {
                    "ReadFloat" => (Prim::F32, 4),
                    "ReadDouble" => (Prim::F64, 8),
                    _ => (Prim::F16, 2),
                };
                let bytes = self.read_bytes(p, size, span)?;
                value_to_rval(decode_prim(prim, &bytes, self.endian))
            }
            "exists" => RVal::Bool(match args.first().map(|a| &a.kind) {
                Some(ExprKind::Ident(n)) => self.lookup(n).is_some(),
                _ => !vals.is_empty(),
            }),
            _ => return Ok(None),
        };
        Ok(Some(v))
    }

    fn find_bytes(&mut self, pat: &[u8], occurrence: i128, span: Span) -> R<i128> {
        if pat.is_empty() {
            return Ok(-1);
        }
        let mut found = -1i128;
        let mut seen = 0i128;
        let mut off = self.io_start;
        let window = 1u64 << 16;
        while off < self.io_end {
            self.step(span)?;
            let n = window.min(self.io_end - off) + (pat.len() as u64 - 1);
            let n = n.min(self.io_end - off);
            let buf = self.read_bytes(off, n, span)?;
            for (i, w) in buf.windows(pat.len()).enumerate() {
                if (i as u64) >= window {
                    break;
                }
                if w == pat {
                    if seen == occurrence {
                        found = i128::from(off + i as u64);
                        return Ok(found);
                    }
                    seen += 1;
                }
            }
            off += window;
        }
        Ok(found)
    }

    /// `ImHex` / `std::format` style `{}` formatting.
    fn fmt_braces(&mut self, vals: &[RVal], span: Span) -> R<String> {
        let fmt = self.arg_text(vals, 0);
        let mut out = String::new();
        let mut args = vals.iter().skip(1);
        let mut chars = fmt.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '{' {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    out.push('{');
                    continue;
                }
                let mut spec = String::new();
                for d in chars.by_ref() {
                    if d == '}' {
                        break;
                    }
                    spec.push(d);
                }
                let v = args.next().cloned().unwrap_or(RVal::Void);
                let spec = spec.split_once(':').map_or("", |(_, s)| s).to_string();
                out.push_str(&self.fmt_one(&v, &spec, span)?);
            } else if c == '}' && chars.peek() == Some(&'}') {
                chars.next();
                out.push('}');
            } else {
                out.push(c);
            }
        }
        Ok(out)
    }

    fn fmt_one(&mut self, v: &RVal, spec: &str, span: Span) -> R<String> {
        let v = self.scalar(v.clone(), span).unwrap_or_else(|_| v.clone());
        let ty = spec.chars().last().filter(char::is_ascii_alphabetic);
        let body = ty.map_or(spec, |_| &spec[..spec.len() - 1]);
        let alt = body.starts_with('#');
        let body = body.trim_start_matches('#');
        let zero = body.starts_with('0');
        let width: usize = body.trim_start_matches('0').parse().unwrap_or(0);
        let s = match (ty, &v) {
            (Some('x'), _) => format!(
                "{}{:x}",
                if alt { "0x" } else { "" },
                self.int_of(&v, span)?
            ),
            (Some('X'), _) => format!(
                "{}{:X}",
                if alt { "0x" } else { "" },
                self.int_of(&v, span)?
            ),
            (Some('b'), _) => format!(
                "{}{:b}",
                if alt { "0b" } else { "" },
                self.int_of(&v, span)?
            ),
            (Some('o'), _) => format!(
                "{}{:o}",
                if alt { "0o" } else { "" },
                self.int_of(&v, span)?
            ),
            (Some('c'), _) => char::from_u32(self.int_of(&v, span)? as u32)
                .unwrap_or('?')
                .to_string(),
            (_, RVal::Str(s)) => s.clone(),
            (_, other) => self.display(other),
        };
        Ok(pad(&s, width, zero))
    }

    /// C `printf`-style formatting (010 Editor).
    fn printf(&mut self, vals: &[RVal], span: Span) -> R<String> {
        let fmt = self.arg_text(vals, 0);
        let mut out = String::new();
        let mut args = vals.iter().skip(1);
        let mut chars = fmt.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            if chars.peek() == Some(&'%') {
                chars.next();
                out.push('%');
                continue;
            }
            let mut flags = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() || matches!(d, '-' | '.' | '#' | 'l' | 'L' | 'h' | 'I') {
                    flags.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            let Some(conv) = chars.next() else { break };
            let v = args.next().cloned().unwrap_or(RVal::Void);
            let zero = flags.starts_with('0');
            let width: usize = flags
                .trim_start_matches(['0', '-', '#'])
                .split('.')
                .next()
                .unwrap_or("")
                .trim_matches(|c: char| !c.is_ascii_digit())
                .parse()
                .unwrap_or(0);
            let s = match conv {
                'd' | 'i' | 'u' => self.int_of(&v, span)?.to_string(),
                'x' => format!("{:x}", self.int_of(&v, span)?),
                'X' => format!("{:X}", self.int_of(&v, span)?),
                'o' => format!("{:o}", self.int_of(&v, span)?),
                'c' => char::from_u32(self.int_of(&v, span)? as u32)
                    .unwrap_or('?')
                    .to_string(),
                'f' | 'g' | 'e' | 'F' => {
                    let prec = flags
                        .split('.')
                        .nth(1)
                        .and_then(|p| p.parse::<usize>().ok())
                        .unwrap_or(6);
                    format!("{:.*}", prec, self.f64_of(&v, span)?)
                }
                's' => self.text_of(&v),
                other => format!("%{other}"),
            };
            out.push_str(&pad(&s, width, zero && conv != 's'));
        }
        Ok(out)
    }
}

fn pad(s: &str, width: usize, zero: bool) -> String {
    if s.len() >= width {
        return s.to_string();
    }
    let fill = if zero { '0' } else { ' ' };
    let (sign, digits) = if zero && s.starts_with('-') {
        ("-", &s[1..])
    } else {
        ("", s)
    };
    format!("{sign}{}{digits}", fill.to_string().repeat(width - s.len()))
}

#[allow(clippy::float_cmp)]
fn values_equal(a: &RVal, b: &RVal) -> bool {
    match (a, b) {
        (RVal::Str(x), RVal::Str(y)) => x == y,
        (RVal::Str(s), RVal::Int(i)) | (RVal::Int(i), RVal::Str(s)) => {
            s.chars().count() == 1 && i128::from(u32::from(s.chars().next().unwrap_or('\0'))) == *i
        }
        (RVal::Float(x), y) | (y, RVal::Float(x)) => match y {
            RVal::Float(z) => x == z,
            RVal::Int(i) => *x == *i as f64,
            _ => false,
        },
        (RVal::Bool(x), RVal::Bool(y)) => x == y,
        (RVal::Bool(x), RVal::Int(i)) | (RVal::Int(i), RVal::Bool(x)) => i128::from(*x) == *i,
        (RVal::Int(x), RVal::Int(y)) => x == y,
        _ => a == b,
    }
}

fn compare(a: &RVal, b: &RVal) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (RVal::Str(x), RVal::Str(y)) => Some(x.cmp(y)),
        (RVal::Int(x), RVal::Int(y)) => Some(x.cmp(y)),
        (RVal::Float(x), RVal::Float(y)) => x.partial_cmp(y),
        (RVal::Float(x), RVal::Int(y)) => x.partial_cmp(&(*y as f64)),
        (RVal::Int(x), RVal::Float(y)) => (*x as f64).partial_cmp(y),
        (RVal::Bool(x), RVal::Bool(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

pub(crate) fn value_to_rval(v: Value) -> RVal {
    match v {
        Value::Int(i) => RVal::Int(i),
        Value::Float(f) => RVal::Float(f),
        Value::Bool(b) => RVal::Bool(b),
        Value::Str(s) => RVal::Str(s),
    }
}

fn rval_to_value(v: &RVal) -> Option<Value> {
    Some(match v {
        RVal::Int(i) => Value::Int(*i),
        RVal::Float(f) => Value::Float(*f),
        RVal::Bool(b) => Value::Bool(*b),
        RVal::Str(s) => Value::Str(s.clone()),
        _ => return None,
    })
}

/// Decodes a fixed-size primitive.
pub(crate) fn decode_prim(p: Prim, bytes: &[u8], endian: Endian) -> Value {
    let u = decode_uint(bytes, endian);
    match p {
        Prim::U(_) | Prim::Char | Prim::Char16 | Prim::Padding => Value::Int(u as i128),
        Prim::S(n) => Value::Int(sign_extend(u, u32::from(n) * 8)),
        Prim::Bool => Value::Bool(u != 0),
        Prim::F16 => Value::Float(f16_to_f64(u as u16)),
        Prim::F32 => Value::Float(f64::from(f32::from_bits(u as u32))),
        Prim::F64 => Value::Float(f64::from_bits(u as u64)),
        Prim::CStr | Prim::WStr => Value::Str(decode_8bit(bytes)),
    }
}

#[allow(clippy::float_cmp)]
fn format_float(f: f64) -> String {
    if f.is_finite() && f == f.trunc() && f.abs() < 1e15 {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

/// Formats a value for display; `format` is `hex`, `dec`, `bin` or `oct`.
pub(crate) fn format_value(v: &Value, p: Prim, format: Option<&str>) -> String {
    match v {
        Value::Int(i) => {
            let i = *i;
            if matches!(p, Prim::Char | Prim::Char16) && format.is_none() {
                return char_literal(i);
            }
            let unsigned = if i < 0 {
                let bits = p.size().unwrap_or(16) * 8;
                if bits >= 128 {
                    i as u128
                } else {
                    (i as u128) & ((1u128 << bits) - 1)
                }
            } else {
                i as u128
            };
            match format {
                Some("hex") => format!(
                    "0x{unsigned:0w$X}",
                    w = (p.size().unwrap_or(1) * 2) as usize
                ),
                Some("dec") if p.is_signed() => i.to_string(),
                Some("dec") => unsigned.to_string(),
                Some("bin") => format!("0b{unsigned:b}"),
                Some("oct") => format!("0o{unsigned:o}"),
                _ if i < 0 && p.is_signed() => i.to_string(),
                None if !p.is_signed() => format!("{unsigned} (0x{unsigned:X})"),
                _ => format!("{i} (0x{unsigned:X})"),
            }
        }
        Value::Float(f) => format_float(*f),
        Value::Bool(b) => b.to_string(),
        Value::Str(s) => quote(s),
    }
}

pub(crate) fn enum_value_text(name: Option<&str>, v: i128) -> String {
    match name {
        Some(n) => format!("{n} (0x{v:X})"),
        None => format!("??? (0x{v:X})"),
    }
}

fn char_literal(i: i128) -> String {
    match u32::try_from(i).ok().and_then(char::from_u32) {
        Some(c) if !c.is_control() => format!("'{c}'"),
        _ => format!("'\\x{:02X}'", i & 0xFFFF),
    }
}

/// Quotes a string for display (escaping control characters, max 256 chars).
pub(crate) fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for (i, c) in s.chars().enumerate() {
        if i >= 256 {
            out.push('…');
            break;
        }
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\x{:02X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// UTF-8 when valid, Latin-1 otherwise.
pub(crate) fn decode_8bit(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
    }
}

pub(crate) fn decode_utf16(bytes: &[u8], endian: Endian) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| match endian {
            Endian::Little => u16::from_le_bytes([c[0], c[1]]),
            Endian::Big => u16::from_be_bytes([c[0], c[1]]),
        })
        .collect();
    String::from_utf16_lossy(&units)
}
