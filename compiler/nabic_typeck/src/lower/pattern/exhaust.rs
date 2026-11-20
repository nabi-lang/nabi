//! Exhaustiveness and reachability of match arms (pattern-matrix usefulness).

#[allow(clippy::wildcard_imports)]
use crate::lower::pattern::*;

/// Enum variants / sum variants fully covered by `p`.
pub(crate) fn covered_variants(p: &HirPattern, out: &mut Vec<u32>) {
    match p {
        HirPattern::EnumVariant {
            variant_index,
            args,
            ..
        } if args.iter().all(hir_pattern_irrefutable) => {
            out.push(*variant_index);
        }
        HirPattern::SumVariant { variant_index, .. } => out.push(*variant_index),
        HirPattern::At { sub, .. } => covered_variants(sub, out),
        HirPattern::Or(alts) => {
            for a in alts {
                covered_variants(a, out);
            }
        }
        _ => {}
    }
}

/// Pattern-matrix constructor (usefulness analysis, Maranget 2007).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Ctor {
    Bool(bool),
    Variant(u32),
    /// The only constructor of a tuple / struct.
    Single,
    /// An integer / `char` literal or range as `lo..=hi`. Ranges never make a
    /// match exhaustive but subsume each other for unreachable arms.
    Int(i128, i128),
    /// A literal of another open domain (strings, floats): only equal
    /// spellings are the same constructor.
    Open(String),
    /// A sequence of this many elements; `true`: of this many or more.
    Len(u32, bool),
}

thread_local! {
    /// The lengths a `List` / `Slice` column splits into in the match
    /// at hand: `0 .. n` exactly, and `n` or more.
    static SEQ_SPLIT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The split of `arms`: past every fixed length, and no shorter than a
/// `..` pattern's items.
fn seq_split(arms: &[HirMatchArm]) -> u32 {
    fn walk(p: &HirPattern, n: &mut u32) {
        match p {
            HirPattern::Slice(s) => {
                *n = (*n).max(if s.rest.is_some() {
                    s.min_len()
                } else {
                    s.min_len() + 1
                });
                s.items().for_each(|(_, p)| walk(p, n));
            }
            HirPattern::At { sub, .. } | HirPattern::SumMember { sub, .. } => walk(sub, n),
            HirPattern::Or(ps) | HirPattern::Tuple(ps) => ps.iter().for_each(|p| walk(p, n)),
            HirPattern::EnumVariant { args, .. } => args.iter().for_each(|p| walk(p, n)),
            HirPattern::Struct { fields, .. } => fields.iter().for_each(|(_, p)| walk(p, n)),
            _ => {}
        }
    }
    let mut n = 0;
    for a in arms {
        walk(&a.pat, &mut n);
    }
    n
}

/// The element type of a `List` / `Slice` / `SliceMut` or array.
fn seq_elem(ctx: &LowerCtx<'_>, ty: &HirTy) -> Option<(HirTy, Option<u32>)> {
    let fixed = crate::lower::pattern::seq_len(ctx, ty)?.fixed();
    match ty {
        HirTy::Array(t, _) => Some(((**t).clone(), fixed)),
        HirTy::Struct(id) => Some((ctx.struct_def(*id).generic_args.first()?.clone(), fixed)),
        _ => None,
    }
}

/// A literal's point in the `Ctor::Int` domain. `u128` above `i128::MAX`
/// saturates, so such patterns overlap.
pub(crate) fn int_point(v: &ConstVal) -> Option<i128> {
    match v {
        ConstVal::Int { value, .. } => Some(*value),
        ConstVal::UInt { value, .. } => Some(i128::try_from(*value).unwrap_or(i128::MAX)),
        ConstVal::Char(c) => Some(i128::from(*c)),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Pat {
    Wild,
    C(Ctor, Vec<Pat>),
    Or(Vec<Pat>),
}

pub(crate) fn simplify(ctx: &LowerCtx<'_>, p: &HirPattern) -> Pat {
    match p {
        HirPattern::Wildcard | HirPattern::Binding { .. } => Pat::Wild,
        HirPattern::At { sub, .. } => simplify(ctx, sub),
        HirPattern::Literal {
            value: ConstVal::Bool(b),
            ..
        } => Pat::C(Ctor::Bool(*b), Vec::new()),
        HirPattern::Literal { value, .. } => match int_point(value) {
            Some(v) => Pat::C(Ctor::Int(v, v), Vec::new()),
            None => Pat::C(Ctor::Open(format!("{value:?}")), Vec::new()),
        },
        HirPattern::Range {
            start,
            end,
            inclusive,
            ..
        } => {
            let lo = start.as_ref().map(int_point);
            let hi = end.as_ref().map(int_point);
            let c = match (lo, hi) {
                (Some(None), _) | (_, Some(None)) => {
                    Ctor::Open(format!("{start:?}..{inclusive}{end:?}"))
                }
                (lo, hi) => {
                    let lo = lo.flatten().unwrap_or(i128::MIN);
                    let hi = match hi.flatten() {
                        Some(h) if !*inclusive => h.saturating_sub(1),
                        Some(h) => h,
                        None => i128::MAX,
                    };
                    Ctor::Int(lo, hi)
                }
            };
            Pat::C(c, Vec::new())
        }
        HirPattern::Or(alts) => Pat::Or(alts.iter().map(|a| simplify(ctx, a)).collect()),
        HirPattern::Tuple(parts) => Pat::C(
            Ctor::Single,
            parts.iter().map(|a| simplify(ctx, a)).collect(),
        ),
        HirPattern::Struct { struct_id, fields } => {
            let n = ctx.struct_def(*struct_id).fields.len();
            let mut args = vec![Pat::Wild; n];
            for (i, f) in fields {
                if let Some(slot) = args.get_mut(*i as usize) {
                    *slot = simplify(ctx, f);
                }
            }
            Pat::C(Ctor::Single, args)
        }
        HirPattern::EnumVariant {
            enum_id,
            variant_index,
            args,
        } => {
            let arity = ctx
                .enum_def(*enum_id)
                .variants
                .get(*variant_index as usize)
                .map_or(args.len(), |v| v.arity);
            let mut sub: Vec<Pat> = args.iter().map(|a| simplify(ctx, a)).collect();
            sub.resize(arity, Pat::Wild);
            Pat::C(Ctor::Variant(*variant_index), sub)
        }
        HirPattern::SumVariant { variant_index, .. } => {
            Pat::C(Ctor::Variant(*variant_index), vec![Pat::Wild])
        }
        HirPattern::SumMember {
            variant_index, sub, ..
        } => Pat::C(Ctor::Variant(*variant_index), vec![simplify(ctx, sub)]),
        HirPattern::Slice(s) => {
            let head: Vec<Pat> = s.head.iter().map(|(_, p)| simplify(ctx, p)).collect();
            let tail: Vec<Pat> = s.tail.iter().map(|(_, p)| simplify(ctx, p)).collect();
            let at = |n: u32, more: bool| {
                let mid = n as usize - head.len() - tail.len();
                let args = head
                    .iter()
                    .cloned()
                    .chain(std::iter::repeat_n(Pat::Wild, mid))
                    .chain(tail.iter().cloned())
                    .collect();
                Pat::C(Ctor::Len(n, more), args)
            };
            let min = s.min_len();
            match (s.fixed, s.rest.is_some()) {
                (Some(n), _) => at(n, false),
                (None, false) => at(min, false),
                (None, true) => {
                    let split = SEQ_SPLIT.get().max(min);
                    let mut alts: Vec<Pat> = (min..split).map(|n| at(n, false)).collect();
                    alts.push(at(split, true));
                    Pat::Or(alts)
                }
            }
        }
    }
}

/// Every constructor of `ty` with its field types; `None` = an open
/// domain (only `_` covers it).
pub(crate) fn ctors_of(ctx: &LowerCtx<'_>, ty: &HirTy) -> Option<Vec<(Ctor, Vec<HirTy>)>> {
    if let Some((elem, fixed)) = seq_elem(ctx, ty) {
        let of = |n: u32| vec![elem.clone(); n as usize];
        if let Some(n) = fixed {
            return Some(vec![(Ctor::Len(n, false), of(n))]);
        }
        let split = SEQ_SPLIT.get();
        return Some(
            (0..split)
                .map(|n| (Ctor::Len(n, false), of(n)))
                .chain([(Ctor::Len(split, true), of(split))])
                .collect(),
        );
    }
    Some(match ty {
        HirTy::Prim(PrimTy::Bool) => vec![
            (Ctor::Bool(false), Vec::new()),
            (Ctor::Bool(true), Vec::new()),
        ],
        HirTy::Tuple(ts) => vec![(Ctor::Single, ts.clone())],
        HirTy::Struct(id) => vec![(
            Ctor::Single,
            ctx.struct_def(*id)
                .fields
                .iter()
                .map(|f| f.ty.clone())
                .collect(),
        )],
        HirTy::Enum(id) => ctx
            .enum_def(*id)
            .variants
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let tys = match (&v.payload_ty, v.arity) {
                    (_, 0) | (None, _) => Vec::new(),
                    (Some(t), 1) => vec![t.clone()],
                    (Some(HirTy::Tuple(ts)), _) => ts.clone(),
                    (Some(_), n) => vec![HirTy::Unknown; n],
                };
                (
                    Ctor::Variant(u32::try_from(i).expect("variant overflow")),
                    tys,
                )
            })
            .collect(),
        HirTy::Sum(vs) => vs
            .iter()
            .enumerate()
            .map(|(i, v)| {
                (
                    Ctor::Variant(u32::try_from(i).expect("variant overflow")),
                    vec![v.clone()],
                )
            })
            .collect(),
        _ => return None,
    })
}

pub(crate) fn sub_tys(ctx: &LowerCtx<'_>, ty: &HirTy, c: &Ctor, arity: usize) -> Vec<HirTy> {
    ctors_of(ctx, ty)
        .and_then(|cs| cs.into_iter().find(|(k, _)| k == c).map(|(_, t)| t))
        .filter(|t| t.len() == arity)
        .unwrap_or_else(|| vec![HirTy::Unknown; arity])
}

/// Rows with or-patterns in the first column expanded.
pub(crate) fn expand_rows(rows: &[Vec<Pat>]) -> Vec<Vec<Pat>> {
    let mut out = Vec::new();
    for r in rows {
        match r.first() {
            Some(Pat::Or(alts)) => {
                let alt_rows: Vec<Vec<Pat>> = alts
                    .iter()
                    .map(|a| {
                        std::iter::once(a.clone())
                            .chain(r[1..].iter().cloned())
                            .collect()
                    })
                    .collect();
                out.extend(expand_rows(&alt_rows));
            }
            _ => out.push(r.clone()),
        }
    }
    out
}

pub(crate) fn specialize(rows: &[Vec<Pat>], c: &Ctor, arity: usize) -> Vec<Vec<Pat>> {
    rows.iter()
        .filter_map(|r| {
            let head: Vec<Pat> = match &r[0] {
                Pat::C(k, args) if k == c => args.clone(),
                Pat::Wild => vec![Pat::Wild; arity],
                _ => return None,
            };
            Some(head.into_iter().chain(r[1..].iter().cloned()).collect())
        })
        .collect()
}

/// Is there a value matched by `q` and by no row?
pub(crate) fn useful(ctx: &LowerCtx<'_>, rows: &[Vec<Pat>], q: &[Pat], tys: &[HirTy]) -> bool {
    let Some(head) = q.first() else {
        return rows.is_empty();
    };
    let rows = expand_rows(rows);
    let ty = tys.first().cloned().unwrap_or(HirTy::Unknown);
    let rest_tys = tys.get(1..).unwrap_or(&[]);
    let with = |args: Vec<Pat>, sub: Vec<HirTy>| -> (Vec<Pat>, Vec<HirTy>) {
        (
            args.into_iter().chain(q[1..].iter().cloned()).collect(),
            sub.into_iter().chain(rest_tys.iter().cloned()).collect(),
        )
    };
    match head {
        Pat::Or(alts) => alts.iter().any(|a| {
            let q2: Vec<Pat> = std::iter::once(a.clone())
                .chain(q[1..].iter().cloned())
                .collect();
            useful(ctx, &rows, &q2, tys)
        }),
        Pat::C(Ctor::Int(lo, hi), _) => {
            // Split `lo..=hi` at every row interval's edge; each piece is
            // inside or outside each row, so it specializes exactly.
            let heads: Vec<(i128, i128)> = rows
                .iter()
                .filter_map(|r| match r.first() {
                    Some(&Pat::C(Ctor::Int(a, b), _)) if a <= b => Some((a, b)),
                    _ => None,
                })
                .collect();
            let mut cuts = vec![*lo];
            for &(a, b) in &heads {
                if *lo < a && a <= *hi {
                    cuts.push(a);
                }
                if *lo <= b && b < *hi {
                    cuts.push(b + 1);
                }
            }
            cuts.sort_unstable();
            cuts.dedup();
            (0..cuts.len()).filter(|_| lo <= hi).any(|i| {
                let (p, q_hi) = (cuts[i], cuts.get(i + 1).map_or(*hi, |n| n - 1));
                let sub: Vec<Vec<Pat>> = rows
                    .iter()
                    .filter(|r| match r.first() {
                        Some(Pat::Wild) => true,
                        Some(&Pat::C(Ctor::Int(a, b), _)) => a <= p && q_hi <= b,
                        _ => false,
                    })
                    .map(|r| r[1..].to_vec())
                    .collect();
                useful(ctx, &sub, &q[1..], rest_tys)
            })
        }
        Pat::C(c, args) => {
            let (q2, t2) = with(args.clone(), sub_tys(ctx, &ty, c, args.len()));
            useful(ctx, &specialize(&rows, c, args.len()), &q2, &t2)
        }
        Pat::Wild => {
            let used: Vec<&Ctor> = rows
                .iter()
                .filter_map(|r| match &r[0] {
                    Pat::C(k, _) => Some(k),
                    _ => None,
                })
                .collect();
            match ctors_of(ctx, &ty) {
                Some(all) if !all.is_empty() && all.iter().all(|(c, _)| used.contains(&c)) => {
                    all.into_iter().any(|(c, sub)| {
                        let n = sub.len();
                        let (q2, t2) = with(vec![Pat::Wild; n], sub);
                        useful(ctx, &specialize(&rows, &c, n), &q2, &t2)
                    })
                }
                _ => {
                    let default: Vec<Vec<Pat>> = rows
                        .iter()
                        .filter(|r| matches!(r[0], Pat::Wild))
                        .map(|r| r[1..].to_vec())
                        .collect();
                    useful(ctx, &default, &q[1..], rest_tys)
                }
            }
        }
    }
}

/// One value no row matches, one pattern per column (Maranget's
/// witness), or `None` when the rows are exhaustive.
pub(crate) fn witness(ctx: &LowerCtx<'_>, rows: &[Vec<Pat>], tys: &[HirTy]) -> Option<Vec<String>> {
    let Some(ty) = tys.first() else {
        return rows.is_empty().then(Vec::new);
    };
    let rows = expand_rows(rows);
    let rest = &tys[1..];
    let used: Vec<&Ctor> = rows
        .iter()
        .filter_map(|r| match &r[0] {
            Pat::C(k, _) => Some(k),
            _ => None,
        })
        .collect();
    let all = ctors_of(ctx, ty).filter(|a| !a.is_empty());
    if let Some(all) = all
        .as_ref()
        .filter(|a| a.iter().all(|(c, _)| used.contains(&c)))
    {
        return all.iter().find_map(|(c, sub)| {
            let n = sub.len();
            let tys2: Vec<HirTy> = sub.iter().chain(rest).cloned().collect();
            let mut w = witness(ctx, &specialize(&rows, c, n), &tys2)?;
            let args: Vec<String> = w.drain(..n).collect();
            w.insert(0, ctor_text(ctx, ty, c, &args));
            Some(w)
        });
    }
    let default: Vec<Vec<Pat>> = rows
        .iter()
        .filter(|r| matches!(r[0], Pat::Wild))
        .map(|r| r[1..].to_vec())
        .collect();
    let mut w = witness(ctx, &default, rest)?;
    let head = all
        .and_then(|a| a.into_iter().find(|(c, _)| !used.contains(&c)))
        .map_or_else(
            || "_".to_owned(),
            |(c, sub)| ctor_text(ctx, ty, &c, &vec!["_".to_owned(); sub.len()]),
        );
    w.insert(0, head);
    Some(w)
}

/// `c` applied to `args`, as source text.
pub(crate) fn ctor_text(ctx: &LowerCtx<'_>, ty: &HirTy, c: &Ctor, args: &[String]) -> String {
    match (c, ty) {
        (Ctor::Bool(b), _) => b.to_string(),
        (Ctor::Single, HirTy::Struct(id)) => {
            let def = ctx.struct_def(*id);
            let fields: Vec<String> = def
                .fields
                .iter()
                .zip(args)
                .filter(|(_, a)| *a != "_")
                .map(|(f, a)| format!("{}: {a}", f.name))
                .collect();
            let name = enum_base_name(&def.name);
            if fields.is_empty() {
                format!("{name} {{ .. }}")
            } else {
                format!("{name} {{ {}, .. }}", fields.join(", "))
            }
        }
        (Ctor::Single, _) if args.len() == 1 => format!("({},)", args[0]),
        (Ctor::Single, _) => format!("({})", args.join(", ")),
        (Ctor::Variant(i), HirTy::Enum(id)) => {
            let def = ctx.enum_def(*id);
            let v = &def.variants[*i as usize].name;
            let base = enum_base_name(&def.name);
            if args.is_empty() {
                format!("{base}::{v}")
            } else {
                format!("{base}::{v}({})", args.join(", "))
            }
        }
        (Ctor::Variant(i), HirTy::Sum(vs)) => ctx.ty_name(&vs[*i as usize]),
        (Ctor::Len(_, more), _) => {
            let mut parts = args.to_vec();
            if *more {
                parts.push("..".to_owned());
            }
            format!("[{}]", parts.join(", "))
        }
        _ => "_".to_owned(),
    }
}

/// Report an arm no value can reach (an error), then check that the
/// unguarded arms cover the scrutinee's type.
pub(crate) fn check_match_arms(
    ctx: &LowerCtx<'_>,
    scrut_ty: &HirTy,
    arms: &[HirMatchArm],
    synthetic: &[bool],
) {
    let tys = [scrut_ty.clone()];
    SEQ_SPLIT.set(seq_split(arms));
    let mut rows: Vec<Vec<Pat>> = Vec::new();
    // The catch-all a foreign `#[non_exhaustive]` type requires is never
    // unreachable.
    let foreign = foreign_non_exhaustive(ctx, scrut_ty);
    // In an instance, reachability turns on the type arguments; the
    // definition check reported what doesn't.
    let instance = in_instance(ctx);
    for (i, a) in arms.iter().enumerate() {
        let p = simplify(ctx, &a.pat);
        let exempt = synthetic.get(i).copied().unwrap_or(false)
            || (foreign.is_some() && hir_pattern_irrefutable(&a.pat));
        if !exempt && !instance && !useful(ctx, &rows, std::slice::from_ref(&p), &tys) {
            // At the arm (its body), not the whole `match`.
            let at = if a.body.span.is_dummy() {
                ctx.here()
            } else {
                a.body.span
            };
            ctx.emit_error_at(
                Code::UNREACHABLE_ARM,
                at,
                format!(
                    "unreachable match arm {}: earlier arms already match every value it \
                     matches",
                    i + 1
                ),
            );
        }
        if a.guard.is_none() {
            rows.push(vec![p]);
        }
    }
    let catch_all = arms
        .iter()
        .any(|a| a.guard.is_none() && hir_pattern_irrefutable(&a.pat));
    if let Some(name) = foreign.filter(|_| !catch_all) {
        ctx.emit_error(
            Code::NON_EXHAUSTIVE,
            format!(
                "match on `{name}` needs a `_` arm: it is `#[non_exhaustive]` in another \
                 package"
            ),
        );
        return;
    }
    if useful(ctx, &rows, &[Pat::Wild], &tys) {
        let w = witness(ctx, &rows, &tys).and_then(|mut w| w.pop());
        check_match_exhaustiveness(ctx, scrut_ty, arms, w);
    }
}

/// Arms must cover the scrutinee. `bool` needs both literals,
/// enums / sums every variant (guarded arms cover nothing), others a
/// catch-all.
pub(crate) fn check_match_exhaustiveness(
    ctx: &LowerCtx<'_>,
    scrut_ty: &HirTy,
    arms: &[HirMatchArm],
    witness: Option<String>,
) {
    let unguarded: Vec<&HirPattern> = arms
        .iter()
        .filter(|a| a.guard.is_none())
        .map(|a| &a.pat)
        .collect();
    if unguarded.iter().any(|p| hir_pattern_irrefutable(p)) {
        return;
    }
    let arms_to_add = missing_arms(ctx, scrut_ty, &unguarded);
    let mut missing: Vec<String> = match scrut_ty {
        HirTy::Prim(PrimTy::Bool) => {
            fn bools(p: &HirPattern, seen: &mut [bool; 2]) {
                match p {
                    HirPattern::Literal {
                        value: ConstVal::Bool(b),
                        ..
                    } => seen[usize::from(*b)] = true,
                    HirPattern::Or(alts) => alts.iter().for_each(|a| bools(a, seen)),
                    HirPattern::At { sub, .. } => bools(sub, seen),
                    _ => {}
                }
            }
            let mut seen = [false, false];
            for p in &unguarded {
                bools(p, &mut seen);
            }
            [("false", seen[0]), ("true", seen[1])]
                .iter()
                .filter(|(_, s)| !s)
                .map(|(n, _)| format!("`{n}`"))
                .collect()
        }
        HirTy::Enum(eid) => {
            let mut covered = Vec::new();
            for p in &unguarded {
                covered_variants(p, &mut covered);
            }
            let def = ctx.enum_def(*eid);
            let base = enum_base_name(&def.name);
            def.variants
                .iter()
                .enumerate()
                .filter(|(i, _)| !covered.contains(&u32::try_from(*i).expect("variant overflow")))
                .map(|(_, v)| format!("`{base}::{}`", v.name))
                .collect()
        }
        HirTy::Sum(variants) => {
            let mut covered = Vec::new();
            for p in &unguarded {
                covered_variants(p, &mut covered);
            }
            variants
                .iter()
                .enumerate()
                .filter(|(i, _)| !covered.contains(&u32::try_from(*i).expect("variant overflow")))
                .map(|(_, v)| format!("`{}`", ctx.ty_name(v)))
                .collect()
        }
        HirTy::Tuple(_) | HirTy::Struct(_) => Vec::new(),
        other => vec![format!(
            "values of `{}` (add a `_` arm)",
            ctx.ty_name(other)
        )],
    };
    if missing.is_empty() {
        // Every top-level case appears but a nested one is missing
        // (`(true, _)` without `(false, _)`).
        missing.push(witness.map_or_else(
            || "some nested values (add a `_` arm)".to_owned(),
            |w| format!("`{w}`"),
        ));
    }
    ctx.emit_error(
        Code::NON_EXHAUSTIVE,
        format!(
            "non-exhaustive match on `{}`: {} not covered",
            ctx.ty_name(scrut_ty),
            missing.join(", ")
        ),
    );
    // Else the cases named above, when each is a pattern (`false`); a
    // nested witness is one of maybe several, so it gets no fix.
    let arms_to_add = arms_to_add.filter(|a| !a.is_empty());
    let complete = arms_to_add.is_some() || *scrut_ty == HirTy::Prim(PrimTy::Bool);
    let arms = arms_to_add.unwrap_or_else(|| {
        missing
            .iter()
            .filter_map(|m| m.strip_prefix('`')?.strip_suffix('`'))
            .filter(|m| !m.contains('`'))
            .map(str::to_owned)
            .collect()
    });
    let (arms, complete) = if arms.is_empty() || arms.len() > 3 {
        (vec!["_".to_owned()], true)
    } else {
        (arms, complete)
    };
    let list: Vec<String> = arms.iter().map(|a| format!("`{a} => ..`")).collect();
    let help = if arms.len() == 1 {
        format!("add {}", list[0])
    } else {
        format!("add the arms {}", list.join(", "))
    };
    ctx.help_last(Some(help));
    if complete {
        add_arms_fix(ctx, &arms);
    }
}

/// Arm patterns for the cases of `ty` no unguarded arm covers; `None`
/// when they are not cases (`bool` is, its literals).
pub(crate) fn missing_arms(
    ctx: &LowerCtx<'_>,
    ty: &HirTy,
    unguarded: &[&HirPattern],
) -> Option<Vec<String>> {
    let mut covered = Vec::new();
    for p in unguarded {
        covered_variants(p, &mut covered);
    }
    let open = |i: usize| !covered.contains(&u32::try_from(i).expect("variant overflow"));
    match ty {
        HirTy::Enum(eid) => {
            let def = ctx.enum_def(*eid);
            let base = ctx.ty_name(ty);
            let base = base.split('<').next().unwrap_or_default().to_owned();
            Some(
                def.variants
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| open(*i))
                    .map(|(_, v)| {
                        let n = crate::lower::expr::variant_arg_tys(v).len();
                        if !v.named_fields.is_empty() {
                            format!("{base}::{} {{ .. }}", v.name)
                        } else if n > 0 {
                            format!("{base}::{}({})", v.name, vec!["_"; n].join(", "))
                        } else {
                            format!("{base}::{}", v.name)
                        }
                    })
                    .collect(),
            )
        }
        HirTy::Sum(cases) => Some(
            cases
                .iter()
                .enumerate()
                .filter(|(i, _)| open(*i))
                .map(|(_, c)| match c {
                    HirTy::None => "none".to_owned(),
                    t => format!("x: {}", ctx.ty_name(t)),
                })
                .collect(),
        ),
        _ => None,
    }
}

/// Insert `arms` before the closing brace of the match just reported,
/// when it spans lines.
pub(crate) fn add_arms_fix(ctx: &LowerCtx<'_>, arms: &[String]) {
    let at = ctx.here();
    let Some(text) = ctx.src(at) else {
        return;
    };
    let Some(close) = text.strip_suffix('}').map(str::len) else {
        return;
    };
    let line_start = text[..close].rfind('\n').map_or(0, |i| i + 1);
    let indent = &text[line_start..close];
    if line_start == 0 || !indent.chars().all(|c| c == ' ' || c == '\t') {
        return;
    }
    let mut body = String::new();
    for a in arms {
        body.push_str(&format!("    {a} => panic(\"todo\"),\n{indent}"));
    }
    if let Some(span) = ctx.sub_span(at, close, close) {
        ctx.fix_last(span, body, "add the missing arms");
    }
}

/// Is a generic body being lowered for concrete type arguments (not
/// for its definition check)?
pub(crate) fn in_instance(ctx: &LowerCtx<'_>) -> bool {
    !ctx.ty_subst.is_empty()
        && !ctx
            .ty_subst
            .values()
            .any(|t| crate::lower::bounds::has_param(ctx, t))
}

/// A struct pattern `pat` on `ty`, a `#[non_exhaustive]`
/// struct of another package, ends in `..` (its package may add fields).
pub(crate) fn struct_pattern_rest(ctx: &LowerCtx<'_>, pat: &AstPattern, ty: &HirTy) {
    let AstPatternKind::Struct {
        path,
        fields,
        rest: false,
    } = &pat.kind
    else {
        return;
    };
    let listed: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    missing_rest(ctx, pat.span, path, &listed, ty);
}

/// `struct_pattern_rest` of a pattern without `..` that lists `listed`.
pub(crate) fn missing_rest(
    ctx: &LowerCtx<'_>,
    at: nabic_diagnostic::Span,
    path: &nabic_ast::Path,
    listed: &[&str],
    ty: &HirTy,
) {
    let Some(name) = foreign_non_exhaustive(ctx, ty) else {
        return;
    };
    let segs: Vec<&str> = path.segments.iter().map(|s| s.name.as_str()).collect();
    let help = if listed.is_empty() {
        format!("{} {{ .. }}", segs.join("::"))
    } else {
        format!("{} {{ {}, .. }}", segs.join("::"), listed.join(", "))
    };
    ctx.emit_error_at(
        Code::PATTERN_ARITY,
        at,
        format!(
            "pattern on `{name}` needs `..`: it is `#[non_exhaustive]` in another package, \
             which may add fields; write `{help}`"
        ),
    );
}

/// `ty`'s name when it is a `#[non_exhaustive]` struct / enum of another
/// package.
pub(crate) fn foreign_non_exhaustive(ctx: &LowerCtx<'_>, ty: &HirTy) -> Option<String> {
    let name = match ty {
        HirTy::Enum(id) => enum_base_name(&ctx.enum_def(*id).name).to_owned(),
        HirTy::Struct(id) => enum_base_name(&ctx.struct_def(*id).name).to_owned(),
        _ => return None,
    };
    (ctx.env.is_non_exhaustive(&name)
        && package_of(&module_of(&name)) != package_of(&ctx.current_module))
    .then(|| ctx.ty_name(ty))
}
