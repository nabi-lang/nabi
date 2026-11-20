//! Objects of static storage: their bytes and the addresses in them.

use crate::{Lx, R};
use nabic_ir_common::ConstVal;
use nabic_mir::{CData, CRef, CReloc, MirStaticDef};
use nabicc_sema::{Const, Def, InitKind, Sym, TInit, fold};

impl Lx<'_> {
    /// A constant's bytes for a `size`-byte scalar of C type `ty` (an
    /// address: its relocation), into `bytes` at `at`.
    fn put(
        &self,
        bytes: &mut [u8],
        relocs: &mut Vec<CReloc>,
        at: u64,
        ty: nabicc_parse::ty::TypeId,
        c: Const,
        loc: nabicc_lex::Loc,
    ) -> R<()> {
        let types = &self.u.types;
        let size = self.size(ty) as usize;
        let be = types.t.big_endian;
        let at_u = at as usize;
        let mut int = |v: u128, n: usize| {
            let b = v.to_le_bytes();
            let dst = &mut bytes[at_u..at_u + n];
            if be {
                for i in 0..n {
                    dst[i] = b[n - 1 - i];
                }
            } else {
                dst.copy_from_slice(&b[..n]);
            }
        };
        let float = types.prim_of(ty).is_some_and(nabicc_target::Prim::is_float);
        match c {
            Const::Int(v) if float => {
                if size == 4 {
                    int(u128::from((v as f32).to_bits()), 4);
                } else {
                    int(u128::from((v as f64).to_bits()), 8);
                }
            }
            Const::Int(v) => int(v as u128, size.min(16)),
            Const::Float(f) if size == 4 => int(u128::from((f as f32).to_bits()), 4),
            Const::Float(f) if float => int(u128::from(f.to_bits()), 8),
            Const::Float(f) => int(f as i128 as u128, size),
            Const::Addr(s, off) => {
                if size as u64 != types.t.ptr {
                    return Err(self.err(loc, "an address in an integer narrower than a pointer"));
                }
                let to = match s {
                    Sym::Global(g) => CRef::Static(self.u.globals[g].symbol.clone()),
                    Sym::Func(f) => match self.fn_ids.get(&f) {
                        Some(id) => CRef::Fn(*id),
                        None => return Err(self.err(loc, "the address of a function not compiled")),
                    },
                };
                relocs.push(CReloc {
                    at: at as u32,
                    to,
                    addend: off,
                });
            }
        }
        Ok(())
    }

    /// The bytes `init` gives an object of `size` bytes.
    pub(crate) fn init_bytes(&self, init: &TInit, size: u64) -> R<(Vec<u8>, Vec<CReloc>)> {
        let mut bytes = vec![0u8; size as usize];
        let mut relocs = Vec::new();
        for it in &init.items {
            match &it.kind {
                InitKind::Bytes(b) => {
                    let end = (it.off as usize + b.len()).min(bytes.len());
                    let n = end.saturating_sub(it.off as usize);
                    bytes[it.off as usize..end].copy_from_slice(&b[..n]);
                }
                InitKind::Expr(e) => {
                    if self.u.types.size_align(e.ty).is_some() && self.record_like(e.ty) {
                        // A whole record from a constant compound literal.
                        let nabicc_sema::TK::Global(g) = &e.kind else {
                            return Err(self
                                .err(e.loc, "initializer element is not a compile-time constant"));
                        };
                        let Def::Init(inner) = &self.u.globals[*g].def else {
                            continue;
                        };
                        let n = self.size(e.ty);
                        let (b, rs) = self.init_bytes(inner, n)?;
                        let o = it.off as usize;
                        bytes[o..o + b.len()].copy_from_slice(&b);
                        relocs.extend(rs.into_iter().map(|r| CReloc {
                            at: r.at + it.off as u32,
                            ..r
                        }));
                        continue;
                    }
                    let Some(c) = fold(&self.u.types, e) else {
                        return Err(self.err(
                            e.loc,
                            "initializer element is not a compile-time constant; set it at run \
                             time, in a function",
                        ));
                    };
                    self.put(&mut bytes, &mut relocs, it.off, e.ty, c, e.loc)?;
                }
                InitKind::Bits(e, b) => {
                    let Some(Const::Int(v)) = fold(&self.u.types, e) else {
                        return Err(
                            self.err(e.loc, "a bitfield's initializer is not an integer constant")
                        );
                    };
                    let n = b.unit as usize;
                    let o = it.off as usize;
                    let be = self.u.types.t.big_endian;
                    let mut w = [0u8; 8];
                    w[..n].copy_from_slice(&bytes[o..o + n]);
                    let mut cur = if be {
                        let mut x = 0u64;
                        for byte in &w[..n] {
                            x = (x << 8) | u64::from(*byte);
                        }
                        x
                    } else {
                        u64::from_le_bytes(w)
                    };
                    let mask = if b.width >= 64 {
                        u64::MAX
                    } else {
                        (1u64 << b.width) - 1
                    };
                    cur = (cur & !(mask << b.at)) | (((v as u64) & mask) << b.at);
                    if be {
                        for i in 0..n {
                            bytes[o + i] = (cur >> (8 * (n - 1 - i))) as u8;
                        }
                    } else {
                        bytes[o..o + n].copy_from_slice(&cur.to_le_bytes()[..n]);
                    }
                }
            }
        }
        Ok((bytes, relocs))
    }

    fn record_like(&self, t: nabicc_parse::ty::TypeId) -> bool {
        matches!(
            self.u.types.get(self.u.types.canon(t)),
            nabicc_parse::ty::Type::Record(_) | nabicc_parse::ty::Type::Array { .. }
        )
    }
}

/// Static object `g` as a MIR static.
pub(crate) fn global(lx: &mut Lx<'_>, g: usize) -> R<MirStaticDef> {
    let gl = &lx.u.globals[g];
    let mut ty = lx.mir_ty(gl.ty, gl.loc)?;
    let size = lx.size(gl.ty);
    if gl.align > u64::from(lx.t.size_align(&ty).1) {
        // `aligned`: an array of so aligned words.
        let w = match gl.align {
            1 => nabic_ir_common::PrimTy::U8,
            2 => nabic_ir_common::PrimTy::U16,
            4 => nabic_ir_common::PrimTy::U32,
            _ => nabic_ir_common::PrimTy::U64,
        };
        let n = size.div_ceil(gl.align.min(8)).max(1) as u32;
        ty = nabic_mir::MirTy::Array(Box::new(nabic_mir::MirTy::Prim(w)), n);
    }
    let (bytes, relocs) = match &gl.def {
        Def::Init(init) => lx.init_bytes(init, size)?,
        _ => (vec![0; size as usize], Vec::new()),
    };
    Ok(MirStaticDef {
        name: gl.symbol.clone(),
        ty,
        init: ConstVal::Unit,
        is_mut: !gl.konst,
        is_extern: gl.def == Def::Extern,
        dll_import: false,
        init_fn: None,
        c: Some(CData {
            global: !gl.internal,
            bytes,
            relocs,
            tls: gl.tls,
        }),
    })
}
