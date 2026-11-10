//! Persistent B+-tree of pieces (CORE-04, CORE-05, CORE-08).
//!
//! Every node is immutable and shared through `Arc`; edits copy only the path
//! from the root to the touched leaves. A `Tree` value is therefore a cheap
//! snapshot: cloning it is one atomic increment, and old versions stay valid
//! for undo history and background jobs without any locking.
//!
//! All operations are expressed through `split` and `concat`, which keep the
//! tree height-balanced (all leaves at the same depth).

use std::io;
use std::ops::ControlFlow;
use std::sync::Arc;

use crate::piece::Piece;

const MAX: usize = 32;
const MIN: usize = MAX / 2;

#[derive(Debug)]
struct Node {
    len: u64,
    height: u32,
    pieces: u64,
    kind: Kind,
}

#[derive(Debug)]
enum Kind {
    Leaf(Vec<Piece>),
    Inner(Vec<Arc<Node>>),
}

type N = Arc<Node>;

impl Node {
    fn leaf(pieces: Vec<Piece>) -> N {
        debug_assert!(!pieces.is_empty() && pieces.len() <= MAX);
        Arc::new(Self {
            len: pieces.iter().map(|p| p.len).sum(),
            height: 0,
            pieces: pieces.len() as u64,
            kind: Kind::Leaf(pieces),
        })
    }

    fn inner(children: Vec<N>) -> N {
        debug_assert!(!children.is_empty() && children.len() <= MAX);
        let height = children[0].height + 1;
        debug_assert!(children.iter().all(|c| c.height + 1 == height));
        Arc::new(Self {
            len: children.iter().map(|c| c.len).sum(),
            height,
            pieces: children.iter().map(|c| c.pieces).sum(),
            kind: Kind::Inner(children),
        })
    }

    fn width(&self) -> usize {
        match &self.kind {
            Kind::Leaf(p) => p.len(),
            Kind::Inner(c) => c.len(),
        }
    }

    fn is_ok_child(&self) -> bool {
        self.width() >= MIN
    }

    fn children(&self) -> &[N] {
        match &self.kind {
            Kind::Inner(c) => c,
            Kind::Leaf(_) => unreachable!("leaf has no children"),
        }
    }

    fn leaf_pieces(&self) -> &[Piece] {
        match &self.kind {
            Kind::Leaf(p) => p,
            Kind::Inner(_) => unreachable!("inner node has no pieces"),
        }
    }
}

/// Builds a node of the given children; splits into two when over capacity.
fn merge_nodes(a: &[N], b: &[N]) -> N {
    let total = a.len() + b.len();
    let mut all = Vec::with_capacity(total);
    all.extend_from_slice(a);
    all.extend_from_slice(b);
    if total <= MAX {
        Node::inner(all)
    } else {
        let right = all.split_off(total / 2);
        Node::inner(vec![Node::inner(all), Node::inner(right)])
    }
}

fn push_piece(out: &mut Vec<Piece>, p: Piece) {
    if p.len == 0 {
        return;
    }
    if let Some(last) = out.last_mut()
        && last.continues_with(&p)
    {
        last.len += p.len;
        return;
    }
    out.push(p);
}

fn merge_leaves(a: &[Piece], b: &[Piece]) -> N {
    let mut all = Vec::with_capacity(a.len() + b.len());
    for p in a.iter().chain(b) {
        push_piece(&mut all, p.clone());
    }
    let total = all.len();
    if total <= MAX {
        Node::leaf(all)
    } else {
        let right = all.split_off(total / 2);
        Node::inner(vec![Node::leaf(all), Node::leaf(right)])
    }
}

fn concat(a: N, b: N) -> N {
    let (h1, h2) = (a.height, b.height);
    match h1.cmp(&h2) {
        std::cmp::Ordering::Less => {
            let children = b.children();
            if h1 + 1 == h2 && a.is_ok_child() {
                merge_nodes(&[a], children)
            } else {
                let joined = concat(a, children[0].clone());
                if joined.height + 1 == h2 {
                    merge_nodes(&[joined], &children[1..])
                } else {
                    merge_nodes(joined.children(), &children[1..])
                }
            }
        }
        std::cmp::Ordering::Equal => {
            if a.is_ok_child() && b.is_ok_child() {
                Node::inner(vec![a, b])
            } else if h1 == 0 {
                merge_leaves(a.leaf_pieces(), b.leaf_pieces())
            } else {
                merge_nodes(a.children(), b.children())
            }
        }
        std::cmp::Ordering::Greater => {
            let children = a.children();
            let last = children.len() - 1;
            if h2 + 1 == h1 && b.is_ok_child() {
                merge_nodes(children, &[b])
            } else {
                let joined = concat(children[last].clone(), b);
                if joined.height + 1 == h1 {
                    merge_nodes(&children[..last], &[joined])
                } else {
                    merge_nodes(&children[..last], joined.children())
                }
            }
        }
    }
}

fn join(a: Option<N>, b: Option<N>) -> Option<N> {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(a), Some(b)) => Some(concat(a, b)),
    }
}

fn from_children(children: &[N]) -> Option<N> {
    match children.len() {
        0 => None,
        1 => Some(children[0].clone()),
        _ => Some(Node::inner(children.to_vec())),
    }
}

fn split(n: &N, pos: u64) -> (Option<N>, Option<N>) {
    if pos == 0 {
        return (None, Some(n.clone()));
    }
    if pos >= n.len {
        return (Some(n.clone()), None);
    }
    match &n.kind {
        Kind::Leaf(pieces) => {
            let mut acc = 0;
            let mut left = Vec::new();
            let mut right = Vec::new();
            for p in pieces {
                if acc + p.len <= pos {
                    left.push(p.clone());
                } else if acc >= pos {
                    right.push(p.clone());
                } else {
                    let (l, r) = p.split(pos - acc);
                    left.push(l);
                    right.push(r);
                }
                acc += p.len;
            }
            (Some(Node::leaf(left)), Some(Node::leaf(right)))
        }
        Kind::Inner(children) => {
            let mut acc = 0;
            for (i, c) in children.iter().enumerate() {
                if pos < acc + c.len {
                    let (cl, cr) = split(c, pos - acc);
                    let left = join(from_children(&children[..i]), cl);
                    let right = join(cr, from_children(&children[i + 1..]));
                    return (left, right);
                }
                acc += c.len;
            }
            unreachable!("position inside node")
        }
    }
}

/// Collapses a root that has a single child.
fn normalize(mut n: Option<N>) -> Option<N> {
    while let Some(node) = &n {
        match &node.kind {
            Kind::Inner(c) if c.len() == 1 => n = Some(c[0].clone()),
            _ => break,
        }
    }
    n
}

/// An immutable, cheaply clonable sequence of pieces.
#[derive(Clone, Debug, Default)]
pub struct Tree {
    root: Option<N>,
}

impl Tree {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a balanced tree from pieces in order.
    pub fn from_pieces(pieces: impl IntoIterator<Item = Piece>) -> Self {
        let mut merged: Vec<Piece> = Vec::new();
        for p in pieces {
            push_piece(&mut merged, p);
        }
        if merged.is_empty() {
            return Self::new();
        }
        let mut level: Vec<N> = merged.chunks(MAX).map(|c| Node::leaf(c.to_vec())).collect();
        while level.len() > 1 {
            level = level.chunks(MAX).map(|c| Node::inner(c.to_vec())).collect();
        }
        Self { root: level.pop() }
    }

    pub fn from_piece(p: Piece) -> Self {
        Self::from_pieces([p])
    }

    pub fn len(&self) -> u64 {
        self.root.as_ref().map_or(0, |r| r.len)
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// Number of pieces (≈ number of edits).
    pub fn piece_count(&self) -> u64 {
        self.root.as_ref().map_or(0, |r| r.pieces)
    }

    pub fn height(&self) -> u32 {
        self.root.as_ref().map_or(0, |r| r.height)
    }

    /// Whether two trees share the same root (cheap identity check).
    pub fn ptr_eq(&self, other: &Self) -> bool {
        match (&self.root, &other.root) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Splits into `[0, pos)` and `[pos, len)`.
    pub fn split(&self, pos: u64) -> (Self, Self) {
        match &self.root {
            None => (Self::new(), Self::new()),
            Some(r) => {
                let (a, b) = split(r, pos.min(r.len));
                (Self { root: normalize(a) }, Self { root: normalize(b) })
            }
        }
    }

    /// Concatenation `self ++ other`.
    pub fn concat(&self, other: &Self) -> Self {
        Self {
            root: normalize(join(self.root.clone(), other.root.clone())),
        }
    }

    /// The sub-sequence `[start, end)` without copying bytes.
    pub fn slice(&self, start: u64, end: u64) -> Self {
        let (_, rest) = self.split(start);
        let (mid, _) = rest.split(end.saturating_sub(start));
        mid
    }

    pub fn insert(&self, pos: u64, other: &Self) -> Self {
        let (a, b) = self.split(pos);
        a.concat(other).concat(&b)
    }

    pub fn remove(&self, start: u64, end: u64) -> Self {
        let (a, rest) = self.split(start);
        let (_, b) = rest.split(end.saturating_sub(start));
        a.concat(&b)
    }

    /// Replaces `[start, end)` with `other`.
    pub fn replace(&self, start: u64, end: u64, other: &Self) -> Self {
        let (a, rest) = self.split(start);
        let (_, b) = rest.split(end.saturating_sub(start));
        a.concat(other).concat(&b)
    }

    /// Visits the pieces intersecting `[start, end)`, clipped to that range.
    /// The callback gets the absolute position of each clipped piece.
    pub fn for_each_piece<B>(
        &self,
        start: u64,
        end: u64,
        mut f: impl FnMut(u64, &Piece) -> ControlFlow<B>,
    ) -> ControlFlow<B> {
        let Some(root) = &self.root else {
            return ControlFlow::Continue(());
        };
        let end = end.min(root.len);
        if start >= end {
            return ControlFlow::Continue(());
        }
        visit(root, 0, start, end, &mut f)
    }

    /// Collects pieces intersecting the range (clipped).
    pub fn pieces(&self, start: u64, end: u64) -> Vec<(u64, Piece)> {
        let mut out = Vec::new();
        let _ = self.for_each_piece::<()>(start, end, |pos, p| {
            out.push((pos, p.clone()));
            ControlFlow::Continue(())
        });
        out
    }

    /// Reads `buf.len()` bytes at `offset`; fails when the range is out of bounds.
    pub fn read(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let end = offset
            .checked_add(buf.len() as u64)
            .filter(|&e| e <= self.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "read past end"))?;
        let mut err = None;
        let _ = self.for_each_piece(offset, end, |pos, p| {
            let dst = &mut buf[(pos - offset) as usize..(pos - offset + p.len) as usize];
            match p.read(0, dst) {
                Ok(()) => ControlFlow::Continue(()),
                Err(e) => {
                    err = Some(e);
                    ControlFlow::Break(())
                }
            }
        });
        err.map_or(Ok(()), Err)
    }

    /// Reads up to `max` bytes at `offset`, clamped to the end.
    pub fn read_vec(&self, offset: u64, max: usize) -> io::Result<Vec<u8>> {
        let len = self.len();
        let n = (len.saturating_sub(offset)).min(max as u64) as usize;
        let mut buf = vec![0; n];
        self.read(offset, &mut buf)?;
        Ok(buf)
    }

    #[cfg(test)]
    fn check_invariants(&self) {
        fn walk(n: &Node, is_root: bool) -> (u64, u32) {
            match &n.kind {
                Kind::Leaf(p) => {
                    assert!(!p.is_empty() && p.len() <= MAX);
                    assert!(p.iter().all(|x| x.len > 0));
                    assert_eq!(n.len, p.iter().map(|x| x.len).sum::<u64>());
                    (n.len, 0)
                }
                Kind::Inner(c) => {
                    assert!(c.len() <= MAX);
                    assert!(is_root || !c.is_empty());
                    let mut total = 0;
                    for child in c {
                        let (l, h) = walk(child, false);
                        assert_eq!(h + 1, n.height);
                        total += l;
                    }
                    assert_eq!(total, n.len);
                    (n.len, n.height)
                }
            }
        }
        if let Some(r) = &self.root {
            walk(r, true);
        }
    }
}

fn visit<B>(
    n: &Node,
    base: u64,
    start: u64,
    end: u64,
    f: &mut impl FnMut(u64, &Piece) -> ControlFlow<B>,
) -> ControlFlow<B> {
    match &n.kind {
        Kind::Leaf(pieces) => {
            let mut pos = base;
            for p in pieces {
                let pend = pos + p.len;
                if pend > start && pos < end {
                    let lo = start.max(pos);
                    let hi = end.min(pend);
                    if lo == pos && hi == pend {
                        f(pos, p)?;
                    } else {
                        f(lo, &p.slice(lo - pos, hi - lo))?;
                    }
                }
                if pend >= end {
                    break;
                }
                pos = pend;
            }
        }
        Kind::Inner(children) => {
            let mut pos = base;
            for c in children {
                let cend = pos + c.len;
                if cend > start && pos < end {
                    visit(c, pos, start, end, f)?;
                }
                if cend >= end {
                    break;
                }
                pos = cend;
            }
        }
    }
    ControlFlow::Continue(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn bytes(t: &Tree) -> Vec<u8> {
        t.read_vec(0, usize::MAX).unwrap()
    }

    fn leaf(data: &[u8]) -> Tree {
        Tree::from_piece(Piece::bytes(data.to_vec()))
    }

    #[test]
    fn basic_edits() {
        let t = leaf(b"hello world");
        let t = t.insert(5, &leaf(b","));
        assert_eq!(bytes(&t), b"hello, world");
        let t = t.remove(0, 7);
        assert_eq!(bytes(&t), b"world");
        let t = t.replace(0, 1, &leaf(b"W"));
        assert_eq!(bytes(&t), b"World");
        assert_eq!(bytes(&t.slice(1, 4)), b"orl");
        t.check_invariants();
    }

    #[test]
    fn many_single_byte_inserts_stay_balanced() {
        let mut t = Tree::new();
        for i in 0..10_000u32 {
            let pos = u64::from(i.wrapping_mul(2_654_435_761) % (i + 1));
            t = t.insert(pos, &leaf(&[i as u8]));
        }
        t.check_invariants();
        assert_eq!(t.len(), 10_000);
        assert!(t.height() <= 4, "height {}", t.height());
    }

    #[test]
    fn old_snapshots_survive() {
        let a = leaf(b"abcdef");
        let b = a.remove(1, 3);
        let c = b.insert(0, &leaf(b"XY"));
        assert_eq!(bytes(&a), b"abcdef");
        assert_eq!(bytes(&b), b"adef");
        assert_eq!(bytes(&c), b"XYadef");
    }

    #[derive(Debug, Clone)]
    enum Op {
        Insert(u16, Vec<u8>),
        Remove(u16, u16),
        Replace(u16, u16, Vec<u8>),
        CopyPaste(u16, u16, u16),
    }

    fn op() -> impl Strategy<Value = Op> {
        let data = proptest::collection::vec(any::<u8>(), 0..40);
        prop_oneof![
            (any::<u16>(), data.clone()).prop_map(|(a, d)| Op::Insert(a, d)),
            (any::<u16>(), any::<u16>()).prop_map(|(a, b)| Op::Remove(a, b)),
            (any::<u16>(), any::<u16>(), data).prop_map(|(a, b, d)| Op::Replace(a, b, d)),
            (any::<u16>(), any::<u16>(), any::<u16>()).prop_map(|(a, b, c)| Op::CopyPaste(a, b, c)),
        ]
    }

    fn range(len: usize, a: u16, b: u16) -> (usize, usize) {
        if len == 0 {
            return (0, 0);
        }
        let s = a as usize % (len + 1);
        let e = s + b as usize % (len - s + 1).min(64);
        (s, e)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        /// NFR-08: the tree behaves exactly like a plain `Vec<u8>`.
        #[test]
        fn matches_naive_model(init in proptest::collection::vec(any::<u8>(), 0..200),
                               ops in proptest::collection::vec(op(), 0..120)) {
            let mut model = init.clone();
            let mut tree = if init.is_empty() { Tree::new() } else { leaf(&init) };
            let mut history = vec![(tree.clone(), model.clone())];
            for op in ops {
                match op {
                    Op::Insert(a, d) => {
                        let at = a as usize % (model.len() + 1);
                        model.splice(at..at, d.iter().copied());
                        tree = tree.insert(at as u64, &if d.is_empty() { Tree::new() } else { leaf(&d) });
                    }
                    Op::Remove(a, b) => {
                        let (s, e) = range(model.len(), a, b);
                        model.drain(s..e);
                        tree = tree.remove(s as u64, e as u64);
                    }
                    Op::Replace(a, b, d) => {
                        let (s, e) = range(model.len(), a, b);
                        model.splice(s..e, d.iter().copied());
                        tree = tree.replace(s as u64, e as u64, &if d.is_empty() { Tree::new() } else { leaf(&d) });
                    }
                    Op::CopyPaste(a, b, c) => {
                        let (s, e) = range(model.len(), a, b);
                        let clip = tree.slice(s as u64, e as u64);
                        let chunk = model[s..e].to_vec();
                        let at = c as usize % (model.len() + 1);
                        model.splice(at..at, chunk);
                        tree = tree.insert(at as u64, &clip);
                    }
                }
                tree.check_invariants();
                prop_assert_eq!(tree.len(), model.len() as u64);
                history.push((tree.clone(), model.clone()));
            }
            prop_assert_eq!(bytes(&tree), model);
            for (t, m) in history {
                prop_assert_eq!(bytes(&t), m);
            }
        }
    }
}
