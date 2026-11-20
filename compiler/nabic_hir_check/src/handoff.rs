//! The use-after-hand-off and scoped-view check.
//!
//! Use after hand-off is an error. A hand-off copies the value at run time:
//! after `Mutex::new(xs)` or `tx.send(xs)`, `xs` names an object no other
//! thread uses, so a write through it never reaches the new owner. A handle
//! taken from a guard (`g.get()`, `g.set(v)`, `g.update(f)`) is unprotected
//! once its `using` block ends. Such a handle is a scoped view: it may not be
//! returned, thrown, yielded or sent out of the block, nor passed to a callee
//! that keeps it. A `ThreadLocal::with` closure's value is one too, for the
//! closure's body (`VIEW`).
//!
//! A forward flow over structured HIR. A place is a local plus a field path
//! (`s`, `s.items`, `t.0`); it is gone on a path from the hand-off (or the
//! guard's drop) on, and using it, a place inside it or one containing it is
//! an error. Aliases are edges: `Same` (a copy, a struct/tuple field) makes
//! two places one object; `In` (a capture, an array/enum literal, a `match`
//! binding) holds one in the other both ways; `Into` (coroutine arguments)
//! one way only. A buffer's elements are one place, `buf ++ [ELEM]`: what is
//! stored into it and what is read out of it are that place, so handing over
//! either side takes the other along. An element taken out (`pop`, `remove`)
//! is `Taken` from it: handing it over takes what was stored there, not the
//! buffer, unless the buffer may hold one object twice (`St::dup`). Going
//! away takes every alias along; assigning a place re-arms it. A value read
//! out of a `Sync` object (an `unsafe impl Sync` type's field, a
//! thread-local's slot) is `WORLD`: other threads may reach it, so handing it
//! over is an error. Paths split on known tags (`bool`,
//! variant, carrier error) so `if`/`while`/`match`/`catch`/`try?`/`??`
//! pass only the paths they let through; equal paths merge, and past
//! `MAX_PATHS` or `MAX_SPLIT_PASSES` all do. Joins are unions; loops run to
//! a fixpoint. Exempt: `Sync` types, `unsafe`, a `match` on a
//! variable (its bindings are the uses) and compiler-made locals.
//!
//! Calls go through per-function summaries: what happens to each parameter
//! (handed over, returned, stored into another, captured, tied to a guard),
//! the exit state on entry parameters and the return, split by the tag of
//! the result (error, `none`, `false`). An `async` future carries its
//! body's final tag. Summaries are a call-graph fixpoint and ship in NABIHIR.
//! A call through a function value may reach any function used as a value
//! with those parameter types and return type (unless loose; closed world,
//! as for `any`); `extern` and intrinsics keep nothing; a `Handoff::new`
//! never leaves its function.
//! Elements are not told apart by index: copying elements from one buffer
//! into another takes them to be distinct objects unless the source may hold
//! one twice.

mod calls;
mod flow;
mod guards;
mod report;
mod state;
mod stmts;
mod summary;

pub(crate) use crate::handoff::state::*;
pub(crate) use crate::handoff::summary::*;

use crate::send::{Child, each_child};
use crate::{Facts, Keeps};
use nabic_core::{HashMap, HashSet};
use nabic_diagnostic::{Code, NabiDiagnostic, Span};
use nabic_hir::{
    HirBlock, HirExpr, HirExprKind, HirFunction, HirMatchArm, HirPattern, HirStmt, HirTy, StructId,
};
use nabic_ir_common::{ConstVal, FunctionId, LocalId, PrimOp, PrimTy};
use nabic_lexer::TokenKind as K;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

/// Who takes a value over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum What {
    Mutex,
    RwLock,
    Channel,
    Handoff,
    /// `task::spawn`: the task's future.
    Task,
    /// `task::spawn_blocking_with_arg`: the value its function gets.
    TaskArg,
    /// A `[move x]` capture.
    Closure,
    /// `xs.freeze()`: the frozen collection takes the storage over.
    Freeze,
}

impl What {
    const fn noun(self) -> &'static str {
        match self {
            Self::Mutex => "a `Mutex`",
            Self::RwLock => "an `RwLock`",
            Self::Channel => "a channel",
            Self::Handoff => "a `Handoff`",
            Self::Task | Self::TaskArg => "a task",
            Self::Closure => "a closure",
            Self::Freeze => "a frozen collection",
        }
    }

    const fn tag(self) -> &'static str {
        match self {
            Self::Mutex => "mutex",
            Self::RwLock => "rwlock",
            Self::Channel => "channel",
            Self::Handoff => "handoff",
            Self::Task => "task",
            Self::TaskArg => "task_arg",
            Self::Closure => "closure",
            Self::Freeze => "freeze",
        }
    }

    fn from_tag(s: &str) -> Option<Self> {
        [
            Self::Mutex,
            Self::RwLock,
            Self::Channel,
            Self::Handoff,
            Self::Task,
            Self::TaskArg,
            Self::Closure,
            Self::Freeze,
        ]
        .into_iter()
        .find(|w| w.tag() == s)
    }
}

/// What runs a `fn` value handed over as `What::Task`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Runner {
    Task,
    Thread,
    Par,
}

impl Runner {
    /// How it took the value, and who owns it now.
    const fn took(self) -> (&'static str, &'static str) {
        match self {
            Self::Task => ("spawned as a task", "the task"),
            Self::Thread => ("given to a thread", "the thread"),
            Self::Par => ("given to a `par` job", "the `par` job"),
        }
    }
}

/// Calls that hand argument `.1` to another owner `.2`.
const HANDOFFS: &[(&str, usize, What)] = &[
    ("core::sync::Mutex__new", 0, What::Mutex),
    ("core::sync::RwLock__new", 0, What::RwLock),
    ("core::channel::Sender__send", 1, What::Channel),
    ("core::channel::Sender__try_send", 1, What::Channel),
    ("core::channel::Sender__send_async", 1, What::Channel),
    ("core::marker::Handoff__new", 0, What::Handoff),
    ("core::task::spawn", 0, What::Task),
    ("core::task::spawn_plain", 0, What::Task),
    ("core::task::spawn_blocking_with_arg", 0, What::TaskArg),
    ("core::marker::move_into", 0, What::Closure),
    ("core::collections::list::List__freeze", 0, What::Freeze),
    ("core::collections::dict::Dict__freeze", 0, What::Freeze),
    ("core::collections::set::Set__freeze", 0, What::Freeze),
];

/// Lock guards: `get` returns, `set` stores, and `update`'s closure
/// does both with a handle the lock protects until `drop`.
const GUARDS: &[(&str, What)] = &[
    ("core::sync::MutexGuard", What::Mutex),
    ("core::sync::RwLockReadGuard", What::RwLock),
    ("core::sync::RwLockWriteGuard", What::RwLock),
];

/// A future of an `async` body that may throw, and its poll: an error
/// comes out of `poll` only if the body ends with one.
const FUTURE: &str = "core::future::TryAsyncFuture";
const POLL: &str = "core::future::TryAsyncFuture__poll";

/// `name` is `path` or an instance of it (`path__mono__..`).
fn is_fn(name: &str, path: &str) -> bool {
    name.strip_prefix(path)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with("__"))
}

/// What `r.lines()` and the other stream adapters wrap their receiver in:
/// a view tied to the receiver like a guard's `get()`, released with it.
const VIEW_FN: &str = "core::marker::view";

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Handoff(usize, What),
    Get,
    Set,
    Update,
    Drop,
}

/// The op of function `name`; `guard`: a method of a `GUARDS` type.
/// Method names end the path (`MutexGuard__get`, a `Drop` impl's
/// `__impl<hash>_drop`), before a generic instance's `__mono__..`.
fn op_of(name: &str, guard: bool) -> Option<Op> {
    if let Some(&(_, i, what)) = HANDOFFS.iter().find(|(p, ..)| is_fn(name, p)) {
        return Some(Op::Handoff(i, what));
    }
    if is_fn(name, VIEW_FN) {
        return Some(Op::Get);
    }
    if !guard {
        return None;
    }
    let path = name.split(nabic_ir_common::MONO_MARK).next()?;
    match path.rsplit('_').next()? {
        "get" => Some(Op::Get),
        "set" => Some(Op::Set),
        "update" => Some(Op::Update),
        "drop" => Some(Op::Drop),
        _ => None,
    }
}

/// Marks a tuple index in a place path (struct fields are bare).
const TUPLE: u32 = 1 << 31;
/// Longest field path followed (alias chains cannot grow past it).
const MAX_DEPTH: usize = 12;
/// Locals the walk adds: each parameter as it was on entry, the return
/// value, and a call's arguments and result.
const SHADOW: u32 = 0x4000_0000;
const TEMP: u32 = 0x5000_0000;
/// The places holding a tested or stored value's tag (`Walk::key`).
const TAG_KEY: u32 = 0x6000_0000;
const RET: LocalId = LocalId(0x7fff_ffff);
/// The return value as a summary root.
const RET_ROOT: u32 = u32::MAX;
/// What other threads may reach: a value read out of a `Sync` object
/// (an `unsafe impl Sync` type's field, a thread-local's slot).
const WORLD: LocalId = LocalId(0x7fff_fffd);
const WORLD_ROOT: u32 = u32::MAX - 1;
/// The guard of a `with` closure's value (`ThreadLocal::with`,
/// `Mutex::with`, ..): it lives for the whole closure body.
const VIEW: LocalId = LocalId(0x7fff_fffc);
/// The functions that lend argument 0's value to the closure in
/// argument 1.
const WITH: &[&str] = &[
    "std::thread::ThreadLocal__with",
    "core::sync::Mutex__with",
    "core::sync::RwLock__read_with",
    "core::sync::RwLock__write_with",
];
/// A path step: the elements of the buffer the place holds.
const ELEM: u32 = 0x3fff_fffe;
/// A path step: argument `i` of an enum value's variant `v`
/// (`payload(v, i)`).
const PAYLOAD: u32 = 0x2000_0000;

/// The path step of argument `i` of variant `v`.
const fn payload(v: u32, i: u32) -> u32 {
    PAYLOAD | (v << 12) | (i & 0xfff)
}

/// Functions whose value is an element taken out of argument 0's buffer:
/// the buffer no longer holds it, unless it may hold it twice.
const TAKES: &[&str] = &[
    "core::collections::list::List__pop",
    "core::collections::list::List__remove",
    "core::collections::deque::Deque__pop_front",
    "core::collections::deque::Deque__pop_back",
    "core::collections::dict::Dict__remove",
    "core::collections::heap::BinaryHeap__pop",
];

/// Functions whose value may be argument 0 itself (no body to walk). A
/// coroutine's priming steps copy through the hand-off's memo, which the
/// hand-off itself accounts for.
const PASSES: &[&str] = &[
    "core::reflect::cast",
    "core::future::prime_frame",
    "core::future::prime_started",
];

/// Collection literals (`[a, b]`, `{k: v}`): their elements are the
/// argument array's, so the result holds an object twice only if two
/// of the written elements are one.
const LITERALS: &[&str] = &[
    "core::collections::list::list_literal",
    "core::collections::set::set_literal",
    "core::collections::dict::dict_literal",
];

/// Modules whose functions move elements between slots of one collection
/// and copy each element of another one once (`stores_twice`).
const TRUSTED: &[&str] = &["core::collections::", "core::par::"];

/// Functions that drop every element of argument 0's buffer.
const CLEARS: &[&str] = &[
    "core::collections::list::List__clear",
    "core::collections::deque::Deque__clear",
    "core::collections::dict::Dict__clear",
];

/// Functions that store every element of argument 1 into argument 0's
/// buffer (`xs.extend(ys)`, `xs += ys`).
const COPIES: &[&str] = &["core::collections::list::List__extend"];

/// `par` functions that give each item of argument 0 to a worker.
const PAR: &[&str] = &["core::par::for_each", "core::par::map", "core::par::filter"];

/// The place up to its first `ELEM` step: the buffer's elements.
fn elem_of(p: &Place) -> Option<Place> {
    let i = p.1.iter().position(|&s| s == ELEM)?;
    Some((p.0, p.1[..=i].to_vec()))
}

struct Frame<'h> {
    label: Option<&'h str>,
    is_loop: bool,
    brk: Cur,
    cont: Cur,
    /// Where the tag of a `break`'s value goes.
    dest: Option<Place>,
}

/// What every walk shares.
struct Checker<'a> {
    env: &'a dyn Facts,
    sync: &'a str,
    fns: HashMap<FunctionId, &'a HirFunction>,
    ops: HashMap<FunctionId, Op>,
    /// `TryAsyncFuture::poll` and its instances.
    polls: HashSet<FunctionId>,
    /// `TAKES`, `PASSES` and `PAR` functions and their instances.
    takes: HashSet<FunctionId>,
    passes: HashSet<FunctionId>,
    par: HashSet<FunctionId>,
    literals: HashSet<FunctionId>,
    clears: HashSet<FunctionId>,
    copies: HashSet<FunctionId>,
    /// `TryAsyncFuture` and its instances.
    futures: HashSet<StructId>,
    sums: RefCell<HashMap<FunctionId, Rc<Summary>>>,
    /// A closure's or coroutine body's function -> the function it is in.
    parent: HashMap<FunctionId, FunctionId>,
    /// A call that handed something over (its span start) -> the callee.
    callees: RefCell<HashMap<(u32, u32), String>>,
    /// `ThreadLocal::with` closures -> the parameter holding the value.
    views: HashMap<FunctionId, usize>,
    /// Where views a callee keeps were taken out, and those calls: what
    /// follows from them is not reported again.
    kept_at: RefCell<HashSet<(u32, u32)>>,
    cands: HashMap<usize, Vec<Cand>>,
    tracked_memo: RefCell<HashMap<HirTy, bool>>,
    whys: RefCell<(Vec<Why>, HashMap<Why, u32>)>,
    /// A `Mutex::new` / `RwLock::new` / `Handoff::new` call (its span
    /// start) -> the variable that holds what it made, and its block.
    owners: RefCell<HashMap<(u32, u32), (String, usize)>>,
    /// A hand-off call (its span start) of a variable or field -> it as
    /// written, and its type.
    direct: RefCell<HashMap<(u32, u32), (String, String)>>,
    unsafe_regions: RefCell<HashMap<u32, Vec<(u32, u32)>>>,
    /// A hand-off call (its span start) -> its argument as written.
    handed: RefCell<HashMap<(u32, u32), String>>,
    /// Hand-off calls (span start) of a `fn` value that may own moved
    /// values.
    fn_owners: RefCell<HashSet<(u32, u32)>>,
    /// Errors (by position) about a value a call handed over, with the
    /// call: dropped when an error inside the call (a closure's) is the
    /// cause.
    via_at: RefCell<HashMap<(u32, u32), Span>>,
    /// A call (its span start) that runs `fn` values it is given on a
    /// thread or as a `par` job, not as a task.
    runners: RefCell<HashMap<(u32, u32), Runner>>,
    /// What each call keeps of its arguments, as the affine check sees it.
    keeps: &'a dyn Keeps,
}

impl Checker<'_> {
    /// A value of this type is not `Sync` (the only ones followed).
    fn tracked(&self, t: &HirTy) -> bool {
        if matches!(
            t,
            HirTy::Unknown | HirTy::Prim(_) | HirTy::Unit | HirTy::Never | HirTy::None
        ) {
            return false;
        }
        if let Some(&v) = self.tracked_memo.borrow().get(t) {
            return v;
        }
        // A buffer is shared by the views of it (`Slice`), whatever its
        // elements.
        let v = !self.env.has_param(t)
            && (matches!(t, HirTy::GcBuf(_)) || !self.env.has_trait(t, self.sync));
        self.tracked_memo.borrow_mut().insert(t.clone(), v);
        v
    }

    fn why_id(&self, w: Why) -> u32 {
        let mut t = self.whys.borrow_mut();
        if let Some(&i) = t.1.get(&w) {
            return i;
        }
        let i = u32::try_from(t.0.len()).unwrap_or(u32::MAX);
        t.0.push(w.clone());
        t.1.insert(w, i);
        i
    }

    fn summary(&self, id: FunctionId) -> Option<Rc<Summary>> {
        self.sums.borrow().get(&id).cloned()
    }

    fn src(&self, s: Span) -> Option<&str> {
        let (_, text) = self.env.sources().file(s.file)?;
        text.get(s.start as usize..s.end as usize)
    }

    fn file_name(&self, file: u32) -> String {
        self.env.sources().path(file).unwrap_or_default().to_owned()
    }

    /// `s` lies inside an `unsafe { .. }` block or an `unsafe fn` body.
    fn in_unsafe(&self, s: Span) -> bool {
        let mut memo = self.unsafe_regions.borrow_mut();
        let regions = memo.entry(s.file).or_insert_with(|| {
            self.env
                .sources()
                .file(s.file)
                .map(|(_, text)| unsafe_regions(text))
                .unwrap_or_default()
        });
        regions.iter().any(|&(a, b)| a <= s.start && s.end <= b)
    }
}

/// The byte ranges of `unsafe { .. }` blocks and `unsafe fn` bodies.
fn unsafe_regions(text: &str) -> Vec<(u32, u32)> {
    let (tokens, _) = nabic_lexer::Lexer::new(text, String::new()).lex_all();
    let close = |open: usize| {
        let mut depth = 0i32;
        tokens[open..].iter().find_map(|t| {
            match t.kind {
                K::LBrace => depth += 1,
                K::RBrace => depth -= 1,
                _ => {}
            }
            (depth == 0).then_some(t.span.end)
        })
    };
    let mut out = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        if t.kind != K::Unsafe {
            continue;
        }
        let open = match tokens.get(i + 1).map(|t| &t.kind) {
            Some(K::LBrace) => Some(i + 1),
            // The body: the first `{` outside the parameter list.
            Some(K::Fn) => {
                let mut depth = 0i32;
                tokens[i + 2..]
                    .iter()
                    .position(|t| {
                        match t.kind {
                            K::LParen | K::LBracket => depth += 1,
                            K::RParen | K::RBracket => depth -= 1,
                            _ => {}
                        }
                        depth == 0 && matches!(t.kind, K::LBrace | K::Semi)
                    })
                    .map(|k| i + 2 + k)
                    .filter(|&k| tokens[k].kind == K::LBrace)
            }
            _ => None,
        };
        if let Some(o) = open {
            if let Some(end) = close(o) {
                out.push((tokens[o].span.start, end));
            }
        }
    }
    out
}

/// The function as written: `store`, `Bag::store` (a closure or an
/// `async` body: the function it is in).
fn shown_fn(c: &Checker<'_>, f: &HirFunction) -> String {
    let mut f = f;
    while let Some(p) = c.parent.get(&f.id).and_then(|p| c.fns.get(p)) {
        f = p;
    }
    let path = f.facts.path(&f.name);
    let path = path.split("__by_").next().unwrap_or(path);
    let leaf = path.rsplit("::").next().unwrap_or(path);
    // A method of a struct of this build: `Struct<id>__name`.
    if let Some((id, m)) = leaf
        .strip_prefix("Struct")
        .and_then(|r| r.split_once("__"))
        .filter(|(id, _)| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
    {
        let structs = c.env.structs();
        if let Some(s) = id.parse::<usize>().ok().and_then(|i| structs.get(i)) {
            let ty = s.name.split(['<', '_']).next().unwrap_or(&s.name);
            let ty = ty.rsplit("::").next().unwrap_or(ty);
            return format!("{ty}::{m}");
        }
    }
    leaf.replace('.', "::").replace("__", "::")
}

/// `f` (or the function it is a closure of) is the standard library's.
fn in_lib(c: &Checker<'_>, f: &HirFunction) -> bool {
    let mut f = f;
    while let Some(p) = c.parent.get(&f.id).and_then(|p| c.fns.get(p)) {
        f = p;
    }
    nabic_core::in_toolchain(f.facts.path(&f.name))
}

/// Check every body for uses after hand-off. `stored`: the summaries of
/// prebuilt libraries (NABIHIR); returned: the errors, and the summaries
/// of this build's bodies, for its NABIHIR when `export`.
pub fn check(
    env: &dyn Facts,
    functions: &[HirFunction],
    stored: &[(String, String)],
    keeps: &dyn Keeps,
    export: bool,
) -> (Vec<NabiDiagnostic>, Vec<(String, String)>) {
    let sync = env.lang("sync_trait");
    if sync.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let extras = env.extras();
    let all: Vec<&HirFunction> = functions.iter().chain(extras.iter()).collect();
    let structs = env.structs();
    let guard = |f: &HirFunction| {
        let Some(HirTy::Struct(id)) = f.params.first().filter(|p| p.name == "self").map(|p| &p.ty)
        else {
            return false;
        };
        structs.get(id.0 as usize).is_some_and(|s| {
            let base = s.name.split("__").next().unwrap_or_default();
            let base = base.split('<').next().unwrap_or_default();
            GUARDS.iter().any(|(g, _)| *g == base)
        })
    };
    let mut ops = HashMap::default();
    let mut polls = HashSet::default();
    let (mut takes, mut passes, mut par) =
        (HashSet::default(), HashSet::default(), HashSet::default());
    let mut literals = HashSet::default();
    let (mut clears, mut copies) = (HashSet::default(), HashSet::default());
    for f in &all {
        if let Some(op) = op_of(&f.name, guard(f)) {
            ops.insert(f.id, op);
        }
        if is_fn(&f.name, POLL) {
            polls.insert(f.id);
        }
        // `xs += ys` (`AddAssign<List<T>>`): both sides the same list type.
        if f.name.contains("_add_assign") && f.params.len() == 2 && f.params[0].ty == f.params[1].ty
        {
            copies.insert(f.id);
        }
        for (set, names) in [
            (&mut takes, TAKES),
            (&mut passes, PASSES),
            (&mut par, PAR),
            (&mut literals, LITERALS),
            (&mut clears, CLEARS),
            (&mut copies, COPIES),
        ] {
            if names.iter().any(|n| is_fn(&f.name, n)) {
                set.insert(f.id);
            }
        }
    }
    #[allow(clippy::cast_possible_truncation)]
    let futures = structs
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            let base = s.name.split("__").next().unwrap_or_default();
            base.split('<').next() == Some(FUTURE)
        })
        .map(|(i, _)| StructId(i as u32))
        .collect();
    drop(structs);
    let by_name: HashMap<&str, &str> = stored
        .iter()
        .map(|(n, s)| (n.as_str(), s.as_str()))
        .collect();
    let mut sums = HashMap::default();
    for f in all.iter().filter(|f| f.body.is_none()) {
        if let Some(s) = by_name
            .get(f.name.as_str())
            .and_then(|t| Summary::decode(t))
        {
            sums.insert(f.id, Rc::new(s));
        }
    }
    let c = Checker {
        env,
        sync,
        fns: all.iter().map(|f| (f.id, *f)).collect(),
        ops,
        polls,
        takes,
        passes,
        par,
        literals,
        clears,
        copies,
        futures,
        sums: RefCell::new(sums),
        parent: parents(&all),
        callees: RefCell::default(),
        views: with_closures(&all),
        kept_at: RefCell::default(),
        cands: HashMap::default(),
        tracked_memo: RefCell::default(),
        whys: RefCell::default(),
        owners: RefCell::default(),
        direct: RefCell::default(),
        unsafe_regions: RefCell::default(),
        handed: RefCell::default(),
        fn_owners: RefCell::default(),
        via_at: RefCell::default(),
        runners: RefCell::default(),
        keeps,
    };
    let mut c = c;
    c.cands = candidates(&c, &all, stored);
    let c = c;

    // The bodies worth a walk: one holding a value that is not `Sync`.
    let walked: Vec<&HirFunction> = all
        .iter()
        .copied()
        .filter(|f| {
            f.body.is_some()
                && !c.ops.contains_key(&f.id)
                && (c.tracked(&f.ret_ty) || f.locals.iter().any(|l| c.tracked(&l.ty)))
        })
        .collect();

    // Summaries to a fixpoint, callees first.
    let order = postorder(&walked);
    let mut readers: HashMap<FunctionId, HashSet<FunctionId>> = HashMap::default();
    let mut queue: std::collections::VecDeque<FunctionId> = order.iter().copied().collect();
    let mut queued: HashSet<FunctionId> = queue.iter().copied().collect();
    let mut budget = 40 * order.len() + 1000;
    // A function's last walk saw its callees' final summaries (a change
    // walks its readers again): its errors are those of that walk.
    let mut found: HashMap<FunctionId, Vec<NabiDiagnostic>> = HashMap::default();
    while let Some(id) = queue.pop_front() {
        queued.remove(&id);
        let f = c.fns[&id];
        let ((s, deps), errs) = walk(&c, f);
        found.insert(id, errs);
        if budget == 0 {
            continue;
        }
        budget -= 1;
        for d in deps {
            readers.entry(d).or_default().insert(id);
        }
        let changed = c
            .summary(id)
            .is_none_or(|old| !(old.same_facts(&s) || old.is_empty() && s.is_empty()));
        if changed {
            c.sums.borrow_mut().insert(id, Rc::new(s));
            for r in readers.get(&id).into_iter().flatten() {
                if queued.insert(*r) {
                    queue.push_back(*r);
                }
            }
        }
    }
    let mut diags: Vec<NabiDiagnostic> = walked
        .iter()
        .filter_map(|f| found.remove(&f.id))
        .flatten()
        .collect();
    let mut seen = HashSet::default();
    diags.retain(|d: &NabiDiagnostic| {
        let at = d.primary.as_ref().map(|l| (l.span.file, l.span.start));
        seen.insert((at, d.message.clone()))
    });
    // A value a call handed over because of an error reported inside the
    // call (a closure argument's own): that error is the one to fix.
    let via = c.via_at.borrow();
    let spans: Vec<Span> = diags
        .iter()
        .filter_map(|d| d.primary.as_ref().map(|l| l.span))
        .collect();
    diags.retain(|d| {
        let Some(at) = d.primary.as_ref().map(|l| l.span) else {
            return true;
        };
        let Some(site) = via.get(&(at.file, at.start)) else {
            return true;
        };
        !spans
            .iter()
            .any(|x| x.file == site.file && x.start >= site.start && x.end <= site.end && *x != at)
    });
    if !export {
        return (diags, Vec::new());
    }
    let sums = c.sums.borrow();
    let mut out: Vec<(String, String)> = walked
        .iter()
        .filter(|f| !f.name.contains(nabic_ir_common::MONO_MARK))
        .filter_map(|f| {
            let s = sums.get(&f.id).filter(|s| !s.is_empty())?;
            Some((f.name.clone(), s.encode()))
        })
        .collect();
    // The functions used as values, by parameter types.
    let mut by_sig: BTreeMap<String, Summary> = BTreeMap::new();
    for cs in c.cands.values() {
        for k in cs {
            let CandFrom::Fn(id) = &k.from else { continue };
            let Some(s) = sums.get(id).filter(|s| !s.is_empty()) else {
                continue;
            };
            let ret = k
                .ret
                .as_ref()
                .map_or_else(String::new, |r| format!("{RET_KEY}{r}"));
            let head = format!("{FN_VALUE}{}{ret}", if k.env { "e" } else { "" });
            let name = std::iter::once(head)
                .chain(k.keys.iter().cloned())
                .collect::<Vec<_>>()
                .join("\u{1f}");
            by_sig.entry(name).or_default().merge(s);
        }
    }
    out.extend(by_sig.into_iter().map(|(n, s)| (n, s.encode())));
    (diags, out)
}

/// The functions used as values (and the stored ones of prebuilt
/// libraries), by their parameter count without the environment.
fn candidates(
    c: &Checker<'_>,
    all: &[&HirFunction],
    stored: &[(String, String)],
) -> HashMap<usize, Vec<Cand>> {
    let mut used: BTreeMap<FunctionId, bool> = BTreeMap::new();
    for f in all {
        let Some(body) = &f.body else { continue };
        nabic_hir::each_block(body, &mut |e| match &e.kind {
            HirExprKind::FnRef(id) => {
                used.entry(*id).or_insert(false);
            }
            HirExprKind::ClosureValue { fn_id, .. } => {
                used.insert(*fn_id, true);
            }
            _ => {}
        });
    }
    let mut out: HashMap<usize, Vec<Cand>> = HashMap::default();
    for (id, env) in used {
        let Some(f) = c.fns.get(&id) else { continue };
        if c.ops.contains_key(&id) {
            continue;
        }
        let keys: Vec<String> = f
            .params
            .iter()
            .skip(usize::from(env))
            .map(|p| p.ty.stable_key())
            .collect();
        out.entry(keys.len()).or_default().push(Cand {
            env,
            keys,
            ret: ret_key(c.env, &f.ret_ty),
            from: CandFrom::Fn(id),
        });
    }
    for (name, text) in stored {
        let mut parts = name.split('\u{1f}');
        let Some(head) = parts.next().and_then(|h| h.strip_prefix(FN_VALUE)) else {
            continue;
        };
        let Some(s) = Summary::decode(text) else {
            continue;
        };
        let (env, ret) = match head.split_once(RET_KEY) {
            Some((env, ret)) => (env, Some(ret.to_owned())),
            None => (head, None),
        };
        let keys: Vec<String> = parts.map(str::to_owned).collect();
        out.entry(keys.len()).or_default().push(Cand {
            env: env == "e",
            keys,
            ret,
            from: CandFrom::Stored(Rc::new(s)),
        });
    }
    out
}

/// The functions passed to `ThreadLocal::with` (a closure, a function
/// name, or a variable bound to one) -> their value parameter.
fn with_closures<'h>(all: &[&'h HirFunction]) -> HashMap<FunctionId, usize> {
    let ids: HashMap<FunctionId, &str> = all.iter().map(|f| (f.id, f.name.as_str())).collect();
    let mut out = HashMap::default();
    for f in all {
        let Some(body) = &f.body else { continue };
        let mut inits: HashMap<LocalId, &'h HirExpr> = HashMap::default();
        let mut note = |b: &'h HirBlock| {
            for s in &b.stmts {
                if let HirStmt::Let { local, init } = s {
                    inits.insert(*local, init);
                }
            }
        };
        note(body);
        nabic_hir::each_block(body, &mut |e| {
            if let HirExprKind::Block(b) = &e.kind {
                note(b);
            }
        });
        nabic_hir::each_block(body, &mut |e| {
            let HirExprKind::Call { callee, args } = &e.kind else {
                return;
            };
            if !ids
                .get(callee)
                .is_some_and(|n| WITH.iter().any(|w| is_fn(n, w)))
            {
                return;
            }
            let mut x = args.get(1);
            for _ in 0..8 {
                match x.map(|x| &x.kind) {
                    Some(HirExprKind::ClosureValue { fn_id, .. }) => {
                        out.insert(*fn_id, 1);
                        return;
                    }
                    Some(HirExprKind::FnRef(id)) => {
                        out.insert(*id, 0);
                        return;
                    }
                    Some(HirExprKind::Local(l)) => x = inits.get(l).copied(),
                    Some(HirExprKind::Block(b)) if b.stmts.is_empty() => x = b.trailing.as_ref(),
                    _ => return,
                }
            }
        });
    }
    out
}

/// Parameter `start` is kept past the call in `exit`: the whole of it, or
/// a field of it, is the same object as a part of another parameter or
/// as what other threads reach. Elements shared both ways (`xs.extend(ys)`)
/// and the return value do not count: the caller follows those.
fn keeps_param(
    ends: &mut SameEnds<'_>,
    start: &Place,
    own: u32,
    root_of: &dyn Fn(LocalId) -> Option<u32>,
) -> bool {
    ends.walk_from(start, |sub, c| match root_of(c.0).filter(|&r| r != own) {
        Some(RET_ROOT) => Visit::Skip,
        Some(r) if r == WORLD_ROOT || !c.1.is_empty() && !sub.contains(&ELEM) => Visit::Stop,
        _ => Visit::Step,
    })
}

/// What a coroutine call returns: its frame holds the arguments.
const CORO_TYPES: &[&str] = &[
    "core::future::AsyncFuture",
    "core::future::TryAsyncFuture",
    "core::iter::GenIter",
    "core::iter::TryGenIter",
    "core::stream::AsyncStream",
    "core::stream::TryAsyncStream",
];

/// The stream adapters that make views, longest first.
const VIEW_METHODS: &[&str] = &[
    ".lines_async(",
    ".bytes_async(",
    ".incoming(",
    ".lines(",
    ".bytes(",
    ".split(",
    ".take(",
];

/// `f.lines()` as its owning form, `f.into_lines()`.
fn owning_form(made: &str) -> Option<String> {
    let i = VIEW_METHODS.iter().filter_map(|m| made.rfind(m)).max()?;
    Some(format!("{}.into_{}", &made[..i], &made[i + 1..]))
}

/// `t` (`g.get()`, `xs`) as a deep copy: `g.get_clone()`, `xs.clone()`.
fn deep_copy(t: &str) -> String {
    t.strip_suffix(".get()")
        .map_or_else(|| format!("{t}.clone()"), |g| format!("{g}.get_clone()"))
}

/// Each closure's and coroutine body's function -> the one making it.
fn parents(all: &[&HirFunction]) -> HashMap<FunctionId, FunctionId> {
    let mut out = HashMap::default();
    for f in all {
        let Some(body) = &f.body else { continue };
        nabic_hir::each_block(body, &mut |e| {
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

/// `fns` callees before callers (by direct calls).
fn postorder(fns: &[&HirFunction]) -> Vec<FunctionId> {
    let ids: HashSet<FunctionId> = fns.iter().map(|f| f.id).collect();
    let callees: HashMap<FunctionId, Vec<FunctionId>> = fns
        .iter()
        .map(|f| {
            let mut v = Vec::new();
            if let Some(b) = &f.body {
                nabic_hir::each_block(b, &mut |e| {
                    if let HirExprKind::Call { callee: id, .. }
                    | HirExprKind::CoroNew { body_fn: id, .. }
                    | HirExprKind::ClosureValue { fn_id: id, .. } = &e.kind
                    {
                        if ids.contains(id) {
                            v.push(*id);
                        }
                    }
                });
            }
            (f.id, v)
        })
        .collect();
    let mut done = HashSet::default();
    let mut out = Vec::new();
    for f in fns {
        let mut stack = vec![(f.id, 0usize)];
        while let Some((id, i)) = stack.pop() {
            if i == 0 && !done.insert(id) {
                continue;
            }
            let next = callees[&id].get(i).copied();
            match next {
                Some(n) => {
                    stack.push((id, i + 1));
                    if !done.contains(&n) {
                        stack.push((n, 0));
                    }
                }
                None => out.push(id),
            }
        }
    }
    out
}

type WalkOut = ((Summary, HashSet<FunctionId>), Vec<NabiDiagnostic>);

fn walk(c: &Checker<'_>, f: &HirFunction) -> WalkOut {
    let mut w = Walk {
        c,
        f,
        cur: Cur::start(),
        frames: Vec::new(),
        exits: BTreeMap::new(),
        mixed: false,
        keys: HashMap::default(),
        dests: Vec::new(),
        pinned: HashSet::default(),
        reported: HashSet::default(),
        temps: HashMap::default(),
        temp_tys: HashMap::default(),
        results: HashMap::default(),
        calls: Vec::new(),
        scopes: Vec::new(),
        returning: false,
        thrown: None,
        sends: false,
        uses: BTreeSet::new(),
        sent_from: BTreeSet::new(),
        renewed: BTreeSet::new(),
        not_renewed: BTreeSet::new(),
        deps: HashSet::default(),
        diags: Vec::new(),
        guard_copies: HashMap::default(),
        guard_parts: HashMap::default(),
        moved_at: HashMap::default(),
    };
    // Each parameter as it came in: what the caller passed.
    for (i, p) in f.params.iter().enumerate() {
        if c.tracked(&p.ty) {
            let s = shadow(i);
            for st in w.cur.states() {
                st.edges
                    .insert(Edge::Same((s, Vec::new()), (p.local, Vec::new())));
            }
        }
    }
    let body = f.body.as_ref().expect("walked bodies have one");
    nabic_hir::each_block(body, &mut |e| {
        if let HirExprKind::AddrOf { place } = &e.kind {
            let mut x = &**place;
            while let HirExprKind::FieldRead { target: t, .. }
            | HirExprKind::TupleField { target: t, .. }
            | HirExprKind::ArrayIndex { array: t, .. } = &x.kind
            {
                x = t;
            }
            if let HirExprKind::Local(l) = x.kind {
                w.pinned.insert(l);
            }
        }
    });
    // A `with` closure's value is tied to `VIEW` for the whole body.
    let view = c
        .views
        .get(&f.id)
        .and_then(|&i| f.params.get(i).map(|p| (i, p)));
    let view = view.filter(|(_, p)| c.tracked(&p.ty));
    if let Some((_, p)) = view {
        w.tie(VIEW, (p.local, Vec::new()), f.span, Tie::Taken);
        // An `async` closure's body is its future: `Future { coro_new .. }`.
        let coro = |e: &HirExpr| match &e.kind {
            HirExprKind::CoroNew { .. } => true,
            HirExprKind::StructInit { fields, .. } => fields
                .iter()
                .any(|x| matches!(x.kind, HirExprKind::CoroNew { .. })),
            _ => false,
        };
        if f.is_async || body.trailing.as_ref().is_some_and(coro) {
            w.diags.push(
                NabiDiagnostic::error(
                    Code::THREAD_LOCAL_ESCAPE,
                    f.span,
                    "an `async` closure can't be given to `with`: its future would keep the \
                     value `with` lends (a `ThreadLocal`'s, a locked `Mutex`'s) past the call, \
                     and may resume on another thread",
                )
                .with_note(
                    "help: give `with` a plain closure, and `await` outside it on a copy \
                     (`.clone()`) or on what you computed from the value",
                ),
            );
        }
    }
    w.block_to(body, Some(&RET_TAG));
    if let (Some(t), false) = (&body.trailing, w.cur.is_empty()) {
        w.store(&(RET, Vec::new()), t, false);
        if view.is_some() {
            w.escapes_guard(t, Exit::Return);
        }
    }
    w.note_renews();
    w.exit_with(Some(&RET_TAG));
    let s = w.summarize();
    #[allow(clippy::cast_possible_truncation)]
    if let Some((i, p)) = view {
        if s.keeps.contains(&(i as u32)) && !w.diags.iter().any(NabiDiagnostic::is_error) {
            w.view_kept(&p.name, f.span);
        }
    }
    ((s, w.deps), w.diags)
}

#[allow(clippy::cast_possible_truncation)]
const fn shadow(i: usize) -> LocalId {
    LocalId(SHADOW + i as u32)
}

/// Where a returned (or a coroutine's step) value's tag goes.
const RET_TAG: Place = (LocalId(0x7fff_fffe), Vec::new());

struct Walk<'c, 'h> {
    c: &'c Checker<'c>,
    f: &'h HirFunction,
    cur: Cur,
    frames: Vec<Frame<'h>>,
    /// The states at the returns, by the returned value's tag.
    exits: BTreeMap<u32, St>,
    /// A coroutine step before the last one may be an error.
    mixed: bool,
    /// The places holding the tags of values being tested or stored (by
    /// the expression's address).
    keys: HashMap<usize, LocalId>,
    /// Calls (by address) whose value's tag goes to a place, innermost
    /// last.
    dests: Vec<(usize, Place)>,
    /// Locals whose address is taken: no tag is kept for them.
    pinned: HashSet<LocalId>,
    reported: HashSet<(Place, u32, u32)>,
    /// A call's (by address) argument / result slot -> its local.
    temps: HashMap<(usize, u32), LocalId>,
    temp_tys: HashMap<LocalId, HirTy>,
    /// What a call's value carries (by address).
    results: HashMap<usize, Vec<Flow>>,
    /// The calls being walked, innermost last (for the hints).
    calls: Vec<&'h HirExpr>,
    /// The blocks being walked (by address), innermost last.
    scopes: Vec<usize>,
    /// Walking a `return`'s value.
    returning: bool,
    /// The error a `return` being walked throws, when it is a local:
    /// `throw_escapes` reports it.
    thrown: Option<LocalId>,
    /// This body may hand something over.
    sends: bool,
    /// Summary facts gathered on the way (`Summary::uses` / `sent_from`),
    /// and `ref` parameter parts renewed / not renewed at a return.
    uses: BTreeSet<Root>,
    sent_from: BTreeSet<Root>,
    renewed: BTreeSet<Root>,
    not_renewed: BTreeSet<Root>,
    /// The summaries this walk read.
    deps: HashSet<FunctionId>,
    diags: Vec<NabiDiagnostic>,
    /// A guard local copied from a place (`let g = env.0`).
    guard_copies: HashMap<LocalId, Place>,
    /// A closure's part that is a captured guard local.
    guard_parts: HashMap<Place, LocalId>,
    /// A stream local moved while it had views: where.
    moved_at: HashMap<LocalId, Span>,
}

fn addr(e: &HirExpr) -> usize {
    std::ptr::from_ref(e) as usize
}
