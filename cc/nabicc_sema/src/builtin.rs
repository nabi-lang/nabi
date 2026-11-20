//! Calls of compiler builtins.

use crate::check::{Cx, R};
use crate::{Arith, Builtin, Function, TExpr, TK};
use nabicc_lex::Loc;
use nabicc_parse::tree::{BinOp, Expr};
use nabicc_parse::ty::{CallConv, Func, QualType, Type, TypeId};
use nabicc_target::Prim;

fn bx(e: TExpr) -> Box<TExpr> {
    Box::new(e)
}

/// C library math functions (also with an `f` / `l` suffix) a
/// `__builtin_` name may call.
const MATH: &[&str] = &[
    "fabs",
    "sqrt",
    "floor",
    "ceil",
    "fmod",
    "pow",
    "exp",
    "exp2",
    "expm1",
    "log",
    "log2",
    "log10",
    "log1p",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "atan2",
    "sinh",
    "cosh",
    "tanh",
    "copysign",
    "trunc",
    "round",
    "lround",
    "llround",
    "rint",
    "lrint",
    "nearbyint",
    "fmin",
    "fmax",
    "fma",
    "ldexp",
    "frexp",
    "modf",
    "cbrt",
    "hypot",
    "scalbn",
    "logb",
    "ilogb",
    "nextafter",
    "remainder",
    "erf",
    "erfc",
    "tgamma",
    "lgamma",
    "fdim",
];

/// The builtin an MSVC intrinsic is.
pub(crate) fn msvc_intrinsic(name: &str) -> Option<&'static str> {
    Some(match name {
        "__lzcnt" => "__builtin_clz",
        "__lzcnt64" => "__builtin_clzll",
        "__popcnt" => "__builtin_popcount",
        "__popcnt64" => "__builtin_popcountll",
        "_byteswap_ushort" => "__builtin_bswap16",
        "_byteswap_ulong" => "__builtin_bswap32",
        "_byteswap_uint64" => "__builtin_bswap64",
        "_ReadWriteBarrier" | "_mm_mfence" | "__faststorefence" => "__sync_synchronize",
        _ => return None,
    })
}

/// Builtins that are the C library's function of the name without the
/// prefix.
const LIBC: &[&str] = &[
    "memcpy",
    "memmove",
    "memset",
    "memcmp",
    "memchr",
    "strlen",
    "strcmp",
    "strncmp",
    "strcpy",
    "strncpy",
    "strcat",
    "strchr",
    "strrchr",
    "strstr",
    "abort",
    "exit",
    "malloc",
    "calloc",
    "realloc",
    "free",
    "abs",
    "labs",
    "llabs",
    "fabs",
    "fabsf",
    "sqrt",
    "sqrtf",
    "floor",
    "floorf",
    "ceil",
    "ceilf",
    "fmod",
    "pow",
    "exp",
    "log",
    "sin",
    "cos",
    "printf",
    "sprintf",
    "snprintf",
    "puts",
    "putchar",
    "copysign",
    "copysignf",
    "trunc",
    "round",
    "fmin",
    "fmax",
    "ldexp",
    "frexp",
    "log2",
    "log10",
    "strspn",
    "strcspn",
    "strpbrk",
    "strdup",
    "stpcpy",
    "vsnprintf",
    "vsprintf",
    "strncat",
    "stpncpy",
    "strlcpy",
    "strlcat",
];

impl Cx<'_> {
    /// A call of builtin `name`.
    pub(crate) fn builtin(&mut self, name: &str, args: &[Expr], loc: Loc) -> R<TExpr> {
        let n = args.len();
        let need = |cx: &Self, k: usize| -> R<()> {
            if n == k {
                Ok(())
            } else {
                Err(cx.err(loc, &format!("`{name}` takes {k} arguments, not {n}")))
            }
        };
        let int = self.int_ty();
        let void = self.prim_ty(Prim::Void);
        let long = self.prim_ty(Prim::Long);
        let name = msvc_intrinsic(name).unwrap_or(name);
        let base = name.strip_prefix("__builtin_").unwrap_or(name).to_owned();
        Ok(match base.as_str() {
            "va_start" => {
                if n < 1 {
                    need(self, 2)?;
                }
                if !self.variadic {
                    return Err(self.err(
                        loc,
                        "`va_start` in a function without `...`: declare it variadic, \
                         `int f(int n, ...)`",
                    ));
                }
                let a = self.va_addr(&args[0])?;
                TExpr::new(TK::VaStart(bx(a)), void, loc)
            }
            "va_end" => {
                need(self, 1)?;
                let a = self.va_addr(&args[0])?;
                TExpr::new(TK::VaEnd(bx(a)), void, loc)
            }
            "va_copy" => {
                need(self, 2)?;
                let d = self.va_addr(&args[0])?;
                let s = self.va_addr(&args[1])?;
                TExpr::new(TK::VaCopy(bx(d), bx(s)), void, loc)
            }
            "expect" | "expect_with_probability" => {
                let v = self.value(&args[0])?;
                for a in &args[1..] {
                    self.value(a)?;
                }
                self.conv(v, long)
            }
            "assume_aligned" => self.value(&args[0])?,
            "assume" | "__assume" => {
                for a in args {
                    self.value(a)?;
                }
                TExpr::new(TK::Int(0), void, loc)
            }
            "unreachable" => TExpr::new(TK::Builtin(Builtin::Unreachable, Vec::new()), void, loc),
            "trap" | "debugtrap" | "__debugbreak" => {
                TExpr::new(TK::Builtin(Builtin::Trap, Vec::new()), void, loc)
            }
            "prefetch" => {
                for a in args {
                    self.value(a)?;
                }
                TExpr::new(TK::Int(0), void, loc)
            }
            "constant_p" => {
                need(self, 1)?;
                let v = self.value(&args[0])?;
                let c = matches!(v.kind, TK::Int(_) | TK::Float(_));
                TExpr::new(TK::Int(i128::from(c)), int, loc)
            }
            "object_size" | "dynamic_object_size" => {
                need(self, 2)?;
                let t = self.value(&args[1])?;
                let st = self.prim_ty(self.types.t.size_t);
                let unknown = match t.kind {
                    TK::Int(k) if k & 2 != 0 => 0,
                    _ => -1,
                };
                let v = self.wrap(self.types.t.size_t, unknown);
                TExpr::new(TK::Int(v), st, loc)
            }
            "clz" | "clzl" | "clzll" | "ctz" | "ctzl" | "ctzll" | "popcount" | "popcountl"
            | "popcountll" => {
                need(self, 1)?;
                let p = match base.trim_start_matches(['c', 'l', 'z', 't', 'p', 'o', 'u', 'n']) {
                    "ll" => Prim::ULongLong,
                    "l" => Prim::ULong,
                    _ => Prim::UInt,
                };
                let p = if base.ends_with("ll") {
                    Prim::ULongLong
                } else if base.ends_with('l') {
                    Prim::ULong
                } else {
                    p.min_rank(Prim::UInt)
                };
                let v = self.value(&args[0])?;
                let t = self.prim_ty(p);
                let v = self.conv(v, t);
                let b = if base.starts_with("clz") {
                    Builtin::Clz
                } else if base.starts_with("ctz") {
                    Builtin::Ctz
                } else {
                    Builtin::Popcount
                };
                TExpr::new(TK::Builtin(b, vec![v]), int, loc)
            }
            "bswap16" | "bswap32" | "bswap64" => {
                need(self, 1)?;
                let p = match base.as_str() {
                    "bswap16" => Prim::UShort,
                    "bswap32" => Prim::UInt,
                    _ => Prim::ULongLong,
                };
                let t = self.prim_ty(p);
                let v = self.value(&args[0])?;
                let v = self.conv(v, t);
                TExpr::new(TK::Builtin(Builtin::Bswap, vec![v]), t, loc)
            }
            "huge_val" | "inf" | "huge_valf" | "inff" | "huge_vall" | "infl" => {
                let p = self.float_suffix(&base);
                TExpr::new(TK::Float(f64::INFINITY), self.prim_ty(p), loc)
            }
            "nan" | "nanf" | "nanl" | "nans" | "nansf" | "nansl" => {
                for a in args {
                    self.value(a)?;
                }
                let p = self.float_suffix(&base);
                TExpr::new(TK::Float(f64::NAN), self.prim_ty(p), loc)
            }
            "isnan" | "isinf" | "isfinite" | "signbit" | "signbitf" | "signbitl" | "isnormal"
            | "isinf_sign" => {
                need(self, 1)?;
                let v = self.value(&args[0])?;
                if !self.is_float(v.ty) {
                    return Err(self.err(loc, &format!("`{name}` needs a floating value")));
                }
                self.float_class(&base, v, loc)
            }
            "fpclassify" => {
                need(self, 6)?;
                let mut ks = Vec::new();
                for a in &args[..5] {
                    let v = self.value(a)?;
                    ks.push(self.conv(v, int));
                }
                let x = self.value(&args[5])?;
                self.fpclassify(ks, x, loc)
            }
            "isgreater" | "isgreaterequal" | "isless" | "islessequal" | "islessgreater"
            | "isunordered" => {
                need(self, 2)?;
                let a = self.value(&args[0])?;
                let b = self.value(&args[1])?;
                self.float_compare(&base, a, b, loc)?
            }
            "add_overflow" | "sub_overflow" | "mul_overflow" | "sadd_overflow"
            | "saddl_overflow" | "saddll_overflow" | "uadd_overflow" | "uaddl_overflow"
            | "uaddll_overflow" | "ssub_overflow" | "usub_overflow" | "smul_overflow"
            | "smull_overflow" | "smulll_overflow" | "umul_overflow" | "umull_overflow"
            | "umulll_overflow" => {
                need(self, 3)?;
                let r = self.value(&args[2])?;
                let Some(q) = self.pointee(r.ty) else {
                    return Err(self.err(loc, "the result of an overflow builtin is a pointer"));
                };
                let rt = self.unqual(q.ty);
                let a = self.value(&args[0])?;
                let b = self.value(&args[1])?;
                let a = self.conv(a, rt);
                let b = self.conv(b, rt);
                let bt = if base.contains("add") {
                    Builtin::AddOverflow
                } else if base.contains("sub") {
                    Builtin::SubOverflow
                } else {
                    Builtin::MulOverflow
                };
                let bool_ = self.prim_ty(Prim::Bool);
                TExpr::new(TK::Builtin(bt, vec![a, b, r]), bool_, loc)
            }
            "alloca" | "_alloca" | "alloca_with_align" => {
                return Err(self.err(
                    loc,
                    "`alloca` is not supported by `nabi cc`: use `malloc` and `free`, or a \
                     fixed-size array",
                ));
            }
            "frame_address" | "return_address" => {
                return Err(self.err(loc, &format!("`{name}` is not supported by `nabi cc`")));
            }
            "classify_type" => TExpr::new(TK::Int(5), int, loc),
            "types_compatible_p" => TExpr::new(TK::Int(0), int, loc),
            _ if name.starts_with("__sync_")
                || name.starts_with("__atomic_")
                || name.starts_with("_Interlocked")
                || name.starts_with("__c11_atomic")
                || name == "_ReadWriteBarrier" =>
            {
                self.atomic(name, args, loc)?
            }
            b if LIBC.contains(&b) => self.libc_call(b, args, loc)?,
            b if MATH.contains(&b)
                || (b.ends_with(['f', 'l']) && MATH.contains(&&b[..b.len() - 1])) =>
            {
                let name: &'static str = Box::leak(b.to_owned().into_boxed_str());
                self.libc_call(name, args, loc)?
            }
            // Fortified calls: the plain function (object sizes are
            // unknown here, so the checks would pass).
            b if b.starts_with("__") && b.ends_with("_chk") => {
                let plain = &b[2..b.len() - 4];
                let keep: &[usize] = match plain {
                    "memcpy" | "memmove" | "memset" | "strncpy" | "strncat" | "stpncpy"
                    | "strlcpy" | "strlcat" => &[0, 1, 2],
                    "strcpy" | "strcat" | "stpcpy" | "snprintf" | "vsnprintf" => &[0, 1],
                    "sprintf" | "vsprintf" => &[0],
                    _ => {
                        return Err(self.err(
                            loc,
                            &format!("builtin `{name}` is not supported by `nabi cc`"),
                        ));
                    }
                };
                // snprintf(s, n, flag, size, fmt, ...): the format on.
                let skip = match plain {
                    "snprintf" | "vsnprintf" => 4,
                    "sprintf" | "vsprintf" => 3,
                    _ => args.len(),
                };
                let mut a: Vec<Expr> = keep.iter().filter_map(|i| args.get(*i).cloned()).collect();
                a.extend(args.iter().skip(skip).cloned());
                let Some(sname) = LIBC.iter().find(|x| **x == plain) else {
                    return Err(self.err(
                        loc,
                        &format!("builtin `{name}` is not supported by `nabi cc`"),
                    ));
                };
                self.libc_call(sname, &a, loc)?
            }
            _ => {
                return Err(self.err(
                    loc,
                    &format!(
                        "builtin `{name}` is not supported by `nabi cc`; write the operation in \
                         C, or call the C library's function"
                    ),
                ));
            }
        })
    }

    fn float_suffix(&self, base: &str) -> Prim {
        if base.ends_with('f') {
            Prim::Float
        } else if base.ends_with('l') && !self.ld_is_double() {
            Prim::LongDouble
        } else {
            Prim::Double
        }
    }

    /// `isnan` and kin as comparisons (`signbit`: `x < 0 || 1 / x < 0`,
    /// which misses a NaN's sign).
    fn float_class(&self, base: &str, v: TExpr, loc: Loc) -> TExpr {
        let int = self.int_ty();
        let ty = v.ty;
        let fl = |x: f64| TExpr::new(TK::Float(x), ty, loc);
        let cmp =
            |c: crate::Cmp, a: TExpr, b: TExpr| TExpr::new(TK::Cmp(c, bx(a), bx(b)), int, loc);
        let abs = TExpr::new(
            TK::Cond(
                bx(cmp(crate::Cmp::Lt, v.clone(), fl(0.0))),
                bx(TExpr::new(TK::Neg(bx(v.clone())), ty, loc)),
                bx(v.clone()),
            ),
            ty,
            loc,
        );
        match base {
            "isnan" => cmp(crate::Cmp::Ne, v.clone(), v),
            "isinf" => cmp(crate::Cmp::Eq, abs, fl(f64::INFINITY)),
            "isfinite" => cmp(crate::Cmp::Lt, abs, fl(f64::INFINITY)),
            "isnormal" => {
                let min = if self.prim_of(ty) == Some(Prim::Float) {
                    f64::from(f32::MIN_POSITIVE)
                } else {
                    f64::MIN_POSITIVE
                };
                let lo = cmp(crate::Cmp::Ge, abs.clone(), fl(min));
                let hi = cmp(crate::Cmp::Lt, abs, fl(f64::INFINITY));
                TExpr::new(TK::And(bx(lo), bx(hi)), int, loc)
            }
            "isinf_sign" => {
                let inf = cmp(crate::Cmp::Eq, abs, fl(f64::INFINITY));
                let neg = cmp(crate::Cmp::Lt, v, fl(0.0));
                let sign = TExpr::new(
                    TK::Cond(
                        bx(neg),
                        bx(TExpr::new(TK::Int(-1), int, loc)),
                        bx(TExpr::new(TK::Int(1), int, loc)),
                    ),
                    int,
                    loc,
                );
                TExpr::new(
                    TK::Cond(bx(inf), bx(sign), bx(TExpr::new(TK::Int(0), int, loc))),
                    int,
                    loc,
                )
            }
            _ => {
                let neg = cmp(crate::Cmp::Lt, v.clone(), fl(0.0));
                let zero = cmp(crate::Cmp::Eq, v.clone(), fl(0.0));
                let inv = TExpr::new(TK::Bin(Arith::Div, bx(fl(1.0)), bx(v)), ty, loc);
                let negz = cmp(crate::Cmp::Lt, inv, fl(0.0));
                let z = TExpr::new(TK::And(bx(zero), bx(negz)), int, loc);
                TExpr::new(TK::Or(bx(neg), bx(z)), int, loc)
            }
        }
    }

    fn fpclassify(&self, ks: Vec<TExpr>, x: TExpr, loc: Loc) -> TExpr {
        let int = self.int_ty();
        let [nan, inf, normal, subnormal, zero]: [TExpr; 5] = ks.try_into().expect("five kinds");
        let isnan = self.float_class("isnan", x.clone(), loc);
        let isinf = self.float_class("isinf", x.clone(), loc);
        let isnorm = self.float_class("isnormal", x.clone(), loc);
        let ty = x.ty;
        let iszero = TExpr::new(
            TK::Cmp(
                crate::Cmp::Eq,
                bx(x),
                bx(TExpr::new(TK::Float(0.0), ty, loc)),
            ),
            int,
            loc,
        );
        let c = |a: TExpr, b: TExpr, e: TExpr| TExpr::new(TK::Cond(bx(a), bx(b), bx(e)), int, loc);
        c(
            isnan,
            nan,
            c(isinf, inf, c(isnorm, normal, c(iszero, zero, subnormal))),
        )
    }

    fn float_compare(&mut self, base: &str, a: TExpr, b: TExpr, loc: Loc) -> R<TExpr> {
        let unordered = |cx: &mut Self, a: TExpr, b: TExpr| -> R<TExpr> {
            let na = cx.arith(BinOp::Ne, a.clone(), a, loc)?;
            let nb = cx.arith(BinOp::Ne, b.clone(), b, loc)?;
            Ok(TExpr::new(TK::Or(bx(na), bx(nb)), cx.int_ty(), loc))
        };
        Ok(match base {
            "isgreater" => self.arith(BinOp::Gt, a, b, loc)?,
            "isgreaterequal" => self.arith(BinOp::Ge, a, b, loc)?,
            "isless" => self.arith(BinOp::Lt, a, b, loc)?,
            "islessequal" => self.arith(BinOp::Le, a, b, loc)?,
            "islessgreater" => {
                let lt = self.arith(BinOp::Lt, a.clone(), b.clone(), loc)?;
                let gt = self.arith(BinOp::Gt, a, b, loc)?;
                TExpr::new(TK::Or(bx(lt), bx(gt)), self.int_ty(), loc)
            }
            _ => unordered(self, a, b)?,
        })
    }

    /// `__builtin_memcpy` and kin: the C library's function, declared
    /// on the spot when no header did.
    fn libc_call(&mut self, name: &str, args: &[Expr], loc: Loc) -> R<TExpr> {
        let f = if let Some(&f) = self.func_by_name.get(name) {
            f
        } else {
            // Unprototyped, returning what the function does.
            let ret = match name {
                "memcpy" | "memmove" | "memset" | "memchr" | "strcpy" | "strncpy" | "strcat"
                | "strchr" | "strrchr" | "strstr" | "malloc" | "calloc" | "realloc" | "strdup"
                | "stpcpy" | "strpbrk" => {
                    let v = self.prim_ty(Prim::Void);
                    self.ptr_to(v)
                }
                "strlen" | "strspn" | "strcspn" => self.prim_ty(self.types.t.size_t),
                "abort" | "exit" | "free" => self.prim_ty(Prim::Void),
                "labs" => self.prim_ty(Prim::Long),
                "llabs" => self.prim_ty(Prim::LongLong),
                "fabsf" | "sqrtf" | "floorf" | "ceilf" | "copysignf" => self.prim_ty(Prim::Float),
                "fabs" | "sqrt" | "floor" | "ceil" | "fmod" | "pow" | "exp" | "log" | "sin"
                | "cos" | "copysign" | "trunc" | "round" | "fmin" | "fmax" | "ldexp" | "frexp"
                | "log2" | "log10" => self.prim_ty(Prim::Double),
                m if m.ends_with('f') && MATH.contains(&&m[..m.len() - 1]) => {
                    self.prim_ty(Prim::Float)
                }
                m if m.ends_with('l') && MATH.contains(&&m[..m.len() - 1]) => {
                    let p = if self.ld_is_double() {
                        Prim::Double
                    } else {
                        Prim::LongDouble
                    };
                    self.prim_ty(p)
                }
                m if MATH.contains(&m) => self.prim_ty(Prim::Double),
                _ => self.int_ty(),
            };
            let ty = self.types.add(Type::Func(Func {
                ret: QualType::new(ret),
                params: Vec::new(),
                variadic: false,
                proto: false,
                cc: CallConv::C,
            }));
            self.funcs.push(Function {
                name: name.to_owned(),
                symbol: name.to_owned(),
                ty,
                internal: false,
                inline: false,
                body: None,
                broken: None,
                loc,
            });
            let f = self.funcs.len() - 1;
            self.func_by_name.insert(name.to_owned(), f);
            f
        };
        let fty = self.funcs[f].ty;
        let callee = TExpr::new(TK::Func(f), fty, loc);
        let Some(ft) = self.func_of(fty) else {
            return Err(self.err(loc, "not a function"));
        };
        let mut out = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let v = self.value(a)?;
            out.push(match ft.params.get(i).filter(|_| ft.proto) {
                Some(p) => {
                    let pt = self.unqual(p.ty);
                    self.conv(v, pt)
                }
                None => self.default_promote(v),
            });
        }
        let ret = self.unqual(ft.ret.ty);
        Ok(TExpr::new(TK::Call(bx(callee), out, fty), ret, loc))
    }

    /// `__atomic_*`, `__sync_*` and MSVC's `_Interlocked*`.
    fn atomic(&mut self, name: &str, args: &[Expr], loc: Loc) -> R<TExpr> {
        let void = self.prim_ty(Prim::Void);
        let bool_ = self.prim_ty(Prim::Bool);
        let mut vals = Vec::new();
        for a in args {
            vals.push(self.value(a)?);
        }
        let ptr_elem = |cx: &mut Self, v: &TExpr| -> R<TypeId> {
            match cx.pointee(v.ty) {
                Some(q) => Ok(cx.unqual(q.ty)),
                None => Err(cx.err(loc, &format!("`{name}` needs a pointer first"))),
            }
        };
        let fence = |_: &mut Self| TExpr::new(TK::Builtin(Builtin::Fence, Vec::new()), void, loc);
        let op = |s: &str| -> Option<Arith> {
            Some(match s {
                "add" => Arith::Add,
                "sub" => Arith::Sub,
                "and" => Arith::And,
                "or" => Arith::Or,
                "xor" => Arith::Xor,
                _ => return None,
            })
        };
        if matches!(
            name,
            "__sync_synchronize"
                | "__atomic_thread_fence"
                | "__atomic_signal_fence"
                | "__c11_atomic_thread_fence"
                | "_ReadWriteBarrier"
        ) {
            return Ok(fence(self));
        }
        if matches!(
            name,
            "__atomic_is_lock_free" | "__atomic_always_lock_free" | "__c11_atomic_is_lock_free"
        ) {
            let n = vals.first().and_then(|v| self.const_int(v)).unwrap_or(0);
            let lock_free = n > 0 && n as u64 <= self.types.t.ptr * 2 && (n & (n - 1)) == 0;
            let int = self.int_ty();
            return Ok(TExpr::new(TK::Int(i128::from(lock_free)), int, loc));
        }
        if vals.is_empty() {
            return Err(self.err(loc, &format!("`{name}` needs arguments")));
        }
        // `_InterlockedOr16_acq` is `_InterlockedOr` on a 16-bit object.
        let ms;
        let name = if let Some(rest) = name.strip_prefix("_Interlocked") {
            let rest = rest
                .trim_end_matches("_acq")
                .trim_end_matches("_rel")
                .trim_end_matches("_nf")
                .trim_end_matches("_np");
            let rest = rest
                .trim_end_matches("64")
                .trim_end_matches("16")
                .trim_end_matches('8');
            ms = format!("_Interlocked{rest}");
            ms.as_str()
        } else {
            name
        };
        let mut et = ptr_elem(self, &vals[0])?;
        if self.is_void(et) {
            et = self.prim_ty(Prim::UChar);
        }
        let mut args = vals;
        let n = args.len();
        let conv_arg = |cx: &mut Self, args: &mut Vec<TExpr>, i: usize| {
            if let Some(a) = args.get(i).cloned() {
                args[i] = cx.conv(a, et);
            }
        };
        let b = |k: Builtin, a: Vec<TExpr>, ty: TypeId| TExpr::new(TK::Builtin(k, a), ty, loc);
        let r = match name {
            "__atomic_load_n" | "__c11_atomic_load" => {
                b(Builtin::AtomicLoad, vec![args.remove(0)], et)
            }
            "__atomic_store_n" | "__c11_atomic_store" => {
                conv_arg(self, &mut args, 1);
                args.truncate(2);
                b(Builtin::AtomicStore, args, void)
            }
            "__atomic_load" => {
                // (ptr, ret*, order)
                let p = args.remove(0);
                let r = args.remove(0);
                let v = b(Builtin::AtomicLoad, vec![p], et);
                let place = TExpr::new(TK::Deref(bx(r)), et, loc);
                TExpr::new(TK::Assign(bx(place), bx(v)), et, loc)
            }
            "__atomic_store" => {
                let p = args.remove(0);
                let v = args.remove(0);
                let v = TExpr::new(TK::Deref(bx(v)), et, loc);
                b(Builtin::AtomicStore, vec![p, v], void)
            }
            "__atomic_exchange_n"
            | "__sync_lock_test_and_set"
            | "__c11_atomic_exchange"
            | "_InterlockedExchange" => {
                conv_arg(self, &mut args, 1);
                args.truncate(2);
                b(Builtin::AtomicExchange, args, et)
            }
            "__atomic_test_and_set" => {
                let p = args.remove(0);
                let one = self.conv(TExpr::new(TK::Int(1), self.int_ty(), loc), et);
                let old = b(Builtin::AtomicExchange, vec![p, one], et);
                self.conv(old, bool_)
            }
            "__atomic_clear" | "__sync_lock_release" => {
                let p = args.remove(0);
                let z = self.conv(TExpr::new(TK::Int(0), self.int_ty(), loc), et);
                b(Builtin::AtomicStore, vec![p, z], void)
            }
            "__atomic_compare_exchange_n"
            | "__c11_atomic_compare_exchange_strong"
            | "__c11_atomic_compare_exchange_weak" => {
                conv_arg(self, &mut args, 2);
                args.truncate(3);
                b(Builtin::AtomicCas, args, bool_)
            }
            "__atomic_compare_exchange" => {
                let des = args.remove(2);
                let des = TExpr::new(TK::Deref(bx(des)), et, loc);
                args.truncate(2);
                args.push(des);
                b(Builtin::AtomicCas, args, bool_)
            }
            "__sync_bool_compare_and_swap" | "__sync_val_compare_and_swap" => {
                conv_arg(self, &mut args, 1);
                conv_arg(self, &mut args, 2);
                args.truncate(3);
                let v = b(Builtin::AtomicCasVal, args.clone(), et);
                if name == "__sync_val_compare_and_swap" {
                    v
                } else {
                    let old = args[1].clone();
                    let int = self.int_ty();
                    TExpr::new(TK::Cmp(crate::Cmp::Eq, bx(v), bx(old)), int, loc)
                }
            }
            "_InterlockedCompareExchange" => {
                // (dest, exchange, comparand): the old value.
                conv_arg(self, &mut args, 1);
                conv_arg(self, &mut args, 2);
                let (p, x, c) = (args[0].clone(), args[1].clone(), args[2].clone());
                b(Builtin::AtomicCasVal, vec![p, c, x], et)
            }
            "_InterlockedIncrement" | "_InterlockedDecrement" => {
                let one = self.conv(TExpr::new(TK::Int(1), self.int_ty(), loc), et);
                let a = if name.ends_with("Increment") {
                    Arith::Add
                } else {
                    Arith::Sub
                };
                b(Builtin::AtomicOpFetch(a), vec![args.remove(0), one], et)
            }
            "_InterlockedAnd" | "_InterlockedOr" | "_InterlockedXor" => {
                conv_arg(self, &mut args, 1);
                args.truncate(2);
                let o = match name {
                    "_InterlockedAnd" => Arith::And,
                    "_InterlockedOr" => Arith::Or,
                    _ => Arith::Xor,
                };
                b(Builtin::AtomicFetch(o), args, et)
            }
            "_InterlockedExchangeAdd" => {
                conv_arg(self, &mut args, 1);
                args.truncate(2);
                b(Builtin::AtomicFetch(Arith::Add), args, et)
            }
            _ => {
                let rest = name
                    .strip_prefix("__atomic_")
                    .or_else(|| name.strip_prefix("__sync_"))
                    .or_else(|| name.strip_prefix("__c11_atomic_"))
                    .unwrap_or(name);
                if n < 2 {
                    return Err(self.err(loc, &format!("`{name}` takes two arguments or more")));
                }
                conv_arg(self, &mut args, 1);
                args.truncate(2);
                if let Some(o) = rest
                    .strip_prefix("fetch_and_")
                    .or_else(|| rest.strip_prefix("fetch_"))
                    .and_then(op)
                {
                    b(Builtin::AtomicFetch(o), args, et)
                } else if let Some(o) = rest
                    .strip_suffix("_and_fetch")
                    .or_else(|| rest.strip_suffix("_fetch"))
                    .and_then(op)
                {
                    b(Builtin::AtomicOpFetch(o), args, et)
                } else if rest == "fetch_nand" || rest == "fetch_and_nand" {
                    b(Builtin::AtomicFetchNand, args, et)
                } else {
                    return Err(self.err(
                        loc,
                        &format!("atomic builtin `{name}` is not supported by `nabi cc`"),
                    ));
                }
            }
        };
        Ok(r)
    }
}

trait MinRank {
    fn min_rank(self, other: Prim) -> Prim;
}

impl MinRank for Prim {
    fn min_rank(self, other: Prim) -> Prim {
        if self.rank() >= other.rank() {
            self
        } else {
            other
        }
    }
}
