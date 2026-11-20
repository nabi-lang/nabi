//! Load combining, above `-O0`: an integer assembled from the bytes of
//! a buffer read one by one, little-endian,
//!
//! ```text
//! b0 = buf_load<u8> B[P]; w0 = cast<u8 -> T>(b0)
//! i1 = Add(P, 1); b1 = buf_load<u8> B[i1]; w1 = cast(b1); s1 = Shl(w1, 8)
//! o1 = BitOr(w0, s1); ...
//! ```
//!
//! becomes one `T` load at element `P` (bytes swapped on a big-endian
//! target), where the target reads unaligned words. Every byte of the `T`
//! is read once at its place; the loads are unchecked (a checked index
//! branches between them), the parts used by the tree alone, and nothing
//! between the loads writes memory or reassigns the buffer or its index.
//! The wide load goes where the last byte load was; the index sums stay.

use nabic_core::HashMap;
use nabic_ir_common::{ConstVal, LocalId, PrimOp, PrimTy};
use nabic_mir::{MirFunction, MirTy, Operand, RValue, Statement, Target};

/// A combined load: `target = ty` at byte element `index` of `buf`.
pub struct Wide {
    pub buf: Operand,
    pub index: Operand,
    pub ty: MirTy,
    pub target: LocalId,
    /// A big-endian target: the bytes swapped after the load.
    pub swap: bool,
}

/// By `(block position, statement)`: `Some` where a combined load
/// replaces the statement, `None` where the statement goes.
pub type Plan = HashMap<(usize, usize), Option<Wide>>;

/// Whether `t` reads unaligned words in one load.
fn unaligned(t: &Target) -> bool {
    matches!(
        t.arch.as_str(),
        "x86_64" | "x86" | "aarch64" | "powerpc64" | "s390x" | "wasm32"
    )
}

/// The combined loads of `f`.
pub fn plan(f: &MirFunction, t: &Target) -> Plan {
    let mut out = Plan::default();
    if !unaligned(t) {
        return out;
    }
    let n = f.locals.len();
    let (mut uses, mut defs) = (vec![0u32; n], vec![0u32; n]);
    let mut any = false;
    for b in &f.blocks {
        for s in &b.stmts {
            nabic_mir::stmt_reads(s, &mut |l| bump(&mut uses, l));
            match s {
                Statement::Assign { target, rvalue } => {
                    bump(&mut defs, *target);
                    any |= matches!(
                        rvalue,
                        RValue::BufLoad {
                            elem_ty: MirTy::Prim(PrimTy::U8),
                            ..
                        }
                    );
                }
                Statement::ArrayWrite { array: l, .. } | Statement::StorageDead(l) => {
                    bump(&mut defs, *l);
                }
                _ => {}
            }
        }
        nabic_mir::term_reads(&b.terminator, &mut |l| bump(&mut uses, l));
        for p in &b.params {
            bump(&mut defs, *p);
        }
    }
    if !any {
        return out;
    }
    for p in &f.params {
        bump(&mut defs, *p);
    }
    let swap = t.endian == "big";
    for (bi, b) in f.blocks.iter().enumerate() {
        // Each local's assignment in this block.
        let mut at: HashMap<LocalId, usize> = HashMap::default();
        for (si, s) in b.stmts.iter().enumerate() {
            if let Statement::Assign { target, .. } = s {
                at.insert(*target, si);
            }
        }
        let cx = Cx {
            stmts: &b.stmts,
            uses: &uses,
            defs: &defs,
            at: &at,
        };
        for root in (0..b.stmts.len()).rev() {
            if out.contains_key(&(bi, root)) {
                continue;
            }
            if let Some((last, tree, w)) = cx.combine(root, swap) {
                for s in tree {
                    out.insert((bi, s), None);
                }
                out.insert((bi, last), Some(w));
            }
        }
    }
    out
}

fn bump(v: &mut [u32], l: LocalId) {
    if let Some(x) = v.get_mut(l.0 as usize) {
        *x += 1;
    }
}

struct Cx<'a> {
    stmts: &'a [Statement],
    uses: &'a [u32],
    defs: &'a [u32],
    at: &'a HashMap<LocalId, usize>,
}

/// A byte of the tree: its load, the buffer, the index and the shift.
struct Byte<'a> {
    load: usize,
    buf: &'a Operand,
    index: &'a Operand,
    shift: i128,
}

impl<'a> Cx<'a> {
    /// The statement assigning `o`, a local read once and assigned once,
    /// in this block before `before`.
    fn only_def(&self, o: &Operand, before: usize) -> Option<(usize, &'a RValue)> {
        let Operand::Copy(l) = o else { return None };
        let i = l.0 as usize;
        if self.uses.get(i) != Some(&1) || self.defs.get(i) != Some(&1) {
            return None;
        }
        let &s = self.at.get(l)?;
        if s >= before {
            return None;
        }
        let Statement::Assign { rvalue, .. } = &self.stmts[s] else {
            return None;
        };
        Some((s, rvalue))
    }

    /// The bytes under `o` (an `or` of shifted, widened byte loads of
    /// type `p`); the statements of the tree go into `tree`.
    fn leaves(
        &self,
        o: &Operand,
        p: PrimTy,
        before: usize,
        tree: &mut Vec<usize>,
        out: &mut Vec<Byte<'a>>,
    ) -> Option<()> {
        let (s, rv) = self.only_def(o, before)?;
        tree.push(s);
        if out.len() >= 8 || tree.len() > 32 {
            return None;
        }
        match rv {
            RValue::BinOp {
                op: PrimOp::BitOr,
                op_ty: MirTy::Prim(q),
                lhs,
                rhs,
            } if *q == p => {
                self.leaves(lhs, p, s, tree, out)?;
                self.leaves(rhs, p, s, tree, out)
            }
            RValue::BinOp {
                op: PrimOp::Shl,
                op_ty: MirTy::Prim(q),
                lhs,
                rhs,
            } if *q == p => {
                let shift = const_int(rhs)?;
                let (c, crv) = self.only_def(lhs, s)?;
                tree.push(c);
                self.widened(c, crv, p, shift, tree, out)
            }
            _ => self.widened(s, rv, p, 0, tree, out),
        }
    }

    /// A byte load at `s`'s operand, widened to `p` by `rv` (at `s`).
    fn widened(
        &self,
        s: usize,
        rv: &'a RValue,
        p: PrimTy,
        shift: i128,
        tree: &mut Vec<usize>,
        out: &mut Vec<Byte<'a>>,
    ) -> Option<()> {
        let RValue::Cast {
            source,
            from_ty: MirTy::Prim(PrimTy::U8),
            to_ty: MirTy::Prim(q),
        } = rv
        else {
            return None;
        };
        if *q != p {
            return None;
        }
        let (z, zrv) = self.only_def(source, s)?;
        let RValue::BufLoad {
            buf,
            index,
            elem_ty: MirTy::Prim(PrimTy::U8),
        } = zrv
        else {
            return None;
        };
        tree.push(z);
        out.push(Byte {
            load: z,
            buf,
            index,
            shift,
        });
        Some(())
    }

    /// `(base, k)` of an index `base + k` (`k` a constant; a sum that
    /// wraps names the same address), or the index itself at 0.
    fn offset(&self, o: &'a Operand) -> (&'a Operand, i128) {
        if let Operand::Copy(l) = o {
            let i = l.0 as usize;
            if self.defs.get(i) == Some(&1) {
                if let Some(&s) = self.at.get(l) {
                    if let Statement::Assign {
                        rvalue:
                            RValue::BinOp {
                                op: PrimOp::Add | PrimOp::WrappingAdd,
                                op_ty: MirTy::Prim(PrimTy::Usize),
                                lhs,
                                rhs,
                            },
                        ..
                    } = &self.stmts[s]
                    {
                        if let Some(k) = const_int(rhs) {
                            return (lhs, k);
                        }
                        if let Some(k) = const_int(lhs) {
                            return (rhs, k);
                        }
                    }
                }
            }
        }
        (o, 0)
    }

    /// Whether statement `s` assigns local `l` (or ends it).
    fn assigns(&self, s: usize, l: LocalId) -> bool {
        match &self.stmts[s] {
            Statement::Assign { target, .. } => *target == l,
            Statement::StorageDead(x) | Statement::ArrayWrite { array: x, .. } => *x == l,
            _ => false,
        }
    }

    /// The combined load rooted at the `or` at `root`: where it goes, the
    /// statements it replaces, and the load.
    fn combine(&self, root: usize, swap: bool) -> Option<(usize, Vec<usize>, Wide)> {
        let Statement::Assign {
            target,
            rvalue:
                RValue::BinOp {
                    op: PrimOp::BitOr,
                    op_ty: MirTy::Prim(p),
                    lhs,
                    rhs,
                },
        } = &self.stmts[root]
        else {
            return None;
        };
        let p = *p;
        let width: i128 = match p {
            PrimTy::U16 | PrimTy::I16 => 2,
            PrimTy::U32 | PrimTy::I32 => 4,
            PrimTy::U64 | PrimTy::I64 => 8,
            _ => return None,
        };
        let mut tree = vec![root];
        let mut bytes = Vec::new();
        self.leaves(lhs, p, root, &mut tree, &mut bytes)?;
        self.leaves(rhs, p, root, &mut tree, &mut bytes)?;
        if bytes.len() as i128 != width {
            return None;
        }
        let buf = bytes[0].buf;
        let (base, _) = self.offset(bytes[0].index);
        let mut seen = 0u8;
        for b in &bytes {
            let (x, k) = self.offset(b.index);
            if b.buf != buf || x != base || !(0..width).contains(&k) || b.shift != 8 * k {
                return None;
            }
            seen |= 1 << k;
        }
        if u32::from(seen) != (1u32 << width) - 1 {
            return None;
        }
        let first = bytes.iter().map(|b| b.load).min()?;
        let last = bytes.iter().map(|b| b.load).max()?;
        // From the first index sum on: the buffer and the base stay. The
        // base itself (byte 0's index, assigned here) is no sum.
        let sums = bytes.iter().filter_map(|b| match b.index {
            Operand::Copy(l) if b.index != base => self.at.get(l).copied(),
            _ => None,
        });
        let lo = sums.chain([first]).min()?;
        for s in lo..last {
            if [buf, base]
                .iter()
                .any(|o| matches!(o, Operand::Copy(l) if self.assigns(s, *l)))
            {
                return None;
            }
        }
        // Between the loads: no write of memory.
        for s in first..=last {
            let ok = match &self.stmts[s] {
                Statement::Loc(..) => true,
                Statement::Assign { rvalue, .. } => matches!(
                    rvalue,
                    RValue::Use(_)
                        | RValue::BinOp { .. }
                        | RValue::UnaryOp { .. }
                        | RValue::Cast { .. }
                        | RValue::Bitcast { .. }
                        | RValue::BufLoad { .. }
                ),
                _ => false,
            };
            if !ok {
                return None;
            }
        }
        // The result is written early: nothing reads it before its
        // assignment.
        if (last..root).any(|s| {
            let mut hit = false;
            nabic_mir::stmt_reads(&self.stmts[s], &mut |l| hit |= l == *target);
            hit
        }) {
            return None;
        }
        tree.retain(|&s| s != last);
        Some((
            last,
            tree,
            Wide {
                buf: buf.clone(),
                index: base.clone(),
                ty: MirTy::Prim(p),
                target: *target,
                swap,
            },
        ))
    }
}

fn const_int(o: &Operand) -> Option<i128> {
    match o {
        Operand::Const(ConstVal::Int { value, .. }) => Some(*value),
        Operand::Const(ConstVal::UInt { value, .. }) => i128::try_from(*value).ok(),
        _ => None,
    }
}
