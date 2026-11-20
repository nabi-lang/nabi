//! Branch relaxation: which branches to labels take their short form.

use crate::lir::Label;

/// Which of the label branches `sites` (`(start, long length, label)`,
/// as laid out with every one long and the labels at `labels`, in code
/// units) take the `short`-unit form, which `reach(start, target)`
/// says reaches; the rest stay long. From all short, the ones out of
/// reach grow until none is: each step only grows code, so this ends.
#[must_use]
pub fn relax(
    sites: &[(usize, u8, Label)],
    labels: &[Option<usize>],
    short: u8,
    reach: &dyn Fn(usize, usize) -> bool,
) -> Vec<bool> {
    relax_each(sites, labels, short, &|_, at, to| reach(at, to))
}

/// `relax` where `reach(k, start, target)` is site `k`'s own reach.
#[must_use]
pub fn relax_each(
    sites: &[(usize, u8, Label)],
    labels: &[Option<usize>],
    short: u8,
    reach: &dyn Fn(usize, usize, usize) -> bool,
) -> Vec<bool> {
    let n = sites.len();
    let mut ok = vec![true; n];
    let mut before = vec![0usize; n + 1];
    for _ in 0..64 {
        for k in 0..n {
            let cut = if ok[k] {
                usize::from(sites[k].1 - short)
            } else {
                0
            };
            before[k + 1] = before[k] + cut;
        }
        // Where offset `x` (no branch's inside) moves.
        let at = |x: usize| x - before[sites.partition_point(|s| s.0 < x)];
        let mut grew = false;
        for k in 0..n {
            let Some(t) = labels[sites[k].2 as usize] else {
                return vec![false; n];
            };
            if ok[k] && !reach(k, at(sites[k].0), at(t)) {
                ok[k] = false;
                grew = true;
            }
        }
        if !grew {
            return ok;
        }
    }
    vec![false; n]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel8(at: usize, to: usize) -> bool {
        (-128..128).contains(&(to as i64 - at as i64 - 2))
    }

    #[test]
    fn short_where_it_reaches() {
        // jcc at 0 to label 0 at 130: in reach once shrunk; jmp at 6 to
        // label 1 at 400: never.
        let sites = [(0, 6, 0), (6, 5, 1)];
        let labels = [Some(130), Some(400)];
        assert_eq!(relax(&sites, &labels, 2, &rel8), vec![true, false]);
    }

    #[test]
    fn growing_one_pushes_another_out() {
        // The first spans the second, whose target is out of reach: it
        // grows, and the first's target moves out of reach with it.
        let sites = [(0, 6, 0), (10, 6, 1)];
        let labels = [Some(135), Some(400)];
        assert_eq!(relax(&sites, &labels, 2, &rel8), vec![false, false]);
        let labels = [Some(131), Some(400)];
        assert_eq!(relax(&sites, &labels, 2, &rel8), vec![true, false]);
    }
}
