//! Relocatable wasm objects into atoms.
//!
//! The tool conventions' `linking` and `reloc.*` sections: one atom per
//! function, data segment, global, table, tag and custom section. COMDAT
//! groups after the first of a name are discarded.

use crate::symtab::{Atom, AtomId, Def, Reloc, SymId, Target};
use crate::wasm::{
    Chunk, DEAD, EXPLICIT_NAME, FuncType, GlobalType, LOCAL, Limits, Link, NO_STRIP, ObjFile,
    R_FUNCTION_INDEX_LEB, R_TYPE_INDEX_LEB, SEG_STRINGS, SEG_TLS, SYM_DATA, SYM_FUNCTION,
    SYM_GLOBAL, SYM_SECTION, SYM_TABLE, SYM_TAG, TLS, TableType, UNDEFINED, has_addend,
};
use nabic_core::HashMap;
use nabic_core::leb::{read_sleb, read_uleb};

pub const MAGIC: &[u8; 8] = b"\0asm\x01\0\0\0";

const SEC_CUSTOM: u8 = 0;
const SEC_TYPE: u8 = 1;
const SEC_IMPORT: u8 = 2;
const SEC_FUNCTION: u8 = 3;
const SEC_TABLE: u8 = 4;
const SEC_MEMORY: u8 = 5;
const SEC_GLOBAL: u8 = 6;
const SEC_EXPORT: u8 = 7;
const SEC_START: u8 = 8;
const SEC_ELEM: u8 = 9;
const SEC_CODE: u8 = 10;
const SEC_DATA: u8 = 11;
const SEC_DATACOUNT: u8 = 12;
const SEC_TAG: u8 = 13;

const LINK_SEGMENT_INFO: u8 = 5;
const LINK_INIT_FUNCS: u8 = 6;
const LINK_COMDAT_INFO: u8 = 7;
const LINK_SYMBOL_TABLE: u8 = 8;

const COMDAT_DATA: u8 = 0;
const COMDAT_FUNCTION: u8 = 1;
const COMDAT_SECTION: u8 = 5;

/// A bounded reader of the object's bytes.
pub struct Rd<'a> {
    pub b: &'a [u8],
    pub at: usize,
    pub end: usize,
}

impl<'a> Rd<'a> {
    pub const fn new(b: &'a [u8], at: usize, end: usize) -> Self {
        Self { b, at, end }
    }

    pub const fn done(&self) -> bool {
        self.at >= self.end
    }

    pub fn u8(&mut self) -> Result<u8, String> {
        if self.at >= self.end {
            return Err("truncated".to_owned());
        }
        self.at += 1;
        Ok(self.b[self.at - 1])
    }

    pub fn uleb(&mut self) -> Result<u64, String> {
        let v = read_uleb(&self.b[..self.end], &mut self.at).ok_or("truncated")?;
        Ok(v)
    }

    pub fn u32(&mut self) -> Result<u32, String> {
        u32::try_from(self.uleb()?).map_err(|_| "integer too large".to_owned())
    }

    pub fn sleb(&mut self) -> Result<i64, String> {
        Ok(read_sleb(&self.b[..self.end], &mut self.at).ok_or("truncated")?)
    }

    pub fn skip(&mut self, n: usize) -> Result<usize, String> {
        let at = self.at;
        if at + n > self.end {
            return Err("truncated".to_owned());
        }
        self.at += n;
        Ok(at)
    }

    pub fn str(&mut self) -> Result<&'a str, String> {
        let n = self.u32()? as usize;
        let at = self.skip(n)?;
        std::str::from_utf8(&self.b[at..at + n]).map_err(|_| "a name is not UTF-8".to_owned())
    }

    fn valtype(&mut self) -> Result<u8, String> {
        let t = self.u8()?;
        match t {
            // i32 i64 f32 f64 v128, funcref externref and the other
            // abstract references.
            0x7f | 0x7e | 0x7d | 0x7c | 0x7b | 0x69..=0x74 => Ok(t),
            _ => Err(format!("value type {t:#x} is not supported")),
        }
    }

    fn limits(&mut self) -> Result<Limits, String> {
        let flags = self.u8()?;
        let min = self.uleb()?;
        let max = if flags & 1 != 0 { self.uleb()? } else { 0 };
        if flags & 8 != 0 {
            return Err("custom page sizes are not supported".to_owned());
        }
        Ok(Limits { flags, min, max })
    }

    fn table_type(&mut self) -> Result<TableType, String> {
        let elem = self.valtype()?;
        Ok(TableType {
            elem,
            limits: self.limits()?,
        })
    }

    fn global_type(&mut self) -> Result<GlobalType, String> {
        let ty = self.valtype()?;
        let m = self.u8()?;
        Ok(GlobalType {
            ty,
            mutable: m == 1,
        })
    }

    /// A constant expression up to and with its `end`.
    fn init_expr(&mut self) -> Result<(usize, usize), String> {
        let start = self.at;
        loop {
            match self.u8()? {
                0x0b => return Ok((start, self.at)),
                0x41 | 0x42 => {
                    self.sleb()?;
                }
                0x43 => {
                    self.skip(4)?;
                }
                0x44 => {
                    self.skip(8)?;
                }
                0x23 | 0xd2 => {
                    self.uleb()?;
                }
                0xd0 => {
                    self.sleb()?;
                }
                0xfd => {
                    self.uleb()?;
                    self.skip(16)?;
                }
                0x6a..=0x6c | 0x7c..=0x7e => {}
                op => return Err(format!("opcode {op:#x} in a constant expression")),
            }
        }
    }
}

/// A function, table, global or tag import.
struct Import {
    module: String,
    field: String,
}

/// One entry of the `linking` symbol table.
struct SymInfo {
    kind: u8,
    flags: u32,
    name: Option<String>,
    index: u32,
    off: u64,
}

/// A relocation as read: type, offset in its section, symbol (or type)
/// index, addend.
#[derive(Clone, Copy)]
struct RawReloc {
    ty: u8,
    off: u32,
    idx: u32,
    addend: i64,
}

/// A section: id, content range (after a custom section's name), name.
struct Sect {
    id: u8,
    start: usize,
    end: usize,
    name: String,
}

fn sections(b: &[u8]) -> Result<Vec<Sect>, String> {
    if !b.starts_with(MAGIC) {
        return Err("not a wasm object".to_owned());
    }
    let mut out = Vec::new();
    let mut r = Rd::new(b, 8, b.len());
    while !r.done() {
        let id = r.u8()?;
        let size = r.u32()? as usize;
        let start = r.skip(size)?;
        let end = start + size;
        let (name, cstart) = if id == SEC_CUSTOM {
            let mut n = Rd::new(b, start, end);
            (n.str()?.to_owned(), n.at)
        } else {
            (String::new(), start)
        };
        out.push(Sect {
            id,
            start: cstart,
            end,
            name,
        });
    }
    Ok(out)
}

/// The symbols `b` defines for the link (not local, not undefined), in
/// symbol table order: what an archive member offers.
pub fn defined_symbols(b: &[u8]) -> Result<Vec<String>, String> {
    let secs = sections(b)?;
    let Some(link) = secs
        .iter()
        .find(|s| s.id == SEC_CUSTOM && s.name == "linking")
    else {
        return Ok(Vec::new());
    };
    let infos = symbol_table(b, link, &[])?;
    Ok(infos
        .into_iter()
        .filter(|s| s.flags & (UNDEFINED | LOCAL) == 0 && s.kind != SYM_SECTION)
        .filter_map(|s| s.name)
        .collect())
}

/// The `linking` section's symbol table; undefined imports without an
/// explicit name get theirs from `import_names` (kind, field) when given.
fn symbol_table(
    b: &[u8],
    link: &Sect,
    import_names: &[Vec<String>],
) -> Result<Vec<SymInfo>, String> {
    let mut r = Rd::new(b, link.start, link.end);
    let version = r.u32()?;
    if version != 2 {
        return Err(format!(
            "linking section version {version} is not supported"
        ));
    }
    while !r.done() {
        let ty = r.u8()?;
        let size = r.u32()? as usize;
        let at = r.skip(size)?;
        if ty != LINK_SYMBOL_TABLE {
            continue;
        }
        let mut s = Rd::new(b, at, at + size);
        let n = s.u32()?;
        // No more entries than bytes left.
        let mut out = Vec::with_capacity((n as usize).min(size));
        for _ in 0..n {
            let kind = s.u8()?;
            let flags = s.u32()?;
            let undefined = flags & UNDEFINED != 0;
            let mut info = SymInfo {
                kind,
                flags,
                name: None,
                index: 0,
                off: 0,
            };
            match kind {
                SYM_FUNCTION | SYM_GLOBAL | SYM_TAG | SYM_TABLE => {
                    info.index = s.u32()?;
                    if !undefined || flags & EXPLICIT_NAME != 0 {
                        info.name = Some(s.str()?.to_owned());
                    } else {
                        let k = match kind {
                            SYM_FUNCTION => 0,
                            SYM_TABLE => 1,
                            SYM_GLOBAL => 3,
                            _ => 4,
                        };
                        info.name = import_names
                            .get(k)
                            .and_then(|v| v.get(info.index as usize))
                            .cloned();
                    }
                }
                SYM_DATA => {
                    info.name = Some(s.str()?.to_owned());
                    if !undefined {
                        info.index = s.u32()?;
                        info.off = s.uleb()?;
                        s.uleb()?;
                    }
                }
                SYM_SECTION => info.index = s.u32()?,
                k => return Err(format!("symbol kind {k} is not supported")),
            }
            out.push(info);
        }
        return Ok(out);
    }
    Ok(Vec::new())
}

/// Parse object `data` (file `fi`, `name` in messages) into `l`.
///
/// Its symbols are defined and referenced as wasm-ld does; archive
/// members it needs load on the way. `explicit`: from the command line,
/// not an archive.
pub fn parse(
    l: &mut Link,
    fi: u32,
    name: String,
    data: Vec<u8>,
    explicit: bool,
) -> Result<(), String> {
    l.files.push(ObjFile {
        name: name.clone(),
        explicit,
        ..ObjFile::default()
    });
    let mut f = read(l, fi, &data).map_err(|e| format!("{name}: {e}"))?;
    let syms = std::mem::take(&mut f.infos);
    let pending = std::mem::take(&mut f.relocs);
    let mut ids: Vec<SymId> = Vec::with_capacity(syms.len());
    let mut called = vec![false; syms.len()];
    for r in pending.iter().flat_map(|(_, v)| v) {
        if r.ty == R_FUNCTION_INDEX_LEB as u8 {
            if let Some(c) = called.get_mut(r.idx as usize) {
                *c = true;
            }
        }
    }
    let mut table_syms = 0;
    for (i, s) in syms.iter().enumerate() {
        if s.kind == SYM_TABLE {
            table_syms += 1;
        }
        let id = symbol(l, fi, &f, i, s, called[i])?;
        ids.push(id);
    }
    if !l.errors.is_empty() {
        return Err(std::mem::take(&mut l.errors).join("\n"));
    }
    // An MVP object: its one table import is the indirect function table,
    // used by `call_indirect` without relocations.
    let tables = f.tables_imported + f.tables.len();
    if tables != table_syms {
        if table_syms != 0 || !f.tables.is_empty() || tables != 1 || !f.table_import_ok {
            return Err(format!(
                "{name}: {tables} table(s) but {table_syms} table symbol(s): build with \
                 -mattr=+call-indirect-overlong or +reference-types"
            ));
        }
        let id = l.syms.intern(crate::wasm::FUNCTION_TABLE);
        l.syms.reference(id, fi, false);
        let x = l.ext_mut(id);
        x.kind.get_or_insert(SYM_TABLE);
        x.flags |= NO_STRIP;
        x.in_obj = true;
        x.pinned = true;
    }
    // Relocations into the atoms, against the symbols.
    let target = |r: &RawReloc| -> Result<(Target, i64), String> {
        if r.ty == R_TYPE_INDEX_LEB as u8 {
            if r.idx as usize >= f.types.len() {
                return Err(format!("{name}: relocation against type {}", r.idx));
            }
            return Ok((Target::Atom(DEAD), i64::from(r.idx)));
        }
        let id = *ids
            .get(r.idx as usize)
            .ok_or_else(|| format!("{name}: relocation against symbol {}", r.idx))?;
        Ok((Target::Sym(id), r.addend))
    };
    let mk = |r: &RawReloc, base: u32| -> Result<Reloc, String> {
        let (target, addend) = target(r)?;
        Ok(Reloc {
            off: r.off - base,
            kind: u16::from(r.ty),
            len: 0,
            pcrel: false,
            target,
            addend,
            sub: None,
        })
    };
    for (si, rs) in &pending {
        let s = &f.sects[*si as usize];
        let base = s.start;
        if s.id == SEC_CODE || s.id == SEC_DATA {
            let chunks = if s.id == SEC_CODE { &f.funcs } else { &f.segs };
            let mut k = 0;
            for r in rs {
                let at = base + r.off as usize;
                while k < chunks.len() {
                    let a = &l.atoms[chunks[k] as usize];
                    if at < a.off as usize + a.size as usize {
                        break;
                    }
                    k += 1;
                }
                let a = chunks
                    .get(k)
                    .copied()
                    .filter(|&a| at >= l.atoms[a as usize].off as usize);
                let Some(a) = a else {
                    return Err(format!(
                        "{name}: relocation at {:#x} outside any chunk",
                        r.off
                    ));
                };
                let start = (l.atoms[a as usize].off as usize - base) as u32;
                let rel = mk(r, start)?;
                l.atoms[a as usize].relocs.push(rel);
            }
        } else if s.id == SEC_CUSTOM {
            let Some(&a) = f.custom_of.get(si) else {
                continue;
            };
            for r in rs {
                let rel = mk(r, 0)?;
                l.atoms[a as usize].relocs.push(rel);
            }
        } else {
            return Err(format!("{name}: relocations in section {}", s.id));
        }
    }
    let file = &mut l.files[fi as usize];
    file.types = f.types;
    file.syms = ids;
    file.funcs = f.funcs;
    file.segs = f.segs;
    file.globals = f.globals;
    file.tables = f.tables;
    file.tags = f.tags;
    file.customs = f.customs;
    file.init_funcs = f.init_funcs;
    file.features = f.features;
    file.producers = f.producers;
    file.data = data;
    l.order.push(fi);
    Ok(())
}

/// What [`read`] gathers before the symbols are entered.
#[derive(Default)]
struct Parsed {
    sects: Vec<Sect>,
    /// File type index: link signature.
    types: Vec<u32>,
    /// Function index: signature.
    func_sigs: Vec<u32>,
    func_imports: Vec<Import>,
    global_imports: Vec<(Import, GlobalType)>,
    table_imports: Vec<(Import, TableType)>,
    tag_imports: Vec<(Import, u32)>,
    tables_imported: usize,
    table_import_ok: bool,
    funcs: Vec<AtomId>,
    segs: Vec<AtomId>,
    globals: Vec<AtomId>,
    tables: Vec<AtomId>,
    tags: Vec<AtomId>,
    customs: Vec<AtomId>,
    /// Section index of a custom section: its atom.
    custom_of: HashMap<u32, AtomId>,
    infos: Vec<SymInfo>,
    relocs: Vec<(u32, Vec<RawReloc>)>,
    init_funcs: Vec<(u32, u32)>,
    features: Vec<(u8, String)>,
    producers: [Vec<(String, String)>; 3],
}

/// Sections, imports and chunks of `b` into atoms of `l`; symbols and
/// relocations are left for [`parse`].
fn read(l: &mut Link, fi: u32, b: &[u8]) -> Result<Parsed, String> {
    let mut p = Parsed {
        sects: sections(b)?,
        ..Parsed::default()
    };
    let mut file_types: Vec<FuncType> = Vec::new();
    let mut func_types: Vec<u32> = Vec::new();
    let mut table_types: Vec<TableType> = Vec::new();
    let mut globals: Vec<(GlobalType, usize, usize)> = Vec::new();
    let mut tag_types: Vec<u32> = Vec::new();
    let mut exports: HashMap<u32, String> = HashMap::default();
    let mut code: Vec<(usize, usize)> = Vec::new();
    let mut data: Vec<(usize, usize)> = Vec::new();
    let mut debug_names: HashMap<u32, String> = HashMap::default();
    let mut seg_info: Vec<(String, u32, u32)> = Vec::new();
    let mut comdats: Vec<(String, Vec<(u8, u32)>)> = Vec::new();
    let mut link_sect = None;
    for (si, s) in p.sects.iter().enumerate() {
        let mut r = Rd::new(b, s.start, s.end);
        match s.id {
            SEC_TYPE => {
                for _ in 0..r.u32()? {
                    let form = r.u8()?;
                    if form != 0x60 {
                        return Err(format!("type form {form:#x} is not supported"));
                    }
                    let mut params = Vec::new();
                    for _ in 0..r.u32()? {
                        params.push(r.valtype()?);
                    }
                    let mut results = Vec::new();
                    for _ in 0..r.u32()? {
                        results.push(r.valtype()?);
                    }
                    file_types.push(FuncType { params, results });
                }
            }
            SEC_IMPORT => {
                for _ in 0..r.u32()? {
                    let module = r.str()?.to_owned();
                    let field = r.str()?.to_owned();
                    let imp = Import { module, field };
                    match r.u8()? {
                        0 => {
                            let t = r.u32()?;
                            func_types.push(t);
                            p.func_imports.push(imp);
                        }
                        1 => {
                            let t = r.table_type()?;
                            p.table_import_ok =
                                imp.field == crate::wasm::FUNCTION_TABLE && t.elem == 0x70;
                            p.tables_imported += 1;
                            p.table_imports.push((imp, t));
                        }
                        2 => {
                            r.limits()?;
                        }
                        3 => {
                            let t = r.global_type()?;
                            p.global_imports.push((imp, t));
                        }
                        4 => {
                            r.u8()?;
                            let t = r.u32()?;
                            p.tag_imports.push((imp, t));
                        }
                        k => return Err(format!("import kind {k} is not supported")),
                    }
                }
            }
            SEC_FUNCTION => {
                for _ in 0..r.u32()? {
                    func_types.push(r.u32()?);
                }
            }
            SEC_TABLE => {
                for _ in 0..r.u32()? {
                    table_types.push(r.table_type()?);
                }
            }
            SEC_GLOBAL => {
                for _ in 0..r.u32()? {
                    let t = r.global_type()?;
                    let (a, z) = r.init_expr()?;
                    globals.push((t, a, z));
                }
            }
            SEC_EXPORT => {
                for _ in 0..r.u32()? {
                    let n = r.str()?.to_owned();
                    let kind = r.u8()?;
                    let idx = r.u32()?;
                    if kind == 0 {
                        exports.insert(idx, n);
                    }
                }
            }
            SEC_TAG => {
                for _ in 0..r.u32()? {
                    r.u8()?;
                    tag_types.push(r.u32()?);
                }
            }
            SEC_CODE => {
                for _ in 0..r.u32()? {
                    let start = r.at;
                    let n = r.u32()? as usize;
                    r.skip(n)?;
                    code.push((start, r.at));
                }
            }
            SEC_DATA => {
                for _ in 0..r.u32()? {
                    let flags = r.u32()?;
                    if flags == 2 {
                        r.u32()?;
                    }
                    if flags != 1 {
                        r.init_expr()?;
                    }
                    let n = r.u32()? as usize;
                    let start = r.skip(n)?;
                    data.push((start, start + n));
                }
            }
            SEC_MEMORY | SEC_START | SEC_ELEM | SEC_DATACOUNT => {}
            SEC_CUSTOM => match s.name.as_str() {
                "linking" => {
                    link_sect = Some(si);
                    let version = r.u32()?;
                    if version != 2 {
                        return Err(format!(
                            "linking section version {version} is not supported"
                        ));
                    }
                    while !r.done() {
                        let ty = r.u8()?;
                        let size = r.u32()? as usize;
                        let at = r.skip(size)?;
                        let mut s = Rd::new(b, at, at + size);
                        match ty {
                            LINK_SEGMENT_INFO => {
                                for _ in 0..s.u32()? {
                                    let n = s.str()?.to_owned();
                                    let align = s.u32()?;
                                    let flags = s.u32()?;
                                    seg_info.push((n, align, flags));
                                }
                            }
                            LINK_INIT_FUNCS => {
                                for _ in 0..s.u32()? {
                                    let prio = s.u32()?;
                                    let sym = s.u32()?;
                                    p.init_funcs.push((prio, sym));
                                }
                            }
                            LINK_COMDAT_INFO => {
                                for _ in 0..s.u32()? {
                                    let n = s.str()?.to_owned();
                                    s.u32()?;
                                    let mut v = Vec::new();
                                    for _ in 0..s.u32()? {
                                        let k = s.u8()?;
                                        v.push((k, s.u32()?));
                                    }
                                    comdats.push((n, v));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                "name" => {
                    // Function names only, for the output's name section.
                    while !r.done() {
                        let ty = r.u8()?;
                        let size = r.u32()? as usize;
                        let at = r.skip(size)?;
                        if ty != 1 {
                            continue;
                        }
                        let mut s = Rd::new(b, at, at + size);
                        for _ in 0..s.u32()? {
                            let i = s.u32()?;
                            debug_names.insert(i, s.str()?.to_owned());
                        }
                    }
                }
                "producers" => {
                    for _ in 0..r.u32()? {
                        let field = r.str()?;
                        let k = match field {
                            "language" => 0,
                            "processed-by" => 1,
                            "sdk" => 2,
                            _ => usize::MAX,
                        };
                        for _ in 0..r.u32()? {
                            let n = r.str()?.to_owned();
                            let v = r.str()?.to_owned();
                            if k != usize::MAX {
                                p.producers[k].push((n, v));
                            }
                        }
                    }
                }
                "target_features" => {
                    for _ in 0..r.u32()? {
                        let prefix = r.u8()?;
                        p.features.push((prefix, r.str()?.to_owned()));
                    }
                }
                n if n.starts_with("reloc.") => {
                    let target = r.u32()?;
                    let mut v = Vec::new();
                    for _ in 0..r.u32()? {
                        let ty = r.u8()?;
                        let off = r.u32()?;
                        let idx = r.u32()?;
                        let addend = if has_addend(u16::from(ty)) {
                            r.sleb()?
                        } else {
                            0
                        };
                        if ty > 26 {
                            return Err(format!("relocation type {ty} is not supported"));
                        }
                        v.push(RawReloc {
                            ty,
                            off,
                            idx,
                            addend,
                        });
                    }
                    if p.sects.get(target as usize).is_none() {
                        return Err(format!("{n} targets section {target}"));
                    }
                    p.relocs.push((target, v));
                }
                _ => {}
            },
            id => return Err(format!("section {id} is not supported")),
        }
    }
    if link_sect.is_none() {
        return Err("not a relocatable object (no linking section)".to_owned());
    }
    for t in &file_types {
        let id = l.sig(t);
        p.types.push(id);
    }
    let sig_of = |t: u32| -> Result<u32, String> {
        p.types
            .get(t as usize)
            .copied()
            .ok_or_else(|| format!("type {t} out of range"))
    };
    let nfi = p.func_imports.len();
    for &t in &func_types {
        p.func_sigs.push(sig_of(t)?);
    }
    if code.len() + nfi != func_types.len() {
        return Err("function and code sections disagree".to_owned());
    }
    // COMDATs: the first of each name is kept.
    let mut gone_funcs = vec![false; code.len()];
    let mut gone_segs = vec![false; data.len()];
    let mut gone_sects = vec![false; p.sects.len()];
    for (n, entries) in &comdats {
        if l.comdats.insert(n.clone()) {
            continue;
        }
        for &(k, i) in entries {
            let i = i as usize;
            match k {
                COMDAT_DATA => {
                    if let Some(g) = gone_segs.get_mut(i) {
                        *g = true;
                    }
                }
                COMDAT_FUNCTION => {
                    if let Some(g) = i.checked_sub(nfi).and_then(|i| gone_funcs.get_mut(i)) {
                        *g = true;
                    }
                }
                COMDAT_SECTION => {
                    if let Some(g) = gone_sects.get_mut(i) {
                        *g = true;
                    }
                }
                _ => {}
            }
        }
    }
    for (i, &(start, end)) in code.iter().enumerate() {
        let fidx = (nfi + i) as u32;
        let name = String::new();
        let a = l.push(
            Atom::new(fi, SEC_CODE.into(), start as u64, (end - start) as u64, 0),
            Chunk::Func {
                sig: p.func_sigs[nfi + i],
                name,
                debug_name: debug_names.remove(&fidx),
                export_name: exports.remove(&fidx),
                body: None,
            },
        );
        if gone_funcs[i] {
            l.discarded.insert(a);
        }
        p.funcs.push(a);
    }
    for (i, &(start, end)) in data.iter().enumerate() {
        let (name, align, mut flags) = seg_info.get(i).cloned().unwrap_or_default();
        if name.starts_with(".tdata") || name.starts_with(".tbss") {
            flags |= SEG_TLS;
        }
        let merge = flags & SEG_STRINGS != 0 && align == 0 && end > start;
        let a = l.push(
            Atom::new(
                fi,
                SEC_DATA.into(),
                start as u64,
                (end - start) as u64,
                align as u8,
            ),
            Chunk::Data { name, flags, merge },
        );
        if gone_segs[i] {
            l.discarded.insert(a);
        }
        p.segs.push(a);
    }
    for &(ty, a, z) in &globals {
        let at = l.push(
            Atom::new(fi, SEC_GLOBAL.into(), 0, 0, 0),
            Chunk::Global {
                ty,
                init: b[a..z].to_vec(),
                name: String::new(),
            },
        );
        p.globals.push(at);
    }
    for &ty in &table_types {
        let at = l.push(
            Atom::new(fi, SEC_TABLE.into(), 0, 0, 0),
            Chunk::Table {
                ty,
                name: String::new(),
            },
        );
        p.tables.push(at);
    }
    for &t in &tag_types {
        let sig = sig_of(t)?;
        let at = l.push(
            Atom::new(fi, SEC_TAG.into(), 0, 0, 0),
            Chunk::Tag {
                sig,
                name: String::new(),
            },
        );
        p.tags.push(at);
    }
    for (si, s) in p.sects.iter().enumerate() {
        if s.id != SEC_CUSTOM || s.name == "linking" || s.name.starts_with("reloc.") {
            continue;
        }
        let merge = matches!(
            s.name.as_str(),
            ".debug_str" | ".debug_str.dwo" | ".debug_line_str"
        ) && s.end > s.start;
        let align = u8::from(matches!(s.name.as_str(), "__llvm_covfun" | "__llvm_covmap")) * 3;
        let a = l.push(
            Atom::new(
                fi,
                SEC_CUSTOM.into(),
                s.start as u64,
                (s.end - s.start) as u64,
                align,
            ),
            Chunk::Custom {
                name: s.name.clone(),
                merge,
            },
        );
        if gone_sects[si] {
            l.discarded.insert(a);
        }
        p.customs.push(a);
        p.custom_of.insert(si as u32, a);
    }
    let names: Vec<Vec<String>> = vec![
        p.func_imports.iter().map(|i| i.field.clone()).collect(),
        p.table_imports.iter().map(|i| i.0.field.clone()).collect(),
        Vec::new(),
        p.global_imports.iter().map(|i| i.0.field.clone()).collect(),
        p.tag_imports.iter().map(|i| i.0.field.clone()).collect(),
    ];
    if let Some(si) = link_sect {
        p.infos = symbol_table(b, &p.sects[si], &names)?;
    }
    // A chunk's name is its first defining symbol's.
    for s in &p.infos {
        if s.flags & UNDEFINED != 0 {
            continue;
        }
        let Some(n) = &s.name else { continue };
        let i = s.index as usize;
        let a = match s.kind {
            SYM_FUNCTION => i.checked_sub(nfi).and_then(|i| p.funcs.get(i)),
            SYM_GLOBAL => i
                .checked_sub(p.global_imports.len())
                .and_then(|i| p.globals.get(i)),
            SYM_TABLE => i
                .checked_sub(p.tables_imported)
                .and_then(|i| p.tables.get(i)),
            SYM_TAG => i
                .checked_sub(p.tag_imports.len())
                .and_then(|i| p.tags.get(i)),
            _ => None,
        };
        if let Some(&a) = a {
            l.chunks[a as usize].set_name(n);
        }
    }
    Ok(p)
}

/// Enter symbol `i` of file `fi`: define it, or reference it (loading the
/// archive member that defines it).
fn symbol(
    l: &mut Link,
    fi: u32,
    p: &Parsed,
    i: usize,
    s: &SymInfo,
    called: bool,
) -> Result<SymId, String> {
    let undefined = s.flags & UNDEFINED != 0;
    let local = s.flags & LOCAL != 0;
    let idx = s.index as usize;
    let nfi = p.func_imports.len();
    let name = s.name.clone().unwrap_or_default();
    let bad = |what: &str| format!("symbol {name}: {what} {idx} out of range");
    // The chunk a definition lives in.
    let (atom, off) = if undefined {
        (None, 0)
    } else {
        match s.kind {
            SYM_FUNCTION => (
                Some(
                    *idx.checked_sub(nfi)
                        .and_then(|i| p.funcs.get(i))
                        .ok_or_else(|| bad("function"))?,
                ),
                0,
            ),
            SYM_DATA => (Some(*p.segs.get(idx).ok_or_else(|| bad("segment"))?), s.off),
            SYM_GLOBAL => (
                Some(
                    *idx.checked_sub(p.global_imports.len())
                        .and_then(|i| p.globals.get(i))
                        .ok_or_else(|| bad("global"))?,
                ),
                0,
            ),
            SYM_TABLE => (
                Some(
                    *idx.checked_sub(p.tables_imported)
                        .and_then(|i| p.tables.get(i))
                        .ok_or_else(|| bad("table"))?,
                ),
                0,
            ),
            SYM_TAG => (
                Some(
                    *idx.checked_sub(p.tag_imports.len())
                        .and_then(|i| p.tags.get(i))
                        .ok_or_else(|| bad("tag"))?,
                ),
                0,
            ),
            _ => (p.custom_of.get(&s.index).copied(), 0),
        }
    };
    let mut flags = s.flags;
    if s.kind == SYM_DATA {
        if let Some(a) = atom {
            if let Chunk::Data { flags: f, .. } = &l.chunks[a as usize] {
                if f & SEG_TLS != 0 {
                    flags |= TLS;
                }
            }
        }
    }
    let sig = match s.kind {
        SYM_FUNCTION => p.func_sigs.get(idx).copied(),
        SYM_TAG if undefined => p
            .tag_imports
            .get(idx)
            .and_then(|t| p.types.get(t.1 as usize).copied()),
        SYM_TAG => atom.and_then(|a| match &l.chunks[a as usize] {
            Chunk::Tag { sig, .. } => Some(*sig),
            _ => None,
        }),
        _ => None,
    };
    let gty = match s.kind {
        SYM_GLOBAL if undefined => p.global_imports.get(idx).map(|g| g.1),
        SYM_GLOBAL => atom.and_then(|a| match &l.chunks[a as usize] {
            Chunk::Global { ty, .. } => Some(*ty),
            _ => None,
        }),
        _ => None,
    };
    let tty = match s.kind {
        SYM_TABLE if undefined => p.table_imports.get(idx).map(|t| t.1),
        SYM_TABLE => atom.and_then(|a| match &l.chunks[a as usize] {
            Chunk::Table { ty, .. } => Some(*ty),
            _ => None,
        }),
        _ => None,
    };
    let import = if undefined {
        match s.kind {
            SYM_FUNCTION => p.func_imports.get(idx),
            SYM_GLOBAL => p.global_imports.get(idx).map(|g| &g.0),
            SYM_TABLE => p.table_imports.get(idx).map(|t| &t.0),
            SYM_TAG => p.tag_imports.get(idx).map(|t| &t.0),
            _ => None,
        }
    } else {
        None
    };
    let module = import.map(|i| i.module.clone());
    let field = import
        .filter(|_| s.flags & EXPLICIT_NAME != 0)
        .map(|i| i.field.clone());
    let info = crate::wasm::SymDesc {
        kind: s.kind,
        flags,
        sig,
        gty,
        tty,
        module,
        field,
        called,
    };
    if local {
        let id = l.syms.intern(&format!("\0{fi}.{i}"));
        if let Some(a) = atom {
            let sym = l.syms.get_mut(id);
            sym.def = Def::Defined { atom: a, off };
            sym.file = fi;
        }
        l.ext_mut(id).set(&info, Some(name));
        return Ok(id);
    }
    // A definition COMDAT-discarded refers to the kept one.
    let kept =
        atom.filter(|a| !l.discarded.contains(a) || !matches!(s.kind, SYM_FUNCTION | SYM_DATA));
    if let Some(a) = kept {
        l.define(&name, fi, a, off, &info);
        return Ok(l.syms.find(&name).expect("interned"));
    }
    if s.kind == SYM_SECTION {
        return Ok(l.syms.intern(&format!("\0{fi}.{i}")));
    }
    let id = l.undefined(&name, fi, &info)?;
    if s.kind == SYM_FUNCTION {
        if let Some(sig) = sig {
            l.files[fi as usize]
                .undef_funcs
                .push((i as u32, sig, called));
        }
    }
    Ok(id)
}
