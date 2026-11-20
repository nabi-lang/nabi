//! Types, exports, custom sections and the module's bytes.

#[allow(clippy::wildcard_imports)]
use crate::wasm::write::*;

/// The type section: `call_indirect` types first (registered by
/// [`scan`]), then imported functions', defined functions', tags'.
pub(crate) fn types(l: &Link, out: &mut Out) {
    let imp: Vec<u32> = out
        .imports
        .iter()
        .filter(|i| i.kind == 0 || i.kind == 4)
        .map(|i| i.sig)
        .collect();
    for s in imp {
        register_type(out, s);
    }
    let sigs: Vec<u32> = out
        .funcs
        .iter()
        .chain(&out.tags)
        .filter_map(|&a| match &l.chunks[a as usize] {
            Chunk::Func { sig, .. } | Chunk::Tag { sig, .. } => Some(*sig),
            _ => None,
        })
        .collect();
    for s in sigs {
        register_type(out, s);
    }
}

/// (name, kind, index) of each export: the memory, then each live
/// exported symbol in symbol table order.
pub(crate) fn exports(
    l: &Link,
    o: &Options,
    out: &mut Out,
) -> Result<Vec<(String, u8, u32)>, String> {
    let mut v = Vec::new();
    if !o.import_memory || o.export_memory {
        v.push(("memory".to_owned(), 2, 0));
    }
    let mutable_globals = out.features.iter().any(|f| f == "mutable-globals");
    let mut next_global = out.imported[3] + (out.globals.len() + out.got.len()) as u32;
    for id in 0..l.syms.syms.len() as SymId {
        if !l.exported(id) || !l.live(id) || l.syms.get(id).name.starts_with('\0') {
            continue;
        }
        let x = l.ext(id);
        let mut n = l.syms.get(id).name.clone();
        match x.kind {
            Some(SYM_FUNCTION) => {
                let Def::Defined { atom, .. } = l.syms.get(id).def else {
                    continue;
                };
                if let Chunk::Func {
                    export_name: Some(e),
                    ..
                } = &l.chunks[atom as usize]
                {
                    n.clone_from(e);
                }
                v.push((n, 0, out.index[atom as usize]));
            }
            Some(SYM_GLOBAL) => {
                let Def::Defined { atom, .. } = l.syms.get(id).def else {
                    continue;
                };
                let synthetic = l.atoms[atom as usize].file == u32::MAX;
                if x.gty.is_some_and(|g| g.mutable) {
                    if !mutable_globals && synthetic && !x.force_export {
                        continue;
                    }
                    if !mutable_globals {
                        return Err(format!(
                            "mutable global exported but 'mutable-globals' feature not present \
                             in inputs: `{n}`. Use --no-check-features to suppress."
                        ));
                    }
                }
                v.push((n, 3, out.index[atom as usize]));
            }
            Some(SYM_TAG) => {
                let Def::Defined { atom, .. } = l.syms.get(id).def else {
                    continue;
                };
                v.push((n, 4, out.index[atom as usize]));
            }
            Some(SYM_TABLE) => {
                let Def::Defined { atom, .. } = l.syms.get(id).def else {
                    continue;
                };
                v.push((n, 1, out.index[atom as usize]));
            }
            Some(SYM_DATA) => {
                out.data_globals.push(id);
                v.push((n, 3, next_global));
                next_global += 1;
            }
            _ => {}
        }
    }
    if !mutable_globals {
        for i in &out.imports {
            if i.kind == 3 && i.gty.is_some_and(|g| g.mutable) {
                return Err(format!(
                    "mutable global imported but 'mutable-globals' feature not present in \
                     inputs: `{}`. Use --no-check-features to suppress.",
                    l.name(i.sym)
                ));
            }
        }
    }
    Ok(v)
}

/// Custom sections of the inputs, by name in order of appearance, each
/// the concatenation of its chunks (DWARF strings merged).
pub(crate) fn customs(l: &Link, o: &Options, out: &mut Out) -> Result<(), String> {
    let strip_debug = o.strip_debug || o.strip_all;
    let mut by_name: HashMap<&str, usize> = HashMap::default();
    let mut groups: Vec<(String, Vec<AtomId>)> = Vec::new();
    for &fi in &l.order {
        for &a in &l.files[fi as usize].customs {
            let Chunk::Custom { name, .. } = &l.chunks[a as usize] else {
                continue;
            };
            let n = name.as_str();
            let skip = matches!(
                n,
                "linking" | "name" | "producers" | "target_features" | ".llvmbc" | ".llvmcmd"
            ) || n.starts_with("reloc.")
                || (strip_debug && n.starts_with(".debug_"));
            if skip || l.discarded.contains(&a) || l.atoms[a as usize].size == 0 {
                continue;
            }
            let k = *by_name.entry(n).or_insert_with(|| {
                groups.push((n.to_owned(), Vec::new()));
                groups.len() - 1
            });
            groups[k].1.push(a);
        }
    }
    for (n, atoms) in groups {
        let mut items = Vec::new();
        let mut group = None;
        for &a in &atoms {
            let merge = matches!(&l.chunks[a as usize], Chunk::Custom { merge: true, .. });
            if !merge {
                items.push(Item::Atom(a));
                continue;
            }
            let g = *group.get_or_insert_with(|| {
                out.merged.push(Merged {
                    atoms: Vec::new(),
                    bytes: Vec::new(),
                    off: 0,
                });
                items.push(Item::Merged(out.merged.len() - 1));
                out.merged.len() - 1
            });
            out.merged[g].atoms.push(a);
        }
        if let Some(g) = group {
            merge_group(l, out, g)?;
        }
        let mut size = 0u64;
        for &it in &items {
            match it {
                Item::Atom(a) => {
                    let at = &l.atoms[a as usize];
                    size = size.next_multiple_of(1 << at.align);
                    out.custom_off.insert(a, size);
                    size += at.size;
                }
                Item::Merged(g) => {
                    out.merged[g].off = size;
                    for &a in &out.merged[g].atoms {
                        out.custom_off.insert(a, size);
                    }
                    size += out.merged[g].bytes.len() as u64;
                }
            }
        }
        out.customs.push(OutCustom {
            name: n,
            items,
            size,
        });
    }
    Ok(())
}

pub(crate) fn section(m: &mut Vec<u8>, id: u8, body: &[u8]) {
    m.push(id);
    uleb(m, body.len() as u64);
    m.extend_from_slice(body);
}

pub(crate) fn limits(b: &mut Vec<u8>, l: Limits) {
    b.push(l.flags);
    uleb(b, l.min);
    if l.flags & 1 != 0 {
        uleb(b, l.max);
    }
}

/// The module's bytes.
pub(crate) fn emit(
    l: &Link,
    o: &Options,
    out: &mut Out,
    exports: &[(String, u8, u32)],
) -> Result<Vec<u8>, String> {
    let mut m: Vec<u8> = crate::wasm::object::MAGIC.to_vec();
    let mut b = Vec::new();
    let ty = |out: &Out, sig: u32| out.type_of.get(&sig).copied().unwrap_or(0);
    // Types.
    if !out.types.is_empty() {
        uleb(&mut b, out.types.len() as u64);
        for &s in &out.types {
            let t = &l.sigs[s as usize];
            b.push(0x60);
            uleb(&mut b, t.params.len() as u64);
            b.extend_from_slice(&t.params);
            uleb(&mut b, t.results.len() as u64);
            b.extend_from_slice(&t.results);
        }
        section(&mut m, 1, &b);
    }
    let memory = Limits {
        flags: u8::from(out.max_pages != 0 || o.shared_memory)
            | if o.shared_memory { 2 } else { 0 },
        min: out.pages,
        max: out.max_pages,
    };
    // Imports.
    let n_imports = out.imports.len() + usize::from(o.import_memory);
    if n_imports > 0 {
        b.clear();
        uleb(&mut b, n_imports as u64);
        if o.import_memory {
            name(&mut b, "env");
            name(&mut b, "memory");
            b.push(2);
            limits(&mut b, memory);
        }
        for i in &out.imports {
            name(&mut b, &i.module);
            name(&mut b, &i.field);
            b.push(i.kind);
            match i.kind {
                0 => uleb(&mut b, u64::from(ty(out, i.sig))),
                1 => {
                    let t = i.tty.expect("table type");
                    b.push(t.elem);
                    limits(&mut b, t.limits);
                }
                3 => {
                    let g = i.gty.expect("global type");
                    b.extend([g.ty, u8::from(g.mutable)]);
                }
                _ => {
                    b.push(0);
                    uleb(&mut b, u64::from(ty(out, i.sig)));
                }
            }
        }
        section(&mut m, 2, &b);
    }
    // Functions.
    if !out.funcs.is_empty() {
        b.clear();
        uleb(&mut b, out.funcs.len() as u64);
        for &a in &out.funcs {
            let Chunk::Func { sig, .. } = &l.chunks[a as usize] else {
                unreachable!()
            };
            uleb(&mut b, u64::from(ty(out, *sig)));
        }
        section(&mut m, 3, &b);
    }
    // Tables.
    if !out.tables.is_empty() {
        b.clear();
        uleb(&mut b, out.tables.len() as u64);
        for &a in &out.tables {
            let Chunk::Table { ty: t, .. } = &l.chunks[a as usize] else {
                unreachable!()
            };
            let mut t = *t;
            if out
                .table_sym
                .is_some_and(|s| l.syms.resolve(Target::Sym(s)).is_some_and(|(x, _)| x == a))
            {
                let n = 1 + out.elems.len() as u64;
                t.limits = Limits {
                    flags: 1,
                    min: n,
                    max: n,
                };
            }
            b.push(t.elem);
            limits(&mut b, t.limits);
        }
        section(&mut m, 4, &b);
    }
    // Memory.
    if !o.import_memory {
        b.clear();
        b.push(1);
        limits(&mut b, memory);
        section(&mut m, 5, &b);
    }
    // Tags.
    if !out.tags.is_empty() {
        b.clear();
        uleb(&mut b, out.tags.len() as u64);
        for &a in &out.tags {
            let Chunk::Tag { sig, .. } = &l.chunks[a as usize] else {
                unreachable!()
            };
            b.push(0);
            uleb(&mut b, u64::from(ty(out, *sig)));
        }
        section(&mut m, 13, &b);
    }
    // Globals.
    let n_globals = out.globals.len() + out.got.len() + out.data_globals.len();
    if n_globals > 0 {
        b.clear();
        uleb(&mut b, n_globals as u64);
        for &a in &out.globals {
            let Chunk::Global { ty: g, init, .. } = &l.chunks[a as usize] else {
                unreachable!()
            };
            b.extend([g.ty, u8::from(g.mutable)]);
            b.extend_from_slice(init);
        }
        for &id in &out.got {
            let x = l.ext(id);
            let tls = x.flags & crate::wasm::TLS != 0;
            let v = match l.syms.get(id).def {
                _ if x.kind == Some(SYM_FUNCTION) => fn_key(l, out, id)
                    .and_then(|k| out.slot.get(&k))
                    .map_or(0, |&s| u64::from(s)),
                Def::Defined { atom, off } => data_va(l, out, atom, off, tls && o.shared_memory),
                Def::Absolute(v) => v,
                _ => 0,
            };
            b.extend([I32, u8::from(o.shared_memory && tls && !x.stub)]);
            i32_const(&mut b, v as i64);
            b.push(0x0b);
        }
        for &id in &out.data_globals {
            let v = match l.syms.get(id).def {
                Def::Defined { atom, off } => {
                    data_va(l, out, atom, off, l.ext(id).flags & crate::wasm::TLS != 0)
                }
                Def::Absolute(v) => v,
                _ => 0,
            };
            b.extend([I32, 0]);
            i32_const(&mut b, v as i64);
            b.push(0x0b);
        }
        section(&mut m, 6, &b);
    }
    // Exports.
    if !exports.is_empty() {
        b.clear();
        uleb(&mut b, exports.len() as u64);
        for (n, k, i) in exports {
            name(&mut b, n);
            b.push(*k);
            uleb(&mut b, u64::from(*i));
        }
        section(&mut m, 7, &b);
    }
    // Start.
    if let Some(a) = out.init_memory {
        b.clear();
        uleb(&mut b, u64::from(out.index[a as usize]));
        section(&mut m, 8, &b);
    }
    // Elements.
    if !out.elems.is_empty() {
        b.clear();
        b.push(1);
        let table = out.table_sym.and_then(|s| index_of(l, out, s)).unwrap_or(0);
        if table == 0 {
            b.push(0);
            i32_const(&mut b, 1);
            b.push(0x0b);
        } else {
            b.push(2);
            uleb(&mut b, u64::from(table));
            i32_const(&mut b, 1);
            b.extend([0x0b, 0]);
        }
        uleb(&mut b, out.elems.len() as u64);
        for &k in &out.elems {
            let f = match k {
                FnKey::Atom(a) => out.index[a as usize],
                FnKey::Sym(id) => out.import_index[&id],
            };
            uleb(&mut b, u64::from(f));
        }
        section(&mut m, 9, &b);
    }
    let emit_bss = emit_bss(o, out);
    let in_binary = |s: &OutSeg| !s.bss || emit_bss;
    let n_data = out.segs.iter().filter(|s| in_binary(s)).count();
    // Data count.
    if n_data > 0 && o.shared_memory {
        b.clear();
        uleb(&mut b, n_data as u64);
        section(&mut m, 12, &b);
    }
    // Code.
    if !out.funcs.is_empty() {
        b.clear();
        uleb(&mut b, out.funcs.len() as u64);
        out.code_off = vec![0; l.atoms.len()];
        for &a in &out.funcs {
            out.code_off[a as usize] = b.len() as u32;
            let at = &l.atoms[a as usize];
            match &l.chunks[a as usize] {
                Chunk::Func {
                    body: Some(body), ..
                } => b.extend_from_slice(body),
                Chunk::Func { body: None, .. } => {
                    let f = &l.files[at.file as usize];
                    let start = b.len();
                    b.extend_from_slice(&f.data[at.off as usize..(at.off + at.size) as usize]);
                    relocate(l, out, at.file, &at.relocs, &mut b[start..], 0, 0)?;
                }
                _ => unreachable!(),
            }
        }
        section(&mut m, 10, &b);
    }
    // Data.
    if n_data > 0 {
        b.clear();
        uleb(&mut b, n_data as u64);
        for s in &out.segs {
            if !in_binary(s) {
                continue;
            }
            if s.passive {
                b.push(1);
            } else {
                b.push(0);
                i32_const(&mut b, s.start as i64);
                b.push(0x0b);
            }
            uleb(&mut b, s.size);
            let base = b.len();
            b.resize(base + s.size as usize, 0);
            for &it in &s.items {
                match it {
                    Item::Atom(a) => {
                        let at = &l.atoms[a as usize];
                        let at_off = base + (at.addr - s.start) as usize;
                        let f = &l.files[at.file as usize];
                        let src = &f.data[at.off as usize..(at.off + at.size) as usize];
                        b[at_off..at_off + src.len()].copy_from_slice(src);
                        relocate(
                            l,
                            out,
                            at.file,
                            &at.relocs,
                            &mut b[at_off..at_off + src.len()],
                            at.addr,
                            0,
                        )?;
                    }
                    Item::Merged(g) => {
                        let mg = &out.merged[g];
                        let at_off = base + mg.off as usize;
                        b[at_off..at_off + mg.bytes.len()].copy_from_slice(&mg.bytes);
                    }
                }
            }
        }
        section(&mut m, 11, &b);
    }
    // Custom sections of the inputs.
    for c in &out.customs {
        b.clear();
        name(&mut b, &c.name);
        let base = b.len();
        b.resize(base + c.size as usize, 0);
        let tomb: u64 = if c.name == ".debug_ranges" || c.name == ".debug_loc" {
            0xffff_fffe
        } else if c.name.starts_with(".debug_") || c.name.starts_with("llvm.func_attr.") {
            0xffff_ffff
        } else {
            0
        };
        for &it in &c.items {
            match it {
                Item::Atom(a) => {
                    let at = &l.atoms[a as usize];
                    let off = base + out.custom_off[&a] as usize;
                    let f = &l.files[at.file as usize];
                    let src = &f.data[at.off as usize..(at.off + at.size) as usize];
                    b[off..off + src.len()].copy_from_slice(src);
                    relocate(
                        l,
                        out,
                        at.file,
                        &at.relocs,
                        &mut b[off..off + src.len()],
                        0,
                        tomb,
                    )?;
                }
                Item::Merged(g) => {
                    let mg = &out.merged[g];
                    let off = base + mg.off as usize;
                    b[off..off + mg.bytes.len()].copy_from_slice(&mg.bytes);
                }
            }
        }
        section(&mut m, 0, &b);
    }
    if !o.strip_all {
        name_section(l, o, out, &mut m, in_binary);
    }
    // Producers.
    let mut prod: [Vec<(&str, &str)>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for &fi in &l.order {
        for (k, list) in l.files[fi as usize].producers.iter().enumerate() {
            for (n, v) in list {
                if !prod[k].iter().any(|(x, _)| x == n) {
                    prod[k].push((n, v));
                }
            }
        }
    }
    let fields = prod.iter().filter(|p| !p.is_empty()).count();
    if fields > 0 && !o.strip_all {
        b.clear();
        name(&mut b, "producers");
        uleb(&mut b, fields as u64);
        for (k, field) in ["language", "processed-by", "sdk"].iter().enumerate() {
            if prod[k].is_empty() {
                continue;
            }
            name(&mut b, field);
            uleb(&mut b, prod[k].len() as u64);
            for (n, v) in &prod[k] {
                name(&mut b, n);
                name(&mut b, v);
            }
        }
        section(&mut m, 0, &b);
    }
    // Target features.
    if !out.features.is_empty() && !o.strip_all {
        b.clear();
        name(&mut b, "target_features");
        uleb(&mut b, out.features.len() as u64);
        for f in &out.features {
            b.push(b'+');
            name(&mut b, f);
        }
        section(&mut m, 0, &b);
    }
    Ok(m)
}
