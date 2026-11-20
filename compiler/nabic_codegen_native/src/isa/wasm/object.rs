//! A relocatable wasm object: the linking conventions' `linking`
//! section (symbol table, segment info), `reloc.CODE` / `reloc.DATA`, the
//! memory, `__stack_pointer`, `__tls_base` and the indirect function
//! table imported from `env`. One data segment per definition, so the
//! linker drops unused ones.

use crate::isa::wasm::{I32, Parts, Rel, Ty, uleb5};
use crate::obj::{Obj, RelKind, Sect};
use nabic_core::HashMap;
use nabic_core::leb::{sleb, uleb};
use std::fmt::Write;

const R_FUNCTION_INDEX_LEB: u8 = 0;
const R_TABLE_INDEX_SLEB: u8 = 1;
const R_TABLE_INDEX_I32: u8 = 2;
const R_MEMORY_ADDR_SLEB: u8 = 4;
const R_MEMORY_ADDR_I32: u8 = 5;
const R_TYPE_INDEX_LEB: u8 = 6;
const R_GLOBAL_INDEX_LEB: u8 = 7;
const R_FUNCTION_OFFSET_I32: u8 = 8;
const R_SECTION_OFFSET_I32: u8 = 9;
const R_TABLE_NUMBER_LEB: u8 = 20;
const R_MEMORY_ADDR_TLS_SLEB: u8 = 21;

const SYM_FUNCTION: u8 = 0;
const SYM_DATA: u8 = 1;
const SYM_GLOBAL: u8 = 2;
const SYM_SECTION: u8 = 3;
const SYM_TABLE: u8 = 5;

const BINDING_WEAK: u32 = 1;
const BINDING_LOCAL: u32 = 2;
const VISIBILITY_HIDDEN: u32 = 4;
const UNDEFINED: u32 = 0x10;
const EXPORTED: u32 = 0x20;
const EXPLICIT_NAME: u32 = 0x40;
const TLS: u32 = 0x100;

const SEG_TLS: u32 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Func,
    Data,
    Global,
    Table,
}

fn section(out: &mut Vec<u8>, id: u8, body: &[u8]) {
    out.push(id);
    uleb(out, body.len() as u64);
    out.extend_from_slice(body);
}

fn name(out: &mut Vec<u8>, s: &str) {
    uleb(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn custom(out: &mut Vec<u8>, n: &str, body: &[u8]) {
    let mut b = Vec::new();
    name(&mut b, n);
    b.extend_from_slice(body);
    section(out, 0, &b);
}

/// A data segment: its piece's bytes, relocations and symbols.
struct Seg {
    name: String,
    align: u32,
    tls: bool,
    bytes: Vec<u8>,
    /// `(offset, symbol, addend)` of pointer words.
    relocs: Vec<(u32, u32, i32)>,
}

#[must_use]
pub fn write(o: &Obj) -> Vec<u8> {
    write_listed(o).0
}

/// The name objdump gives relocation type `ty`.
const fn rel_name(ty: u8) -> &'static str {
    match ty {
        R_FUNCTION_INDEX_LEB => "R_WASM_FUNCTION_INDEX_LEB",
        R_TABLE_INDEX_SLEB => "R_WASM_TABLE_INDEX_SLEB",
        R_MEMORY_ADDR_SLEB => "R_WASM_MEMORY_ADDR_SLEB",
        R_TYPE_INDEX_LEB => "R_WASM_TYPE_INDEX_LEB",
        R_GLOBAL_INDEX_LEB => "R_WASM_GLOBAL_INDEX_LEB",
        R_TABLE_NUMBER_LEB => "R_WASM_TABLE_NUMBER_LEB",
        _ => "R_WASM_MEMORY_ADDR_TLS_SLEB",
    }
}

/// `write`, and with `Obj::asm` set the listing of each function.
#[must_use]
pub fn write_listed(o: &Obj) -> (Vec<u8>, Option<String>) {
    let w: &Parts = o.wasm.as_ref().expect("a wasm object");
    let n = o.syms.len();
    // Segments, and where each data symbol is.
    let (pieces, _) = o.pieces();
    let mut segs: Vec<Seg> = Vec::new();
    let mut data_at: HashMap<u32, (u32, u32, u32)> = HashMap::default();
    for p in &pieces {
        if p.sect == Sect::Text {
            continue;
        }
        let bytes = o.sects[&p.sect].data[p.start as usize..p.end as usize].to_vec();
        let first = &o.syms[p.syms[0] as usize].name;
        let tls = p.syms.iter().any(|s| w.tls.contains(s));
        let zero = bytes.iter().all(|b| *b == 0) && p.relocs.is_empty();
        let prefix = match p.sect {
            _ if tls => ".tbss.",
            Sect::Data if zero => ".bss.",
            Sect::Data => ".data.",
            _ => ".rodata.",
        };
        let i = segs.len() as u32;
        for &s in &p.syms {
            if let Some((_, off, size)) = o.syms[s as usize].def {
                data_at.insert(s, (i, off - p.start, size));
            }
        }
        let relocs = p
            .relocs
            .iter()
            .filter(|r| r.kind == RelKind::Abs64)
            .map(|r| {
                let at = r.off as usize;
                let add = i32::from_le_bytes(bytes[at..at + 4].try_into().expect("a word"));
                (r.off, r.sym, add)
            })
            .collect();
        segs.push(Seg {
            name: format!("{prefix}{first}"),
            align: p.align,
            tls,
            bytes,
            relocs,
        });
    }
    // What each symbol is.
    let mut kind: Vec<Option<Kind>> = vec![None; n];
    for c in &w.code {
        kind[c.sym as usize] = Some(Kind::Func);
    }
    for s in data_at.keys() {
        kind[*s as usize] = Some(Kind::Data);
    }
    let mut tls_ref = vec![false; n];
    let is_fn = |s: u32| w.fns.contains(&s) || w.sigs.contains_key(&s);
    let mark = |kind: &mut Vec<Option<Kind>>, s: u32, k: Kind| {
        if kind[s as usize].is_none() {
            kind[s as usize] = Some(k);
        }
    };
    for c in &w.code {
        for r in &c.relocs {
            match r.rel {
                Rel::Call(s) => mark(&mut kind, s, Kind::Func),
                Rel::Addr(s) => {
                    let k = if is_fn(s) { Kind::Func } else { Kind::Data };
                    mark(&mut kind, s, k);
                }
                Rel::Tls(s) => {
                    tls_ref[s as usize] = true;
                    mark(&mut kind, s, Kind::Data);
                }
                Rel::Global(s) => mark(&mut kind, s, Kind::Global),
                Rel::Table(s) => mark(&mut kind, s, Kind::Table),
                Rel::Type(_) => {}
            }
        }
    }
    for g in &segs {
        for &(_, s, _) in &g.relocs {
            let k = if is_fn(s) { Kind::Func } else { Kind::Data };
            mark(&mut kind, s, k);
        }
    }
    // Types.
    let mut types: Vec<Ty> = Vec::new();
    let type_of = |t: &Ty, types: &mut Vec<Ty>| -> u32 {
        if let Some(i) = types.iter().position(|x| x == t) {
            return i as u32;
        }
        types.push(t.clone());
        types.len() as u32 - 1
    };
    // Index spaces: imports first.
    let defined_fn: Vec<bool> = (0..n)
        .map(|s| w.code.iter().any(|c| c.sym == s as u32))
        .collect();
    let mut fn_index = vec![u32::MAX; n];
    let mut global_index = vec![u32::MAX; n];
    let mut table_index = vec![u32::MAX; n];
    let mut imports = Vec::new();
    let mut nimp_fn = 0u32;
    let (mut nglobal, mut ntable) = (0u32, 0u32);
    {
        let mut b = Vec::new();
        name(&mut b, "env");
        name(&mut b, "__linear_memory");
        b.extend([2, 0, 0]);
        imports.push(b);
    }
    for s in 0..n {
        match kind.get(s).copied().flatten() {
            Some(Kind::Func) if !defined_fn[s] => {
                let ty = w.sigs.get(&(s as u32)).cloned().unwrap_or_default();
                let t = type_of(&ty, &mut types);
                let (m, f) = w
                    .imports
                    .get(&(s as u32))
                    .cloned()
                    .unwrap_or_else(|| ("env".to_owned(), o.syms[s].name.clone()));
                let mut b = Vec::new();
                name(&mut b, &m);
                name(&mut b, &f);
                b.push(0);
                uleb(&mut b, u64::from(t));
                imports.push(b);
                fn_index[s] = nimp_fn;
                nimp_fn += 1;
            }
            Some(Kind::Global) => {
                let mut b = Vec::new();
                name(&mut b, "env");
                name(&mut b, &o.syms[s].name);
                b.extend([3, I32, 1]);
                imports.push(b);
                global_index[s] = nglobal;
                nglobal += 1;
            }
            Some(Kind::Table) => {
                let mut b = Vec::new();
                name(&mut b, "env");
                name(&mut b, &o.syms[s].name);
                b.extend([1, 0x70, 0, 0]);
                imports.push(b);
                table_index[s] = ntable;
                ntable += 1;
            }
            _ => {}
        }
    }
    let mut funcs = Vec::new();
    for (i, c) in w.code.iter().enumerate() {
        fn_index[c.sym as usize] = nimp_fn + i as u32;
        funcs.push(type_of(&c.ty, &mut types));
    }
    // The symbol table: every symbol defined or referenced.
    let mut sym_index = vec![u32::MAX; n];
    let mut symtab = Vec::new();
    // `--emit=asm`: each symbol's kind, whether defined, its flags.
    let mut attrs: Vec<(u32, Kind, bool, u32)> = Vec::new();
    let mut nsyms = 0u32;
    for s in 0..n {
        let Some(k) = kind[s] else { continue };
        let y = &o.syms[s];
        let defined = match k {
            Kind::Func => defined_fn[s],
            Kind::Data => data_at.contains_key(&(s as u32)),
            _ => false,
        };
        let mut flags = 0;
        if !defined {
            flags |= UNDEFINED;
        } else if !y.global {
            flags |= BINDING_LOCAL;
        }
        if y.weak {
            flags |= BINDING_WEAK;
        }
        if y.hidden {
            flags |= VISIBILITY_HIDDEN;
        }
        if w.exports.contains(&(s as u32)) {
            flags |= EXPORTED;
        }
        if tls_ref[s] || w.tls.contains(&(s as u32)) {
            flags |= TLS;
        }
        let explicit = !defined && k == Kind::Func && w.imports.contains_key(&(s as u32));
        if explicit {
            flags |= EXPLICIT_NAME;
        }
        attrs.push((s as u32, k, defined, flags));
        let mut b = Vec::new();
        match k {
            Kind::Func | Kind::Global | Kind::Table => {
                b.push(match k {
                    Kind::Func => SYM_FUNCTION,
                    Kind::Global => SYM_GLOBAL,
                    _ => SYM_TABLE,
                });
                uleb(&mut b, u64::from(flags));
                let idx = match k {
                    Kind::Func => fn_index[s],
                    Kind::Global => global_index[s],
                    _ => table_index[s],
                };
                uleb(&mut b, u64::from(idx));
                if defined || explicit {
                    name(&mut b, &y.name);
                }
            }
            Kind::Data => {
                b.push(SYM_DATA);
                uleb(&mut b, u64::from(flags));
                name(&mut b, &y.name);
                if let Some((seg, off, size)) = data_at.get(&(s as u32)) {
                    uleb(&mut b, u64::from(*seg));
                    uleb(&mut b, u64::from(*off));
                    uleb(&mut b, u64::from(*size));
                }
            }
        }
        symtab.push(b);
        sym_index[s] = nsyms;
        nsyms += 1;
    }
    // `-g`: a section symbol per debug section, which come after the
    // data section.
    let first_extra = 4 + u32::from(!segs.is_empty());
    let mut sect_sym = Vec::new();
    for i in 0..o.extra.len() as u32 {
        let mut b = vec![SYM_SECTION];
        uleb(&mut b, u64::from(BINDING_LOCAL));
        uleb(&mut b, u64::from(first_extra + i));
        symtab.push(b);
        sect_sym.push(nsyms);
        nsyms += 1;
    }
    // The code section, its relocations, the operands of this object's
    // own indices filled in.
    let mut code = Vec::new();
    uleb(&mut code, w.code.len() as u64);
    let mut code_rel = Vec::new();
    let src = o.src.is_some();
    let mut asm = o.asm.as_ref().filter(|_| !src).map(|_| String::new());
    let mut printer = crate::isa::wasm::enc::Printer::default();
    // `--emit=asm`: each function's body and relocated operands.
    let mut fn_src: Vec<FnText> = Vec::new();
    for c in &w.code {
        let mut body = c.body.clone();
        let mut rel = Vec::new();
        for r in &c.relocs {
            let at = r.off as usize;
            let (ty, idx, value): (u8, u32, Option<u32>) = match &r.rel {
                Rel::Call(s) => (
                    R_FUNCTION_INDEX_LEB,
                    sym_index[*s as usize],
                    Some(fn_index[*s as usize]),
                ),
                Rel::Addr(s) if kind[*s as usize] == Some(Kind::Func) => {
                    (R_TABLE_INDEX_SLEB, sym_index[*s as usize], None)
                }
                Rel::Addr(s) => (R_MEMORY_ADDR_SLEB, sym_index[*s as usize], None),
                Rel::Tls(s) => (R_MEMORY_ADDR_TLS_SLEB, sym_index[*s as usize], None),
                Rel::Global(s) => (
                    R_GLOBAL_INDEX_LEB,
                    sym_index[*s as usize],
                    Some(global_index[*s as usize]),
                ),
                Rel::Table(s) => (
                    R_TABLE_NUMBER_LEB,
                    sym_index[*s as usize],
                    Some(table_index[*s as usize]),
                ),
                Rel::Type(t) => {
                    let i = type_of(t, &mut types);
                    (R_TYPE_INDEX_LEB, i, Some(i))
                }
            };
            if let Some(v) = value {
                let mut b = Vec::new();
                uleb5(&mut b, v);
                body[at..at + 5].copy_from_slice(&b);
            }
            rel.push((ty, at as u32, idx, &r.rel));
        }
        if src {
            let ops = rel
                .iter()
                .map(|&(_, at, _, r)| {
                    let op = match r {
                        Rel::Type(t) => ty_text(t),
                        Rel::Tls(s) => format!("{}@TLSREL", quote(&o.syms[*s as usize].name)),
                        Rel::Call(s) | Rel::Addr(s) | Rel::Global(s) | Rel::Table(s) => {
                            quote(&o.syms[*s as usize].name)
                        }
                    };
                    (at, op)
                })
                .collect();
            fn_src.push((body.clone(), ops));
        }
        let mut sized = Vec::new();
        uleb(&mut sized, body.len() as u64);
        if let (Some(out), Some((locals, lines))) = (asm.as_mut(), &c.text) {
            let skip = sized.len() as u32;
            let relocs: Vec<(u32, &'static str, String)> = rel
                .iter()
                .map(|&(ty, at, idx, r)| {
                    let target = match r {
                        Rel::Type(_) => idx.to_string(),
                        Rel::Call(s)
                        | Rel::Addr(s)
                        | Rel::Tls(s)
                        | Rel::Global(s)
                        | Rel::Table(s) => o.syms[*s as usize].name.clone(),
                    };
                    (skip + at, rel_name(ty), target)
                })
                .collect();
            let mut f = sized.clone();
            f.extend_from_slice(&body);
            let name = &o.syms[c.sym as usize].name;
            let l = crate::isa::wasm::enc::Func {
                name,
                locals,
                code: &f,
                start: skip,
                lines,
                relocs: &relocs,
            };
            crate::isa::wasm::enc::list(out, &l, &mut printer);
        }
        let base = (code.len() + sized.len()) as u32;
        for (ty, at, idx, _) in rel {
            code_rel.push((ty, base + at, idx, 0i32));
        }
        code.extend_from_slice(&sized);
        code.extend_from_slice(&body);
    }
    // The data section: segments at increasing offsets.
    let mut data = Vec::new();
    uleb(&mut data, segs.len() as u64);
    let mut data_rel = Vec::new();
    let mut vaddr = 0u32;
    for g in &segs {
        vaddr = vaddr.next_multiple_of(1 << g.align);
        data.push(0);
        data.push(0x41);
        sleb(&mut data, i64::from(vaddr as i32));
        data.push(0x0b);
        uleb(&mut data, g.bytes.len() as u64);
        let start = data.len() as u32;
        let mut bytes = g.bytes.clone();
        for &(off, s, add) in &g.relocs {
            let f = kind[s as usize] == Some(Kind::Func);
            bytes[off as usize..off as usize + 4].fill(0);
            data_rel.push((
                if f {
                    R_TABLE_INDEX_I32
                } else {
                    R_MEMORY_ADDR_I32
                },
                start + off,
                sym_index[s as usize],
                if f { 0 } else { add },
            ));
        }
        data.extend_from_slice(&bytes);
        vaddr += g.bytes.len() as u32;
    }

    let mut out = b"\0asm\x01\0\0\0".to_vec();
    let mut nsect = 0u32;
    let mut b = Vec::new();
    uleb(&mut b, types.len() as u64);
    for (p, r) in &types {
        b.push(0x60);
        uleb(&mut b, p.len() as u64);
        b.extend_from_slice(p);
        uleb(&mut b, r.len() as u64);
        b.extend_from_slice(r);
    }
    section(&mut out, 1, &b);
    nsect += 1;
    let mut b = Vec::new();
    uleb(&mut b, imports.len() as u64);
    for i in &imports {
        b.extend_from_slice(i);
    }
    section(&mut out, 2, &b);
    nsect += 1;
    let mut b = Vec::new();
    uleb(&mut b, funcs.len() as u64);
    for t in &funcs {
        uleb(&mut b, u64::from(*t));
    }
    section(&mut out, 3, &b);
    nsect += 1;
    let code_sect = nsect;
    section(&mut out, 10, &code);
    nsect += 1;
    let data_sect = nsect;
    if !segs.is_empty() {
        section(&mut out, 11, &data);
    }
    let mut extra_rel = Vec::new();
    for (i, (_, n, _, sec)) in o.extra.iter().enumerate() {
        let mut rs = Vec::new();
        for r in &sec.relocs {
            let at = r.off as usize;
            let add = i32::from_le_bytes(sec.data[at..at + 4].try_into().expect("a word"));
            let (ty, idx) = match r.kind {
                RelKind::SecOff32 => (R_SECTION_OFFSET_I32, sect_sym[r.sym as usize]),
                _ => (R_FUNCTION_OFFSET_I32, sym_index[r.sym as usize]),
            };
            rs.push((ty, r.off, idx, add));
        }
        let name = format!(".{}", n.trim_start_matches('_'));
        custom(&mut out, &name, &sec.data);
        extra_rel.push((name, first_extra + i as u32, rs));
    }
    // `linking`.
    let mut l = Vec::new();
    uleb(&mut l, 2);
    if !segs.is_empty() {
        let mut b = Vec::new();
        uleb(&mut b, segs.len() as u64);
        for g in &segs {
            name(&mut b, &g.name);
            uleb(&mut b, u64::from(g.align));
            uleb(&mut b, u64::from(if g.tls { SEG_TLS } else { 0 }));
        }
        l.push(5);
        uleb(&mut l, b.len() as u64);
        l.extend_from_slice(&b);
    }
    let mut b = Vec::new();
    uleb(&mut b, u64::from(nsyms));
    for s in &symtab {
        b.extend_from_slice(s);
    }
    l.push(8);
    uleb(&mut l, b.len() as u64);
    l.extend_from_slice(&b);
    custom(&mut out, "linking", &l);
    let rel_sect = |out: &mut Vec<u8>, n: &str, sect: u32, rs: &[(u8, u32, u32, i32)]| {
        let mut b = Vec::new();
        uleb(&mut b, u64::from(sect));
        uleb(&mut b, rs.len() as u64);
        for &(ty, off, idx, add) in rs {
            b.push(ty);
            uleb(&mut b, u64::from(off));
            uleb(&mut b, u64::from(idx));
            if matches!(
                ty,
                R_MEMORY_ADDR_SLEB
                    | R_MEMORY_ADDR_I32
                    | R_MEMORY_ADDR_TLS_SLEB
                    | R_FUNCTION_OFFSET_I32
                    | R_SECTION_OFFSET_I32
            ) {
                sleb(&mut b, i64::from(add));
            }
        }
        custom(out, n, &b);
    };
    if !code_rel.is_empty() {
        rel_sect(&mut out, "reloc.CODE", code_sect, &code_rel);
    }
    if !data_rel.is_empty() {
        rel_sect(&mut out, "reloc.DATA", data_sect, &data_rel);
    }
    for (name, sect, rs) in &extra_rel {
        if !rs.is_empty() {
            rel_sect(&mut out, &format!("reloc.{name}"), *sect, rs);
        }
    }
    let mut feats: Vec<&str> = vec![
        "bulk-memory",
        "mutable-globals",
        "nontrapping-fptoint",
        "reference-types",
        "sign-ext",
    ];
    if w.threads {
        feats.insert(0, "atomics");
    }
    let mut b = Vec::new();
    uleb(&mut b, feats.len() as u64);
    for f in feats {
        b.push(b'+');
        name(&mut b, f);
    }
    custom(&mut out, "target_features", &b);
    if src {
        let s = Source {
            o,
            w,
            attrs: &attrs,
            fns: &fn_src,
            segs: &segs,
            data_at: &data_at,
            kind: &kind,
            features: &b,
        };
        asm = Some(s.write());
    }
    (out, asm)
}

/// A function's body and `(offset, operand)` of its relocated fields.
type FnText = (Vec<u8>, Vec<(u32, String)>);

/// A value type's name.
const fn vt_name(t: u8) -> &'static str {
    match t {
        crate::isa::wasm::I64 => "i64",
        crate::isa::wasm::F32 => "f32",
        crate::isa::wasm::F64 => "f64",
        crate::isa::wasm::V128 => "v128",
        _ => "i32",
    }
}

/// `name` as an assembler operand.
fn quote(name: &str) -> String {
    crate::asm::code::quote(name)
}

/// A function type as `.functype` and `call_indirect` write it.
fn ty_text((p, r): &Ty) -> String {
    let names = |ts: &[u8]| -> String {
        ts.iter()
            .map(|&t| vt_name(t))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!("({}) -> ({})", names(p), names(r))
}

/// What `--emit=asm` writes of an object: LLVM's wasm assembly, which
/// `llvm-mc -triple=wasm32-wasi -filetype=obj` turns back into it.
struct Source<'a> {
    o: &'a Obj,
    w: &'a Parts,
    attrs: &'a [(u32, Kind, bool, u32)],
    fns: &'a [FnText],
    segs: &'a [Seg],
    data_at: &'a HashMap<u32, (u32, u32, u32)>,
    kind: &'a [Option<Kind>],
    features: &'a [u8],
}

impl Source<'_> {
    fn name(&self, s: u32) -> String {
        quote(&self.o.syms[s as usize].name)
    }

    /// A symbol's binding and visibility.
    fn bind(&self, out: &mut String, s: u32, defined: bool, flags: u32) {
        let n = self.name(s);
        if flags & VISIBILITY_HIDDEN != 0 {
            let _ = writeln!(out, "\t.hidden\t{n}");
        }
        if flags & BINDING_WEAK != 0 {
            let _ = writeln!(out, "\t.weak\t{n}");
        } else if defined && flags & BINDING_LOCAL == 0 {
            let _ = writeln!(out, "\t.globl\t{n}");
        }
        if flags & EXPORTED != 0 {
            let _ = writeln!(out, "\t.export_name\t{n}, {n}");
        }
    }

    fn write(&self) -> String {
        let (o, w) = (self.o, self.w);
        let mut out = String::new();
        // Declarations: globals, tables, every function's type, imports.
        for &(s, k, defined, flags) in self.attrs {
            let n = self.name(s);
            match k {
                Kind::Global => {
                    let _ = writeln!(out, "\t.globaltype\t{n}, i32");
                }
                Kind::Table => {
                    let _ = writeln!(out, "\t.tabletype\t{n}, funcref");
                }
                Kind::Func => {
                    let ty = w
                        .code
                        .iter()
                        .find(|c| c.sym == s)
                        .map(|c| c.ty.clone())
                        .or_else(|| w.sigs.get(&s).cloned())
                        .unwrap_or_default();
                    let _ = writeln!(out, "\t.functype\t{n} {}", ty_text(&ty));
                    if !defined {
                        if flags & EXPLICIT_NAME != 0 {
                            let (m, f) = &w.imports[&s];
                            let _ = writeln!(out, "\t.import_module\t{n}, {}", quote(m));
                            let _ = writeln!(out, "\t.import_name\t{n}, {}", quote(f));
                        }
                        self.bind(&mut out, s, false, flags);
                    }
                }
                Kind::Data if !defined => self.bind(&mut out, s, false, flags),
                Kind::Data => {}
            }
        }
        let flags_of = |s: u32| self.attrs.iter().find(|a| a.0 == s).map_or(0, |a| a.3);
        // The functions, each in its section.
        for (c, (body, relocs)) in w.code.iter().zip(self.fns) {
            let n = self.name(c.sym);
            let _ = writeln!(
                out,
                "\n\t.section\t.text.{},\"\",@",
                o.syms[c.sym as usize].name
            );
            self.bind(&mut out, c.sym, true, flags_of(c.sym));
            let _ = writeln!(out, "\t.type\t{n},@function\n{n}:");
            let _ = writeln!(out, "\t.functype\t{n} {}", ty_text(&c.ty));
            let Some((locals, lines)) = &c.text else {
                continue;
            };
            if !locals.is_empty() {
                let l: Vec<&str> = locals.iter().map(|&t| vt_name(t)).collect();
                let _ = writeln!(out, "\t.local\t{}", l.join(", "));
            }
            let f = crate::isa::wasm::enc::SrcFunc {
                code: body,
                lines,
                relocs,
            };
            crate::isa::wasm::enc::source(&mut out, &f);
        }
        // The data segments, each a section.
        for (i, g) in self.segs.iter().enumerate() {
            let flags = if g.tls { "T" } else { "" };
            let _ = writeln!(out, "\n\t.section\t{},\"{flags}\",@", g.name);
            let _ = writeln!(out, "\t.p2align\t{}", g.align);
            let mut here: Vec<(u32, u32, u32)> = self
                .data_at
                .iter()
                .filter(|(_, d)| d.0 == i as u32)
                .map(|(&s, d)| (d.1, s, d.2))
                .collect();
            here.sort_unstable();
            let words: Vec<(u32, String)> = g
                .relocs
                .iter()
                .map(|&(off, s, add)| {
                    let f = self.kind[s as usize] == Some(Kind::Func);
                    (off, plus(&self.name(s), if f { 0 } else { add }))
                })
                .collect();
            let mut at = 0u32;
            let mut sizes = Vec::new();
            let mut k = 0;
            loop {
                let next = here.get(k).map_or(u32::MAX, |x| x.0);
                if next <= at {
                    let (_, s, size) = here[k];
                    let n = self.name(s);
                    let _ = writeln!(out, "\t.type\t{n},@object");
                    self.bind(&mut out, s, true, flags_of(s));
                    let _ = writeln!(out, "{n}:");
                    sizes.push((n, size));
                    k += 1;
                    continue;
                }
                let len = g.bytes.len() as u32;
                if at >= len {
                    break;
                }
                let stop = next.min(len);
                at = bytes(&mut out, &g.bytes, at, stop, &words);
            }
            for (n, size) in sizes {
                let _ = writeln!(out, "\t.size\t{n}, {size}");
            }
        }
        // Debug info: custom sections of data, offsets into functions and
        // into the other sections symbolic.
        let mut targets = vec![false; o.extra.len()];
        for (_, _, _, sec) in &o.extra {
            for r in sec.relocs.iter().filter(|r| r.kind == RelKind::SecOff32) {
                targets[r.sym as usize] = true;
            }
        }
        for (i, (_, n, _, sec)) in o.extra.iter().enumerate() {
            let name = format!(".{}", n.trim_start_matches('_'));
            let _ = writeln!(out, "\n\t.section\t{name},\"\",@");
            if targets[i] {
                let _ = writeln!(out, ".Lsec{i}:");
            }
            let words: Vec<(u32, String)> = sec
                .relocs
                .iter()
                .map(|r| {
                    let at = r.off as usize;
                    let add = i32::from_le_bytes(sec.data[at..at + 4].try_into().expect("a word"));
                    let base = match r.kind {
                        RelKind::SecOff32 => format!(".Lsec{}", r.sym),
                        _ => self.name(r.sym),
                    };
                    (r.off, plus(&base, add))
                })
                .collect();
            bytes(&mut out, &sec.data, 0, sec.data.len() as u32, &words);
        }
        let _ = writeln!(out, "\n\t.section\t.custom_section.target_features,\"\",@");
        bytes(&mut out, self.features, 0, self.features.len() as u32, &[]);
        out
    }
}

/// `base` plus `add`.
fn plus(base: &str, add: i32) -> String {
    match add {
        0 => base.to_owned(),
        a if a < 0 => format!("{base}-{}", -i64::from(a)),
        a => format!("{base}+{a}"),
    }
}

/// `b[from..to]` as data directives, `words` (`(offset, expression)`)
/// as `.int32`s; the offset reached (past a word that crosses `to`).
fn bytes(out: &mut String, b: &[u8], from: u32, to: u32, words: &[(u32, String)]) -> u32 {
    let mut at = from;
    while at < to {
        if let Some((_, e)) = words.iter().find(|w| w.0 == at) {
            let _ = writeln!(out, "\t.int32\t{e}");
            at += 4;
            continue;
        }
        let stop = words
            .iter()
            .map(|w| w.0)
            .filter(|&o| o > at)
            .min()
            .map_or(to, |o| o.min(to));
        let run = &b[at as usize..stop as usize];
        let zeros = run.iter().take_while(|&&x| x == 0).count();
        if zeros >= 8 {
            let _ = writeln!(out, "\t.skip\t{zeros}");
            at += zeros as u32;
            continue;
        }
        let end = run
            .windows(8)
            .position(|w| w.iter().all(|&x| x == 0))
            .filter(|&p| p > 0)
            .unwrap_or(run.len());
        text(out, &run[..end]);
        at += end as u32;
    }
    at
}

/// Bytes `b`: runs of text as `.ascii` (`.asciz` with the NUL after),
/// the rest as `.byte`s.
fn text(out: &mut String, b: &[u8]) {
    let shown = |x: u8| (0x20..0x7f).contains(&x);
    let mut i = 0;
    while i < b.len() {
        let n = b[i..].iter().take_while(|&&x| shown(x)).count();
        if n >= 4 {
            let mut t = String::new();
            for &x in &b[i..i + n] {
                if x == b'"' || x == b'\\' {
                    t.push('\\');
                }
                t.push(x as char);
            }
            i += n;
            if b.get(i) == Some(&0) {
                i += 1;
                let _ = writeln!(out, "\t.asciz\t\"{t}\"");
            } else {
                let _ = writeln!(out, "\t.ascii\t\"{t}\"");
            }
            continue;
        }
        // Up to 16 bytes, to the next run of text.
        let mut j = i;
        while j < b.len() && j - i < 16 {
            if b[j..].iter().take_while(|&&x| shown(x)).count() >= 4 {
                break;
            }
            j += 1;
        }
        let j = j.max(i + 1);
        let v: Vec<String> = b[i..j].iter().map(ToString::to_string).collect();
        let _ = writeln!(out, "\t.byte\t{}", v.join(", "));
        i = j;
    }
}
