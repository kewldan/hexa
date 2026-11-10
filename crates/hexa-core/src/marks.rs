//! Bookmarks, labels and annotations anchored to data (NAV-14…18).
//!
//! Marks are byte ranges that follow the data: every edit, undo, redo and
//! history jump is replayed on them as a [`Change`], so a bookmark on a header
//! stays on that header when bytes are inserted in front of it.

use serde::{Deserialize, Serialize};

use crate::history::Change;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum MarkKind {
    #[default]
    Bookmark,
    /// A named offset usable in goto expressions (NAV-16).
    Label,
    /// A region with a Markdown note shown in the grid (NAV-17).
    Annotation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Mark {
    pub id: u64,
    pub kind: MarkKind,
    pub start: u64,
    pub end: u64,
    pub name: String,
    /// CSS colour or palette name.
    pub color: String,
    /// Markdown note.
    pub note: String,
    /// Data type shown for the range (smart bookmark), e.g. `u32` or `string`.
    pub data_type: Option<String>,
    /// Set when every byte of the mark was deleted.
    pub orphaned: bool,
}

impl Default for Mark {
    fn default() -> Self {
        Self {
            id: 0,
            kind: MarkKind::Bookmark,
            start: 0,
            end: 0,
            name: String::new(),
            color: "blue".into(),
            note: String::new(),
            data_type: None,
            orphaned: false,
        }
    }
}

/// Moves a range through one change (NAV-18).
///
/// Bytes outside the replaced span keep their identity; a mark overlapping the
/// replaced span grows or shrinks to cover the replacement. Insertions at the
/// very start of a mark push it forward; insertions at its end stay outside.
pub fn map_through(start: u64, end: u64, c: Change) -> (u64, u64, bool) {
    let c_end = c.start + c.removed;
    let delta = |v: u64| (i128::from(v) + i128::from(c.inserted) - i128::from(c.removed)) as u64;
    if end <= c.start && !(start == end && start == c.start && c.removed > 0) {
        return (start, end, false);
    }
    if start >= c_end {
        return (delta(start), delta(end), false);
    }
    // Overlap with the replaced span.
    let new_start = start.min(c.start);
    let new_end = if end > c_end {
        delta(end)
    } else {
        c.start + c.inserted
    };
    let fully_deleted = start >= c.start && end <= c_end && c.inserted == 0;
    (
        new_start,
        new_end.max(new_start),
        fully_deleted && end > start,
    )
}

/// Applies a change to every mark.
pub fn map_marks(marks: &mut [Mark], c: Change) {
    if c.removed == 0 && c.inserted == 0 {
        return;
    }
    for m in marks {
        let (s, e, gone) = map_through(m.start, m.end, c);
        m.start = s;
        m.end = e;
        if gone {
            m.orphaned = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(start: u64, removed: u64, inserted: u64) -> Change {
        Change {
            start,
            removed,
            inserted,
        }
    }

    #[test]
    fn marks_follow_their_bytes() {
        // Insert before: shift.
        assert_eq!(map_through(10, 20, ch(0, 0, 5)), (15, 25, false));
        // Insert exactly at the start: the mark keeps its bytes.
        assert_eq!(map_through(10, 20, ch(10, 0, 5)), (15, 25, false));
        // Insert at the end: outside.
        assert_eq!(map_through(10, 20, ch(20, 0, 5)), (10, 20, false));
        // Insert inside: grows.
        assert_eq!(map_through(10, 20, ch(15, 0, 5)), (10, 25, false));
        // Delete before: shift back.
        assert_eq!(map_through(10, 20, ch(0, 4, 0)), (6, 16, false));
        // Delete a part: shrink.
        assert_eq!(map_through(10, 20, ch(15, 10, 0)), (10, 15, false));
        // Delete everything: orphaned zero-length mark.
        assert_eq!(map_through(10, 20, ch(5, 20, 0)), (5, 5, true));
        // Overwrite inside: unchanged extent.
        assert_eq!(map_through(10, 20, ch(12, 2, 2)), (10, 20, false));
        // Edit after: unchanged.
        assert_eq!(map_through(10, 20, ch(30, 3, 9)), (10, 20, false));
    }
}
