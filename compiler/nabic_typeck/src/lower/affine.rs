//! Affine resources: a value whose type implements `Drop`
//! (a `File`, a socket, a channel `Sender`, an `Allocation`, ..) has one
//! owner. `let b = a`, storing it into a struct, a tuple, an enum or a
//! container, capturing it in a closure or an `async` block and returning
//! it move it; using the old name afterwards is an error, and so is a
//! second release. A struct, tuple or sum holding one is affine too.
//! `clone()` makes an independent value; `marker::replace` / `take` move a
//! part out; `marker::share` makes a handle nobody follows (`unsafe`).
//!
//! A call takes a value only when the callee keeps it: stores or returns
//! it, closes it, or passes it to a call that keeps it. Any other call
//! borrows it, and the caller keeps it. What a callee keeps is a
//! per-function summary, a call-graph fixpoint that ships in NABIHIR.
//!
//! Releasing: the lowering binds each affine `let` (and parameter, and
//! coroutine capture) to a hidden flag and a release on every exit of its
//! scope while the flag is set, as `using` does; this pass clears the flag
//! where the value moves or is closed, and leaves it unset for a value the
//! body does not own. A type whose close can fail (`io::Close`) must be
//! closed explicitly, `try x.close()` or `using`: leaving its scope open on
//! a path is an error, except on a path an error leaves by (its release
//! there drops the close's own error).
//!
//! The walk is a forward flow over structured HIR, per value part
//! (`h.f`): live, moved, released. Joins are unions; loops run to a
//! fixpoint. Values taken out of a container or read through a raw
//! pointer are not followed. A cancelled task leaves through its
//! cleanups: that path is not checked for a missing close.

mod flow;
mod release;

pub(crate) use crate::lower::affine::release::*;
pub(crate) use nabic_hir_check::is_hole_temp;

use crate::lower::throws::{ErrFrame, path_expr};
use crate::lower::{Env, LowerCtx};
use crate::{HirBlock, HirExpr, HirExprKind, HirFunction, HirPattern, HirStmt, HirTy};
use nabic_ast::{Block, Expr, ExprKind, Stmt, StmtKind};
use nabic_core::{HashMap, HashSet};
use nabic_diagnostic::{Code, NabiDiagnostic, Span, article};
use nabic_hir_check::{Keeps, STREAM_VIEWS};
use nabic_ir_common::{ConstVal, FunctionId, LocalId, PrimTy};
use std::cell::RefCell;
use std::collections::BTreeSet;

/// How a value of an affine type is released.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// The compiler releases it (`Drop::drop` cannot fail).
    Auto,
    /// Its close can fail: the program closes it.
    Close,
}

/// Lock guards: only `using` releases one, checked apart.
const GUARDS: &[&str] = &[
    "core::sync::MutexGuard",
    "core::sync::RwLockReadGuard",
    "core::sync::RwLockWriteGuard",
];

/// Secrets and the keys held in them: their `drop` only wipes early, and
/// the collector wipes them anyway, so copies are fine.
const WIPED: &[&str] = &[
    "core::crypto::Secret",
    "core::crypto::SecretWords",
    "core::crypto::Aead",
    "core::crypto::x25519::PrivateKey",
    "core::crypto::ed25519::SigningKey",
    "core::crypto::ecdsa::SigningKey",
    "core::crypto::rsa::PrivateKey",
    "core::crypto::mlkem::DecapsulationKey768",
];

/// A command is a configuration copied freely and run again: it is not
/// followed. The `Stdio` given to it moves into it (a `Stdio::File` owns
/// its file, which the run closes).
const VIEWS: &[&str] = &["std::process::Command"];

/// Name prefix of a release flag (`#own<local>`) and of the binding it
/// guards (`#rel<local>`).
const OWN: &str = "#own";
const REL: &str = "#rel";

/// Nesting past which a type is taken as not affine.
const DEPTH: u32 = 32;

/// The release kind of `ty`, if it is affine.
pub(crate) fn affine(ctx: &LowerCtx<'_>, ty: &HirTy) -> Option<Kind> {
    if !matches!(
        ty,
        HirTy::Struct(_) | HirTy::Enum(_) | HirTy::Tuple(_) | HirTy::Sum(_)
    ) {
        return None;
    }
    affine_in(ctx, ty, 0)
}

/// `affine`, memoized per type: a type nesting itself twice per level
/// (`W<W<W<..>>>` with two fields) is walked once per distinct type. A
/// type met again while it is being classified counts as not affine.
fn affine_in(ctx: &LowerCtx<'_>, ty: &HirTy, depth: u32) -> Option<Kind> {
    if depth > DEPTH {
        return None;
    }
    if let Some(k) = ctx.env.affine_memo.borrow().get(ty) {
        return *k;
    }
    ctx.env.affine_memo.borrow_mut().insert(ty.clone(), None);
    let k = affine_walk(ctx, ty, depth);
    ctx.env.affine_memo.borrow_mut().insert(ty.clone(), k);
    k
}

fn affine_walk(ctx: &LowerCtx<'_>, ty: &HirTy, depth: u32) -> Option<Kind> {
    let join = |a: Option<Kind>, b: Option<Kind>| match (a, b) {
        (Some(Kind::Close), _) | (_, Some(Kind::Close)) => Some(Kind::Close),
        (Some(_), _) | (_, Some(_)) => Some(Kind::Auto),
        _ => None,
    };
    let parts = |ts: &[HirTy]| {
        ts.iter()
            .fold(None, |k, t| join(k, affine_in(ctx, t, depth + 1)))
    };
    match ty {
        HirTy::Struct(sid) => {
            let def = ctx.struct_def(*sid);
            let base = def.name.split("__").next().unwrap_or(&def.name);
            let base = base.split('<').next().unwrap_or(base);
            if GUARDS.contains(&base)
                || VIEWS.contains(&base)
                || STREAM_VIEWS.contains(&base)
                || WIPED.iter().any(|w| {
                    base.ends_with(w.rsplit("::").next().unwrap_or(w))
                        && base.starts_with("core::crypto::")
                })
            {
                return None;
            }
            let own = ctx.has_trait(ty, ctx.lang("drop_method"));
            let close =
                !ctx.lang("close_trait").is_empty() && ctx.has_trait(ty, ctx.lang("close_trait"));
            let fields: Vec<HirTy> = def.fields.iter().map(|f| f.ty.clone()).collect();
            let inner = parts(&fields);
            if close {
                return Some(Kind::Close);
            }
            // Its own `drop` releases its parts and cannot fail.
            if own { Some(Kind::Auto) } else { inner }
        }
        HirTy::Enum(eid) => {
            let def = ctx.enum_def(*eid);
            if VIEWS.contains(&def.name.split("__").next().unwrap_or(&def.name)) {
                return None;
            }
            let tys: Vec<HirTy> = def
                .variants
                .iter()
                .flat_map(crate::lower::expr::variant_arg_tys)
                .collect();
            parts(&tys)
        }
        HirTy::Tuple(ts) | HirTy::Sum(ts) => parts(ts),
        _ => None,
    }
}

/// A loop's write-back of a `match` arm's binding into the matched place
/// (`desugar::ARM_BACK`): the two are one value, nothing moves.
fn arm_back(value: &HirExpr) -> bool {
    let mut v = value;
    while let HirExprKind::WithField { value: x, .. } = &v.kind {
        v = x;
    }
    matches!(&v.kind, HirExprKind::Block(b) if b.label.as_deref() == Some(crate::lower::desugar::ARM_BACK))
}

/// `std::process::Stdio`, whose moves the command takes over.
fn is_stdio(ctx: &LowerCtx<'_>, ty: &HirTy) -> bool {
    matches!(ty, HirTy::Enum(e)
        if ctx.enum_def(*e).name.split("__").next() == Some("std::process::Stdio"))
}

// The check.

const LIVE: u8 = 1;
const MOVED: u8 = 2;
const RELEASED: u8 = 4;
/// Holds nothing to release (a variant with no resource in it): moving
/// it moves nothing.
const EMPTY: u8 = 8;

/// Pseudo parameter index of capture `k` of a closure or coroutine body.
const SLOT: u32 = 1 << 16;

/// Methods that close or release what they are called on.
const CLOSERS: &[&str] = &["close", "close_async", "unmap", "wipe", "wait_with_output"];

/// What a function does with its parameters, as callers see it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Summary {
    /// Parameters (and captures, `SLOT + k`) the call keeps.
    keeps: BTreeSet<u32>,
    /// The result may be a value it does not own (a container's element).
    alias: bool,
}

impl Summary {
    fn encode(&self) -> String {
        let keeps: Vec<String> = self.keeps.iter().map(u32::to_string).collect();
        format!("{}|{}", keeps.join(","), u8::from(self.alias))
    }

    fn decode(s: &str) -> Option<Self> {
        let (k, a) = s.split_once('|')?;
        let keeps = k
            .split(',')
            .filter(|x| !x.is_empty())
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        Some(Self {
            keeps,
            alias: a == "1",
        })
    }
}

/// `core::marker::share`: a handle that is not followed; `view`:
/// one the hand-off check ties to its owner's scope instead.
const SHARES: &[&str] = &["core::marker::share", "core::marker::view"];
/// `marker::replace(ref place, v)` / `marker::take(ref place)`: the part
/// moves out, the place keeps a new value.
const SWAPS: &[&str] = &["core::marker::replace", "core::marker::take"];

/// What a not yet started frame runs on a hand-off's poll.
const PRIME_FRAME: &str = "core::future::prime_frame";

/// Key prefix of a summary in the stored list (beside the hand-off ones).
const KEY: &str = "#affine\u{1f}";

/// What an expression's value is, for the binding that takes it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Val {
    /// Not affine (or `none`): nothing to follow.
    Plain,
    /// A value this body owns.
    Owned,
    /// An affine value it does not own (a container's element).
    Alias,
}

const fn combine(a: Val, b: Val) -> Val {
    match (a, b) {
        (Val::Alias, _) | (_, Val::Alias) => Val::Alias,
        (Val::Owned, _) | (_, Val::Owned) => Val::Owned,
        _ => Val::Plain,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Use,
    Move,
}

type State = Option<Vec<u8>>;

fn join(a: State, b: State) -> State {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(mut a), Some(b)) => {
            if a.len() < b.len() {
                a.resize(b.len(), 0);
            }
            for (x, y) in a.iter_mut().zip(b) {
                *x |= y;
            }
            Some(a)
        }
    }
}

fn get(st: &State, i: usize) -> u8 {
    st.as_ref().and_then(|v| v.get(i).copied()).unwrap_or(0)
}

fn set(st: &mut State, i: usize, v: u8) {
    if let Some(s) = st {
        if s.len() <= i {
            s.resize(i + 1, 0);
        }
        s[i] = v;
    }
}

struct Checker<'a, 'e> {
    env: &'a Env<'e>,
    ctx: &'a LowerCtx<'e>,
    fns: HashMap<FunctionId, &'a HirFunction>,
    sums: RefCell<HashMap<FunctionId, Summary>>,
    /// Captures a creator moved in: (body, capture index).
    owned_slots: RefCell<HashSet<(FunctionId, u32)>>,
    memo: RefCell<HashMap<HirTy, Option<Shape>>>,
}

/// A type's release kind and its affine parts (field paths).
type Shape = (Kind, Vec<Vec<u32>>);

/// Rewrites a final walk asks for.
#[derive(Default)]
struct Edits {
    /// Flag `let`s / assignments (their value) to start unset.
    unset: HashSet<usize>,
    /// Places a value moves out of: the flags to clear there.
    clears: HashMap<usize, Vec<LocalId>>,
}

fn addr(e: &HirExpr) -> usize {
    std::ptr::from_ref(e) as usize
}

impl Checker<'_, '_> {
    /// The release kind and the affine parts (field paths) of `ty`.
    fn shape(&self, ty: &HirTy) -> Option<Shape> {
        let kind = affine(self.ctx, ty)?;
        if let Some(s) = self.memo.borrow().get(ty) {
            return s.clone();
        }
        let mut leaves = Vec::new();
        self.leaves(ty, &mut Vec::new(), &mut leaves, 0);
        let s = Some((kind, leaves));
        self.memo.borrow_mut().insert(ty.clone(), s.clone());
        s
    }

    /// The affine parts of `ty` below `path`: a struct without its own
    /// `Drop` and a tuple are split into their fields, a sum into its
    /// members (the index of the member is a path step).
    fn leaves(&self, ty: &HirTy, path: &mut Vec<u32>, out: &mut Vec<Vec<u32>>, depth: u32) {
        if affine(self.ctx, ty).is_none() {
            return;
        }
        let fields: Option<Vec<HirTy>> = match ty {
            HirTy::Struct(sid)
                if depth < 8 && !self.ctx.has_trait(ty, self.ctx.lang("drop_method")) =>
            {
                Some(
                    self.ctx
                        .struct_def(*sid)
                        .fields
                        .iter()
                        .map(|f| f.ty.clone())
                        .collect(),
                )
            }
            HirTy::Tuple(ts) | HirTy::Sum(ts) if depth < 8 => Some(ts.clone()),
            _ => None,
        };
        match fields {
            Some(fs) => {
                for (i, t) in fs.iter().enumerate() {
                    path.push(u32::try_from(i).unwrap_or(u32::MAX));
                    self.leaves(t, path, out, depth + 1);
                    path.pop();
                }
            }
            None => out.push(path.clone()),
        }
    }

    fn summary(&self, id: FunctionId) -> Option<Summary> {
        self.sums.borrow().get(&id).cloned()
    }

    fn ty_name(&self, ty: &HirTy) -> String {
        self.ctx.ty_name(ty)
    }
}

/// One followed value: a local and its parts.
struct Root {
    /// The variable, or the source of a hidden temporary.
    name: String,
    /// `name` is a variable the program wrote.
    named: bool,
    ty: HirTy,
    kind: Kind,
    /// This body must release it.
    owned: bool,
    /// A borrowed parameter (or capture): moving it keeps it.
    param: Option<u32>,
    leaves: Vec<Vec<u32>>,
    base: usize,
    at: Span,
}

struct Target {
    label: Option<String>,
    is_loop: bool,
    depth: usize,
    breaks: State,
    conts: State,
    /// What the `break`s carry out.
    val: Option<Val>,
}

#[allow(clippy::struct_excessive_bools)] // independent walk modes
struct Walk<'c, 'a, 'e> {
    c: &'c Checker<'a, 'e>,
    f: &'a HirFunction,
    report: bool,
    /// On a path that is not checked (a cancelled task's way out).
    quiet: bool,
    /// Walking a panic's message: the process ends, nothing is closed.
    dying: bool,
    /// Locals holding a frame's captures through `prime_frame`.
    primed: HashSet<LocalId>,
    /// Binding an assignment's value.
    assigning: bool,
    roots: Vec<Root>,
    root_of: HashMap<LocalId, usize>,
    /// Pattern bindings that name a part of a followed place.
    alias_of: HashMap<LocalId, (usize, Vec<u32>)>,
    slots: usize,
    /// Slot -> where it last moved, and whether that released it.
    why: HashMap<usize, (Span, bool)>,
    /// Slot -> the place that moved it (`p` for `p.f` when `p` moved).
    gave: HashMap<usize, Vec<u32>>,
    /// Captures moved into a closure without a capture list.
    captured: HashSet<Span>,
    /// Slot -> the match arm that left it open: its binding, the
    /// binding's type and the arm's body.
    arm_open: HashMap<usize, (LocalId, HirTy, Span, Vec<u32>)>,
    scopes: Vec<Vec<usize>>,
    /// Root -> the `let` that redeclared its variable in its scope.
    hidden: HashMap<usize, Span>,
    targets: Vec<Target>,
    out: Summary,
    /// Params (and captures) owned at entry.
    owned_params: HashSet<u32>,
    flags: HashMap<LocalId, LocalId>,
    flag_locals: HashSet<LocalId>,
    /// Var -> whether its last binding was owned (for its flag).
    bound: HashMap<LocalId, bool>,
    edits: Edits,
    /// The result of the last call with `ref` parameters.
    ref_result: Val,
    inits: HashMap<LocalId, &'a HirExpr>,
    env_local: Option<LocalId>,
    errors: Vec<NabiDiagnostic>,
    seen: HashSet<(u32, u32, String)>,
    last: Span,
}

impl<'c, 'a, 'e> Walk<'c, 'a, 'e> {
    fn new(c: &'c Checker<'a, 'e>, f: &'a HirFunction, report: bool) -> Self {
        let mut flags = HashMap::default();
        let mut flag_locals = HashSet::default();
        for l in &f.locals {
            if let Some(n) = l.name.as_deref().and_then(|n| n.strip_prefix(OWN)) {
                if let Ok(v) = n.parse::<u32>() {
                    flags.insert(LocalId(v), l.id);
                    flag_locals.insert(l.id);
                }
            }
        }
        let mut inits = HashMap::default();
        if let Some(b) = &f.body {
            let mut lets = |b: &'a HirBlock| {
                for s in &b.stmts {
                    if let HirStmt::Let { local, init } = s {
                        inits.insert(*local, init);
                    }
                }
            };
            lets(b);
            crate::each_block(b, &mut |e| {
                if let HirExprKind::Block(b)
                | HirExprKind::Loop { body: b, .. }
                | HirExprKind::While { body: b, .. }
                | HirExprKind::If { then_block: b, .. } = &e.kind
                {
                    lets(b);
                }
            });
        }
        let env_local = f
            .params
            .first()
            .filter(|p| p.name == "__env")
            .map(|p| p.local);
        Self {
            c,
            f,
            report,
            quiet: false,
            dying: false,
            primed: HashSet::default(),
            assigning: false,
            roots: Vec::new(),
            root_of: HashMap::default(),
            alias_of: HashMap::default(),
            slots: 0,
            why: HashMap::default(),
            gave: HashMap::default(),
            captured: HashSet::default(),
            arm_open: HashMap::default(),
            scopes: vec![Vec::new()],
            hidden: HashMap::default(),
            targets: Vec::new(),
            out: Summary::default(),
            owned_params: HashSet::default(),
            flags,
            flag_locals,
            bound: HashMap::default(),
            edits: Edits::default(),
            ref_result: Val::Plain,
            inits,
            env_local,
            errors: Vec::new(),
            seen: HashSet::default(),
            last: f.span,
        }
    }

    fn local_name(&self, l: LocalId) -> Option<&'a str> {
        self.f
            .locals
            .get(l.0 as usize)
            .filter(|x| x.id == l)
            .or_else(|| self.f.locals.iter().find(|x| x.id == l))
            .and_then(|x| x.name.as_deref())
            .filter(|n| !n.starts_with('#') && !n.starts_with("__"))
    }

    /// A `let` of `l` that redeclares a followed variable of this
    /// scope: the old value stays as it was, out of reach by name.
    fn note_hidden(&mut self, l: LocalId, at: Span) {
        let (Some(name), Some(scope)) = (self.local_name(l), self.scopes.last()) else {
            return;
        };
        for &r in scope {
            let root = &self.roots[r];
            if root.named && root.name == name && self.root_of.get(&l) != Some(&r) {
                self.hidden.entry(r).or_insert(at);
            }
        }
    }

    /// `prime_frame(captures, ..)`: a frame's captures, copied on a
    /// hand-off and assigned back; the frame keeps owning them.
    fn let_stmt(&mut self, local: LocalId, init: &'a HirExpr, st: State) -> State {
        if self.flag_locals.contains(&local) {
            self.flag_init(local, init);
            return st;
        }
        if let Some(args) = self.prime_args(init) {
            self.primed.insert(local);
            return self.prime(args, st);
        }
        self.bind_value(local, init, st)
    }

    /// The captures handed to `prime_frame` are only read.
    fn prime(&mut self, args: &'a [HirExpr], mut st: State) -> State {
        for a in args {
            let parts = match &a.kind {
                HirExprKind::Tuple { fields } => fields.iter().collect(),
                _ => vec![a],
            };
            for x in parts {
                st = self.expr(x, st, Mode::Use).0;
            }
        }
        st
    }

    /// A capture back from `prime_frame`: still the same value.
    fn primed_back(&self, value: &HirExpr) -> bool {
        matches!(&value.kind, HirExprKind::TupleField { target, .. }
            if matches!(target.kind, HirExprKind::Local(l) if self.primed.contains(&l)))
    }

    fn prime_args(&self, e: &'a HirExpr) -> Option<&'a [HirExpr]> {
        let HirExprKind::Call { callee, args } = &e.kind else {
            return None;
        };
        self.c
            .fns
            .get(callee)
            .is_some_and(|f| f.name.split("__mono__").next() == Some(PRIME_FRAME))
            .then_some(args.as_slice())
    }

    fn local_name_raw(&self, l: LocalId) -> Option<&'a str> {
        self.f
            .locals
            .get(l.0 as usize)
            .filter(|x| x.id == l)
            .or_else(|| self.f.locals.iter().find(|x| x.id == l))
            .and_then(|x| x.name.as_deref())
    }

    fn local_ty(&self, l: LocalId) -> Option<&'a HirTy> {
        self.f
            .locals
            .get(l.0 as usize)
            .filter(|x| x.id == l)
            .or_else(|| self.f.locals.iter().find(|x| x.id == l))
            .map(|x| &x.ty)
    }

    fn error(
        &mut self,
        code: Code,
        at: Span,
        msg: String,
        extra: impl FnOnce(NabiDiagnostic) -> NabiDiagnostic,
    ) {
        if !self.report || self.quiet || at.is_dummy() {
            return;
        }
        if !self.seen.insert((at.file, at.start, msg.clone())) {
            return;
        }
        let d = extra(NabiDiagnostic::error(code, at, msg));
        self.errors.push(d);
    }

    fn line(&self, s: Span, from: Span) -> String {
        if s.file == from.file {
            format!("line {}", s.line)
        } else {
            let path = self.c.env.sources.path(s.file).unwrap_or_default();
            let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
            format!("{name}:{}", s.line)
        }
    }

    /// Follow local `l` (of `ty`) from here: a new root, or the old one
    /// reset.
    fn bind(
        &mut self,
        l: LocalId,
        ty: &HirTy,
        owned: bool,
        param: Option<u32>,
        at: Span,
        st: &mut State,
    ) -> Option<usize> {
        let (kind, leaves) = self.c.shape(ty)?;
        if leaves.is_empty() {
            return None;
        }
        let fresh = !self.root_of.contains_key(&l);
        let r = if let Some(&r) = self.root_of.get(&l) {
            self.roots[r].owned = owned;
            self.roots[r].param = param;
            r
        } else {
            let (name, named) = match self.local_name(l) {
                Some(n) => (n.to_owned(), true),
                None => (
                    self.c
                        .ctx
                        .src(at)
                        .filter(|t| !t.is_empty() && t.len() <= 40 && !t.contains('\n'))
                        .unwrap_or("")
                        .to_owned(),
                    false,
                ),
            };
            let base = self.slots;
            self.slots += leaves.len();
            self.roots.push(Root {
                name,
                named,
                ty: ty.clone(),
                kind,
                owned,
                param,
                leaves,
                base,
                at,
            });
            let r = self.roots.len() - 1;
            self.root_of.insert(l, r);
            r
        };
        self.roots[r].at = at;
        let root = &self.roots[r];
        for i in 0..root.leaves.len() {
            set(st, root.base + i, LIVE);
            self.why.remove(&(root.base + i));
            self.arm_open.remove(&(root.base + i));
        }
        // A variable leaves with the scope that declared it (an
        // assignment declares nothing).
        if fresh || !self.assigning {
            if let Some(s) = self.scopes.last_mut() {
                if !s.contains(&r) {
                    s.push(r);
                }
            }
        }
        Some(r)
    }

    /// Stop following local `l` (it now holds what this body does not own).
    fn unbind(&self, l: LocalId, st: &mut State) {
        if let Some(&r) = self.root_of.get(&l) {
            let root = &self.roots[r];
            for i in 0..root.leaves.len() {
                set(st, root.base + i, 0);
            }
        }
    }

    /// The root and field path `e` reads, if it is a followed place.
    fn place(&self, e: &HirExpr) -> Option<(usize, Vec<u32>)> {
        match &e.kind {
            HirExprKind::Local(l) => self
                .alias_of
                .get(l)
                .cloned()
                .or_else(|| self.root_of.get(l).map(|&r| (r, Vec::new()))),
            HirExprKind::FieldRead {
                target,
                field_index,
                ..
            } => {
                let (r, mut p) = self.place(target)?;
                p.push(*field_index);
                Some((r, p))
            }
            HirExprKind::TupleField { target, index, .. } => {
                let (r, mut p) = self.place(target)?;
                p.push(*index);
                Some((r, p))
            }
            _ => None,
        }
    }

    /// The slots of root `r` at `path` and below (or the one `path` is
    /// inside of).
    fn slots_at(&self, r: usize, path: &[u32]) -> Vec<usize> {
        let root = &self.roots[r];
        root.leaves
            .iter()
            .enumerate()
            .filter(|(_, l)| l.starts_with(path) || path.starts_with(l))
            .map(|(i, _)| root.base + i)
            .collect()
    }

    /// How a diagnostic names the place `(r, path)`: `` `x.f` ``, or for
    /// a hidden temporary the expression it holds the value of.
    fn shown(&self, r: usize, path: &[u32]) -> String {
        let root = &self.roots[r];
        let place = self.place_text(r, path);
        let alias = self
            .alias_of
            .iter()
            .any(|(l, (x, q))| *x == r && path.starts_with(q) && self.local_name(*l).is_some());
        if root.named || alias {
            return format!("`{place}`");
        }
        let of = if root.name.is_empty() {
            "this value".to_owned()
        } else {
            format!("the value of `{}`", root.name)
        };
        match place.split_once('.') {
            Some((_, f)) if f.bytes().all(|b| b.is_ascii_digit()) => format!("element {f} of {of}"),
            Some((_, f)) => format!("field `{f}` of {of}"),
            None => of,
        }
    }

    /// `path` cut before the first field of a standard library type.
    fn user_path(&self, r: usize, path: &[u32]) -> Vec<u32> {
        let mut t = self.roots[r].ty.clone();
        let mut out = Vec::new();
        for &i in path {
            t = match &t {
                HirTy::Struct(sid) => {
                    let def = self.c.ctx.struct_def(*sid);
                    if nabic_core::in_toolchain(&def.name) {
                        break;
                    }
                    def.fields
                        .get(i as usize)
                        .map_or(HirTy::Unknown, |f| f.ty.clone())
                }
                HirTy::Tuple(ts) | HirTy::Sum(ts) => {
                    ts.get(i as usize).cloned().unwrap_or(HirTy::Unknown)
                }
                _ => break,
            };
            out.push(i);
        }
        out
    }

    /// The type of the place `(r, path)`.
    fn ty_at(&self, r: usize, path: &[u32]) -> HirTy {
        let mut t = self.roots[r].ty.clone();
        for &i in path {
            t = match &t {
                HirTy::Struct(sid) => self
                    .c
                    .ctx
                    .struct_def(*sid)
                    .fields
                    .get(i as usize)
                    .map_or(HirTy::Unknown, |f| f.ty.clone()),
                HirTy::Tuple(ts) | HirTy::Sum(ts) => {
                    ts.get(i as usize).cloned().unwrap_or(HirTy::Unknown)
                }
                _ => t,
            };
        }
        t
    }

    /// The place `(r, path)` as written: from the pattern binding that
    /// names the longest part of it, if one is in scope.
    fn place_text(&self, r: usize, path: &[u32]) -> String {
        let root = &self.roots[r];
        let bound = self
            .alias_of
            .iter()
            .filter(|(_, (x, q))| *x == r && path.starts_with(q))
            .filter_map(|(l, (_, q))| Some((q.len(), *l, self.local_name(*l)?)))
            .max_by_key(|&(n, l, _)| (n, std::cmp::Reverse(l.0)));
        if let Some((n, l, name)) = bound {
            let ty = self.local_ty(l).cloned().unwrap_or(HirTy::Unknown);
            return self.part_text(name.to_owned(), ty, &path[n..]);
        }
        let s = if root.named {
            root.name.clone()
        } else {
            String::new()
        };
        self.part_text(s, root.ty.clone(), path)
    }

    /// `s`, of type `ty`, followed by the fields `path` steps through.
    fn part_text(&self, mut s: String, mut ty: HirTy, path: &[u32]) -> String {
        for &i in path {
            match &ty {
                HirTy::Struct(sid) => {
                    let def = self.c.ctx.struct_def(*sid);
                    match def.fields.get(i as usize) {
                        Some(f) => {
                            s = format!("{s}.{}", f.name);
                            ty = f.ty.clone();
                        }
                        None => break,
                    }
                }
                HirTy::Tuple(ts) => {
                    s = format!("{s}.{i}");
                    ty = ts.get(i as usize).cloned().unwrap_or(HirTy::Unknown);
                }
                // The member a pattern narrowed to: not written.
                HirTy::Sum(ts) => ty = ts.get(i as usize).cloned().unwrap_or(HirTy::Unknown),
                _ => break,
            }
        }
        s
    }

    /// A use of the place `(r, path)` at `at`: an error after a move.
    fn use_at(&mut self, r: usize, path: &[u32], at: Span, st: &State, releasing: bool) {
        for s in self.slots_at(r, path) {
            let v = get(st, s);
            if v & (MOVED | RELEASED) == 0 {
                continue;
            }
            let who = self.shown(r, path);
            let Some(&(site, released)) = self.why.get(&s) else {
                continue;
            };
            let maybe = v & LIVE != 0;
            let did = if released { "closed" } else { "moved" };
            let line = self.line(site, at);
            let head = if maybe {
                format!("{who} may have been {did} at {line}")
            } else {
                format!("{who} was {did} at {line}")
            };
            let msg = if releasing {
                format!("{head}; releasing it again here would release it twice")
            } else {
                format!("{head}, and is used here")
            };
            // The type of what moved: the used part when it is itself a
            // resource, else the place that gave it away.
            let gave_path = self.gave.get(&s).cloned().unwrap_or_else(|| path.to_vec());
            let ty_at = Some(self.ty_at(r, path))
                .filter(|t| self.c.shape(t).is_some())
                .unwrap_or_else(|| self.ty_at(r, &gave_path));
            let ty = self.c.ty_name(&ty_at);
            let a_ty = article(&ty);
            let help = if released {
                format!(
                    "help: {a_ty} is released once; do the work before it is closed, or open \
                     a new one"
                )
            } else if is_stdio(self.c.ctx, &ty_at) {
                "help: a `Stdio::File` belongs to the command it is given to, which closes it \
                 when it runs; give each command its own file \
                 (`Stdio::File(try f.try_clone())`)"
                    .to_owned()
            } else {
                let gave = self.shown(r, &gave_path);
                let copy = self.copy_method(&ty_at).map_or_else(String::new, |m| {
                    format!(", or keep a copy with `.{m}` before it moves")
                });
                let borrow = if self.captured.contains(&site) && self.roots[r].named {
                    format!(
                        "; to only borrow it, capture it with `[ref {}]`",
                        self.roots[r].name
                    )
                } else {
                    String::new()
                };
                format!(
                    "help: {a_ty} has one owner, and {gave} gave it away; use the new \
                     owner{copy}{borrow}"
                )
            };
            let here = if released {
                "closed here"
            } else {
                "moved here"
            };
            self.error(Code::USE_AFTER_MOVE, at, msg, |d| {
                d.with_secondary(site, here).with_note(help)
            });
            return;
        }
    }

    /// How a copy of a `ty` is made, if it can be: `clone()`, or
    /// `try_clone()` (a `File`, a socket).
    fn copy_method(&self, ty: &HirTy) -> Option<&'static str> {
        let ctx = self.c.ctx;
        let clone = ctx.lang("clone_trait");
        if !clone.is_empty() && ctx.has_trait(ty, clone) {
            Some("clone()")
        } else if self
            .c
            .env
            .methods_on(ty, "try_clone")
            .is_some_and(|m| !m.is_empty())
        {
            Some("try_clone()")
        } else {
            None
        }
    }

    /// Move (or release) the place `(r, path)`.
    fn move_at(&mut self, r: usize, path: &[u32], e: &HirExpr, st: &mut State, release: bool) {
        let at = e.span;
        self.use_at(r, path, at, st, false);
        let slots = self.slots_at(r, path);
        if !slots.is_empty() && slots.iter().all(|&s| get(st, s) == EMPTY) {
            return;
        }
        for &s in &slots {
            set(st, s, if release { RELEASED } else { MOVED });
            self.why.insert(s, (at, release));
            self.gave.insert(s, path.to_vec());
        }
        let root = &self.roots[r];
        if !root.owned {
            if let Some(p) = root.param {
                self.out.keeps.insert(p);
            }
        }
        if self.report {
            let flag = self.root_local(r).and_then(|l| self.flags.get(&l).copied());
            if let Some(flag) = flag {
                self.edits.clears.entry(addr(e)).or_default().push(flag);
            }
        }
    }

    fn root_local(&self, r: usize) -> Option<LocalId> {
        self.root_of.iter().find(|&(_, &x)| x == r).map(|(l, _)| *l)
    }

    /// A `drop` (from `using`, or a release the compiler adds) of `e`.
    fn dropped(&mut self, e: &'a HirExpr, st: &mut State) {
        let Some((r, path)) = self.place(e) else {
            let s = std::mem::take(st);
            *st = self.expr(e, s, Mode::Use).0;
            return;
        };
        for s in self.slots_at(r, &path) {
            let v = get(st, s);
            if v & MOVED != 0 {
                self.use_at(r, &path, e.span, st, true);
                break;
            }
        }
        for s in self.slots_at(r, &path) {
            if get(st, s) & LIVE != 0 {
                set(st, s, RELEASED);
                self.why.insert(s, (e.span, true));
            }
        }
        if !self.roots[r].owned {
            if let Some(p) = self.roots[r].param {
                self.out.keeps.insert(p);
            }
        }
        if self.report {
            let flag = self.root_local(r).and_then(|l| self.flags.get(&l).copied());
            if let Some(flag) = flag {
                self.edits.clears.entry(addr(e)).or_default().push(flag);
            }
        }
    }

    /// Roots of the scopes from `depth` on leave here: a value whose close
    /// can fail must be closed by now.
    fn leave(&mut self, st: &State, depth: usize, at: Span) {
        let roots: Vec<usize> = self.scopes[depth.min(self.scopes.len())..]
            .iter()
            .flatten()
            .copied()
            .collect();
        for r in roots {
            self.leaves_open(r, st, at);
        }
    }

    /// On a cancelled task's way out: every open leaf of an owned
    /// value whose close can fail counts as released.
    fn cancel_releases(&self, st: &mut State) {
        for root in &self.roots {
            if !root.owned || root.kind != Kind::Close {
                continue;
            }
            for i in 0..root.leaves.len() {
                let b = get(st, root.base + i);
                if b & LIVE != 0 {
                    set(st, root.base + i, (b & !LIVE) | RELEASED);
                }
            }
        }
    }

    fn leaves_open(&mut self, r: usize, st: &State, at: Span) {
        let root = &self.roots[r];
        if self.dying || !root.owned || root.kind != Kind::Close {
            return;
        }
        let open: Vec<usize> = (0..root.leaves.len())
            .filter(|&i| get(st, root.base + i) & LIVE != 0)
            .collect();
        let Some(&first) = open.first() else { return };
        if let Some((l, ty, arm, q)) = self.arm_open.get(&(root.base + first)).cloned() {
            if let Some(name) = self.local_name(l) {
                // The part of the binding left open (`x.b`), when it
                // closed the others.
                let leaf = &root.leaves[first];
                let rel = self.user_path(r, leaf);
                let rel = rel
                    .get(q.len()..)
                    .filter(|_| rel.starts_with(&q))
                    .unwrap_or(&[]);
                let mut whole = q.clone();
                whole.extend_from_slice(rel);
                let ty = if rel.is_empty() {
                    ty
                } else {
                    self.ty_at(r, &whole)
                };
                let name = self.part_text(name.to_owned(), self.ty_at(r, &q), rel);
                let ty = self.c.ty_name(&ty);
                let aty = article(&ty);
                let msg = format!("`{name}` holds {aty} that this arm never closes");
                let help = format!(
                    "help: closing {aty} can fail, so it is not closed for you: add \
                     `try {name}.close();` at the end of this arm"
                );
                self.error(Code::UNCLOSED_RESOURCE, root.at, msg, |d| {
                    d.with_secondary(arm, "it is still open when this arm ends")
                        .with_note(help)
                });
                return;
            }
        }
        let leaf = root.leaves[first].clone();
        let named = root.named;
        // The standard library's fields are private: name what holds them.
        let path = self.user_path(r, &leaf);
        let for_temp = self
            .root_local(r)
            .and_then(|l| self.local_name_raw(l))
            .is_some_and(|n| n.starts_with("#for"));
        let who = if !named && for_temp {
            "the value this loop walks".to_owned()
        } else {
            self.shown(r, &path)
        };
        let somewhere = get(st, self.roots[r].base + first) & (MOVED | RELEASED) != 0;
        let ty = self.c.ty_name(&self.ty_at(r, &leaf));
        let aty = article(&ty);
        let root = &self.roots[r];
        let bound = root.at;
        let msg = if somewhere {
            format!("{who} holds {aty} that is not closed on every path")
        } else {
            format!("{who} holds {aty} that is never closed")
        };
        let hidden = self.hidden.get(&r).copied();
        let help = if let Some(h) = hidden {
            format!(
                "help: a new `let` does not close the old `{name}`; close or move it before \
                 redeclaring it: add `try {}.close();` before line {}, or move it into the new \
                 value (`let {name} = io::BufReader::new({name});`)",
                self.place_text(r, &path),
                h.line,
                name = root.name
            )
        } else if for_temp {
            format!(
                "help: closing {aty} can fail, so it is not closed for you: bind it before the \
                 loop and close it after: `let it = ..; try for x in it {{ .. }} try it.close();`"
            )
        } else if root.named {
            format!(
                "help: closing {aty} can fail, so it is not closed for you: add `try {}.close();` \
                 before it leaves its scope, or bind it with `using {} = .. {{ .. }}`",
                self.place_text(r, &path),
                root.name
            )
        } else {
            format!(
                "help: closing {aty} can fail, so it is not closed for you: bind it with \
                 `let x = ..;` and add `try x.close();`, or use `using x = .. {{ .. }}`"
            )
        };
        let (at, why) = match hidden {
            Some(h) => (h, "it is still open when a new `let` hides it here"),
            None => (at, "it is still open when it leaves here"),
        };
        self.error(Code::UNCLOSED_RESOURCE, bound, msg, |d| {
            d.with_secondary(at, why).with_note(help)
        });
    }
}

/// The parents of closure and coroutine bodies.
fn parents(all: &[&HirFunction]) -> HashMap<FunctionId, FunctionId> {
    let mut out = HashMap::default();
    for f in all {
        let Some(body) = &f.body else { continue };
        crate::each_block(body, &mut |e| {
            if let HirExprKind::ClosureValue { fn_id: id, .. }
            | HirExprKind::CoroNew { body_fn: id, .. } = &e.kind
            {
                if *id != f.id {
                    out.entry(*id).or_insert(f.id);
                }
            }
        });
    }
    out
}

/// What each function does with its parameters (`Sums::keeps`), for
/// `check` and the hand-off check.
#[derive(Default)]
pub(crate) struct Sums(HashMap<FunctionId, Summary>);

impl Keeps for Sums {
    fn keeps(&self, f: FunctionId, i: u32) -> bool {
        self.0.get(&f).is_some_and(|s| s.keeps.contains(&i))
    }
}

/// The bodies holding an affine value.
fn walked<'h>(c: &Checker<'_, '_>, all: &[&'h HirFunction]) -> Vec<&'h HirFunction> {
    all.iter()
        .copied()
        .filter(|f| {
            f.body.is_some()
                && (f.params.iter().any(|p| c.shape(&p.ty).is_some())
                    || f.locals.iter().any(|l| c.shape(&l.ty).is_some())
                    || c.shape(&f.ret_ty).is_some())
        })
        .collect()
}

/// Every function's summary: those of prebuilt libraries (`stored`), and
/// this build's to a fixpoint.
pub(crate) fn summaries(
    env: &Env<'_>,
    functions: &[HirFunction],
    stored: &[(String, String)],
) -> Sums {
    if env.names.lang_item("drop_method").is_empty() {
        return Sums::default();
    }
    let no_subst = HashMap::default();
    let ctx = LowerCtx::new(env, &no_subst, None, Span::DUMMY);
    let extras = env.sink.extras.borrow();
    let all: Vec<&HirFunction> = functions.iter().chain(extras.iter()).collect();
    let by_name: HashMap<&str, &str> = stored
        .iter()
        .filter_map(|(n, s)| Some((n.strip_prefix(KEY)?, s.as_str())))
        .collect();
    let mut sums = HashMap::default();
    for f in all.iter().filter(|f| f.body.is_none()) {
        if let Some(s) = by_name
            .get(f.name.as_str())
            .and_then(|t| Summary::decode(t))
        {
            sums.insert(f.id, s);
        }
    }
    let c = Checker {
        env,
        ctx: &ctx,
        fns: all.iter().map(|f| (f.id, *f)).collect(),
        sums: RefCell::new(sums),
        owned_slots: RefCell::default(),
        memo: RefCell::default(),
    };
    let walked = walked(&c, &all);
    // Every parameter starts borrowed.
    for _ in 0..12 {
        let mut changed = false;
        for f in &walked {
            let mut w = Walk::new(&c, f, false);
            run(&mut w);
            let mut s = w.out;
            if let Some(old) = c.summary(f.id) {
                s.keeps.extend(old.keeps);
                s.alias |= old.alias;
            }
            if c.summary(f.id).as_ref() != Some(&s) {
                c.sums.borrow_mut().insert(f.id, s);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Sums(c.sums.into_inner())
}

/// Check every body; report each use after a move and each value whose
/// close can fail left open, and fix up the release flags. `sums`: from
/// `summaries`; returned: this build's, when `export`.
pub(crate) fn check(
    env: &Env<'_>,
    functions: &mut [HirFunction],
    sums: Sums,
    export: bool,
) -> Vec<(String, String)> {
    if env.names.lang_item("drop_method").is_empty() {
        return Vec::new();
    }
    let no_subst = HashMap::default();
    let ctx = LowerCtx::new(env, &no_subst, None, Span::DUMMY);
    let (out, edits) = {
        let extras = env.sink.extras.borrow();
        let all: Vec<&HirFunction> = functions.iter().chain(extras.iter()).collect();
        let c = Checker {
            env,
            ctx: &ctx,
            fns: all.iter().map(|f| (f.id, *f)).collect(),
            sums: RefCell::new(sums.0),
            owned_slots: RefCell::default(),
            memo: RefCell::default(),
        };
        let walked = walked(&c, &all);
        // The final walk, creators before the bodies they make.
        let parent = parents(&all);
        let depth = |mut id: FunctionId| {
            let mut d = 0;
            while let Some(p) = parent.get(&id) {
                id = *p;
                d += 1;
                if d > 64 {
                    break;
                }
            }
            d
        };
        let mut order = walked.clone();
        order.sort_by_key(|f| depth(f.id));
        let mut diags = Vec::new();
        let mut edits: HashMap<FunctionId, Edits> = HashMap::default();
        for f in &order {
            let mut w = Walk::new(&c, f, true);
            let keeps = c.summary(f.id).map(|s| s.keeps).unwrap_or_default();
            w.owned_params = keeps.into_iter().collect();
            for k in c
                .owned_slots
                .borrow()
                .iter()
                .filter(|(b, _)| *b == f.id)
                .map(|(_, k)| *k)
            {
                w.owned_params.insert(SLOT + k);
            }
            run(&mut w);
            diags.extend(w.errors);
            edits.insert(f.id, w.edits);
        }
        let out = if export {
            let sums = c.sums.borrow();
            walked
                .iter()
                .filter(|f| !f.name.contains(nabic_ir_common::MONO_MARK))
                .filter_map(|f| {
                    let s = sums.get(&f.id).filter(|s| !s.keeps.is_empty() || s.alias)?;
                    Some((format!("{KEY}{}", f.name), s.encode()))
                })
                .collect()
        } else {
            Vec::new()
        };
        env.sink.diagnostics.borrow_mut().extend(diags);
        (out, edits)
    };
    for f in functions.iter_mut() {
        if let Some(e) = edits.get(&f.id) {
            apply(f, e);
        }
    }
    for f in env.sink.extras.borrow_mut().iter_mut() {
        if let Some(e) = edits.get(&f.id) {
            apply(f, e);
        }
    }
    out
}

/// Walk `w`'s function from entry to its exits.
fn run(w: &mut Walk<'_, '_, '_>) {
    let f = w.f;
    let Some(body) = &f.body else { return };
    let mut st: State = Some(Vec::new());
    for (i, p) in f.params.iter().enumerate() {
        if p.by_ref || p.name == "__env" {
            continue;
        }
        let i = u32::try_from(i).unwrap_or(u32::MAX);
        let owned = w.owned_params.contains(&i);
        let ty = p.ty.clone();
        if w.bind(p.local, &ty, owned, Some(i), f.span, &mut st)
            .is_some()
        {
            w.bound.insert(p.local, owned);
        }
    }
    let (st, val) = w.block(body, st, Mode::Move);
    if val == Val::Alias {
        w.out.alias = true;
    }
    if st.is_some() {
        let at = body.trailing.as_ref().map_or(w.last, |t| t.span);
        w.leave(&st, 0, at);
    }
}

fn apply(f: &mut HirFunction, e: &Edits) {
    if e.unset.is_empty() && e.clears.is_empty() {
        return;
    }
    let Some(body) = &mut f.body else { return };
    // Pre-order: a node is rewritten before its children, which live on
    // the heap and keep the addresses the walk saw.
    crate::each_block_mut(body, &mut |x| {
        let a = std::ptr::from_ref::<HirExpr>(x) as usize;
        if e.unset.contains(&a) {
            x.kind = HirExprKind::Const(ConstVal::Bool(false));
            return;
        }
        let Some(flags) = e.clears.get(&a) else {
            return;
        };
        let span = x.span;
        let ty = x.ty.clone();
        let inner = std::mem::replace(x, flag_value(false, span));
        let stmts = flags
            .iter()
            .map(|&f| HirStmt::Assign {
                local: f,
                value: flag_value(false, Span::DUMMY),
            })
            .collect();
        *x = HirExpr {
            kind: HirExprKind::Block(Box::new(HirBlock {
                stmts,
                trailing: Some(inner),
                label: None,
            })),
            ty,
            span,
        };
    });
}
