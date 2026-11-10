//! Myers' O(ND) difference algorithm on bytes, bounded by a maximal edit
//! distance (DIFF-02).

/// One step of an edit script turning `a` into `b`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edit {
    Equal(usize),
    Delete(usize),
    Insert(usize),
}

/// Shortest edit script of `a` → `b`, or `None` when it needs more than
/// `max_d` insertions and deletions. Memory is `O(max_d²)`.
pub(crate) fn diff(a: &[u8], b: &[u8], max_d: usize) -> Option<Vec<Edit>> {
    let n = a.len() as isize;
    let m = b.len() as isize;
    let max = (a.len() + b.len()).min(max_d) as isize;
    let off = max + 1;
    let mut v = vec![0i32; 2 * max as usize + 3];
    let mut trace: Vec<Vec<i32>> = Vec::new();
    for d in 0..=max {
        trace.push(v[(off - d) as usize..=(off + d) as usize].to_vec());
        let mut k = -d;
        while k <= d {
            let i = (off + k) as usize;
            let mut x = if k == -d || (k != d && v[i - 1] < v[i + 1]) {
                v[i + 1] as isize
            } else {
                v[i - 1] as isize + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[i] = x as i32;
            if x >= n && y >= m {
                return Some(backtrack(&trace, n, m, d));
            }
            k += 2;
        }
    }
    None
}

fn backtrack(trace: &[Vec<i32>], n: isize, m: isize, d_end: isize) -> Vec<Edit> {
    let mut rev: Vec<Edit> = Vec::new();
    let mut push = |e: Edit| match (rev.last_mut(), e) {
        (Some(Edit::Equal(x)), Edit::Equal(y))
        | (Some(Edit::Delete(x)), Edit::Delete(y))
        | (Some(Edit::Insert(x)), Edit::Insert(y)) => *x += y,
        (_, e) => {
            let zero = matches!(e, Edit::Equal(0) | Edit::Delete(0) | Edit::Insert(0));
            if !zero {
                rev.push(e);
            }
        }
    };
    let (mut x, mut y) = (n, m);
    for d in (0..=d_end).rev() {
        if d == 0 {
            debug_assert_eq!(x, y);
            push(Edit::Equal(x as usize));
            break;
        }
        // Values after step d-1, indexed by k + d.
        let vd = &trace[d as usize];
        let get = |k: isize| vd[(k + d) as usize] as isize;
        let k = x - y;
        let prev_k = if k == -d || (k != d && get(k - 1) < get(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = get(prev_k);
        let prev_y = prev_x - prev_k;
        let mid_x = if prev_k == k + 1 { prev_x } else { prev_x + 1 };
        push(Edit::Equal((x - mid_x) as usize));
        if prev_k == k + 1 {
            push(Edit::Insert(1));
        } else {
            push(Edit::Delete(1));
        }
        x = prev_x;
        y = prev_y;
    }
    rev.reverse();
    rev
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn apply(a: &[u8], b: &[u8], edits: &[Edit]) -> (Vec<u8>, usize) {
        let (mut i, mut j) = (0, 0);
        let mut out = Vec::new();
        let mut cost = 0;
        for e in edits {
            match *e {
                Edit::Equal(n) => {
                    assert_eq!(a[i..i + n], b[j..j + n]);
                    out.extend_from_slice(&a[i..i + n]);
                    i += n;
                    j += n;
                }
                Edit::Delete(n) => {
                    i += n;
                    cost += n;
                }
                Edit::Insert(n) => {
                    out.extend_from_slice(&b[j..j + n]);
                    j += n;
                    cost += n;
                }
            }
        }
        assert_eq!(i, a.len());
        (out, cost)
    }

    fn lcs(a: &[u8], b: &[u8]) -> usize {
        let mut dp = vec![vec![0usize; b.len() + 1]; a.len() + 1];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                dp[i][j] = if a[i] == b[j] {
                    dp[i + 1][j + 1] + 1
                } else {
                    dp[i + 1][j].max(dp[i][j + 1])
                };
            }
        }
        dp[0][0]
    }

    #[test]
    fn classic_example() {
        let e = diff(b"ABCABBA", b"CBABAC", 100).unwrap();
        let (out, cost) = apply(b"ABCABBA", b"CBABAC", &e);
        assert_eq!(out, b"CBABAC");
        assert_eq!(cost, 5);
        assert!(diff(b"ABCABBA", b"CBABAC", 4).is_none());
        assert_eq!(diff(b"", b"", 0).unwrap(), vec![]);
        assert_eq!(diff(b"", b"xy", 2).unwrap(), vec![Edit::Insert(2)]);
        assert_eq!(diff(b"xy", b"xy", 0).unwrap(), vec![Edit::Equal(2)]);
    }

    proptest! {
        #[test]
        fn optimal(a in proptest::collection::vec(0u8..4, 0..60),
                   b in proptest::collection::vec(0u8..4, 0..60)) {
            let e = diff(&a, &b, 1000).unwrap();
            let (out, cost) = apply(&a, &b, &e);
            prop_assert_eq!(out, b.clone());
            prop_assert_eq!(cost, a.len() + b.len() - 2 * lcs(&a, &b));
        }
    }
}
