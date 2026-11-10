//! Undo history as a tree of persistent snapshots (CORE-09…13).
//!
//! Each entry stores the root of the piece tree after the edit. Because trees
//! share structure, memory grows with the number of edits, not with file size.
//! Undoing and then editing starts a new branch; the old branch stays
//! reachable through [`History::goto`].

use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::tree::Tree;

/// Shape of an edit relative to the previous state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub start: u64,
    pub removed: u64,
    pub inserted: u64,
}

impl Change {
    /// The change that undoes this one.
    pub fn inverse(self) -> Self {
        Self {
            start: self.start,
            removed: self.inserted,
            inserted: self.removed,
        }
    }
}

#[derive(Debug, Clone)]
struct Entry {
    tree: Tree,
    parent: Option<usize>,
    children: Vec<usize>,
    redo_child: Option<usize>,
    label: String,
    time_ms: u64,
    change: Option<Change>,
}

/// One history entry as shown to the user.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItem {
    pub id: usize,
    pub parent: Option<usize>,
    pub label: String,
    pub time_ms: u64,
    pub current: bool,
    pub saved: bool,
    /// Whether this entry is on the path that redo would follow.
    pub on_redo_path: bool,
}

#[derive(Debug, Clone)]
pub struct History {
    entries: Vec<Entry>,
    current: usize,
    saved: Option<usize>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl History {
    pub fn new(initial: Tree) -> Self {
        Self {
            entries: vec![Entry {
                tree: initial,
                parent: None,
                children: Vec::new(),
                redo_child: None,
                label: "Open".into(),
                time_ms: now_ms(),
                change: None,
            }],
            current: 0,
            saved: Some(0),
        }
    }

    pub fn tree(&self) -> &Tree {
        &self.entries[self.current].tree
    }

    pub fn current(&self) -> usize {
        self.current
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn is_modified(&self) -> bool {
        self.saved != Some(self.current)
    }

    pub fn can_undo(&self) -> bool {
        self.entries[self.current].parent.is_some()
    }

    pub fn can_redo(&self) -> bool {
        self.entries[self.current].redo_child.is_some()
    }

    /// Adds a new entry after the current one.
    pub fn commit(&mut self, tree: Tree, label: impl Into<String>, change: Change) -> usize {
        let id = self.entries.len();
        self.entries.push(Entry {
            tree,
            parent: Some(self.current),
            children: Vec::new(),
            redo_child: None,
            label: label.into(),
            time_ms: now_ms(),
            change: Some(change),
        });
        let cur = &mut self.entries[self.current];
        cur.children.push(id);
        cur.redo_child = Some(id);
        self.current = id;
        id
    }

    /// Whether the current entry can be amended in place (coalescing).
    pub fn can_amend(&self, id: usize) -> bool {
        self.current == id
            && self.entries[id].children.is_empty()
            && self.saved != Some(id)
            && self.entries[id].parent.is_some()
    }

    /// Replaces the state of the current entry (CORE-10 coalescing).
    pub fn amend(&mut self, tree: Tree, change: Change) {
        let e = &mut self.entries[self.current];
        e.tree = tree;
        e.change = Some(change);
        e.time_ms = now_ms();
    }

    /// Steps back; returns the change that was undone.
    pub fn undo(&mut self) -> Option<Change> {
        let e = &self.entries[self.current];
        let parent = e.parent?;
        let change = e.change;
        let id = self.current;
        self.entries[parent].redo_child = Some(id);
        self.current = parent;
        change
    }

    /// Steps forward along the most recent branch; returns the redone change.
    pub fn redo(&mut self) -> Option<Change> {
        let next = self.entries[self.current].redo_child?;
        self.current = next;
        self.entries[next].change
    }

    /// Jumps to any entry of the undo tree (CORE-13).
    pub fn goto(&mut self, id: usize) -> bool {
        if id >= self.entries.len() {
            return false;
        }
        // Make redo follow the path towards `id` from every ancestor.
        let mut node = id;
        while let Some(parent) = self.entries[node].parent {
            self.entries[parent].redo_child = Some(node);
            node = parent;
        }
        self.current = id;
        true
    }

    pub fn mark_saved(&mut self) {
        self.saved = Some(self.current);
    }

    /// Replaces the tree of the current entry without changing its content
    /// (rebasing on a freshly saved file, CORE-12).
    pub fn rebase_current(&mut self, tree: Tree) {
        debug_assert_eq!(tree.len(), self.tree().len());
        self.entries[self.current].tree = tree;
    }

    /// Replaces the tree of the current entry (external growth, FILE-07).
    pub(crate) fn set_current_tree(&mut self, tree: Tree) {
        self.entries[self.current].tree = tree;
    }

    /// Drops every entry but the current one (history can no longer be served).
    pub fn collapse(&mut self) {
        let mut e = self.entries[self.current].clone();
        e.parent = None;
        e.children.clear();
        e.redo_child = None;
        e.change = None;
        let saved = self.saved == Some(self.current);
        self.entries = vec![e];
        self.current = 0;
        self.saved = saved.then_some(0);
    }

    /// Replaces trees of all entries (used after an external file swap).
    pub fn map_trees(&mut self, mut f: impl FnMut(&Tree) -> Tree) {
        for e in &mut self.entries {
            e.tree = f(&e.tree);
        }
    }

    pub fn items(&self) -> Vec<HistoryItem> {
        let mut on_path = vec![false; self.entries.len()];
        let mut n = Some(self.current);
        while let Some(i) = n {
            on_path[i] = true;
            n = self.entries[i].redo_child;
        }
        self.entries
            .iter()
            .enumerate()
            .map(|(id, e)| HistoryItem {
                id,
                parent: e.parent,
                label: e.label.clone(),
                time_ms: e.time_ms,
                current: id == self.current,
                saved: self.saved == Some(id),
                on_redo_path: on_path[id],
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::piece::Piece;

    fn t(b: &[u8]) -> Tree {
        Tree::from_piece(Piece::bytes(b.to_vec()))
    }

    fn ch() -> Change {
        Change {
            start: 0,
            removed: 0,
            inserted: 1,
        }
    }

    #[test]
    fn undo_redo_and_branches() {
        let mut h = History::new(t(b"a"));
        let first = h.commit(t(b"ab"), "1", ch());
        h.commit(t(b"abc"), "2", ch());
        assert!(h.is_modified());
        h.undo();
        h.undo();
        assert!(!h.is_modified());
        assert!(h.redo().is_some());
        assert_eq!(h.current(), first);
        // New branch.
        let branch = h.commit(t(b"abX"), "3", ch());
        assert!(!h.can_redo());
        h.undo();
        assert_eq!(h.redo(), Some(ch()));
        assert_eq!(h.current(), branch);
        // The old branch is still reachable.
        assert!(h.goto(2));
        assert_eq!(h.tree().len(), 3);
        h.undo();
        assert_eq!(h.redo(), Some(ch()));
        assert_eq!(h.current(), 2);
    }

    #[test]
    fn amend_only_on_leaf_unsaved_entries() {
        let mut h = History::new(t(b"a"));
        assert!(!h.can_amend(0));
        let id = h.commit(t(b"ab"), "1", ch());
        assert!(h.can_amend(id));
        h.mark_saved();
        assert!(!h.can_amend(id));
    }
}
