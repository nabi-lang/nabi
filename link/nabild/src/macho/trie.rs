//! The dyld export trie: names to `(flags, image offset)`.

use nabic_core::leb::{uleb, uleb_len};

struct Node {
    terminal: Option<(u64, u64)>,
    edges: Vec<(Vec<u8>, usize)>,
}

fn build_node(nodes: &mut Vec<Node>, syms: &[(Vec<u8>, u64, u64)], depth: usize) -> usize {
    let id = nodes.len();
    nodes.push(Node {
        terminal: None,
        edges: Vec::new(),
    });
    let mut rest = syms;
    if let Some((n, f, a)) = rest.first() {
        if n.len() == depth {
            nodes[id].terminal = Some((*f, *a));
            rest = &rest[1..];
        }
    }
    while let Some((first, ..)) = rest.first() {
        let b = first[depth];
        let n = rest.iter().take_while(|s| s.0[depth] == b).count();
        let (group, tail) = rest.split_at(n);
        let last = &group[n - 1].0;
        let mut lcp = depth + 1;
        while lcp < first.len() && lcp < last.len() && first[lcp] == last[lcp] {
            lcp += 1;
        }
        let child = build_node(nodes, group, lcp);
        nodes[id].edges.push((first[depth..lcp].to_vec(), child));
        rest = tail;
    }
    id
}

/// Serialize the trie of `syms` (`name, flags, offset`).
#[must_use]
pub fn build(mut syms: Vec<(String, u64, u64)>) -> Vec<u8> {
    if syms.is_empty() {
        return Vec::new();
    }
    syms.sort_by(|a, b| a.0.cmp(&b.0));
    syms.dedup_by(|a, b| a.0 == b.0);
    let syms: Vec<(Vec<u8>, u64, u64)> = syms
        .into_iter()
        .map(|(n, f, a)| (n.into_bytes(), f, a))
        .collect();
    let mut nodes = Vec::new();
    build_node(&mut nodes, &syms, 0);
    let term = |n: &Node| n.terminal.map_or(0, |(f, a)| uleb_len(f) + uleb_len(a));
    let mut offs = vec![0usize; nodes.len()];
    loop {
        let mut at = 0;
        let mut changed = false;
        for (i, n) in nodes.iter().enumerate() {
            if offs[i] != at {
                offs[i] = at;
                changed = true;
            }
            let t = term(n);
            at += uleb_len(t as u64) + t + 1;
            for (label, child) in &n.edges {
                at += label.len() + 1 + uleb_len(offs[*child] as u64);
            }
        }
        if !changed {
            break;
        }
    }
    let mut out = Vec::new();
    for n in &nodes {
        match n.terminal {
            Some((f, a)) => {
                uleb(&mut out, (uleb_len(f) + uleb_len(a)) as u64);
                uleb(&mut out, f);
                uleb(&mut out, a);
            }
            None => out.push(0),
        }
        out.push(n.edges.len() as u8);
        for (label, child) in &n.edges {
            out.extend_from_slice(label);
            out.push(0);
            uleb(&mut out, offs[*child] as u64);
        }
    }
    while out.len() % 8 != 0 {
        out.push(0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nabic_core::HashSet;

    #[test]
    fn reads_back() {
        let names = [
            "__mh_execute_header",
            "_main",
            "_mainly",
            "_ma",
            "_zeta",
            "_a",
        ];
        let t = build(
            names
                .iter()
                .enumerate()
                .map(|(i, n)| ((*n).to_owned(), 0, i as u64 * 16))
                .collect(),
        );
        let mut got = HashSet::default();
        let mut d = crate::macho::dylib::Dylib::default();
        crate::macho::dylib::walk_trie_for_test(&t, &mut d);
        got.extend(d.exports);
        let want: HashSet<String> = names.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(got, want);
    }
}
