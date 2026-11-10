//! Evaluation result: a flat arena of nodes forming the structure tree shown
//! next to the hex grid (STRC-10, STRC-12, STRC-13).

use std::collections::HashMap;
use std::sync::Arc;

use hexa_core::Tree;
use serde::{Deserialize, Serialize};

use crate::ast::{Endian, Program, TypeRef};
use crate::prim::Prim;

/// Node identifier (index into [`TemplateResult::nodes`]).
pub type NodeId = u32;

/// Raw decoded value of a node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum Value {
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(String),
}

impl Value {
    pub fn as_int(&self) -> Option<i128> {
        match self {
            Value::Int(v) => Some(*v),
            Value::Bool(b) => Some(i128::from(*b)),
            _ => None,
        }
    }
}

/// Highlight colour: palette slot (0..5) or explicit `rrggbb`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Color {
    Palette(u8),
    Rgb(String),
}

/// What a node represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeKind {
    Primitive,
    Enum,
    Bitfield,
    BitMember,
    Struct,
    Union,
    Array,
    String,
    Pointer,
    Padding,
    /// `auto x = expr;` computed value (STRC-02); has no bytes.
    Computed,
}

/// Checksum declared with `[[checksum("crc32", start, end)]]` (RE-06).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChecksumSpec {
    pub algorithm: String,
    /// Covered byte range `start..end` (absolute offsets).
    pub start: u64,
    pub end: u64,
}

/// One node of the structure tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub id: NodeId,
    pub parent: Option<NodeId>,
    /// Identifier used in expressions (`[3]` for array elements).
    pub name: String,
    /// Display name from `[[name("...")]]`.
    pub display_name: Option<String>,
    pub type_name: String,
    pub kind: NodeKind,
    pub offset: u64,
    pub size: u64,
    /// Bit position inside the byte at `offset` for bit-field members.
    pub bit_offset: Option<u32>,
    pub bit_size: Option<u32>,
    /// Formatted value (`"42 (0x2A)"`, `"\"IHDR\""`, `"RGB (0x2)"`).
    pub value: Option<String>,
    pub raw: Option<Value>,
    pub children: Vec<NodeId>,
    pub color: Option<Color>,
    pub comment: Option<String>,
    pub error: Option<String>,
    /// Matching enum variant(s) (STRC-16).
    pub enum_name: Option<String>,
    pub is_array: bool,
    /// Element count of arrays; children count otherwise.
    pub len: u64,
    /// Index inside the parent array for virtual elements of lazy arrays.
    pub elem_index: Option<u64>,
    pub hidden: bool,
    /// Children are produced on demand by [`children`] (STRC-10).
    pub lazy: bool,
    pub endian: Endian,
    /// Canonical primitive name for editable scalars (STRC-14).
    pub prim: Option<String>,
    pub format: Option<String>,
    pub checksum: Option<ChecksumSpec>,
}

impl Node {
    pub(crate) fn new(
        name: impl Into<String>,
        type_name: impl Into<String>,
        kind: NodeKind,
    ) -> Self {
        Self {
            id: 0,
            parent: None,
            name: name.into(),
            display_name: None,
            type_name: type_name.into(),
            kind,
            offset: 0,
            size: 0,
            bit_offset: None,
            bit_size: None,
            value: None,
            raw: None,
            children: Vec::new(),
            color: None,
            comment: None,
            error: None,
            enum_name: None,
            is_array: false,
            len: 0,
            elem_index: None,
            hidden: false,
            lazy: false,
            endian: Endian::Little,
            prim: None,
            format: None,
            checksum: None,
        }
    }

    /// Name shown in the UI.
    pub fn label(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.name)
    }

    /// Byte range `offset..offset+size`.
    pub fn range(&self) -> std::ops::Range<u64> {
        self.offset..self.offset.saturating_add(self.size)
    }
}

/// Error or warning raised during evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalMessage {
    pub message: String,
    pub line: u32,
    pub col: u32,
    /// Data offset where it happened.
    pub offset: u64,
    pub node: Option<NodeId>,
    pub warning: bool,
}

/// How lazily stored array elements are produced.
#[derive(Debug, Clone)]
pub(crate) enum LazyElems {
    /// Contiguous primitives / enums.
    Prim {
        prim: Prim,
        endian: Endian,
        enum_ty: Option<String>,
        format: Option<String>,
        elem_type: String,
        color: Option<Color>,
    },
    /// Struct elements past the eager limit: offsets and sizes only.
    Struct {
        ty: TypeRef,
        endian: Endian,
        first: u64,
        spans: Vec<(u64, u64)>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct LazyArray {
    pub elems: LazyElems,
    pub elem_size: u64,
}

/// Enum variants `(name, low, high)` for display and encoding.
pub(crate) type EnumInfo = Vec<(String, i128, i128)>;

/// Result of evaluating a template.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateResult {
    pub nodes: Vec<Node>,
    pub roots: Vec<NodeId>,
    /// `std::print` / `Printf` output.
    pub log: Vec<String>,
    pub errors: Vec<EvalMessage>,
    /// Statements executed (deterministic cost measure).
    pub steps: u64,
    /// Evaluation hit a node limit and stopped early.
    pub truncated: bool,
    #[serde(skip)]
    pub(crate) tree: Option<Tree>,
    #[serde(skip)]
    pub(crate) lazy: HashMap<NodeId, LazyArray>,
    #[serde(skip)]
    pub(crate) enums: HashMap<String, (EnumInfo, bool)>,
    #[serde(skip)]
    pub(crate) program: Option<Arc<Program>>,
}

impl TemplateResult {
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id as usize)
    }

    /// Whether evaluation finished without errors (warnings allowed).
    pub fn is_ok(&self) -> bool {
        self.errors.iter().all(|e| e.warning)
    }

    /// Dotted path of a node (`hdr.chunks[2].length`).
    pub fn path(&self, id: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = self.node(id);
        while let Some(n) = cur {
            parts.push(n.name.clone());
            cur = n.parent.and_then(|p| self.node(p));
        }
        parts.reverse();
        let mut s = String::new();
        for p in parts {
            if !s.is_empty() && !p.starts_with('[') {
                s.push('.');
            }
            s.push_str(&p);
        }
        s
    }

    /// Finds a node by dotted path (see [`TemplateResult::path`]).
    pub fn find(&self, path: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| self.path(n.id) == path)
    }

    /// Enum variant name(s) for a value of enum type `ty`.
    pub fn enum_variant(&self, ty: &str, v: i128) -> Option<String> {
        let (vars, flags) = self.enums.get(ty)?;
        enum_display(vars, *flags, v)
    }

    /// Variants of an enum type for UI pickers.
    pub fn enum_variants(&self, ty: &str) -> Vec<(String, i128)> {
        self.enums
            .get(ty)
            .map(|(v, _)| v.iter().map(|(n, lo, _)| (n.clone(), *lo)).collect())
            .unwrap_or_default()
    }
}

/// Name of the variant matching `v`, or `A | C` for flag enums.
pub(crate) fn enum_display(vars: &EnumInfo, flags: bool, v: i128) -> Option<String> {
    if let Some((n, _, _)) = vars.iter().find(|(_, lo, hi)| v >= *lo && v <= *hi) {
        return Some(n.clone());
    }
    if flags && v != 0 {
        let mut rest = v;
        let mut names = Vec::new();
        for (n, lo, hi) in vars {
            if lo == hi && *lo != 0 && (*lo & (*lo - 1)) == 0 && v & *lo == *lo {
                names.push(n.clone());
                rest &= !*lo;
            }
        }
        if !names.is_empty() {
            if rest != 0 {
                names.push(format!("0x{rest:X}"));
            }
            return Some(names.join(" | "));
        }
    }
    None
}

/// Pages through the children of `id` (STRC-10). For lazy arrays the
/// elements are produced on demand; their `id` is the array id and
/// `elem_index` identifies them.
pub fn children(result: &TemplateResult, id: NodeId, range: std::ops::Range<u64>) -> Vec<Node> {
    let Some(node) = result.node(id) else {
        return Vec::new();
    };
    if let Some(lazy) = result.lazy.get(&id) {
        let total = node.len;
        let end = range.end.min(total);
        let start = range.start.min(end);
        let eager = node.children.len() as u64;
        let mut out = Vec::new();
        for i in start..end {
            if i < eager {
                if let Some(c) = result.node(node.children[i as usize]) {
                    out.push(c.clone());
                }
                continue;
            }
            if let Some(n) = lazy_element(result, node, lazy, i) {
                out.push(n);
            }
        }
        return out;
    }
    let end = (range.end as usize).min(node.children.len());
    let start = (range.start as usize).min(end);
    node.children[start..end]
        .iter()
        .filter_map(|&c| result.node(c).cloned())
        .collect()
}

pub(crate) fn lazy_element(
    result: &TemplateResult,
    array: &Node,
    lazy: &LazyArray,
    i: u64,
) -> Option<Node> {
    if i >= array.len {
        return None;
    }
    match &lazy.elems {
        LazyElems::Prim {
            prim,
            endian,
            enum_ty,
            format,
            elem_type,
            color,
        } => {
            let offset = array.offset + i * lazy.elem_size;
            let mut buf = vec![0u8; lazy.elem_size as usize];
            let tree = result.tree.as_ref()?;
            let mut n = Node::new(format!("[{i}]"), elem_type.clone(), NodeKind::Primitive);
            n.id = array.id;
            n.parent = Some(array.id);
            n.offset = offset;
            n.size = lazy.elem_size;
            n.elem_index = Some(i);
            n.endian = *endian;
            n.prim = Some(prim.name());
            n.format.clone_from(format);
            n.color.clone_from(color);
            match tree.read(offset, &mut buf) {
                Ok(()) => {
                    let raw = crate::eval::decode_prim(*prim, &buf, *endian);
                    if let Some(et) = enum_ty {
                        n.kind = NodeKind::Enum;
                        let v = raw.as_int().unwrap_or(0);
                        n.enum_name = result.enum_variant(et, v);
                        n.value = Some(crate::eval::enum_value_text(n.enum_name.as_deref(), v));
                    } else {
                        n.value = Some(crate::eval::format_value(&raw, *prim, format.as_deref()));
                    }
                    n.raw = Some(raw);
                }
                Err(e) => n.error = Some(e.to_string()),
            }
            Some(n)
        }
        LazyElems::Struct {
            ty, first, spans, ..
        } => {
            let (offset, size) = *spans.get(usize::try_from(i.checked_sub(*first)?).ok()?)?;
            let mut n = Node::new(format!("[{i}]"), ty.name.clone(), NodeKind::Struct);
            n.id = array.id;
            n.parent = Some(array.id);
            n.offset = offset;
            n.size = size;
            n.elem_index = Some(i);
            n.lazy = true;
            Some(n)
        }
    }
}

/// Re-evaluates a struct element of a lazy array as its own result tree so
/// the UI can expand it (STRC-10). Parent fields are not available to the
/// element in this mode.
pub fn expand_element(
    result: &TemplateResult,
    array: NodeId,
    index: u64,
) -> Option<TemplateResult> {
    let node = result.node(array)?;
    let lazy = result.lazy.get(&array)?;
    let LazyElems::Struct {
        ty,
        endian,
        first,
        spans,
    } = &lazy.elems
    else {
        return None;
    };
    let (offset, _) = *spans.get(usize::try_from(index.checked_sub(*first)?).ok()?)?;
    let program = result.program.as_ref()?;
    let tree = result.tree.as_ref()?;
    let opts = crate::eval::EvalOptions {
        base_offset: offset,
        root_type: Some(ty.name.clone()),
        root_name: Some(format!("{}[{index}]", node.name)),
        endian: Some(*endian),
        ..crate::eval::EvalOptions::default()
    };
    crate::eval::evaluate(program, tree, &opts, &hexa_core::Cancel::new()).ok()
}

/// Location of a byte inside the tree (STRC-13 grid → tree sync).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeHit {
    /// Root-to-leaf ids of the deepest node covering the offset.
    pub path: Vec<NodeId>,
    /// Element index when the deepest hit is inside a lazy array.
    pub element_index: Option<u64>,
}

/// Finds the deepest node covering `offset`.
pub fn node_at(result: &TemplateResult, offset: u64) -> Option<NodeHit> {
    let mut best: Option<NodeHit> = None;
    for &r in &result.roots {
        let mut path = Vec::new();
        if let Some(hit) = descend(result, r, offset, &mut path) {
            let better = best.as_ref().is_none_or(|b| hit.path.len() > b.path.len());
            if better {
                best = Some(hit);
            }
        }
    }
    best
}

fn descend(
    result: &TemplateResult,
    id: NodeId,
    offset: u64,
    path: &mut Vec<NodeId>,
) -> Option<NodeHit> {
    let n = result.node(id)?;
    let covers = n.range().contains(&offset);
    // Pointers own bytes of their own plus a pointee elsewhere.
    if !covers && n.kind != NodeKind::Pointer && n.kind != NodeKind::Struct {
        return None;
    }
    path.push(id);
    let mut best: Option<NodeHit> = None;
    for &c in &n.children {
        if let Some(h) = descend(result, c, offset, path)
            && best.as_ref().is_none_or(|b| h.path.len() > b.path.len())
        {
            best = Some(h);
        }
    }
    if best.is_none() && covers {
        let mut element_index = None;
        if let Some(l) = result.lazy.get(&id) {
            match &l.elems {
                LazyElems::Prim { .. } if l.elem_size > 0 => {
                    element_index = Some((offset - n.offset) / l.elem_size);
                }
                LazyElems::Struct { first, spans, .. } => {
                    element_index = spans
                        .iter()
                        .position(|(o, s)| (*o..o + s).contains(&offset))
                        .map(|p| p as u64 + first);
                }
                LazyElems::Prim { .. } => {}
            }
        }
        best = Some(NodeHit {
            path: path.clone(),
            element_index,
        });
    }
    path.pop();
    best
}

/// Leaf byte ranges for grid highlighting: `(offset, size, color, id)` of
/// every visible leaf node, in document order (STRC-12).
pub fn highlights(result: &TemplateResult) -> Vec<(u64, u64, Color, NodeId)> {
    let mut out = Vec::new();
    for n in &result.nodes {
        if n.hidden || n.size == 0 || n.kind == NodeKind::Computed {
            continue;
        }
        let leaf = n.children.is_empty() || n.kind == NodeKind::Bitfield;
        if !leaf || n.kind == NodeKind::BitMember {
            continue;
        }
        let color = n.color.clone().unwrap_or(Color::Palette((n.id % 6) as u8));
        out.push((n.offset, n.size, color, n.id));
    }
    out.sort_by_key(|h| (h.0, h.3));
    out
}

/// Nodes carrying a checksum attribute (RE-06).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChecksumField {
    pub node: NodeId,
    pub path: String,
    pub algorithm: String,
    pub start: u64,
    pub end: u64,
    /// Where the stored checksum lives.
    pub offset: u64,
    pub size: u64,
    pub endian: Endian,
    pub stored: Option<i128>,
}

/// Lists every field marked with `[[checksum(...)]]`.
pub fn checksum_fields(result: &TemplateResult) -> Vec<ChecksumField> {
    result
        .nodes
        .iter()
        .filter_map(|n| {
            let c = n.checksum.as_ref()?;
            Some(ChecksumField {
                node: n.id,
                path: result.path(n.id),
                algorithm: c.algorithm.clone(),
                start: c.start,
                end: c.end,
                offset: n.offset,
                size: n.size,
                endian: n.endian,
                stored: n.raw.as_ref().and_then(Value::as_int),
            })
        })
        .collect()
}
