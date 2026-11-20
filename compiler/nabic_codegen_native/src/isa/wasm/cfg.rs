//! Structured control flow from LIR labels and jumps: the dominator tree
//! translation of "Beyond Relooper" (Ramsey): a loop per loop header, a
//! block ending before each merge node, every other node placed where its
//! only predecessor branches to it. An irreducible graph runs as a
//! `br_table` dispatch loop instead.

use crate::isa::wasm::Em;
use crate::isa::wasm::enc::EMPTY;
use crate::lir::{Inst, Label, V};

/// How a node ends; `T` names nodes (while building: labels or nodes).
#[derive(Clone, Copy, Debug)]
enum Term<T = usize> {
    Goto(T),
    /// On the `I8` value, to the first node, else the second.
    Br(V, T, T),
    /// On `Em::ovf`, to the first node (the overflow), else the second.
    Ovf(T, T),
    /// `Inst::Ret` at this index.
    Ret(usize),
    Trap,
}

/// A jump target while building.
#[derive(Clone, Copy, Debug)]
enum To {
    Label(Label),
    Node(usize),
}

struct Node<T = usize> {
    /// Instruction indices of the body.
    insts: Vec<usize>,
    term: Term<T>,
}

pub struct Graph {
    nodes: Vec<Node>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Loop(usize),
    Block(usize),
    If,
}

impl Graph {
    pub fn new(insts: &[Inst]) -> Self {
        let mut nodes: Vec<Node<To>> = Vec::new();
        let new = |nodes: &mut Vec<Node<To>>| {
            nodes.push(Node {
                insts: Vec::new(),
                term: Term::Trap,
            });
            nodes.len() - 1
        };
        new(&mut nodes);
        let mut label_node: Vec<Option<usize>> = Vec::new();
        // The current node has no instructions yet: a label names it.
        let mut fresh = true;
        for (i, inst) in insts.iter().enumerate() {
            let cur = nodes.len() - 1;
            let term = match inst {
                // Positions only: the node stays fresh.
                Inst::Line(..) => {
                    nodes[cur].insts.push(i);
                    continue;
                }
                Inst::Label(l) => {
                    let l = *l as usize;
                    if label_node.len() <= l {
                        label_node.resize(l + 1, None);
                    }
                    if fresh {
                        label_node[l] = Some(cur);
                    } else {
                        let n = new(&mut nodes);
                        nodes[cur].term = Term::Goto(To::Node(n));
                        label_node[l] = Some(n);
                        fresh = true;
                    }
                    continue;
                }
                Inst::Jmp(l) => Term::Goto(To::Label(*l)),
                Inst::Br { c, t, f } => Term::Br(*c, To::Label(*t), To::Label(*f)),
                Inst::Ovf { fail, .. } => {
                    nodes[cur].insts.push(i);
                    Term::Ovf(To::Label(*fail), To::Node(cur + 1))
                }
                Inst::Ret { .. } => Term::Ret(i),
                Inst::Trap => Term::Trap,
                _ => {
                    nodes[cur].insts.push(i);
                    fresh = false;
                    continue;
                }
            };
            nodes[cur].term = term;
            new(&mut nodes);
            fresh = true;
        }
        let at = |t: To| match t {
            To::Node(n) => n,
            To::Label(l) => label_node[l as usize].expect("a bound label"),
        };
        let nodes = nodes
            .into_iter()
            .map(|n| Node {
                insts: n.insts,
                term: match n.term {
                    Term::Goto(a) => Term::Goto(at(a)),
                    Term::Br(c, a, b) => Term::Br(c, at(a), at(b)),
                    Term::Ovf(a, b) => Term::Ovf(at(a), at(b)),
                    Term::Ret(i) => Term::Ret(i),
                    Term::Trap => Term::Trap,
                },
            })
            .collect();
        Self { nodes }
    }

    fn succs(&self, n: usize) -> Vec<usize> {
        match self.nodes[n].term {
            Term::Goto(a) => vec![a],
            Term::Br(V::I(c), a, b) => vec![if c & 0xff != 0 { a } else { b }],
            Term::Br(_, a, b) | Term::Ovf(a, b) => vec![a, b],
            Term::Ret(_) | Term::Trap => Vec::new(),
        }
    }
}

/// Facts of the reachable graph.
struct Shape {
    /// Reverse postorder number (`usize::MAX`: unreachable).
    rpo: Vec<usize>,
    order: Vec<usize>,
    header: Vec<bool>,
    merge: Vec<bool>,
    /// Dominator tree children.
    kids: Vec<Vec<usize>>,
    reducible: bool,
}

fn shape(g: &Graph) -> Shape {
    let n = g.nodes.len();
    let succ: Vec<Vec<usize>> = (0..n).map(|i| g.succs(i)).collect();
    // Postorder by an explicit stack.
    let mut post = Vec::new();
    let mut seen = vec![false; n];
    let mut stack = vec![(0usize, 0usize)];
    seen[0] = true;
    while let Some(&mut (v, ref mut i)) = stack.last_mut() {
        if *i < succ[v].len() {
            let w = succ[v][*i];
            *i += 1;
            if !seen[w] {
                seen[w] = true;
                stack.push((w, 0));
            }
        } else {
            post.push(v);
            stack.pop();
        }
    }
    let order: Vec<usize> = post.iter().rev().copied().collect();
    let mut rpo = vec![usize::MAX; n];
    for (i, &v) in order.iter().enumerate() {
        rpo[v] = i;
    }
    let mut preds = vec![Vec::new(); n];
    for &v in &order {
        for &w in &succ[v] {
            preds[w].push(v);
        }
    }
    // Cooper, Harvey, Kennedy.
    let mut idom = vec![usize::MAX; n];
    idom[0] = 0;
    let mut changed = true;
    while changed {
        changed = false;
        for &v in order.iter().skip(1) {
            let mut new = usize::MAX;
            for &p in &preds[v] {
                if idom[p] == usize::MAX {
                    continue;
                }
                new = if new == usize::MAX {
                    p
                } else {
                    let (mut a, mut b) = (p, new);
                    while a != b {
                        while rpo[a] > rpo[b] {
                            a = idom[a];
                        }
                        while rpo[b] > rpo[a] {
                            b = idom[b];
                        }
                    }
                    a
                };
            }
            if idom[v] != new {
                idom[v] = new;
                changed = true;
            }
        }
    }
    let dominates = |a: usize, mut b: usize| loop {
        if a == b {
            return true;
        }
        if b == 0 {
            return false;
        }
        b = idom[b];
    };
    let mut header = vec![false; n];
    let mut forward = vec![0u32; n];
    let mut reducible = true;
    for &v in &order {
        for &w in &succ[v] {
            if rpo[w] <= rpo[v] {
                header[w] = true;
                reducible &= dominates(w, v);
            } else {
                forward[w] += 1;
            }
        }
    }
    let merge = forward.iter().map(|&f| f >= 2).collect();
    let mut kids = vec![Vec::new(); n];
    for &v in order.iter().skip(1) {
        kids[idom[v]].push(v);
    }
    Shape {
        rpo,
        order,
        header,
        merge,
        kids,
        reducible,
    }
}

impl Em<'_> {
    /// The code of `g`.
    pub(crate) fn structured(&mut self, g: &Graph, insts: &[Inst]) {
        let s = shape(g);
        if s.reducible {
            let mut ctx = Vec::new();
            self.tree(g, &s, insts, 0, &mut ctx);
        } else {
            self.dispatch(g, &s, insts);
        }
    }

    fn tree(&mut self, g: &Graph, s: &Shape, insts: &[Inst], x: usize, ctx: &mut Vec<Ctx>) {
        let mut merges: Vec<usize> = s.kids[x].iter().copied().filter(|&k| s.merge[k]).collect();
        merges.sort_unstable_by_key(|&k| std::cmp::Reverse(s.rpo[k]));
        if s.header[x] {
            self.a.loop_();
            ctx.push(Ctx::Loop(x));
            self.within(g, s, insts, x, &merges, ctx);
            ctx.pop();
            self.a.end();
        } else {
            self.within(g, s, insts, x, &merges, ctx);
        }
    }

    fn within(
        &mut self,
        g: &Graph,
        s: &Shape,
        insts: &[Inst],
        x: usize,
        ys: &[usize],
        ctx: &mut Vec<Ctx>,
    ) {
        if let Some((&y, rest)) = ys.split_first() {
            self.a.block();
            ctx.push(Ctx::Block(y));
            self.within(g, s, insts, x, rest, ctx);
            ctx.pop();
            self.a.end();
            self.tree(g, s, insts, y, ctx);
            return;
        }
        for &i in &g.nodes[x].insts {
            self.mark(i);
            self.inst(&insts[i]);
        }
        match g.nodes[x].term {
            Term::Goto(t) => self.branch(g, s, insts, x, t, ctx),
            Term::Br(V::I(c), t, f) => {
                self.branch(g, s, insts, x, if c & 0xff != 0 { t } else { f }, ctx);
            }
            Term::Br(c, t, f) => {
                self.used = [0; 4];
                self.val(c, crate::lir::K::I8);
                self.two(g, s, insts, x, t, f, ctx);
            }
            Term::Ovf(t, f) => {
                self.get(self.ovf);
                self.two(g, s, insts, x, t, f, ctx);
            }
            Term::Ret(i) => {
                if let Inst::Ret { vals } = &insts[i] {
                    self.ret(vals);
                }
            }
            Term::Trap => self.a.unreachable(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn two(
        &mut self,
        g: &Graph,
        s: &Shape,
        insts: &[Inst],
        x: usize,
        t: usize,
        f: usize,
        ctx: &mut Vec<Ctx>,
    ) {
        self.a.if_(EMPTY);
        ctx.push(Ctx::If);
        self.branch(g, s, insts, x, t, ctx);
        self.a.else_();
        self.branch(g, s, insts, x, f, ctx);
        ctx.pop();
        self.a.end();
    }

    fn branch(
        &mut self,
        g: &Graph,
        s: &Shape,
        insts: &[Inst],
        x: usize,
        t: usize,
        ctx: &mut Vec<Ctx>,
    ) {
        let back = s.rpo[t] <= s.rpo[x];
        if back || s.merge[t] {
            let want = if back { Ctx::Loop(t) } else { Ctx::Block(t) };
            let at = ctx
                .iter()
                .rposition(|c| *c == want)
                .expect("a branch target in scope");
            self.a.br((ctx.len() - 1 - at) as u32);
        } else {
            self.tree(g, s, insts, t, ctx);
        }
    }

    /// Every reachable node in a block of one loop, `br_table` on the
    /// node's position to enter it.
    fn dispatch(&mut self, g: &Graph, s: &Shape, insts: &[Inst]) {
        let n = s.order.len();
        let pc = self.new_local(crate::isa::wasm::I32);
        let mut pos = vec![usize::MAX; g.nodes.len()];
        for (i, &v) in s.order.iter().enumerate() {
            pos[v] = i;
        }
        self.a.loop_();
        for _ in 0..n {
            self.a.block();
        }
        self.get(pc);
        self.a.br_table(0..n as u32 - 1, n as u32 - 1);
        for (i, &v) in s.order.iter().enumerate() {
            self.a.end();
            let up = (n - 1 - i) as u64;
            let go = |e: &mut Self, t: usize, extra: u64| {
                e.i32c(pos[t] as i32);
                e.set(pc);
                e.a.br((up + extra) as u32);
            };
            for &k in &g.nodes[v].insts {
                self.mark(k);
                self.inst(&insts[k]);
            }
            match g.nodes[v].term {
                Term::Goto(t) => go(self, t, 0),
                Term::Br(V::I(c), t, f) => go(self, if c & 0xff != 0 { t } else { f }, 0),
                Term::Br(c, t, f) => {
                    self.used = [0; 4];
                    self.val(c, crate::lir::K::I8);
                    self.a.if_(EMPTY);
                    go(self, t, 1);
                    self.a.else_();
                    go(self, f, 1);
                    self.a.end();
                }
                Term::Ovf(t, f) => {
                    self.get(self.ovf);
                    self.a.if_(EMPTY);
                    go(self, t, 1);
                    self.a.else_();
                    go(self, f, 1);
                    self.a.end();
                }
                Term::Ret(r) => {
                    if let Inst::Ret { vals } = &insts[r] {
                        self.ret(vals);
                    }
                }
                Term::Trap => self.a.unreachable(),
            }
        }
        self.a.end();
    }
}
