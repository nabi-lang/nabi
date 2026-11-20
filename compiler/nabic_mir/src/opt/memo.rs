//! Per-function results kept between builds. Each cached step (cleanup
//! of a body, the passes after inlining) reads its body and, of the
//! module, only what it asks about the functions the body names and a
//! few module facts: those, hashed, key its result. A body an edit left
//! alone is not optimized again.
//!
//! Keys and entries name functions and structs by their names, not by
//! this build's ids, so another program's build that has the same body
//! takes its result too (a standard library function's).

use crate::opt::Opt;
use crate::{MirFunction, MirModule, RValue, Statement};
use nabic_core::codec::{Codec, Dec, Enc, IdKind};
use nabic_core::hash::fast_add;
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::FunctionId;

/// Where the results are kept.
pub trait Memo: Sync {
    /// The entry at `key`. A `shared` one is a standard library
    /// function's, the same in other programs' builds.
    fn get(&self, key: u64, shared: bool) -> Option<Vec<u8>>;
    fn put(&self, key: u64, shared: bool, value: &[u8]);
    /// The path of source file `file`, and the file of a path.
    fn path(&self, file: u32) -> Option<&str>;
    fn file(&self, path: &str) -> Option<u32>;
}

/// Seeds the check, a second hash of what the key hashes.
const CHECK: u64 = 0x9e37_79b9_7f4a_7c15;

/// Names by id and ids by name (`None`: a name given twice).
#[derive(Default)]
struct Names {
    of: HashMap<u32, String>,
    id: HashMap<String, Option<u32>>,
}

impl Names {
    fn add(&mut self, id: u32, name: &str) {
        self.of.insert(id, name.to_owned());
        self.id
            .entry(name.to_owned())
            .and_modify(|v| *v = None)
            .or_insert(Some(id));
    }

    /// The name of `id`, when it names only `id`.
    fn name(&self, id: u32) -> Option<&str> {
        let n = self.of.get(&id)?;
        self.id.get(n).copied().flatten()?;
        Some(n)
    }
}

/// A step's cache within one module.
pub(crate) struct Steps<'a> {
    memo: &'a dyn Memo,
    salt: u64,
    fns: Names,
    structs: Names,
    reprs: std::sync::Arc<std::collections::BTreeMap<u32, nabic_hir::Repr>>,
}

impl<'a> Steps<'a> {
    /// What every step of `m` at `opt` reads beyond the body: the level
    /// and the target. A struct's list-length bound follows from its name
    /// (`range::bounds`), which keys hold.
    pub(crate) fn new(memo: &'a dyn Memo, m: &MirModule, opt: Opt) -> Self {
        let salt = format!("{opt:?} {:016x}", m.target.spec.hash());
        let mut fns = Names::default();
        for f in &m.functions {
            fns.add(f.id.0, &f.name);
        }
        let mut structs = Names::default();
        for (id, k) in &m.struct_keys {
            structs.add(*id, k);
        }
        Self {
            memo,
            salt: fast_add(0, salt.as_bytes()),
            fns,
            structs,
            reprs: m.target.reprs.clone(),
        }
    }

    /// `ids` by name, a struct's with its `#[repr]`; `None` when one has
    /// no name of its own.
    fn names(&self, ids: &[(IdKind, u32)], out: &mut Enc) -> Option<()> {
        for &(k, id) in ids {
            match k {
                IdKind::Function => out.bytes(self.fns.name(id)?.as_bytes()),
                IdKind::Struct => {
                    out.bytes(self.structs.name(id)?.as_bytes());
                    out.bytes(format!("{:?}", self.reprs.get(&id)).as_bytes());
                }
                IdKind::Enum | IdKind::InferVar => return None,
            }
        }
        Some(())
    }

    /// `go(f)` (what it returns: `flag`), or its result kept from a run
    /// on the same input. `step` names the step, `flag` is its input
    /// flag and `facts` what it reads of other functions.
    pub(crate) fn run(
        &self,
        step: u8,
        f: &mut MirFunction,
        flag: bool,
        facts: &[(FunctionId, u64)],
        go: impl FnOnce(&mut MirFunction) -> bool,
    ) -> bool {
        let shared = nabic_core::in_toolchain(f.path());
        let Some((key, check)) = self.key(step, f, flag, facts) else {
            return go(f);
        };
        if let Some((c, out, g)) = self.memo.get(key, shared).and_then(|v| self.read(&v)) {
            if c == check {
                *f = g;
                return out;
            }
        }
        let out = go(f);
        if let Some(v) = self.entry(check, out, f) {
            self.memo.put(key, shared, &v);
        }
        out
    }

    fn key(
        &self,
        step: u8,
        f: &MirFunction,
        flag: bool,
        facts: &[(FunctionId, u64)],
    ) -> Option<(u64, u64)> {
        let mut e = Enc::new(true);
        e.no_ids = true;
        e.ids = Some(Vec::new());
        f.enc(&mut e);
        e.u(u64::from(step));
        flag.enc(&mut e);
        // By name: ids are this build's.
        let mut named: Vec<(&str, u64)> = Vec::with_capacity(facts.len());
        for (id, bits) in facts {
            named.push((self.fns.name(id.0)?, *bits));
        }
        named.sort_unstable();
        for (name, bits) in named {
            e.bytes(name.as_bytes());
            e.u(bits);
        }
        // Spans name files by this build's ids.
        for &file in &e.files.clone() {
            e.bytes(self.memo.path(file)?.as_bytes());
        }
        let ids = e.ids.take().unwrap_or_default();
        self.names(&ids, &mut e)?;
        Some((
            fast_add(self.salt, &e.buf),
            fast_add(self.salt ^ CHECK, &e.buf),
        ))
    }

    /// The entry of result `f`: its files and ids by name, then `check`,
    /// `flag` and the function.
    fn entry(&self, check: u64, flag: bool, f: &MirFunction) -> Option<Vec<u8>> {
        let mut v = Enc::new(true);
        v.ids = Some(Vec::new());
        check.enc(&mut v);
        flag.enc(&mut v);
        f.enc(&mut v);
        let mut out = Enc::new(false);
        out.u(v.files.len() as u64);
        for &file in &v.files {
            out.u(u64::from(file));
            out.bytes(self.memo.path(file)?.as_bytes());
        }
        let mut seen = HashSet::default();
        let ids: Vec<(IdKind, u32)> = v
            .ids
            .take()
            .unwrap_or_default()
            .into_iter()
            .filter(|&(k, id)| seen.insert((k as u8, id)))
            .collect();
        out.u(ids.len() as u64);
        for &(k, id) in &ids {
            let (tag, name) = match k {
                IdKind::Function => (0, self.fns.name(id)?),
                IdKind::Struct => (1, self.structs.name(id)?),
                IdKind::Enum | IdKind::InferVar => return None,
            };
            out.u(tag);
            out.u(u64::from(id));
            out.bytes(name.as_bytes());
        }
        out.bytes(&v.buf);
        Some(out.buf)
    }

    /// An entry read back with this build's file and id numbers.
    fn read(&self, bytes: &[u8]) -> Option<(u64, bool, MirFunction)> {
        let mut d = Dec::new(bytes);
        let n = usize::try_from(d.u()?).ok()?;
        let mut files: Vec<u32> = Vec::new();
        for _ in 0..n {
            let old = usize::try_from(d.u()?).ok()?;
            let path = std::str::from_utf8(d.bytes()?).ok()?;
            let now = self.memo.file(path)?;
            if files.len() <= old {
                files.resize(old + 1, 0);
            }
            files[old] = now;
        }
        let n = d.u()?;
        let mut ids: HashMap<(u8, u32), u32> = HashMap::default();
        for _ in 0..n {
            let tag = d.u()?;
            let old = u32::try_from(d.u()?).ok()?;
            let name = std::str::from_utf8(d.bytes()?).ok()?;
            let (k, names) = if tag == 0 {
                (0, &self.fns)
            } else {
                (1, &self.structs)
            };
            ids.insert((k, old), names.id.get(name).copied().flatten()?);
        }
        let body = d.bytes()?;
        let map = |k: IdKind, v: u32| match k {
            IdKind::Function => ids.get(&(0, v)).copied(),
            IdKind::Struct => ids.get(&(1, v)).copied(),
            IdKind::Enum | IdKind::InferVar => None,
        };
        let mut d = Dec::new(body);
        d.files = Some(&files);
        d.ids = Some(&map);
        let check = u64::dec(&mut d)?;
        let flag = bool::dec(&mut d)?;
        let f = MirFunction::dec(&mut d)?;
        d.done().then_some((check, flag, f))
    }
}

/// The functions `f` calls or takes the address of, each once, in order.
pub(crate) fn named(f: &MirFunction) -> Vec<FunctionId> {
    let mut out: Vec<FunctionId> = f
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .filter_map(|s| match s {
            Statement::Assign {
                rvalue: RValue::Call { callee, .. } | RValue::FnAddr { callee, .. },
                ..
            } => Some(*callee),
            _ => None,
        })
        .collect();
    out.sort_unstable_by_key(|id| id.0);
    out.dedup();
    out
}

/// Per function `f` names, whether it is in each of `sets`.
pub(crate) fn facts(f: &MirFunction, sets: &[&HashSet<FunctionId>]) -> Vec<(FunctionId, u64)> {
    named(f)
        .into_iter()
        .map(|id| {
            let bits = sets
                .iter()
                .enumerate()
                .fold(0u64, |b, (i, s)| b | (u64::from(s.contains(&id)) << i));
            (id, bits)
        })
        .collect()
}
