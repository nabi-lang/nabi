//! The -g description of a function: scopes and variable parts.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

impl FnLower<'_, '_> {
    /// The `-g` description of this function: its scopes (by first
    /// position) and named locals; optimized, also the bodies inlined and
    /// the locals whose assignments went (`MirFunction::dbg`).
    pub(crate) fn debug_facts(&self) -> crate::lir::FnDebug {
        let f = self.f;
        let track = self.c.track;
        let mut scopes: Vec<(u32, u32)> = Vec::new();
        let mut decl: HashMap<u32, (u32, u32)> = HashMap::default();
        let (mut line, mut scope) = (f.span.line, 0);
        for s in f.blocks.iter().flat_map(|b| &b.stmts) {
            match s {
                Statement::Loc(sp, sc) => {
                    (line, scope) = (sp.line, sc.id);
                    if sc.id != 0 && !scopes.iter().any(|(i, _)| *i == sc.id) {
                        scopes.push((sc.id, sc.parent));
                    }
                }
                Statement::Assign { target, .. } => {
                    decl.entry(target.0).or_insert((line, scope));
                }
                _ => {}
            }
        }
        // Optimized: the bodies inlined; with registers, the binds too
        // (wasm32 describes the locals still assigned).
        let none = nabic_mir::FnDbg::default();
        let dbg = if self.c.opt { &f.dbg } else { &none };
        let binds = if track { &dbg.binds[..] } else { &[] };
        // Assigned still; declared at an inlined call, not in the body.
        let assigned: HashSet<u32> = decl.keys().copied().collect();
        for &(l, line, sc) in &dbg.decls {
            decl.insert(l.0, (line, sc.id));
        }
        let more = dbg
            .inlined
            .iter()
            .map(|i| (i.scope, i.parent))
            .chain(binds.iter().map(|b| (b.scope.id, b.scope.parent)))
            .chain(dbg.decls.iter().map(|d| (d.2.id, d.2.parent)));
        for (id, parent) in more {
            if id != 0 && !scopes.iter().any(|(i, _)| *i == id) {
                scopes.push((id, parent));
            }
        }
        scopes.sort_unstable();
        let params: HashSet<u32> = f
            .blocks
            .iter()
            .flat_map(|b| b.params.iter().map(|p| p.0))
            .collect();
        let inl_params: HashSet<u32> = dbg
            .inlined
            .iter()
            .flat_map(|i| i.params.iter().map(|p| p.0))
            .collect();
        let vars = f
            .locals
            .iter()
            .filter_map(|l| {
                let name = l.name.as_deref().filter(|n| nabic_mir::debug_visible(n))?;
                // A read-only array has no bytes of its own to show.
                if params.contains(&l.id.0) || self.ro.contains_key(&l.id) {
                    return None;
                }
                let arg = f.params.iter().position(|p| *p == l.id).map(|i| i + 1);
                // Optimized, a local no assignment of which is left is
                // what its bind says (else not described).
                let mut at = decl.get(&l.id.0).copied();
                let mut slot = self.local_mem(l.id);
                let mut konst = None;
                let mut parts = Vec::new();
                // A parameter taken apart on entry: its fields' locals.
                let split = arg.is_some()
                    && binds
                        .iter()
                        .any(|b| b.local == l.id && b.value.is_none() && !b.parts.is_empty());
                if self.c.opt && !assigned.contains(&l.id.0) && (arg.is_none() || split) {
                    let b = binds.iter().find(|b| b.local == l.id)?;
                    at = at.or(Some((b.line, b.scope.id)));
                    slot = None;
                    match &b.value {
                        Some(Operand::Copy(y)) => slot = self.local_mem(*y),
                        Some(Operand::Const(c)) => konst = const_bits(c),
                        None => parts = self.debug_parts(&l.ty, &b.parts),
                    }
                }
                let slot = match slot {
                    Some(Mem::Frame(s, _)) => s,
                    _ if track => crate::lir::NO_SLOT,
                    _ => return None,
                };
                let (line, scope) = at.unwrap_or((f.span.line, 0));
                let ty = self
                    .c
                    .debug
                    .and_then(|d| d.locals.get(&(f.id, l.id)))
                    .filter(|t| t.fits(&l.ty))
                    .cloned()
                    .unwrap_or_else(|| nabic_mir::DebugTy::Mir(l.ty.clone()));
                Some(crate::lir::DebugVar {
                    name: name.to_owned(),
                    slot,
                    ty,
                    line,
                    scope,
                    arg,
                    param: arg.is_some() || inl_params.contains(&l.id.0),
                    konst,
                    parts,
                })
            })
            .collect();
        let inlined = dbg
            .inlined
            .iter()
            .map(|i| crate::lir::InlineSite {
                scope: i.scope,
                parent: i.parent,
                name: i.name.clone(),
                file: i.decl.file,
                line: i.decl.line,
                call_line: i.call.line,
                call_col: i.call.col.max(1),
            })
            .collect();
        crate::lir::FnDebug {
            name: f.path().to_owned(),
            file: f.span.file,
            line: f.span.line,
            scopes,
            vars,
            ret: f.debug_ret(),
            opt: track,
            inlined,
        }
    }

    /// The parts of a `ty` built of `ops` (a bind's `parts`), laid out.
    pub(crate) fn debug_parts(
        &self,
        ty: &MirTy,
        ops: &[Option<Operand>],
    ) -> Vec<(u32, u32, crate::lir::PartSrc)> {
        let (MirTy::Struct(_, ps) | MirTy::Tuple(ps)) = ty else {
            return Vec::new();
        };
        if ps.len() != ops.len() {
            return Vec::new();
        }
        let offs = self.c.t.field_offsets(ty);
        ps.iter()
            .zip(offs)
            .zip(ops)
            .filter_map(|((p, off), o)| {
                let size = self.c.t.size_align(p).0;
                let src = match o {
                    Some(Operand::Copy(y)) => match self.local_mem(*y) {
                        Some(Mem::Frame(s, 0)) => PartSrc::Slot(s),
                        _ => PartSrc::Gone,
                    },
                    Some(Operand::Const(c)) if size <= 8 => {
                        const_bits(c).map_or(PartSrc::Gone, PartSrc::Const)
                    }
                    _ => PartSrc::Gone,
                };
                (size > 0).then_some((off, size, src))
            })
            .collect()
    }
}
