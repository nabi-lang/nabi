//! Function types, calls and returns.

#[allow(clippy::wildcard_imports)]
use crate::isa::wasm::*;

/// `(params, results, sret)` of `f`: lowering's record, else what its
/// `Param` and `Ret` instructions say.
pub(crate) fn signature(f: &Func) -> (Vec<Part>, Option<Vec<Part>>) {
    if let Some(s) = &f.sig {
        return s.clone();
    }
    let mut params: Vec<Part> = Vec::new();
    let mut sret = false;
    for i in &f.insts {
        if let Inst::Param { part, .. } = i {
            match part.loc {
                Loc::Sret => sret = true,
                Loc::Int(n) => {
                    let n = n as usize;
                    if params.len() <= n {
                        params.resize(n + 1, *part);
                    }
                    params[n] = *part;
                }
                _ => {}
            }
        }
    }
    let ret = f.insts.iter().find_map(|i| match i {
        Inst::Ret { vals } => Some(vals.iter().map(|v| v.0).collect::<Vec<_>>()),
        _ => None,
    });
    (
        params,
        if sret {
            None
        } else {
            Some(ret.unwrap_or_default())
        },
    )
}

/// The type of `params` / `ret` parts.
pub(crate) fn fn_ty(params: &[Part], ret: Option<&[Part]>, variadic: bool) -> Ty {
    let mut p: Vec<u8> = Vec::new();
    if ret.is_none() {
        p.push(I32);
    }
    p.extend(
        params
            .iter()
            .filter(|x| matches!(x.loc, Loc::Int(_)))
            .map(|x| vt(x.k)),
    );
    if variadic {
        p.push(I32);
    }
    (
        p,
        ret.map(|r| r.iter().map(|x| vt(x.k)).collect())
            .unwrap_or_default(),
    )
}

/// The type of a C function `params -> ret` classified as `abi`.
pub fn c_type(
    t: &nabic_mir::Target,
    params: &[nabic_mir::MirTy],
    ret: &nabic_mir::MirTy,
    abi: &nabic_codegen::abi::FnAbi,
) -> Ty {
    let pw = t.pointer_width;
    let ptr = || crate::cc::coerced(&CType::PTR, pw);
    let shapes: Vec<crate::cc::Shape> = params
        .iter()
        .zip(&abi.args)
        .map(|(p, a)| match a {
            ArgAbi::Direct if *p == MirTy::Prim(PrimTy::Str) && !abi.nabi => ptr(),
            ArgAbi::Direct => crate::cc::shape(p, t),
            ArgAbi::Coerce(c) => crate::cc::coerced(c, pw),
            ArgAbi::Indirect { byval: true } => {
                let (size, align) = t.size_align(p);
                crate::cc::byval(size.max(1), align, pw)
            }
            ArgAbi::Indirect { byval: false } => ptr(),
        })
        .collect();
    let r = match &abi.ret {
        RetAbi::Direct if *ret != MirTy::Unit => Some(crate::cc::shape(ret, t)),
        RetAbi::Coerce(c) => Some(crate::cc::coerced(c, pw)),
        _ => None,
    };
    let l = crate::cc::assign(
        crate::cc::Conv::Wasm32,
        &shapes,
        r.as_ref(),
        abi.ret == RetAbi::Sret,
        None,
    );
    fn_ty(&l.params.concat(), l.ret.as_deref(), abi.variadic)
}

/// The type of a nabi function `params -> ret` (`tls`: the hidden
/// `nabi_tls` last), as `FnLower::nabi_shapes` places it.
pub fn nabi_type(
    t: &nabic_mir::Target,
    params: &[nabic_mir::MirTy],
    ret: &nabic_mir::MirTy,
    tls: bool,
) -> Ty {
    let pw = t.pointer_width;
    let ptr = || crate::cc::coerced(&nabic_codegen::abi::CType::PTR, pw);
    let lead = nabic_codegen::abi::nabi_by_ref(ret, t);
    let mut shapes = Vec::new();
    if lead {
        shapes.push(ptr());
    }
    shapes.extend(params.iter().map(|p| {
        if nabic_codegen::abi::nabi_by_ref(p, t) {
            ptr()
        } else {
            crate::cc::shape(p, t)
        }
    }));
    if tls {
        shapes.push(ptr());
    }
    let r = (*ret != nabic_mir::MirTy::Unit && !lead).then(|| crate::cc::shape(ret, t));
    let l = crate::cc::assign(crate::cc::Conv::Wasm32, &shapes, r.as_ref(), false, None);
    fn_ty(&l.params.concat(), l.ret.as_deref(), false)
}

impl Em<'_> {
    pub(crate) fn ret(&mut self, vals: &[(Part, V)]) {
        self.used = [0; 4];
        if !self.sret {
            for (n, (p, v)) in vals.iter().enumerate().take(self.results.len()) {
                let _ = n;
                self.val_ext(*v, p.k, p.signed);
            }
        }
        if self.frame > 0 {
            self.get(self.fp);
            self.i32c(self.frame as i32);
            self.a.num(Num::I32Add);
            self.a.global_set(self.sp);
        }
        self.a.return_();
    }
}
