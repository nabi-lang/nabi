//! Imports, the indirect function table and the index spaces.

#[allow(clippy::wildcard_imports)]
use crate::wasm::write::*;

/// `__indirect_function_table`: the linker defines it when code uses it.
pub(crate) fn function_table(l: &mut Link, out: &mut Out) -> Result<(), String> {
    let Some(id) = l.syms.find(FUNCTION_TABLE) else {
        return Ok(());
    };
    if l.ext(id).kind.is_some_and(|k| k != SYM_TABLE) {
        return Err(format!(
            "reserved symbol must be of type table: `{FUNCTION_TABLE}`"
        ));
    }
    if l.defined(id) {
        return Err(format!(
            "reserved symbol must not be defined in input files: `{FUNCTION_TABLE}`"
        ));
    }
    if !l.ext(id).live {
        return Ok(());
    }
    define_table(l, out, id);
    Ok(())
}

pub(crate) fn define_table(l: &mut Link, out: &mut Out, id: SymId) {
    let ty = TableType {
        elem: 0x70,
        limits: Limits::default(),
    };
    let mut a = Atom::new(u32::MAX, 0, 0, 0, 0);
    a.live = true;
    let at = l.push(
        a,
        Chunk::Table {
            ty,
            name: FUNCTION_TABLE.to_owned(),
        },
    );
    let s = l.syms.get_mut(id);
    s.def = Def::Defined { atom: at, off: 0 };
    s.file = u32::MAX;
    let x = l.ext_mut(id);
    x.kind = Some(SYM_TABLE);
    x.flags = HIDDEN;
    x.tty = Some(ty);
    out.table_sym = Some(id);
}

/// An import's identity: kind, module, name and type.
pub(crate) type ImportKey = (
    u8,
    String,
    String,
    u32,
    Option<GlobalType>,
    Option<TableType>,
);

/// The imports: the memory, then each live undefined symbol with an
/// explicit import name, in symbol table order, once per (module, name,
/// type).
pub(crate) fn imports(l: &Link, out: &mut Out) -> Result<(), String> {
    let mut seen: HashMap<ImportKey, u32> = HashMap::default();
    for id in 0..l.syms.syms.len() as SymId {
        let s = l.syms.get(id);
        if s.name.starts_with('\0') || !matches!(s.def, Def::Undefined | Def::Lazy { .. }) {
            continue;
        }
        let x = l.ext(id);
        let Some(kind) = x.kind else { continue };
        if kind == SYM_DATA || !x.live || !x.in_obj || s.weak_ref || x.field.is_none() {
            continue;
        }
        let module = x.module.clone().unwrap_or_else(|| "env".to_owned());
        let field = x.field.clone().unwrap_or_default();
        let (sig, gty, tty) = (x.sig.unwrap_or(NONE), x.gty, x.tty);
        let k = match kind {
            SYM_FUNCTION => 0,
            SYM_TABLE => 1,
            SYM_GLOBAL => 3,
            _ => 4,
        };
        if k == 0 && sig == NONE {
            return Err(format!("imported function {} has no signature", s.name));
        }
        let key = (kind, module.clone(), field.clone(), sig, gty, tty);
        let idx = if let Some(&i) = seen.get(&key) {
            i
        } else {
            let i = out.imported[k];
            out.imported[k] += 1;
            seen.insert(key, i);
            out.imports.push(Import {
                module,
                field,
                kind: k as u8,
                sig,
                gty,
                tty,
                sym: id,
            });
            i
        };
        out.import_index.insert(id, idx);
    }
    Ok(())
}

/// Function, global, tag and table indexes: imports, the linker's, then
/// each file's.
pub(crate) fn indexes(l: &Link, out: &mut Out) {
    out.index = vec![NONE; l.atoms.len()];
    let order: Vec<usize> = l.order.iter().map(|&f| f as usize).collect();
    let live = |a: &AtomId| l.atoms[*a as usize].live;
    let mut funcs: Vec<AtomId> = l.synth_funcs.iter().copied().filter(live).collect();
    let mut globals: Vec<AtomId> = l.synth_globals.iter().copied().filter(live).collect();
    let mut tags = Vec::new();
    let mut tables = Vec::new();
    for &fi in &order {
        let f = &l.files[fi];
        funcs.extend(f.funcs.iter().copied().filter(live));
        globals.extend(f.globals.iter().copied().filter(live));
        tags.extend(f.tags.iter().copied().filter(live));
        tables.extend(f.tables.iter().copied().filter(live));
    }
    if let Some(t) = out.table_sym {
        let a = l.atom_of(t);
        // An MVP object's `call_indirect` uses table 0.
        if l.ext(t).pinned {
            tables.insert(0, a);
        } else {
            tables.push(a);
        }
    }
    for (base, list) in [
        (out.imported[0], &funcs),
        (out.imported[3], &globals),
        (out.imported[4], &tags),
        (out.imported[1], &tables),
    ] {
        for (i, &a) in list.iter().enumerate() {
            out.index[a as usize] = base + i as u32;
        }
    }
    out.funcs = funcs;
    out.globals = globals;
    out.tags = tags;
    out.tables = tables;
}

/// The output index of what symbol `id` names (function, global, table
/// or tag).
pub(crate) fn index_of(l: &Link, out: &Out, id: SymId) -> Option<u32> {
    match l.syms.get(id).def {
        Def::Defined { atom, .. } => Some(out.index[atom as usize]).filter(|&i| i != NONE),
        _ => out.import_index.get(&id).copied(),
    }
}

/// The function symbol `id` names, for its table slot.
pub(crate) fn fn_key(l: &Link, out: &Out, id: SymId) -> Option<FnKey> {
    if l.ext(id).kind != Some(SYM_FUNCTION) || l.ext(id).stub {
        return None;
    }
    match l.syms.get(id).def {
        Def::Defined { atom, .. } => Some(FnKey::Atom(atom)),
        _ => out.import_index.contains_key(&id).then_some(FnKey::Sym(id)),
    }
}

/// The global index of symbol `id`'s GOT entry.
pub(crate) fn got_index(out: &Out, id: SymId) -> Option<u32> {
    let k = *out.got_of.get(&id)?;
    Some(out.imported[3] + out.globals.len() as u32 + k)
}

pub(crate) fn add_slot(out: &mut Out, k: FnKey) {
    let next = 1 + out.elems.len() as u32;
    if let std::collections::hash_map::Entry::Vacant(e) = out.slot.entry(k) {
        e.insert(next);
        out.elems.push(k);
    }
}

pub(crate) fn register_type(out: &mut Out, sig: u32) -> u32 {
    if let Some(&t) = out.type_of.get(&sig) {
        return t;
    }
    let t = out.types.len() as u32;
    out.types.push(sig);
    out.type_of.insert(sig, t);
    t
}

/// Relocations of the live code and data, in file order: types used by
/// `call_indirect`, table slots of address-taken functions, GOT entries,
/// undefined symbols, TLS misuse.
pub(crate) fn scan(l: &mut Link, o: &Options, out: &mut Out) -> Result<(), String> {
    let mut errs: Vec<String> = Vec::new();
    let mut seen = nabic_core::HashSet::default();
    for k in 0..l.order.len() {
        let fi = l.order[k];
        let f = &l.files[fi as usize];
        for &a in f.funcs.iter().chain(&f.segs) {
            let at = &l.atoms[a as usize];
            if !at.live {
                continue;
            }
            for r in &at.relocs {
                if r.kind == R_TYPE_INDEX_LEB {
                    register_type(out, f.types[r.addend as usize]);
                    continue;
                }
                let Target::Sym(id) = r.target else { continue };
                let defined = l.defined(id);
                if is_table_index(r.kind) {
                    if let Some(k) = fn_key(l, out, id) {
                        add_slot(out, k);
                    }
                }
                if matches!(r.kind, R_GLOBAL_INDEX_LEB | R_GLOBAL_INDEX_I32)
                    && l.ext(id).kind != Some(SYM_GLOBAL)
                    && !out.got_of.contains_key(&id)
                {
                    out.got_of.insert(id, out.got.len() as u32);
                    out.got.push(id);
                    if let Some(k) = fn_key(l, out, id) {
                        add_slot(out, k);
                    }
                }
                if matches!(r.kind, R_MEMORY_ADDR_TLS_SLEB | R_MEMORY_ADDR_TLS_SLEB64) {
                    if !defined {
                        errs.push(format!(
                            "{}: relocation R_WASM_MEMORY_ADDR_TLS_SLEB cannot be used against \
                             an undefined symbol `{}`",
                            f.name,
                            l.name(id)
                        ));
                    } else if o.shared_memory && l.ext(id).flags & crate::wasm::TLS == 0 {
                        errs.push(format!(
                            "{}: relocation R_WASM_MEMORY_ADDR_TLS_SLEB cannot be used against \
                             non-TLS symbol `{}`",
                            f.name,
                            l.name(id)
                        ));
                    }
                }
                let s = l.syms.get(id);
                if defined || s.weak_ref || l.ext(id).field.is_some() {
                    continue;
                }
                if seen.insert((fi, id)) {
                    errs.push(format!(
                        "{}: undefined symbol: {}",
                        f.name,
                        demangle(l.name(id))
                    ));
                }
            }
        }
    }
    if !errs.is_empty() {
        if errs.len() > 20 {
            let n = errs.len() - 20;
            errs.truncate(20);
            errs.push(format!("too many errors emitted ({n} more)"));
        }
        return Err(errs.join("\n"));
    }
    // Table slots need the indirect function table.
    if out.table_sym.is_none() && !out.elems.is_empty() {
        let id = l.syms.intern(FUNCTION_TABLE);
        define_table(l, out, id);
    }
    Ok(())
}
