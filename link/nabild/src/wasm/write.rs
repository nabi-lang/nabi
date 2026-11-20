//! Layout and the module: data segments in linear memory, the index
//! spaces, the indirect function table, the functions the linker makes
//! and every section, in wasm-ld's order.

mod emit;
mod indexes;
mod memory;
mod names;
mod relocate;
mod synthetic;

pub(crate) use crate::wasm::write::emit::*;
pub(crate) use crate::wasm::write::indexes::*;
pub(crate) use crate::wasm::write::memory::*;
pub(crate) use crate::wasm::write::names::*;
pub(crate) use crate::wasm::write::relocate::*;
pub(crate) use crate::wasm::write::synthetic::*;

use crate::symtab::{Atom, AtomId, Def, Reloc, SymId, Target};
use crate::wasm::{
    ABSOLUTE, Chunk, FUNCTION_TABLE, FuncType, GlobalType, HIDDEN, Limits, Link, Options, PAGE,
    SEG_TLS, SYM_DATA, SYM_FUNCTION, SYM_GLOBAL, SYM_SECTION, SYM_TABLE, SYM_TAG, TableType,
};
use crate::wasm::{
    R_FUNCTION_INDEX_I32, R_FUNCTION_INDEX_LEB, R_FUNCTION_OFFSET_I32, R_FUNCTION_OFFSET_I64,
    R_GLOBAL_INDEX_I32, R_GLOBAL_INDEX_LEB, R_MEMORY_ADDR_I32, R_MEMORY_ADDR_I64,
    R_MEMORY_ADDR_LEB, R_MEMORY_ADDR_LEB64, R_MEMORY_ADDR_LOCREL_I32, R_MEMORY_ADDR_REL_SLEB,
    R_MEMORY_ADDR_REL_SLEB64, R_MEMORY_ADDR_SLEB, R_MEMORY_ADDR_SLEB64, R_MEMORY_ADDR_TLS_SLEB,
    R_MEMORY_ADDR_TLS_SLEB64, R_SECTION_OFFSET_I32, R_TABLE_INDEX_I32, R_TABLE_INDEX_I64,
    R_TABLE_INDEX_REL_SLEB, R_TABLE_INDEX_REL_SLEB64, R_TABLE_INDEX_SLEB, R_TABLE_INDEX_SLEB64,
    R_TABLE_NUMBER_LEB, R_TAG_INDEX_LEB, R_TYPE_INDEX_LEB, is_table_index,
};
use nabic_core::HashMap;
use nabic_core::leb::{put_sleb, put_uleb, read_uleb, sleb, uleb};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const NONE: u32 = u32::MAX;
const STACK_ALIGN: u64 = 16;
const I32: u8 = 0x7f;

/// An output data segment.
pub(crate) struct OutSeg {
    name: String,
    items: Vec<Item>,
    /// log2.
    align: u8,
    size: u64,
    start: u64,
    bss: bool,
    passive: bool,
    index: u32,
}

impl OutSeg {
    fn tls(&self) -> bool {
        self.name == ".tdata"
    }
}

/// An input chunk of an output segment or custom section, or a group of
/// string chunks merged into one.
#[derive(Clone, Copy)]
enum Item {
    Atom(AtomId),
    Merged(usize),
}

/// Strings of several chunks, deduplicated and tail merged.
struct Merged {
    atoms: Vec<AtomId>,
    bytes: Vec<u8>,
    /// Offset in the segment or custom section.
    off: u64,
}

/// A custom section of the output.
struct OutCustom {
    name: String,
    items: Vec<Item>,
    size: u64,
}

/// An import of the output.
struct Import {
    module: String,
    field: String,
    kind: u8,
    sig: u32,
    gty: Option<GlobalType>,
    tty: Option<TableType>,
    sym: SymId,
}

/// A function of the output: defined (its chunk) or imported (its
/// symbol).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) enum FnKey {
    Atom(AtomId),
    Sym(SymId),
}

/// Everything layout assigns.
pub(crate) struct Out {
    segs: Vec<OutSeg>,
    merged: Vec<Merged>,
    /// Merged chunk: its group and pieces.
    pieces: HashMap<AtomId, (usize, Pieces)>,
    /// Data chunk: its output segment.
    seg_of: HashMap<AtomId, usize>,
    imports: Vec<Import>,
    /// Imported symbol: its index (function, global, table or tag).
    import_index: HashMap<SymId, u32>,
    imported: [u32; 6],
    /// Per atom: function, global, table or tag index.
    index: Vec<u32>,
    funcs: Vec<AtomId>,
    globals: Vec<AtomId>,
    tables: Vec<AtomId>,
    tags: Vec<AtomId>,
    types: Vec<u32>,
    type_of: HashMap<u32, u32>,
    /// Function: its table slot.
    slot: HashMap<FnKey, u32>,
    elems: Vec<FnKey>,
    /// Symbols of GOT relocations: an immutable global each, after the
    /// others.
    got: Vec<SymId>,
    got_of: HashMap<SymId, u32>,
    /// The indirect function table's symbol, when there is one.
    table_sym: Option<SymId>,
    /// Per function atom: offset in the code section's payload.
    code_off: Vec<u32>,
    /// Custom section chunk: its offset in the output section.
    custom_off: HashMap<AtomId, u64>,
    customs: Vec<OutCustom>,
    tls_start: u64,
    init_memory: Option<AtomId>,
    flag_addr: u64,
    pages: u64,
    max_pages: u64,
    features: Vec<String>,
    /// Exported data symbols: address globals after the others.
    data_globals: Vec<SymId>,
}

/// A relocated LEB128 field: 5 padded bytes, cut at the end of `b`.
fn put_uleb5(b: &mut [u8], v: u32) {
    let mut x = [0; 5];
    put_uleb(&mut x, u64::from(v));
    let n = b.len().min(5);
    b[..n].copy_from_slice(&x[..n]);
}

fn put_sleb5(b: &mut [u8], v: i32) {
    let mut x = [0; 5];
    put_sleb(&mut x, i64::from(v));
    let n = b.len().min(5);
    b[..n].copy_from_slice(&x[..n]);
}

fn name(b: &mut Vec<u8>, s: &str) {
    uleb(b, s.len() as u64);
    b.extend_from_slice(s.as_bytes());
}

fn i32_const(b: &mut Vec<u8>, v: i64) {
    b.push(0x41);
    sleb(b, i64::from(v as i32));
}

/// A function's size and body.
fn func_bytes(body: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(body.len() + 5);
    uleb(&mut b, body.len() as u64);
    b.extend_from_slice(body);
    b
}

/// Lay out and write `o.out`; the warnings, one per line.
pub fn write(l: &mut Link, o: &Options) -> Result<String, String> {
    let mut out = Out {
        segs: Vec::new(),
        merged: Vec::new(),
        pieces: HashMap::default(),
        seg_of: HashMap::default(),
        imports: Vec::new(),
        import_index: HashMap::default(),
        imported: [0; 6],
        index: Vec::new(),
        funcs: Vec::new(),
        globals: Vec::new(),
        tables: Vec::new(),
        tags: Vec::new(),
        types: Vec::new(),
        type_of: HashMap::default(),
        slot: HashMap::default(),
        elems: Vec::new(),
        got: Vec::new(),
        got_of: HashMap::default(),
        table_sym: None,
        code_off: Vec::new(),
        custom_off: HashMap::default(),
        customs: Vec::new(),
        tls_start: 0,
        init_memory: None,
        flag_addr: 0,
        pages: 0,
        max_pages: 0,
        features: Vec::new(),
        data_globals: Vec::new(),
    };
    function_table(l, &mut out)?;
    segments(l, &mut out)?;
    features(l, o, &mut out)?;
    layout(l, o, &mut out)?;
    for s in &out.segs {
        if !c_ident(&s.name) {
            continue;
        }
        for (pre, v) in [("__start_", s.start), ("__stop_", s.start + s.size)] {
            if let Some(id) = l.syms.find(&format!("{pre}{}", s.name)) {
                if !l.defined(id) {
                    l.syms.get_mut(id).def = Def::Absolute(v);
                    let x = l.ext_mut(id);
                    x.kind = Some(SYM_DATA);
                    x.flags = HIDDEN | ABSOLUTE;
                    x.live = true;
                }
            }
        }
    }
    for e in &o.exports {
        if !l.syms.find(e).is_some_and(|id| l.defined(id)) {
            return Err(format!("symbol exported via --export not found: {e}"));
        }
    }
    imports(l, &mut out)?;
    scan(l, o, &mut out)?;
    init_memory(l, o, &mut out);
    indexes(l, &mut out);
    let inits = init_funcs(l)?;
    bodies(l, o, &out, &inits);
    wrappers(l, &mut out, &inits);
    types(l, &mut out);
    let exports = exports(l, o, &mut out)?;
    customs(l, o, &mut out)?;
    let bytes = emit(l, o, &mut out, &exports)?;
    let _ = std::fs::remove_file(&o.out);
    std::fs::write(&o.out, bytes).map_err(|e| format!("cannot write {}: {e}", o.out.display()))?;
    #[cfg(unix)]
    {
        let _ = std::fs::set_permissions(&o.out, std::fs::Permissions::from_mode(0o755));
    }
    Ok(crate::wasm::warnings(l))
}

/// The output's target features: those the inputs use, checked against
/// what they disallow and what shared memory and TLS need.
fn features(l: &Link, o: &Options, out: &mut Out) -> Result<(), String> {
    let mut used: HashMap<&str, &str> = HashMap::default();
    let mut disallowed: HashMap<&str, &str> = HashMap::default();
    let mut tls = false;
    for &fi in &l.order {
        let f = &l.files[fi as usize];
        for (p, n) in &f.features {
            match p {
                b'+' => {
                    used.entry(n).or_insert(&f.name);
                }
                b'-' => {
                    disallowed.entry(n).or_insert(&f.name);
                }
                p => return Err(format!("Unrecognized feature policy prefix {p}")),
            }
        }
        tls |= f.segs.iter().any(|&a| {
            l.atoms[a as usize].live
                && matches!(&l.chunks[a as usize], Chunk::Data { flags, .. } if flags & SEG_TLS != 0)
        });
    }
    let mut errs = Vec::new();
    if o.shared_memory {
        if let Some(f) = disallowed.get("shared-mem") {
            errs.push(format!(
                "--shared-memory is disallowed by {f} because it was not compiled with \
                 'atomics' or 'bulk-memory' features."
            ));
        }
        for f in ["atomics", "bulk-memory"] {
            if !used.contains_key(f) {
                errs.push(format!(
                    "'{f}' feature must be used in order to use shared memory"
                ));
            }
        }
    }
    if tls {
        for f in ["atomics", "bulk-memory"] {
            if !used.contains_key(f) {
                errs.push(format!(
                    "'{f}' feature must be used in order to use thread-local storage"
                ));
            }
        }
    }
    for &fi in &l.order {
        let f = &l.files[fi as usize];
        for (p, n) in &f.features {
            if *p == b'+' {
                if let Some(by) = disallowed.get(n.as_str()) {
                    errs.push(format!(
                        "Target feature '{n}' used in {} is disallowed by {by}. Use \
                         --no-check-features to suppress.",
                        f.name
                    ));
                }
            }
        }
    }
    if !errs.is_empty() {
        return Err(errs.join("\n"));
    }
    let mut v: Vec<String> = used.keys().map(|&s| s.to_owned()).collect();
    v.sort();
    out.features = v;
    Ok(())
}

fn set_global(l: &mut Link, id: SymId, v: u64) {
    if let Def::Defined { atom, .. } = l.syms.get(id).def {
        if let Chunk::Global { init, .. } = &mut l.chunks[atom as usize] {
            init.clear();
            i32_const(init, v as i64);
            init.push(0x0b);
        }
    }
}
