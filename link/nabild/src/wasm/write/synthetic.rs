//! The functions the linker makes.

#[allow(clippy::wildcard_imports)]
use crate::wasm::write::*;

/// Passive segments (shared memory) and `.bss` in an imported memory are
/// set up by `__wasm_init_memory`, the start function.
pub(crate) fn init_memory(l: &mut Link, o: &Options, out: &mut Out) {
    let emit_bss = emit_bss(o, out);
    let needed = out
        .segs
        .iter()
        .any(|s| s.passive || (o.import_memory && s.bss && !emit_bss));
    if needed {
        let id = l.synth_func("__wasm_init_memory", &FuncType::default());
        let a = l.atom_of(id);
        l.atoms[a as usize].live = true;
        out.init_memory = Some(a);
        if o.shared_memory {
            if let Some(t) = l.synth.tls_base {
                let ta = l.atom_of(t);
                l.atoms[ta as usize].live = true;
            }
        }
    }
    if o.shared_memory
        && out
            .got
            .iter()
            .any(|&id| l.ext(id).flags & crate::wasm::TLS != 0)
    {
        let id = l.synth_func("__wasm_apply_global_tls_relocs", &FuncType::default());
        let a = l.atom_of(id);
        l.atoms[a as usize].live = true;
        if let Some(t) = l.synth.tls_base {
            let ta = l.atom_of(t);
            l.atoms[ta as usize].live = true;
        }
    }
    if o.shared_memory {
        let relocs = out.segs.iter().filter(|s| s.tls()).any(|s| {
            s.items
                .iter()
                .any(|&it| matches!(it, Item::Atom(a) if !l.atoms[a as usize].relocs.is_empty()))
        });
        if relocs {
            let id = l.synth_func("__wasm_apply_tls_relocs", &FuncType::default());
            let a = l.atom_of(id);
            l.atoms[a as usize].live = true;
            if let Chunk::Func { body, .. } = &mut l.chunks[a as usize] {
                *body = Some(func_bytes(&[0, 0x0b]));
            }
        }
    }
}

/// The live constructors of every file, by priority (stable).
pub(crate) fn init_funcs(l: &Link) -> Result<Vec<(u32, SymId)>, String> {
    let mut v = Vec::new();
    for &fi in &l.order {
        let f = &l.files[fi as usize];
        for &(prio, si) in &f.init_funcs {
            let Some(&id) = f.syms.get(si as usize) else {
                continue;
            };
            let Some((a, _)) = l.syms.resolve(Target::Sym(id)) else {
                continue;
            };
            if l.discarded.contains(&a) || !l.atoms[a as usize].live {
                continue;
            }
            if l.ext(id)
                .sig
                .is_some_and(|s| !l.sigs[s as usize].params.is_empty())
            {
                return Err(format!(
                    "constructor functions cannot take arguments: {}",
                    l.name(id)
                ));
            }
            v.push((prio, id));
        }
    }
    v.sort_by_key(|&(p, _)| p);
    Ok(v)
}

pub(crate) fn func_index(l: &Link, out: &Out, id: SymId) -> u32 {
    index_of(l, out, id).unwrap_or(0)
}

pub(crate) fn set_body(l: &mut Link, id: SymId, body: &[u8]) {
    if let Def::Defined { atom, .. } = l.syms.get(id).def {
        if let Chunk::Func { body: b, .. } = &mut l.chunks[atom as usize] {
            *b = Some(func_bytes(body));
        }
    }
}

/// The bodies of `__wasm_call_ctors`, `__wasm_init_memory` and
/// `__wasm_init_tls`.
pub(crate) fn bodies(l: &mut Link, o: &Options, out: &Out, inits: &[(u32, SymId)]) {
    let ctors = l.synth.call_ctors;
    let mut b = vec![0];
    for &(_, id) in inits {
        b.push(0x10);
        uleb(&mut b, u64::from(func_index(l, out, id)));
        let results = l
            .ext(id)
            .sig
            .map_or(0, |s| l.sigs[s as usize].results.len());
        b.extend(std::iter::repeat_n(0x1a, results));
    }
    b.push(0x0b);
    set_body(l, ctors, &b);
    if let Some(a) = out.init_memory {
        let body = init_memory_body(l, o, out);
        if let Chunk::Func { body: b, .. } = &mut l.chunks[a as usize] {
            *b = Some(func_bytes(&body));
        }
    }
    if let Some(id) = l.synth.init_tls {
        let mut b = vec![0];
        if let Some(s) = out.segs.iter().find(|s| s.tls()) {
            let tls_base = l
                .synth
                .tls_base
                .and_then(|t| index_of(l, out, t))
                .unwrap_or(0);
            b.extend([0x20, 0, 0x24]);
            uleb(&mut b, u64::from(tls_base));
            b.extend([0x20, 0]);
            i32_const(&mut b, 0);
            i32_const(&mut b, s.size as i64);
            b.extend([0xfc, 0x08]);
            uleb(&mut b, u64::from(s.index));
            b.push(0);
        }
        for n in ["__wasm_apply_tls_relocs", "__wasm_apply_global_tls_relocs"] {
            if let Some(t) = l.syms.find(n) {
                if l.defined(t) && l.live(t) {
                    b.push(0x10);
                    uleb(&mut b, u64::from(func_index(l, out, t)));
                }
            }
        }
        b.push(0x0b);
        set_body(l, id, &b);
    }
    // TLS GOT entries: `__tls_base` plus the offset, per thread.
    if let Some(t) = l.syms.find("__wasm_apply_global_tls_relocs") {
        let tls_base = l
            .synth
            .tls_base
            .and_then(|b| index_of(l, out, b))
            .unwrap_or(0);
        let mut b = vec![0];
        for &id in &out.got {
            if l.ext(id).flags & crate::wasm::TLS == 0 {
                continue;
            }
            let Def::Defined { atom, off } = l.syms.get(id).def else {
                continue;
            };
            b.push(0x23);
            uleb(&mut b, u64::from(tls_base));
            i32_const(&mut b, data_va(l, out, atom, off, true) as i64);
            b.extend([0x6a, 0x24]);
            uleb(&mut b, u64::from(got_index(out, id).unwrap_or(0)));
        }
        b.push(0x0b);
        set_body(l, t, &b);
    }
}

/// Copy the passive segments into memory (and clear `.bss`) once: the
/// first thread to swap the flag from 0 to 1 does, the others wait for 2.
pub(crate) fn init_memory_body(l: &Link, o: &Options, out: &Out) -> Vec<u8> {
    let shared = o.shared_memory;
    let emit_bss = emit_bss(o, out);
    let needs = |s: &OutSeg| s.passive || (o.import_memory && s.bss && !emit_bss);
    let flag = out.flag_addr as i64;
    let mut b = vec![0];
    let memarg = |b: &mut Vec<u8>| b.extend([2, 0]);
    if shared {
        b.extend([0x02, 0x40, 0x02, 0x40, 0x02, 0x40]);
        i32_const(&mut b, flag);
        i32_const(&mut b, 0);
        i32_const(&mut b, 1);
        b.extend([0xfe, 0x48]);
        memarg(&mut b);
        b.extend([0x0e, 2, 0, 1, 2, 0x0b]);
    }
    let tls_base = l
        .synth
        .tls_base
        .and_then(|t| index_of(l, out, t))
        .unwrap_or(0);
    for s in &out.segs {
        if !needs(s) {
            continue;
        }
        i32_const(&mut b, s.start as i64);
        if shared && s.tls() {
            i32_const(&mut b, s.start as i64);
            b.push(0x24);
            uleb(&mut b, u64::from(tls_base));
        }
        // memory.fill (value 0) or memory.init (from offset 0).
        i32_const(&mut b, 0);
        i32_const(&mut b, s.size as i64);
        if s.bss {
            b.extend([0xfc, 0x0b, 0]);
        } else {
            b.extend([0xfc, 0x08]);
            uleb(&mut b, u64::from(s.index));
            b.push(0);
        }
    }
    if shared {
        i32_const(&mut b, flag);
        i32_const(&mut b, 2);
        b.extend([0xfe, 0x17]);
        memarg(&mut b);
        i32_const(&mut b, flag);
        i32_const(&mut b, -1);
        b.extend([0xfe, 0x00]);
        memarg(&mut b);
        b.extend([0x1a, 0x0c, 1, 0x0b]);
        i32_const(&mut b, flag);
        i32_const(&mut b, 1);
        b.extend([0x42, 0x7f]);
        b.extend([0xfe, 0x01]);
        memarg(&mut b);
        b.extend([0x1a, 0x0b]);
    }
    for s in &out.segs {
        if needs(s) && !s.bss && !(shared && s.tls()) {
            b.extend([0xfc, 0x09]);
            uleb(&mut b, u64::from(s.index));
        }
    }
    b.push(0x0b);
    b
}

/// A command without a caller of `__wasm_call_ctors`: each exported
/// function is wrapped to run the constructors first and
/// `__wasm_call_dtors` after.
pub(crate) fn wrappers(l: &mut Link, out: &mut Out, inits: &[(u32, SymId)]) {
    let ctors = l.synth.call_ctors;
    let used = l.ext(ctors).in_obj || l.exported(ctors);
    if used || (inits.is_empty() && l.synth.call_dtors.is_none()) {
        return;
    }
    let ctors_live = l.live(ctors);
    let wrap: Vec<SymId> = (0..l.syms.syms.len() as SymId)
        .filter(|&id| l.exported(id) && l.ext(id).kind == Some(SYM_FUNCTION))
        .filter(|&id| matches!(l.syms.get(id).def, Def::Defined { .. }))
        .collect();
    for id in wrap {
        let inner = func_index(l, out, id);
        let (sig, ename) = match l.syms.get(id).def {
            Def::Defined { atom, .. } => match &l.chunks[atom as usize] {
                Chunk::Func {
                    sig, export_name, ..
                } => (*sig, export_name.clone()),
                _ => continue,
            },
            _ => continue,
        };
        let n = l.syms.get(id).name.clone();
        let mut b = vec![0];
        if ctors_live {
            b.push(0x10);
            uleb(&mut b, u64::from(func_index(l, out, ctors)));
        }
        for i in 0..l.sigs[sig as usize].params.len() {
            b.push(0x20);
            uleb(&mut b, i as u64);
        }
        b.push(0x10);
        uleb(&mut b, u64::from(inner));
        if let Some(d) = l.synth.call_dtors {
            b.push(0x10);
            uleb(&mut b, u64::from(func_index(l, out, d)));
        }
        b.push(0x0b);
        let w = l.synth_func(
            &format!("{n}.command_export"),
            &l.sigs[sig as usize].clone(),
        );
        let wa = l.atom_of(w);
        l.atoms[wa as usize].live = true;
        if let Chunk::Func {
            body, export_name, ..
        } = &mut l.chunks[wa as usize]
        {
            *body = Some(func_bytes(&b));
            *export_name = Some(ename.unwrap_or_else(|| n.clone()));
        }
        let flags = l.ext(id).flags;
        let force = l.ext(id).force_export;
        let x = l.ext_mut(w);
        x.flags = (flags | crate::wasm::EXPORTED) & !HIDDEN;
        x.force_export = force;
        let x = l.ext_mut(id);
        x.flags = (x.flags | HIDDEN) & !crate::wasm::EXPORTED;
        x.force_export = false;
        out.index.resize(l.atoms.len(), NONE);
        out.index[wa as usize] = out.imported[0] + out.funcs.len() as u32;
        out.funcs.push(wa);
    }
}
