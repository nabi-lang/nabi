//! A function body: locals, blocks and statements.

use crate::{Lx, R};
use nabic_ir_common::{BlockId, ConstVal, LocalId, PrimOp, PrimTy};
use nabic_mir::{
    BasicBlock, LocalDecl, MirFunction, MirTy, Operand, RValue, Statement, Terminator,
};
use nabicc_parse::ty::Func;
use nabicc_sema::{Body, TS, TStmt};

/// Where a C local lives.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Var {
    /// A MIR local holding its value.
    Reg(LocalId),
    /// A MIR local whose slot is the object (reached by its address).
    Mem(LocalId),
}

/// The function being lowered.
pub(crate) struct Fb<'a, 'b> {
    pub lx: &'a mut Lx<'b>,
    pub f: &'a mut MirFunction,
    /// The open block (`None`: what follows is unreachable).
    pub cur: Option<usize>,
    pub vars: Vec<Var>,
    pub labels: Vec<BlockId>,
    pub brk: Vec<BlockId>,
    pub cont: Vec<BlockId>,
    pub body: &'a Body,
}

/// Lower body `b` of `m` (its declaration filled in).
pub(crate) fn lower(lx: &mut Lx<'_>, m: &mut MirFunction, b: &Body, _ft: &Func) -> R<()> {
    let mut fb = Fb {
        lx,
        f: m,
        cur: None,
        vars: Vec::new(),
        labels: Vec::new(),
        brk: Vec::new(),
        cont: Vec::new(),
        body: b,
    };
    let param_tys = fb.f.param_tys.clone();
    for (i, l) in b.locals.iter().enumerate() {
        let param = b.params.iter().position(|p| *p as usize == i);
        let ty = match param {
            Some(k) => param_tys[k].clone(),
            None => fb.lx.mir_ty(l.ty, l.loc)?,
        };
        let id = fb.local(ty, Some(&l.name));
        if param.is_some() {
            fb.f.params.push(id);
        }
        fb.vars.push(if l.addressed {
            Var::Mem(id)
        } else {
            Var::Reg(id)
        });
    }
    for _ in 0..b.labels {
        let bb = fb.block();
        fb.labels.push(bb);
    }
    let entry = fb.block();
    fb.cur = Some(entry.0 as usize);
    for s in &b.stmts {
        fb.stmt(s)?;
    }
    if fb.cur.is_some() {
        // Off the end: `main` returns 0, others an unspecified value.
        let ret = fb.f.ret_ty.clone();
        let v = fb.zero(&ret);
        fb.term(Terminator::Return(v));
    }
    // Entry first.
    let e =
        fb.f.blocks
            .iter()
            .position(|bb| bb.id == entry)
            .unwrap_or(0);
    let eb = fb.f.blocks.remove(e);
    fb.f.blocks.insert(0, eb);
    Ok(())
}

impl Fb<'_, '_> {
    pub(crate) fn local(&mut self, ty: MirTy, name: Option<&str>) -> LocalId {
        let id = LocalId(self.f.locals.len() as u32);
        self.f.locals.push(LocalDecl {
            id,
            name: name.filter(|n| !n.is_empty()).map(str::to_owned),
            ty,
        });
        id
    }

    pub(crate) fn block(&mut self) -> BlockId {
        let id = BlockId(self.f.blocks.len() as u32);
        self.f.blocks.push(BasicBlock {
            id,
            params: Vec::new(),
            stmts: Vec::new(),
            terminator: Terminator::Unreachable,
        });
        id
    }

    fn index(&self, b: BlockId) -> usize {
        self.f
            .blocks
            .iter()
            .position(|x| x.id == b)
            .expect("a block of this function")
    }

    /// Continue in block `b`.
    pub(crate) fn enter(&mut self, b: BlockId) {
        if self.cur.is_some() {
            self.term(Terminator::Goto {
                target: b,
                args: Vec::new(),
            });
        }
        self.cur = Some(self.index(b));
    }

    pub(crate) fn push(&mut self, s: Statement) {
        let i = if let Some(i) = self.cur {
            i
        } else {
            let b = self.block();
            let i = self.index(b);
            self.cur = Some(i);
            i
        };
        self.f.blocks[i].stmts.push(s);
    }

    pub(crate) fn term(&mut self, t: Terminator) {
        if let Some(i) = self.cur.take() {
            self.f.blocks[i].terminator = t;
        }
    }

    pub(crate) fn goto(&mut self, b: BlockId) {
        self.term(Terminator::Goto {
            target: b,
            args: Vec::new(),
        });
    }

    pub(crate) fn branch(&mut self, c: Operand, t: BlockId, e: BlockId) {
        self.term(Terminator::If {
            cond: c,
            then_bb: t,
            then_args: Vec::new(),
            else_bb: e,
            else_args: Vec::new(),
        });
    }

    /// A new local of `ty` set to `rv`.
    pub(crate) fn tmp(&mut self, ty: MirTy, rv: RValue) -> Operand {
        let l = self.local(ty, None);
        self.push(Statement::Assign {
            target: l,
            rvalue: rv,
        });
        Operand::Copy(l)
    }

    /// A zero (or nothing) of `ty`.
    pub(crate) fn zero(&mut self, ty: &MirTy) -> Option<Operand> {
        Some(match ty {
            MirTy::Unit => return None,
            MirTy::Prim(p) => Operand::Const(crate::expr::cval(0, *p)),
            t => {
                let l = self.local(t.clone(), None);
                let a = self.tmp(MirTy::Prim(PrimTy::Usize), RValue::AddrOf(l));
                let size = self.lx.t.size_align(t).0;
                self.memzero(a, u64::from(size));
                Operand::Copy(l)
            }
        })
    }

    /// `n` zero bytes at address `a`.
    pub(crate) fn memzero(&mut self, a: Operand, n: u64) {
        let mut off = 0;
        while off < n {
            let (k, p) = match n - off {
                r if r >= 8 && self.lx.t.pointer_width == 64 => (8, PrimTy::U64),
                r if r >= 4 => (4, PrimTy::U32),
                r if r >= 2 => (2, PrimTy::U16),
                _ => (1, PrimTy::U8),
            };
            let at = self.offset(a.clone(), off);
            self.push(Statement::RawStore {
                addr: at,
                value: Operand::Const(crate::expr::cval(0, p)),
                ty: MirTy::Prim(p),
                volatile: false,
            });
            off += k;
        }
    }

    /// `a + off`.
    pub(crate) fn offset(&mut self, a: Operand, off: u64) -> Operand {
        if off == 0 {
            return a;
        }
        let w = MirTy::Prim(PrimTy::Usize);
        self.tmp(
            w.clone(),
            RValue::BinOp {
                op: PrimOp::WrappingAdd,
                op_ty: w,
                lhs: a,
                rhs: Operand::Const(ConstVal::UInt {
                    value: u128::from(off),
                    ty: PrimTy::Usize,
                }),
            },
        )
    }

    fn at(&mut self, s: &TStmt) {
        if self.lx.debug && s.loc.line != 0 {
            let sp = self.lx.span(s.loc);
            self.push(Statement::Loc(sp, nabic_mir::Scope::default()));
        }
    }

    pub(crate) fn stmt(&mut self, s: &TStmt) -> R<()> {
        if !matches!(s.kind, TS::Block(_) | TS::Label(..)) {
            self.at(s);
        }
        match &s.kind {
            TS::Expr(e) => {
                self.effect(e)?;
            }
            TS::Init(l, init) => {
                let a = self.var_addr(*l);
                let size = self.lx.size(self.body.locals[*l as usize].ty);
                let var = self.vars[*l as usize];
                match var {
                    Var::Reg(r) => {
                        // A scalar: its one value.
                        let ty = self.f.locals[r.0 as usize].ty.clone();
                        let v = match init.items.first() {
                            Some(nabicc_sema::InitItem {
                                kind: nabicc_sema::InitKind::Expr(e),
                                ..
                            }) => self.val(e)?,
                            _ => self.zero(&ty).unwrap_or(Operand::Const(ConstVal::Unit)),
                        };
                        self.push(Statement::Assign {
                            target: r,
                            rvalue: RValue::Use(v),
                        });
                    }
                    Var::Mem(_) => {
                        let a = a.expect("an object in memory");
                        self.init(a, size, init)?;
                    }
                }
            }
            TS::Block(ss) => {
                for s in ss {
                    self.stmt(s)?;
                }
            }
            TS::If(c, a, b) => {
                let (t, e, end) = (self.block(), self.block(), self.block());
                self.cond_branch(c, t, if b.is_some() { e } else { end })?;
                self.cur = Some(self.index(t));
                self.stmt(a)?;
                self.goto(end);
                if let Some(b) = b {
                    self.cur = Some(self.index(e));
                    self.stmt(b)?;
                    self.goto(end);
                }
                self.cur = Some(self.index(end));
            }
            TS::While(c, b) => {
                let (head, body, end) = (self.block(), self.block(), self.block());
                self.enter(head);
                self.cond_branch(c, body, end)?;
                self.cur = Some(self.index(body));
                self.brk.push(end);
                self.cont.push(head);
                self.stmt(b)?;
                self.brk.pop();
                self.cont.pop();
                self.goto(head);
                self.cur = Some(self.index(end));
            }
            TS::Do(b, c) => {
                let (body, test, end) = (self.block(), self.block(), self.block());
                self.enter(body);
                self.brk.push(end);
                self.cont.push(test);
                self.stmt(b)?;
                self.brk.pop();
                self.cont.pop();
                self.enter(test);
                self.cond_branch(c, body, end)?;
                self.cur = Some(self.index(end));
            }
            TS::For(init, c, step, b) => {
                if let Some(i) = init {
                    self.stmt(i)?;
                }
                let (head, body, next, end) =
                    (self.block(), self.block(), self.block(), self.block());
                self.enter(head);
                match c {
                    Some(c) => self.cond_branch(c, body, end)?,
                    None => self.goto(body),
                }
                self.cur = Some(self.index(body));
                self.brk.push(end);
                self.cont.push(next);
                self.stmt(b)?;
                self.brk.pop();
                self.cont.pop();
                self.enter(next);
                if let Some(e) = step {
                    self.effect(e)?;
                }
                self.goto(head);
                self.cur = Some(self.index(end));
            }
            TS::Switch(v, b, cases, default) => {
                let x = self.val(v)?;
                let ty = self.lx.mir_ty(v.ty, v.loc)?;
                let MirTy::Prim(p) = ty else {
                    return Err(self.lx.err(v.loc, "a `switch` on a non-integer"));
                };
                let end = self.block();
                let other = default.map_or(end, |d| self.labels[d as usize]);
                let mut cs: Vec<(i128, i128, BlockId)> = cases
                    .iter()
                    .map(|(lo, hi, l)| (*lo, *hi, self.labels[*l as usize]))
                    .collect();
                cs.sort_by_key(|c| c.0);
                // The value in a local: the tests read it many times.
                let xl = match x {
                    Operand::Copy(l) => Operand::Copy(l),
                    c @ Operand::Const(_) => self.tmp(ty, RValue::Use(c)),
                };
                self.dispatch(&xl, p, &cs, other);
                self.brk.push(end);
                self.stmt(b)?;
                self.brk.pop();
                self.enter(end);
            }
            TS::Break => {
                let b = *self.brk.last().expect("checked");
                self.goto(b);
            }
            TS::Continue => {
                let b = *self.cont.last().expect("checked");
                self.goto(b);
            }
            TS::Return(e) => {
                let v = match e {
                    Some(e) if self.f.ret_ty != MirTy::Unit => Some(self.value(e)?),
                    Some(e) => {
                        self.effect(e)?;
                        None
                    }
                    None => {
                        let ret = self.f.ret_ty.clone();
                        self.zero(&ret)
                    }
                };
                self.term(Terminator::Return(v));
            }
            TS::Goto(l) => {
                let b = self.labels[*l as usize];
                self.goto(b);
            }
            TS::Label(l, s) => {
                let b = self.labels[*l as usize];
                self.enter(b);
                self.stmt(s)?;
            }
            TS::Null => {}
            TS::Asm => {
                return Err(self.lx.err(
                    s.loc,
                    "`nabi cc` does not compile inline assembly; write the code in C, or define \
                     the macro that selects the library's portable code path",
                ));
            }
        }
        Ok(())
    }

    /// Jump to the block of the case holding `x` (sorted `cs`), else to
    /// `other`: a binary search of compares.
    fn dispatch(&mut self, x: &Operand, p: PrimTy, cs: &[(i128, i128, BlockId)], other: BlockId) {
        let ty = MirTy::Prim(p);
        let cmp = |fb: &mut Self, op: PrimOp, v: i128| {
            fb.tmp(
                MirTy::Prim(PrimTy::Bool),
                RValue::BinOp {
                    op,
                    op_ty: ty.clone(),
                    lhs: x.clone(),
                    rhs: Operand::Const(crate::expr::cval(v, p)),
                },
            )
        };
        match cs {
            [] => self.goto(other),
            [(lo, hi, b)] => {
                if lo == hi {
                    let c = cmp(self, PrimOp::Eq, *lo);
                    self.branch(c, *b, other);
                } else {
                    let ge = self.block();
                    let c = cmp(self, PrimOp::Ge, *lo);
                    self.branch(c, ge, other);
                    self.cur = Some(self.index(ge));
                    let c = cmp(self, PrimOp::Le, *hi);
                    self.branch(c, *b, other);
                }
            }
            _ if cs.len() <= 4 => {
                for (i, (lo, hi, b)) in cs.iter().enumerate() {
                    let next = if i + 1 == cs.len() {
                        other
                    } else {
                        self.block()
                    };
                    if lo == hi {
                        let c = cmp(self, PrimOp::Eq, *lo);
                        self.branch(c, *b, next);
                    } else {
                        self.dispatch(x, p, &cs[i..=i], next);
                    }
                    if i + 1 < cs.len() {
                        self.cur = Some(self.index(next));
                    }
                }
            }
            _ => {
                let mid = cs.len() / 2;
                let (left, right) = (self.block(), self.block());
                let c = cmp(self, PrimOp::Lt, cs[mid].0);
                self.branch(c, left, right);
                self.cur = Some(self.index(left));
                self.dispatch(x, p, &cs[..mid], other);
                self.cur = Some(self.index(right));
                self.dispatch(x, p, &cs[mid..], other);
            }
        }
    }

    /// The address of local `l`'s object, if it lives in memory.
    pub(crate) fn var_addr(&mut self, l: u32) -> Option<Operand> {
        let var = self.vars[l as usize];
        match var {
            Var::Mem(m) => Some(self.tmp(MirTy::Prim(PrimTy::Usize), RValue::AddrOf(m))),
            Var::Reg(_) => None,
        }
    }

    /// Initialize the `size` bytes at `a` with `init`: zeroed, then its
    /// stores.
    pub(crate) fn init(&mut self, a: Operand, size: u64, init: &nabicc_sema::TInit) -> R<()> {
        let covered: u64 = init
            .items
            .iter()
            .map(|it| match &it.kind {
                nabicc_sema::InitKind::Bytes(b) => b.len() as u64,
                nabicc_sema::InitKind::Expr(e) => self.lx.size(e.ty),
                nabicc_sema::InitKind::Bits(..) => 0,
            })
            .sum();
        // Everything stored over: no zeroing first.
        let whole = covered >= size
            && init
                .items
                .iter()
                .all(|it| !matches!(it.kind, nabicc_sema::InitKind::Bits(..)))
            && init.items.len() == 1;
        if !whole {
            self.memzero(a.clone(), size);
        }
        for it in &init.items {
            let at = self.offset(a.clone(), it.off);
            match &it.kind {
                nabicc_sema::InitKind::Bytes(b) => {
                    for (i, byte) in b.iter().enumerate() {
                        if *byte == 0 && !whole {
                            continue;
                        }
                        let p = self.offset(at.clone(), i as u64);
                        self.push(Statement::RawStore {
                            addr: p,
                            value: Operand::Const(ConstVal::UInt {
                                value: u128::from(*byte),
                                ty: PrimTy::U8,
                            }),
                            ty: MirTy::Prim(PrimTy::U8),
                            volatile: false,
                        });
                    }
                }
                nabicc_sema::InitKind::Expr(e) => {
                    let ty = self.lx.mir_ty(e.ty, e.loc)?;
                    if matches!(ty, MirTy::Prim(_)) {
                        let v = self.val(e)?;
                        self.push(Statement::RawStore {
                            addr: at,
                            value: v,
                            ty,
                            volatile: false,
                        });
                    } else {
                        let src = self.agg_addr(e)?;
                        let n = self.lx.size(e.ty);
                        self.copy(at, src, n, &ty);
                    }
                }
                nabicc_sema::InitKind::Bits(e, b) => {
                    let v = self.val(e)?;
                    let fty = self.lx.mir_ty(e.ty, e.loc)?;
                    self.store_bits(at, *b, v, &fty);
                }
            }
        }
        Ok(())
    }
}
