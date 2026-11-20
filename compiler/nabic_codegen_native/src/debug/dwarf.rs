//! `-g`: DWARF 4: a subprogram per positioned function (and
//! generated ones) named by nabi path, parameters and named locals in
//! `fp`-relative slots within lexical blocks, their types, and a line
//! table.
//!
//! Optimized code (`FnDebug::opt`) adds: a variable's location by code
//! range (`.debug_loc`; a register, its slot, a constant, pieces of
//! those; none where its value is gone), the bodies inlined as
//! `DW_TAG_inlined_subroutine` of an abstract subprogram each, and line
//! rows naming each one's file, marking the end of the prologue and
//! only a new line a statement.

mod abbrev;
mod line;
mod types;

pub(crate) use crate::debug::dwarf::abbrev::*;
pub(crate) use crate::debug::dwarf::line::*;
pub(crate) use crate::debug::dwarf::types::*;

use crate::debug::locs::Place;
use crate::obj::{FnRange, Obj, RelKind, Reloc, Section};
use nabic_core::HashMap;
use nabic_core::leb::{sleb, uleb};
use nabic_ir_common::PrimTy;
use nabic_mir::gc_abi::HEADER_SIZE;
use nabic_mir::{DebugTy, MirModule, MirTy, Target};
use std::path::{Path, PathBuf};

/// The low `n` bytes of `v` in the target's byte order.
fn int(be: bool, v: u64, n: usize) -> Vec<u8> {
    if be {
        v.to_be_bytes()[8 - n..].to_vec()
    } else {
        v.to_le_bytes()[..n].to_vec()
    }
}

fn cstr(b: &mut Vec<u8>, s: &str) {
    b.extend_from_slice(s.as_bytes());
    b.push(0);
}

/// Code of scope `id` and its nested scopes as merged `[start, end)`
/// ranges (interleaved with other scopes' code).
fn scope_code(id: u32, d: &crate::lir::FnDebug, f: &FnRange) -> Vec<(u32, u32)> {
    let mut ids = vec![id];
    let mut n = 0;
    while n < ids.len() {
        let p = ids[n];
        ids.extend(d.scopes.iter().filter(|s| s.1 == p).map(|s| s.0));
        n += 1;
    }
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (i, &(at, _, _, sc)) in f.lines.iter().enumerate() {
        let end = f.lines.get(i + 1).map_or(f.len, |n| n.0);
        if end <= at || !ids.contains(&sc) {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.1 == at => last.1 = end,
            _ => out.push((at, end)),
        }
    }
    out
}

/// Add the debug sections of `obj`'s functions.
pub fn write(obj: &mut Obj, m: &MirModule, sources: &nabic_diagnostic::SourceMap) {
    let t = &m.target;
    let fns: Vec<&FnRange> = obj.fns.iter().filter(|f| f.debug.is_some()).collect();
    // The unit is named by its first function with a position; generated
    // functions without one are at line 0 of its file.
    let Some(first_file) = fns
        .iter()
        .filter_map(|f| f.debug.as_ref().map(|d| d.0.file))
        .find(|&f| f != 0)
    else {
        return;
    };
    // Files numbered by first use: an object names only its own, so a unit's
    // doesn't change when another file appears.
    let mut used: Vec<u32> = vec![first_file];
    for f in &fns {
        let d = f.debug.as_ref().map(|d| &d.0);
        let sites = d.iter().flat_map(|d| &d.inlined).map(|i| i.file);
        for id in d.map(|d| d.file).into_iter().chain(sites) {
            if id != 0 && !used.contains(&id) {
                used.push(id);
            }
        }
    }
    // wasm32's optimized variables have places (`FnRange::locs`) too.
    let opt = fns.iter().any(|f| {
        !f.locs.is_empty()
            || f.debug
                .as_ref()
                .is_some_and(|d| d.0.opt || !d.0.inlined.is_empty())
    });
    let fid = |id: u32| {
        let id = if id == 0 { first_file } else { id };
        used.iter()
            .position(|&u| u == id)
            .map_or(0, |i| i as u32 + 1)
    };
    let all = sources.files();
    let files: Vec<PathBuf> = used
        .iter()
        .map(|&id| {
            all.get(id as usize - 1)
                .map_or_else(PathBuf::new, |(p, _)| sources.output_path(p).into())
        })
        .collect();
    let cu_path = files[0].clone();
    let dir_of = |p: &Path| {
        p.parent()
            .map_or_else(String::new, |d| d.to_string_lossy().into_owned())
    };

    // The sections this adds, by `Obj::extra` index.
    let base = obj.extra.len() as u32;
    let (abbrev_sect, line_sect, ranges_sect) = (base, base + 2, base + 3);
    // Optimized: `.debug_loc` next (empty or not), then any frames.
    let loc_sect = base + 4;
    let frame_sect = base + 4 + u32::from(opt);
    let first = obj.fns.iter().min_by_key(|f| f.start);
    let mut info = Info {
        b: Vec::new(),
        relocs: Vec::new(),
        types: HashMap::default(),
        t,
        m,
        building: HashMap::default(),
        any_open: None,
        ranges: Vec::new(),
        ranges_relocs: Vec::new(),
        loc: Vec::new(),
        loc_relocs: Vec::new(),
        cu_low: if obj.elf || obj.wasm.is_some() {
            0
        } else {
            first.map_or(0, |f| f.start)
        },
        split: obj.elf || obj.wasm.is_some(),
        aw: if obj.c32() { 4 } else { 8 },
        be: obj.be,
    };
    // Header (length patched below), then the unit.
    info.u32(0);
    info.b.extend_from_slice(&int(obj.be, 4, 2));
    info.sec_off(abbrev_sect, 0);
    info.b.push(info.aw as u8);
    info.b.push(A_CU);
    cstr(&mut info.b, "nabic");
    info.b
        .extend_from_slice(&int(obj.be, u64::from(DW_LANG_RUST), 2));
    cstr(
        &mut info.b,
        &cu_path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
    );
    cstr(&mut info.b, &dir_of(&cu_path));
    info.sec_off(line_sect, 0);
    if info.split {
        info.b.resize(info.b.len() + info.aw, 0);
        info.sec_off(ranges_sect, 0);
        for f in &obj.fns {
            info.range(f, 0, f.len);
        }
        info.ranges.resize(info.ranges.len() + 2 * info.aw, 0);
    } else {
        // The text section is one range from its first function.
        let text_start = obj.fns.first().map_or(0, |f| f.start);
        let text_end = obj.fns.last().map_or(0, |f| f.start + f.len);
        info.addr(first.map_or(0, |f| f.sym), first.map_or(0, |f| f.start));
        info.u32(text_end - text_start);
    }
    // Types first, so variables refer back to them.
    for f in &fns {
        let d = &f.debug.as_ref().expect("debug").0;
        for v in &d.vars {
            info.debug_ty(&v.ty);
        }
        if let Some(p) = d.ret {
            info.base(p);
        }
    }
    // The bodies inlined: an abstract subprogram each.
    let mut origins: HashMap<(String, u32, u32), u32> = HashMap::default();
    for f in &fns {
        let d = &f.debug.as_ref().expect("debug").0;
        for i in &d.inlined {
            let key = (i.name.clone(), i.file, i.line);
            if origins.contains_key(&key) {
                continue;
            }
            origins.insert(key, info.b.len() as u32);
            info.b.push(A_ABSTRACT);
            cstr(&mut info.b, &nabic_core::local::unmangle(&i.name));
            uleb(&mut info.b, u64::from(fid(i.file)));
            uleb(&mut info.b, u64::from(i.line));
            info.b.push(DW_INL_INLINED);
        }
    }
    for f in &fns {
        let (d, offs) = f.debug.as_ref().expect("debug");
        let ret = d.ret.and_then(|p| info.base(p));
        // Inlined bodies with code of their own.
        let sites: Vec<u32> = d
            .inlined
            .iter()
            .map(|i| i.scope)
            .filter(|&c| !scope_code(c, d, f).is_empty())
            .collect();
        let kids = !sites.is_empty() || d.vars.iter().any(|v| info.debug_ty(&v.ty).is_some());
        info.b.push(match (ret.is_some(), kids) {
            (true, true) => A_SUB_RET,
            (false, true) => A_SUB,
            (true, false) => A_SUB_RET_LEAF,
            (false, false) => A_SUB_LEAF,
        });
        // An item in a block by its own name.
        cstr(&mut info.b, &nabic_core::local::unmangle(&d.name));
        uleb(&mut info.b, u64::from(fid(d.file)));
        uleb(&mut info.b, u64::from(d.line));
        info.addr(f.sym, f.start);
        if info.split {
            info.addr(f.sym, f.start + f.len);
        } else {
            info.u32(f.len);
        }
        // The frame pointer: `x29` / `rbp` / `ebp` / `r11` / `s0` / `r31`
        // (powerpc64) / `r11` (s390x).
        let fp = if obj.ppc64 {
            DW_OP_REG31
        } else if obj.s390x {
            DW_OP_REG11
        } else if obj.riscv {
            DW_OP_REG8
        } else if obj.arm {
            DW_OP_REG11
        } else if obj.i386 {
            DW_OP_REG5
        } else if obj.x86 {
            DW_OP_REG6
        } else {
            DW_OP_REG29
        };
        // wasm: `DW_OP_WASM_location` of the frame base's local.
        match obj.wasm.as_ref().and_then(|w| w.fp.get(&f.sym)) {
            Some(&l) => {
                let mut e = vec![DW_OP_WASM_LOCATION, 0];
                uleb(&mut e, u64::from(l));
                uleb(&mut info.b, e.len() as u64);
                info.b.extend_from_slice(&e);
            }
            None => info.b.extend_from_slice(&[1, fp]),
        }
        if let Some(r) = ret {
            info.type_ref(r);
        }
        // Scopes that hold a variable or an inlined body (or one that
        // does).
        let mut live: Vec<u32> = Vec::new();
        for s in d.vars.iter().map(|v| v.scope).chain(sites.iter().copied()) {
            let mut s = s;
            while s != 0 && !live.contains(&s) {
                live.push(s);
                s = d.scopes.iter().find(|(i, _)| *i == s).map_or(0, |x| x.1);
            }
        }
        let var_die = |info: &mut Info<'_>, i: usize| {
            let v = &d.vars[i];
            let Some(ty) = info.debug_ty(&v.ty) else {
                return;
            };
            let cell = matches!(v.ty, DebugTy::Cell(_));
            // Optimized: one location for all the code, a list, or none.
            let (one, list) = if d.opt || !f.locs.is_empty() {
                let spans = f.locs.get(i).map_or(&[][..], Vec::as_slice);
                match spans {
                    [(0, hi, p)] if *hi >= f.len => (Some(place_expr(p, cell)), None),
                    [] => (None, None),
                    _ => {
                        let l: Vec<_> = spans
                            .iter()
                            .map(|(a, b, p)| (*a, *b, place_expr(p, cell)))
                            .collect();
                        (None, Some(info.loc_list(f, &l)))
                    }
                }
            } else {
                let mut loc = vec![DW_OP_FBREG];
                sleb(&mut loc, i64::from(offs[i]));
                if cell {
                    loc.extend([DW_OP_DEREF, DW_OP_PLUS_UCONST]);
                    uleb(&mut loc, u64::from(HEADER_SIZE));
                }
                (Some(loc), None)
            };
            info.b.push(match (v.param, &one, list) {
                (true, Some(_), _) => A_PARAM,
                (false, Some(_), _) => A_VAR,
                (true, None, Some(_)) => A_PARAM_LIST,
                (false, None, Some(_)) => A_VAR_LIST,
                (true, None, None) => A_PARAM_NONE,
                (false, None, None) => A_VAR_NONE,
            });
            cstr(&mut info.b, &v.name);
            uleb(&mut info.b, u64::from(fid(d.file_of(v.scope))));
            uleb(&mut info.b, u64::from(v.line));
            info.u32(ty);
            if let Some(loc) = one {
                uleb(&mut info.b, loc.len() as u64);
                info.b.extend_from_slice(&loc);
            } else if let Some(at) = list {
                info.sec_off(loc_sect, at);
            }
        };
        let cx = ScopeCx {
            d,
            live: &live,
            f,
            ranges_sect,
            var_die: &var_die,
            origins: &origins,
            fid: &fid,
        };
        if kids {
            scope(&mut info, 0, &cx);
            info.b.push(0);
        }
    }
    info.b.push(0);
    let len = info.b.len() as u32 - 4;
    let len = int(obj.be, u64::from(len), 4);
    info.b[..4].copy_from_slice(&len);

    let unit = if obj.x86 || obj.i386 || obj.wasm.is_some() {
        1
    } else if obj.s390x {
        2
    } else {
        4
    };
    let line = line_program(&fns, &files, &dir_of, &fid, (unit, info.aw, obj.be));
    // No `.eh_frame` on 32-bit ARM: debuggers read the frames here.
    let frames = obj.arm.then(|| debug_frame(&obj.fns, frame_sect));
    let mut sect = |name: &'static str, data: Vec<u8>, relocs: Vec<Reloc>| {
        obj.extra.push((
            "__DWARF",
            name,
            0x0200_0000,
            Section {
                data,
                relocs,
                align: 0,
            },
        ));
    };
    let apple = !obj.elf
        && !obj.coff
        && obj.wasm.is_none()
        && fns
            .iter()
            .any(|f| f.debug.as_ref().is_some_and(|d| d.0.opt));
    sect(
        "__debug_abbrev",
        abbrevs(obj.elf || obj.wasm.is_some(), opt, apple),
        Vec::new(),
    );
    sect("__debug_info", info.b, info.relocs);
    sect("__debug_line", line.0, line.1);
    sect("__debug_ranges", info.ranges, info.ranges_relocs);
    if opt {
        sect("__debug_loc", info.loc, info.loc_relocs);
    }
    if let Some((data, relocs)) = frames {
        sect("__debug_frame", data, relocs);
    }
}

/// 32-bit ARM `.debug_frame` (section `sect` of the extras): a CIE and an
/// FDE per function, the frame `arm32::PROLOGUE` makes.
fn debug_frame(fns: &[FnRange], sect: u32) -> (Vec<u8>, Vec<Reloc>) {
    let mut b = Vec::new();
    let mut relocs = Vec::new();
    // CIE: version 1, no augmentation, code align 4, data align -4,
    // return address `lr`; CFA = sp.
    let mut cie = vec![0xff, 0xff, 0xff, 0xff, 1, 0, 4, 0x7c, 14, 0x0c, 13, 0];
    while (cie.len() + 4) % 4 != 0 {
        cie.push(0);
    }
    b.extend_from_slice(&(cie.len() as u32).to_le_bytes());
    b.extend_from_slice(&cie);
    for f in fns {
        let mut fde = Vec::new();
        relocs.push(Reloc {
            off: b.len() as u32 + 4,
            kind: RelKind::SecOff32,
            sym: sect,
        });
        fde.extend_from_slice(&0u32.to_le_bytes());
        relocs.push(Reloc {
            off: b.len() as u32 + 8,
            kind: RelKind::Abs64,
            sym: f.sym,
        });
        fde.extend_from_slice(&f.start.to_le_bytes());
        fde.extend_from_slice(&f.len.to_le_bytes());
        // push {r4-r11, lr}: CFA = sp + 36, lr at CFA - 4, r11..r4
        // below; mov r11, sp: CFA = r11 + 36.
        fde.extend_from_slice(&[0x41, 0x0e, 36, 0x8e, 1]);
        for r in (4..=11u8).rev() {
            fde.extend_from_slice(&[0x80 | r, 13 - r]);
        }
        fde.extend_from_slice(&[0x41, 0x0d, 11]);
        while (fde.len() + 4) % 4 != 0 {
            fde.push(0);
        }
        b.extend_from_slice(&(fde.len() as u32).to_le_bytes());
        b.extend_from_slice(&fde);
    }
    (b, relocs)
}

/// What `scope` writes a function's scopes from.
struct ScopeCx<'a> {
    d: &'a crate::lir::FnDebug,
    /// Scopes holding a variable or an inlined body, or one that does.
    live: &'a [u32],
    f: &'a FnRange,
    ranges_sect: u32,
    var_die: &'a dyn Fn(&mut Info<'_>, usize),
    /// The abstract subprogram of each body inlined.
    origins: &'a HashMap<(String, u32, u32), u32>,
    fid: &'a dyn Fn(u32) -> u32,
}

/// The variables of scope `id` and its lexical blocks (those holding a
/// variable of their own) and inlined bodies, nested.
fn scope(info: &mut Info<'_>, id: u32, cx: &ScopeCx<'_>) {
    let d = cx.d;
    for (i, v) in d.vars.iter().enumerate() {
        if v.scope == id || (id == 0 && !cx.live.contains(&v.scope) && v.scope != 0) {
            (cx.var_die)(info, i);
        }
    }
    for &(c, parent) in &d.scopes {
        if parent != id || !cx.live.contains(&c) {
            continue;
        }
        let code = scope_code(c, d, cx.f);
        if let Some(site) = d.inlined.iter().find(|i| i.scope == c) {
            // Optimized away: nothing of it to show.
            if code.is_empty() {
                continue;
            }
            info.b.push(A_INLINED);
            let key = (site.name.clone(), site.file, site.line);
            let origin = cx.origins.get(&key).copied().unwrap_or(0);
            info.u32(origin);
            let at = info.ranges.len() as u32;
            info.sec_off(cx.ranges_sect, at);
            for (lo, hi) in code {
                info.range(cx.f, lo, hi);
            }
            info.ranges.resize(info.ranges.len() + 2 * info.aw, 0);
            uleb(&mut info.b, u64::from((cx.fid)(d.file_of(site.parent))));
            uleb(&mut info.b, u64::from(site.call_line));
            uleb(&mut info.b, u64::from(site.call_col));
            scope(info, c, cx);
            info.b.push(0);
            continue;
        }
        // Without variables of its own (or, optimized, without code), its
        // blocks go in the parent (as LLVM writes them).
        let own = d
            .vars
            .iter()
            .any(|v| v.scope == c && info.debug_ty(&v.ty).is_some());
        if !own || (d.opt && code.is_empty()) {
            scope(info, c, cx);
            continue;
        }
        info.b.push(A_BLOCK);
        let at = info.ranges.len() as u32;
        info.sec_off(cx.ranges_sect, at);
        for (lo, hi) in code {
            info.range(cx.f, lo, hi);
        }
        info.ranges.resize(info.ranges.len() + 2 * info.aw, 0);
        scope(info, c, cx);
        info.b.push(0);
    }
}

/// The DWARF expression of `p` (`cell`: the value is behind the
/// reference there, past the object header).
fn place_expr(p: &Place, cell: bool) -> Vec<u8> {
    let mut e = Vec::new();
    match p {
        Place::Frame(off) => {
            e.push(DW_OP_FBREG);
            sleb(&mut e, i64::from(*off));
            if cell {
                e.extend([DW_OP_DEREF, DW_OP_PLUS_UCONST]);
                uleb(&mut e, u64::from(HEADER_SIZE));
            }
        }
        Place::Reg(r, _) if cell => {
            if *r < 32 {
                e.push(DW_OP_BREG0 + *r as u8);
            } else {
                e.push(DW_OP_BREGX);
                uleb(&mut e, u64::from(*r));
            }
            sleb(&mut e, i64::from(HEADER_SIZE));
        }
        Place::Reg(r, _) => {
            if *r < 32 {
                e.push(DW_OP_REG0 + *r as u8);
            } else {
                e.push(DW_OP_REGX);
                uleb(&mut e, u64::from(*r));
            }
        }
        // wasm: the local's value, the reference itself for a cell.
        Place::Local(l) => {
            e.extend([DW_OP_WASM_LOCATION, 0]);
            uleb(&mut e, u64::from(*l));
            if cell {
                e.push(DW_OP_PLUS_UCONST);
                uleb(&mut e, u64::from(HEADER_SIZE));
            } else {
                e.push(DW_OP_STACK_VALUE);
            }
        }
        Place::Const(c) => {
            e.push(DW_OP_CONSTU);
            uleb(&mut e, *c);
            e.push(DW_OP_STACK_VALUE);
        }
        Place::Pieces(ps) => {
            for (size, p) in ps {
                if let Some(p) = p {
                    e.extend(place_expr(p, false));
                }
                e.push(DW_OP_PIECE);
                uleb(&mut e, u64::from(*size));
            }
        }
    }
    e
}
