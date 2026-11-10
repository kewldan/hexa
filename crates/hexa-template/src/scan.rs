//! Structure scan (SRCH-11): finds offsets where a template type parses
//! cleanly — no assertion failures or evaluation errors within limits.

use hexa_core::{Cancel, Tree};

use crate::ast::Program;
use crate::diag::{Error, Result};
use crate::eval::{EvalOptions, Limits, evaluate};

/// Options for [`scan_valid_with`].
#[derive(Debug, Clone, PartialEq)]
pub struct ScanOptions {
    /// Per-candidate evaluation limits (kept small: most candidates fail fast).
    pub limits: Limits,
    /// Stop after this many matches.
    pub max_results: usize,
    /// Skip past a match instead of testing offsets inside it.
    pub skip_matched: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            limits: Limits {
                max_nodes: 100_000,
                max_steps: 1_000_000,
                eager_elements: 1_000,
                ..Limits::default()
            },
            max_results: 10_000,
            skip_matched: false,
        }
    }
}

/// Offsets in `range` (every `step` bytes) at which `type_name` evaluates
/// without errors (SRCH-11).
pub fn scan_valid(
    tree: &Tree,
    program: &Program,
    type_name: &str,
    range: std::ops::Range<u64>,
    step: u64,
    cancel: &Cancel,
) -> Result<Vec<u64>> {
    scan_valid_with(
        tree,
        program,
        type_name,
        range,
        step,
        &ScanOptions::default(),
        cancel,
    )
}

/// [`scan_valid`] with explicit options.
pub fn scan_valid_with(
    tree: &Tree,
    program: &Program,
    type_name: &str,
    range: std::ops::Range<u64>,
    step: u64,
    opts: &ScanOptions,
    cancel: &Cancel,
) -> Result<Vec<u64>> {
    if step == 0 {
        return Err(Error::invalid("scan step must be at least 1"));
    }
    if program.type_def(type_name).is_none() && crate::prim::Prim::from_name(type_name).is_none() {
        return Err(Error::invalid(format!("unknown type `{type_name}`")));
    }
    let end = range.end.min(tree.len());
    let mut out = Vec::new();
    let mut off = range.start;
    while off < end && out.len() < opts.max_results {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let eo = EvalOptions {
            base_offset: off,
            root_type: Some(type_name.to_string()),
            limits: opts.limits.clone(),
            ..EvalOptions::default()
        };
        let r = evaluate(program, tree, &eo, cancel)?;
        let size = r
            .roots
            .first()
            .and_then(|id| r.node(*id))
            .map_or(0, |n| n.size);
        if r.is_ok() && !r.truncated && size > 0 {
            out.push(off);
            if opts.skip_matched {
                off = off.saturating_add(size.max(step).next_multiple_of(step));
                continue;
            }
        }
        off = off.saturating_add(step);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hexa_core::piece::Piece;

    #[test]
    fn finds_valid_records() {
        let src = r#"struct Rec { char magic[2]; assert(magic == "RC", "magic"); u8 len; u8 data[len]; };"#;
        let program = crate::parse(src).unwrap();
        let mut data = vec![0u8; 4];
        data.extend_from_slice(b"RC\x02ab");
        data.extend_from_slice(&[9, 9]);
        data.extend_from_slice(b"RC\x09"); // truncated: length past the end
        let tree = Tree::from_piece(Piece::bytes(data));
        let c = Cancel::new();
        assert_eq!(
            scan_valid(&tree, &program, "Rec", 0..100, 1, &c).unwrap(),
            [4]
        );
        assert!(scan_valid(&tree, &program, "Nope", 0..10, 1, &c).is_err());
        assert!(scan_valid(&tree, &program, "Rec", 0..10, 0, &c).is_err());
        let cancel = Cancel::new();
        cancel.cancel();
        assert!(scan_valid(&tree, &program, "Rec", 0..10, 1, &cancel).is_err());
    }

    #[test]
    fn skip_matched_and_limits() {
        let program = crate::parse("struct W { u16 v; assert(v != 0, \"zero\"); };").unwrap();
        let tree = Tree::from_piece(Piece::bytes(vec![1, 1, 1, 1, 0, 0, 1, 1]));
        let c = Cancel::new();
        assert_eq!(
            scan_valid(&tree, &program, "W", 0..8, 1, &c).unwrap(),
            [0, 1, 2, 3, 5, 6]
        );
        let opts = ScanOptions {
            skip_matched: true,
            max_results: 2,
            ..ScanOptions::default()
        };
        assert_eq!(
            scan_valid_with(&tree, &program, "W", 0..8, 1, &opts, &c).unwrap(),
            [0, 2]
        );
    }
}
