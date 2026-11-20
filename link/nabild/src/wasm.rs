//! The WebAssembly linker, from wasm-ld's command line (the subset nabic
//! passes).
//!
//! wasm32 core modules from relocatable objects: a command (`_start`) or,
//! with `--no-entry`, a reactor; with `--shared-memory`, passive segments
//! and thread-local storage for wasi-threads. No PIC, shared libraries or
//! LTO.

pub mod object;

mod write;

use crate::symtab::{Atom, AtomId, Def, SymId, SymbolTable, Target};
use nabic_core::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const SYM_FUNCTION: u8 = 0;
pub const SYM_DATA: u8 = 1;
pub const SYM_GLOBAL: u8 = 2;
pub const SYM_SECTION: u8 = 3;
pub const SYM_TAG: u8 = 4;
pub const SYM_TABLE: u8 = 5;

pub const WEAK: u32 = 0x1;
pub const LOCAL: u32 = 0x2;
pub const HIDDEN: u32 = 0x4;
pub const UNDEFINED: u32 = 0x10;
pub const EXPORTED: u32 = 0x20;
pub const EXPLICIT_NAME: u32 = 0x40;
pub const NO_STRIP: u32 = 0x80;
pub const TLS: u32 = 0x100;
pub const ABSOLUTE: u32 = 0x200;

pub const SEG_STRINGS: u32 = 0x1;
pub const SEG_TLS: u32 = 0x2;
pub const SEG_RETAIN: u32 = 0x4;

pub const R_FUNCTION_INDEX_LEB: u16 = 0;
pub const R_TABLE_INDEX_SLEB: u16 = 1;
pub const R_TABLE_INDEX_I32: u16 = 2;
pub const R_MEMORY_ADDR_LEB: u16 = 3;
pub const R_MEMORY_ADDR_SLEB: u16 = 4;
pub const R_MEMORY_ADDR_I32: u16 = 5;
pub const R_TYPE_INDEX_LEB: u16 = 6;
pub const R_GLOBAL_INDEX_LEB: u16 = 7;
pub const R_FUNCTION_OFFSET_I32: u16 = 8;
pub const R_SECTION_OFFSET_I32: u16 = 9;
pub const R_TAG_INDEX_LEB: u16 = 10;
pub const R_MEMORY_ADDR_REL_SLEB: u16 = 11;
pub const R_TABLE_INDEX_REL_SLEB: u16 = 12;
pub const R_GLOBAL_INDEX_I32: u16 = 13;
pub const R_MEMORY_ADDR_LEB64: u16 = 14;
pub const R_MEMORY_ADDR_SLEB64: u16 = 15;
pub const R_MEMORY_ADDR_I64: u16 = 16;
pub const R_MEMORY_ADDR_REL_SLEB64: u16 = 17;
pub const R_TABLE_INDEX_SLEB64: u16 = 18;
pub const R_TABLE_INDEX_I64: u16 = 19;
pub const R_TABLE_NUMBER_LEB: u16 = 20;
pub const R_MEMORY_ADDR_TLS_SLEB: u16 = 21;
pub const R_FUNCTION_OFFSET_I64: u16 = 22;
pub const R_MEMORY_ADDR_LOCREL_I32: u16 = 23;
pub const R_TABLE_INDEX_REL_SLEB64: u16 = 24;
pub const R_MEMORY_ADDR_TLS_SLEB64: u16 = 25;
pub const R_FUNCTION_INDEX_I32: u16 = 26;

/// Relocation types that carry an addend.
#[must_use]
pub const fn has_addend(ty: u16) -> bool {
    matches!(
        ty,
        R_MEMORY_ADDR_LEB
            | R_MEMORY_ADDR_SLEB
            | R_MEMORY_ADDR_I32
            | R_FUNCTION_OFFSET_I32
            | R_SECTION_OFFSET_I32
            | R_MEMORY_ADDR_REL_SLEB
            | R_MEMORY_ADDR_LEB64
            | R_MEMORY_ADDR_SLEB64
            | R_MEMORY_ADDR_I64
            | R_MEMORY_ADDR_REL_SLEB64
            | R_MEMORY_ADDR_TLS_SLEB
            | R_FUNCTION_OFFSET_I64
            | R_MEMORY_ADDR_LOCREL_I32
            | R_MEMORY_ADDR_TLS_SLEB64
    )
}

/// A function address (table slot) relocation.
#[must_use]
pub const fn is_table_index(ty: u16) -> bool {
    matches!(
        ty,
        R_TABLE_INDEX_SLEB
            | R_TABLE_INDEX_I32
            | R_TABLE_INDEX_REL_SLEB
            | R_TABLE_INDEX_SLEB64
            | R_TABLE_INDEX_I64
            | R_TABLE_INDEX_REL_SLEB64
    )
}

pub const FUNCTION_TABLE: &str = "__indirect_function_table";
const PAGE: u64 = 0x1_0000;

#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct FuncType {
    pub params: Vec<u8>,
    pub results: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct GlobalType {
    pub ty: u8,
    pub mutable: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct Limits {
    pub flags: u8,
    pub min: u64,
    pub max: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TableType {
    pub elem: u8,
    pub limits: Limits,
}

/// What an atom holds.
#[derive(Debug)]
pub enum Chunk {
    /// The placeholder for dead targets.
    Dead,
    /// A function: its bytes in the file (size and body), or `body` for
    /// one the linker made.
    Func {
        sig: u32,
        name: String,
        debug_name: Option<String>,
        export_name: Option<String>,
        body: Option<Vec<u8>>,
    },
    /// A data segment's contents; `flags` are its `SEG_*`.
    Data {
        name: String,
        flags: u32,
        merge: bool,
    },
    Global {
        ty: GlobalType,
        init: Vec<u8>,
        name: String,
    },
    Table {
        ty: TableType,
        name: String,
    },
    Tag {
        sig: u32,
        name: String,
    },
    /// A custom section of an object.
    Custom {
        name: String,
        merge: bool,
    },
}

impl Chunk {
    fn set_name(&mut self, n: &str) {
        let (Self::Func { name: slot, .. }
        | Self::Global { name: slot, .. }
        | Self::Table { name: slot, .. }
        | Self::Tag { name: slot, .. }) = self
        else {
            return;
        };
        if slot.is_empty() {
            n.clone_into(slot);
        }
    }
}

/// A symbol as an object declares it.
pub struct SymDesc {
    pub kind: u8,
    pub flags: u32,
    pub sig: Option<u32>,
    pub gty: Option<GlobalType>,
    pub tty: Option<TableType>,
    pub module: Option<String>,
    pub field: Option<String>,
    pub called: bool,
}

/// Per-symbol wasm facts the format-neutral table does not keep.
#[derive(Clone, Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct SymExt {
    /// `SYM_*`; `None` for a name only an archive offers.
    pub kind: Option<u8>,
    /// The definition's flags (an undefined symbol's: its first
    /// reference's).
    pub flags: u32,
    pub sig: Option<u32>,
    pub gty: Option<GlobalType>,
    pub tty: Option<TableType>,
    /// Import module and explicit import name.
    pub module: Option<String>,
    pub field: Option<String>,
    /// Some reference calls it (`call`, not only its address).
    pub called: bool,
    /// `--export`, `--export-if-defined`, the entry.
    pub force_export: bool,
    /// A linker-made trap: its address is the null pointer.
    pub stub: bool,
    /// Named by an object (not only offered by an archive).
    pub in_obj: bool,
    /// Live whatever references it.
    pub pinned: bool,
    /// Referenced by live code (undefined and absolute symbols).
    pub live: bool,
    /// A local symbol's name (its table key is unique to the file).
    pub local: Option<String>,
}

impl SymExt {
    fn set(&mut self, d: &SymDesc, local: Option<String>) {
        self.kind = Some(d.kind);
        self.flags = d.flags;
        self.sig = d.sig;
        self.gty = d.gty;
        self.tty = d.tty;
        self.called |= d.called;
        self.in_obj = true;
        self.local = local;
    }
}

#[derive(Default)]
pub struct ObjFile {
    pub name: String,
    pub data: Vec<u8>,
    /// From the command line (live from the start), not an archive.
    pub explicit: bool,
    /// File type index: link signature.
    pub types: Vec<u32>,
    /// Symbol table index: symbol.
    pub syms: Vec<SymId>,
    pub funcs: Vec<AtomId>,
    pub segs: Vec<AtomId>,
    pub globals: Vec<AtomId>,
    pub tables: Vec<AtomId>,
    pub tags: Vec<AtomId>,
    pub customs: Vec<AtomId>,
    /// (priority, symbol index).
    pub init_funcs: Vec<(u32, u32)>,
    /// Undefined functions: (symbol index, signature, called).
    pub undef_funcs: Vec<(u32, u32, bool)>,
    /// (`+` or `-`, feature).
    pub features: Vec<(u8, String)>,
    /// language, processed-by, sdk: (name, version).
    pub producers: [Vec<(String, String)>; 3],
}

#[derive(Clone, Debug)]
pub enum InputKind {
    File(PathBuf),
    Lib(String),
}

#[derive(Clone, Debug)]
pub struct Input {
    pub kind: InputKind,
    /// `-Bstatic`: `-l` finds archives only.
    pub bstatic: bool,
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct Options {
    pub out: PathBuf,
    pub lib_dirs: Vec<PathBuf>,
    pub inputs: Vec<Input>,
    /// `None`: `--no-entry`.
    pub entry: Option<String>,
    pub stack_first: bool,
    pub stack_size: u64,
    pub global_base: u64,
    pub initial_memory: u64,
    pub max_memory: u64,
    pub import_memory: bool,
    pub export_memory: bool,
    pub shared_memory: bool,
    /// `--export`: must be defined.
    pub exports: Vec<String>,
    pub exports_if_defined: Vec<String>,
    pub undefined: Vec<String>,
    pub gc: bool,
    pub strip_debug: bool,
    pub strip_all: bool,
    /// `--icf=safe`: fold identical functions ([`fold_code`]).
    pub icf: bool,
}

/// Parse wasm-ld arguments (the subset nabic passes).
pub fn parse_args<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Result<Options, String> {
    let mut o = Options {
        out: PathBuf::from("a.out"),
        lib_dirs: Vec::new(),
        inputs: Vec::new(),
        entry: Some("_start".to_owned()),
        stack_first: true,
        stack_size: PAGE,
        global_base: 0,
        initial_memory: 0,
        max_memory: 0,
        import_memory: false,
        export_memory: false,
        shared_memory: false,
        exports: Vec::new(),
        exports_if_defined: Vec::new(),
        undefined: Vec::new(),
        gc: true,
        strip_debug: false,
        strip_all: false,
        icf: false,
    };
    let mut bstatic = true;
    let args: Vec<OsString> = args.iter().map(|a| a.as_ref().to_owned()).collect();
    let mut it = args.iter();
    let next = |it: &mut std::slice::Iter<'_, OsString>, flag: &str| {
        it.next()
            .map(|a| a.to_string_lossy().into_owned())
            .ok_or_else(|| format!("{flag}: missing argument"))
    };
    let num = |flag: &str, v: &str| -> Result<u64, String> {
        v.strip_prefix("0x")
            .map_or_else(|| v.parse().ok(), |h| u64::from_str_radix(h, 16).ok())
            .ok_or_else(|| format!("{flag}: bad number {v}"))
    };
    while let Some(a) = it.next() {
        let s = a.to_string_lossy();
        let (flag, mut inline) = match s.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_owned())),
            _ => (s.as_ref(), None),
        };
        let mut value = |it: &mut std::slice::Iter<'_, OsString>| match inline.take() {
            Some(v) => Ok(v),
            None => next(it, flag),
        };
        match flag {
            "-o" => o.out = next(&mut it, "-o")?.into(),
            "--stack-first" => o.stack_first = true,
            "--no-stack-first" => o.stack_first = false,
            "-z" => {
                let z = next(&mut it, "-z")?;
                let Some(v) = z.strip_prefix("stack-size=") else {
                    return Err(format!("-z {z}: unsupported option"));
                };
                o.stack_size = num("-z stack-size", v)?;
            }
            "--max-memory" => o.max_memory = num(flag, &value(&mut it)?)?,
            "--initial-memory" => o.initial_memory = num(flag, &value(&mut it)?)?,
            "--global-base" => o.global_base = num(flag, &value(&mut it)?)?,
            "--shared-memory" => o.shared_memory = true,
            "--import-memory" => o.import_memory = true,
            "--export-memory" => o.export_memory = true,
            "--export" => o.exports.push(value(&mut it)?),
            "--export-if-defined" => o.exports_if_defined.push(value(&mut it)?),
            "--undefined" | "-u" => o.undefined.push(value(&mut it)?),
            "--no-entry" => o.entry = None,
            "--entry" | "-e" => o.entry = Some(value(&mut it)?),
            "--gc-sections" => o.gc = true,
            "--no-gc-sections" => o.gc = false,
            "--strip-all" | "-s" => o.strip_all = true,
            "--strip-debug" | "-S" => o.strip_debug = true,
            // Only what is safe is ever folded.
            "--icf" => {
                o.icf = match value(&mut it)?.as_str() {
                    "safe" | "all" => true,
                    "none" => false,
                    v => return Err(format!("--icf={v}: expected safe, all or none")),
                };
            }
            "-Bstatic" => bstatic = true,
            "-Bdynamic" => bstatic = false,
            "-L" => o.lib_dirs.push(next(&mut it, "-L")?.into()),
            "-l" => o.inputs.push(Input {
                kind: InputKind::Lib(next(&mut it, "-l")?),
                bstatic,
            }),
            _ if s.starts_with("-L") => o.lib_dirs.push(PathBuf::from(&s[2..])),
            _ if s.starts_with("-l") => o.inputs.push(Input {
                kind: InputKind::Lib(s[2..].to_owned()),
                bstatic,
            }),
            _ if s.starts_with('-') => return Err(format!("unknown argument: {s}")),
            _ => o.inputs.push(Input {
                kind: InputKind::File(a.into()),
                bstatic,
            }),
        }
    }
    if o.stack_size % 16 != 0 {
        return Err("stack size must be 16-byte aligned".to_owned());
    }
    Ok(o)
}

/// An input archive: its bytes and which members are loaded.
struct ArchiveIn {
    path: PathBuf,
    data: Vec<u8>,
    members: Vec<(String, std::ops::Range<usize>)>,
    loaded: Vec<bool>,
}

/// The symbols the linker defines.
#[derive(Default)]
pub struct Synth {
    pub call_ctors: SymId,
    pub stack_pointer: SymId,
    /// With `--shared-memory`.
    pub tls_base: Option<SymId>,
    pub tls_size: Option<SymId>,
    pub tls_align: Option<SymId>,
    pub init_tls: Option<SymId>,
    /// Data symbols defined when referenced: (symbol, which).
    pub optional: Vec<(SymId, &'static str)>,
    /// `__wasm_call_dtors`, called by command export wrappers.
    pub call_dtors: Option<SymId>,
}

pub struct Link {
    pub atoms: Vec<Atom>,
    /// What each atom holds.
    pub chunks: Vec<Chunk>,
    pub syms: SymbolTable,
    pub ext: Vec<SymExt>,
    pub files: Vec<ObjFile>,
    /// Files in the order wasm-ld lists them: each after the archive
    /// members it pulled in.
    pub order: Vec<u32>,
    archives: Vec<ArchiveIn>,
    /// COMDAT names seen.
    comdats: HashSet<String>,
    pub discarded: HashSet<AtomId>,
    pub sigs: Vec<FuncType>,
    sig_ids: HashMap<FuncType, u32>,
    /// Linker-made functions and globals, in creation order.
    pub synth_funcs: Vec<AtomId>,
    pub synth_globals: Vec<AtomId>,
    pub synth: Synth,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    /// Symbols moved to the copy their folded function or literal became.
    pub moved: HashSet<SymId>,
}

/// The atom standing for dead and absent targets.
pub const DEAD: AtomId = 0;

impl Default for Link {
    fn default() -> Self {
        Self::new()
    }
}

impl Link {
    #[must_use]
    pub fn new() -> Self {
        Self {
            atoms: vec![Atom::new(u32::MAX, 0, 0, 0, 0)],
            chunks: vec![Chunk::Dead],
            syms: SymbolTable::default(),
            ext: Vec::new(),
            files: Vec::new(),
            order: Vec::new(),
            archives: Vec::new(),
            comdats: HashSet::default(),
            discarded: HashSet::default(),
            sigs: Vec::new(),
            sig_ids: HashMap::default(),
            synth_funcs: Vec::new(),
            synth_globals: Vec::new(),
            synth: Synth::default(),
            errors: Vec::new(),
            warnings: Vec::new(),
            moved: HashSet::default(),
        }
    }

    pub fn push(&mut self, a: Atom, c: Chunk) -> AtomId {
        self.atoms.push(a);
        self.chunks.push(c);
        (self.atoms.len() - 1) as AtomId
    }

    /// The link-wide id of signature `t`.
    pub fn sig(&mut self, t: &FuncType) -> u32 {
        if let Some(&i) = self.sig_ids.get(t) {
            return i;
        }
        let i = self.sigs.len() as u32;
        self.sigs.push(t.clone());
        self.sig_ids.insert(t.clone(), i);
        i
    }

    pub fn ext_mut(&mut self, id: SymId) -> &mut SymExt {
        let i = id as usize;
        if self.ext.len() <= i {
            self.ext.resize(i + 1, SymExt::default());
        }
        &mut self.ext[i]
    }

    #[must_use]
    pub fn ext(&self, id: SymId) -> &SymExt {
        static NONE: SymExt = SymExt {
            kind: None,
            flags: 0,
            sig: None,
            gty: None,
            tty: None,
            module: None,
            field: None,
            called: false,
            force_export: false,
            stub: false,
            in_obj: false,
            pinned: false,
            live: false,
            local: None,
        };
        self.ext.get(id as usize).unwrap_or(&NONE)
    }

    #[must_use]
    pub fn file_name(&self, fi: u32) -> String {
        self.files
            .get(fi as usize)
            .map_or_else(|| "<internal>".to_owned(), |f| f.name.clone())
    }

    /// A symbol's name as messages and the name section show it.
    #[must_use]
    pub fn name(&self, id: SymId) -> &str {
        self.ext(id)
            .local
            .as_deref()
            .unwrap_or(&self.syms.get(id).name)
    }

    fn kind_mismatch(&mut self, id: SymId, fi: u32, kind: u8) -> bool {
        let Some(k) = self.ext(id).kind else {
            return false;
        };
        if k == kind || matches!(self.syms.get(id).def, Def::Lazy { .. }) {
            return false;
        }
        let kinds = ["function", "data", "global", "section", "tag", "table"];
        let s = self.syms.get(id);
        let msg = format!(
            "symbol type mismatch: {}\n>>> defined as {} in {}\n>>> defined as {} in {}",
            s.name,
            kinds[k as usize],
            self.file_name(s.file),
            kinds[kind as usize],
            self.file_name(fi)
        );
        self.errors.push(msg);
        true
    }

    /// A definition of `name` in atom `atom` at `off` of file `fi`.
    fn define(&mut self, name: &str, fi: u32, atom: AtomId, off: u64, d: &SymDesc) {
        let id = self.syms.intern(name);
        self.ext_mut(id).in_obj = true;
        if self.kind_mismatch(id, fi, d.kind) {
            return;
        }
        let weak = d.flags & WEAK != 0;
        let def = Def::Defined { atom, off };
        match self.syms.define(id, def, fi, weak, d.flags & HIDDEN != 0) {
            Ok(()) => {
                if self.syms.get(id).def == def {
                    let force = self.ext(id).force_export;
                    let called = self.ext(id).called;
                    let x = self.ext_mut(id);
                    x.set(d, None);
                    x.force_export = force;
                    x.called |= called;
                }
            }
            Err(prev) => {
                let msg = format!(
                    "duplicate symbol: {name}\n>>> defined in {}\n>>> defined in {}",
                    self.file_name(prev),
                    self.file_name(fi)
                );
                self.errors.push(msg);
            }
        }
    }

    /// A reference from file `fi` to `name`; a lazy definition loads
    /// unless the reference is weak (functions and data only).
    fn undefined(&mut self, name: &str, fi: u32, d: &SymDesc) -> Result<SymId, String> {
        let id = self.syms.intern(name);
        let weak = d.flags & WEAK != 0;
        if self.kind_mismatch(id, fi, d.kind) {
            return Ok(id);
        }
        let lazy = matches!(self.syms.get(id).def, Def::Lazy { .. });
        let fresh = self.ext(id).kind.is_none() || lazy;
        if lazy && weak && matches!(d.kind, SYM_FUNCTION | SYM_DATA) {
            let s = self.syms.get_mut(id);
            s.referenced = true;
            if self.ext(id).kind.is_none() {
                self.ext_mut(id).set(d, None);
            }
            self.ext_mut(id).called |= d.called;
            return Ok(id);
        }
        if let Some(f) = self.syms.reference(id, fi, weak) {
            self.load_member(f)?;
        }
        let defined = matches!(
            self.syms.get(id).def,
            Def::Defined { .. } | Def::Absolute(_) | Def::Synthetic
        );
        let x = self.ext_mut(id);
        x.in_obj = true;
        if defined {
            x.called |= d.called;
            return Ok(id);
        }
        if fresh {
            x.set(d, None);
            x.module.clone_from(&d.module);
            x.field.clone_from(&d.field);
            return Ok(id);
        }
        // An address-only reference gives way to a call's signature.
        if d.called && !x.called && x.sig != d.sig {
            x.sig = d.sig;
            x.flags = d.flags;
            x.module.clone_from(&d.module);
            x.field.clone_from(&d.field);
        }
        x.called |= d.called;
        if x.sig.is_none() {
            x.sig = d.sig;
        }
        // The first import name and module stand; another is an error.
        let mut err = None;
        for (what, new) in [("name", &d.field), ("module", &d.module)] {
            let Some(new) = new else { continue };
            let old = if what == "name" {
                &mut x.field
            } else {
                &mut x.module
            };
            match old {
                None => *old = Some(new.clone()),
                Some(o) if o != new => err = Some((what, o.clone(), new.clone())),
                Some(_) => {}
            }
        }
        if let Some((what, old, new)) = err {
            let prev = self.file_name(self.syms.get(id).file);
            let msg = format!(
                "import {what} mismatch for symbol: {name}\n>>> defined as {old} in {prev}\n\
                 >>> defined as {new} in {}",
                self.file_name(fi)
            );
            self.errors.push(msg);
        }
        Ok(id)
    }

    /// Load archive member `(ai, mi)` if it is not yet.
    fn load_member(&mut self, (ai, mi): (u32, u32)) -> Result<(), String> {
        let a = &mut self.archives[ai as usize];
        if a.loaded[mi as usize] {
            return Ok(());
        }
        a.loaded[mi as usize] = true;
        let (name, range) = a.members[mi as usize].clone();
        let data = a.data[range].to_vec();
        let shown = format!("{}({name})", a.path.display());
        let fi = self.files.len() as u32;
        object::parse(self, fi, shown, data, false)
    }

    fn add_file(&mut self, path: &Path) -> Result<(), String> {
        let raw =
            std::fs::read(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        if raw.starts_with(crate::archive::MAGIC) {
            return self.add_archive(path, raw);
        }
        if raw.starts_with(b"BC\xc0\xde") {
            return Err(format!("{}: LLVM bitcode is not supported", path.display()));
        }
        if !raw.starts_with(b"\0asm") {
            return Err(format!("unknown file type: {}", path.display()));
        }
        let fi = self.files.len() as u32;
        object::parse(self, fi, path.display().to_string(), raw, true)
    }

    /// Each member's definitions become lazy, in member order; a member
    /// defining a wanted symbol loads at once.
    fn add_archive(&mut self, path: &Path, data: Vec<u8>) -> Result<(), String> {
        let ai = self.archives.len() as u32;
        let members: Vec<(String, std::ops::Range<usize>)> = {
            let a = crate::archive::parse(&data).map_err(|e| format!("{}: {e}", path.display()))?;
            let base = data.as_ptr() as usize;
            a.members
                .iter()
                .map(|m| {
                    let s = m.data.as_ptr() as usize - base;
                    (m.name.clone(), s..s + m.data.len())
                })
                .collect()
        };
        let n = members.len();
        let mut offers = Vec::with_capacity(n);
        for (name, r) in &members {
            let m = &data[r.clone()];
            if !m.starts_with(b"\0asm") {
                self.warnings.push(format!(
                    "{}: archive member '{name}' is not a wasm object",
                    path.display()
                ));
                offers.push(Vec::new());
                continue;
            }
            let syms = object::defined_symbols(m)
                .map_err(|e| format!("{}({name}): {e}", path.display()))?;
            offers.push(syms);
        }
        self.archives.push(ArchiveIn {
            path: path.to_owned(),
            data,
            members,
            loaded: vec![false; n],
        });
        for (mi, names) in offers.into_iter().enumerate() {
            for name in names {
                if self.archives[ai as usize].loaded[mi] {
                    break;
                }
                let id = self.syms.intern(&name);
                let s = self.syms.get_mut(id);
                match s.def {
                    Def::Undefined if s.referenced && !s.weak_ref => {
                        self.load_member((ai, mi as u32))?;
                    }
                    Def::Undefined => {
                        s.def = Def::Lazy {
                            archive: ai,
                            member: mi as u32,
                        };
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// A linker-made function `name` of signature `sig` (`body` set at
    /// layout), hidden.
    fn synth_func(&mut self, name: &str, sig: &FuncType) -> SymId {
        let sig = self.sig(sig);
        let a = self.push(
            Atom::new(u32::MAX, 0, 0, 0, 0),
            Chunk::Func {
                sig,
                name: name.to_owned(),
                debug_name: None,
                export_name: None,
                body: Some(Vec::new()),
            },
        );
        self.synth_funcs.push(a);
        let id = self.syms.intern(name);
        let s = self.syms.get_mut(id);
        s.def = Def::Defined { atom: a, off: 0 };
        s.file = u32::MAX;
        let x = self.ext_mut(id);
        x.kind = Some(SYM_FUNCTION);
        x.flags = HIDDEN;
        x.sig = Some(sig);
        id
    }

    /// A linker-made i32 global `name`.
    fn synth_global(&mut self, name: &str, mutable: bool, flags: u32) -> SymId {
        let ty = GlobalType { ty: 0x7f, mutable };
        let a = self.push(
            Atom::new(u32::MAX, 0, 0, 0, 0),
            Chunk::Global {
                ty,
                init: vec![0x41, 0, 0x0b],
                name: name.to_owned(),
            },
        );
        self.synth_globals.push(a);
        let id = self.syms.intern(name);
        let s = self.syms.get_mut(id);
        s.def = Def::Defined { atom: a, off: 0 };
        s.file = u32::MAX;
        let x = self.ext_mut(id);
        x.kind = Some(SYM_GLOBAL);
        x.flags = flags;
        x.gty = Some(ty);
        id
    }

    /// A trap of signature `sig` named `debug_name` in the name section;
    /// its address is the null pointer.
    fn stub(&mut self, debug_name: &str, sig: u32) -> SymId {
        let a = self.push(
            Atom::new(u32::MAX, 0, 0, 0, 0),
            Chunk::Func {
                sig,
                name: debug_name.to_owned(),
                debug_name: Some(debug_name.to_owned()),
                export_name: None,
                body: Some(vec![0x03, 0x00, 0x00, 0x0b]),
            },
        );
        self.synth_funcs.push(a);
        let id = self.syms.intern(&format!("\0stub.{a}"));
        let s = self.syms.get_mut(id);
        s.def = Def::Defined { atom: a, off: 0 };
        s.file = u32::MAX;
        let x = self.ext_mut(id);
        x.kind = Some(SYM_FUNCTION);
        x.flags = LOCAL | HIDDEN;
        x.sig = Some(sig);
        x.stub = true;
        x.local = Some(debug_name.to_owned());
        id
    }

    /// The signature as wasm-ld prints it: `(i32, i32) -> i32`.
    #[must_use]
    pub fn sig_text(&self, sig: u32) -> String {
        let name = |t: &u8| -> &'static str {
            match t {
                0x7f => "i32",
                0x7e => "i64",
                0x7d => "f32",
                0x7c => "f64",
                0x7b => "v128",
                0x70 => "funcref",
                0x6f => "externref",
                _ => "ref",
            }
        };
        let t = &self.sigs[sig as usize];
        let p: Vec<&str> = t.params.iter().map(name).collect();
        let r = match t.results.len() {
            0 => "void".to_owned(),
            1 => name(&t.results[0]).to_owned(),
            _ => format!(
                "[{}]",
                t.results.iter().map(name).collect::<Vec<_>>().join(", ")
            ),
        };
        format!("({}) -> {r}", p.join(", "))
    }

    /// Whether symbol `id` survives: a defined one with its chunk, an
    /// undefined or absolute one referenced by live code.
    #[must_use]
    pub fn live(&self, id: SymId) -> bool {
        match self.syms.get(id).def {
            Def::Defined { atom, .. } => self.atoms[atom as usize].live,
            _ => self.ext(id).live,
        }
    }

    /// The chunk symbol `id` is defined in ([`DEAD`] if none).
    #[must_use]
    pub fn atom_of(&self, id: SymId) -> AtomId {
        match self.syms.get(id).def {
            Def::Defined { atom, .. } => atom,
            _ => DEAD,
        }
    }

    #[must_use]
    pub fn defined(&self, id: SymId) -> bool {
        matches!(
            self.syms.get(id).def,
            Def::Defined { .. } | Def::Absolute(_)
        )
    }

    /// Exported: defined, not local, and marked so or forced.
    #[must_use]
    pub fn exported(&self, id: SymId) -> bool {
        let x = self.ext(id);
        self.defined(id) && x.flags & LOCAL == 0 && (x.force_export || x.flags & EXPORTED != 0)
    }
}

/// `-l<name>`: `lib<name>.a` in each directory (`:file` as named).
fn find_library(name: &str, i: &Input, o: &Options) -> Result<PathBuf, String> {
    if let Some(file) = name.strip_prefix(':') {
        return o
            .lib_dirs
            .iter()
            .map(|d| d.join(file))
            .find(|p| p.is_file())
            .ok_or_else(|| format!("unable to find library -l{name}"));
    }
    for d in &o.lib_dirs {
        if !i.bstatic && d.join(format!("lib{name}.so")).is_file() {
            return Err(format!(
                "-l{name}: {} is a shared library, which wasm32 does not link",
                d.join(format!("lib{name}.so")).display()
            ));
        }
        let p = d.join(format!("lib{name}.a"));
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(format!("unable to find library -l{name}"))
}

/// Data symbols wasm-ld defines when they are referenced.
const OPTIONAL: &[&str] = &[
    "__dso_handle",
    "__data_end",
    "__stack_low",
    "__stack_high",
    "__global_base",
    "__heap_base",
    "__heap_end",
    "__memory_base",
    "__table_base",
    "__wasm_first_page_end",
];

/// Link `o.out`; the warnings, one per line.
pub fn link(o: &Options) -> Result<String, String> {
    let mut l = Link::new();
    let void = FuncType::default();
    l.synth.call_ctors = l.synth_func("__wasm_call_ctors", &void);
    l.synth.stack_pointer = l.synth_global("__stack_pointer", true, 0);
    if o.shared_memory {
        l.synth.tls_base = Some(l.synth_global("__tls_base", true, HIDDEN));
        l.synth.tls_size = Some(l.synth_global("__tls_size", false, HIDDEN));
        l.synth.tls_align = Some(l.synth_global("__tls_align", false, HIDDEN));
        let arg = FuncType {
            params: vec![0x7f],
            results: Vec::new(),
        };
        l.synth.init_tls = Some(l.synth_func("__wasm_init_tls", &arg));
    }
    let sp = l.synth.stack_pointer;
    let spa = l.atom_of(sp);
    l.atoms[spa as usize].root = true;
    for i in &o.inputs {
        match &i.kind {
            InputKind::File(p) => l.add_file(p)?,
            InputKind::Lib(n) => {
                let p = find_library(n, i, o)?;
                l.add_file(&p)?;
            }
        }
        if !l.errors.is_empty() {
            return Err(l.errors.join("\n"));
        }
    }
    // `--undefined`, `--export*` and the entry load what defines them.
    let wanted: Vec<&String> = o
        .undefined
        .iter()
        .chain(&o.exports)
        .chain(&o.exports_if_defined)
        .chain(o.entry.as_ref())
        .collect();
    for n in wanted {
        if let Some(id) = l.syms.find(n) {
            if let Def::Lazy { archive, member } = l.syms.get(id).def {
                l.syms.get_mut(id).def = Def::Undefined;
                l.load_member((archive, member))?;
            }
        }
    }
    if !l.errors.is_empty() {
        let e = l.errors.join("\n");
        return Err(warned(&l, e));
    }
    let entry = match &o.entry {
        Some(e) => {
            let id = l.syms.find(e).filter(|&id| l.defined(id));
            let Some(id) = id else {
                return Err(format!(
                    "entry symbol not defined (pass --no-entry to suppress): {e}"
                ));
            };
            l.ext_mut(id).force_export = true;
            Some(id)
        }
        None => None,
    };
    // Without a caller of `__wasm_call_ctors`, exports are wrapped to
    // call it, and `__wasm_call_dtors` after.
    let ctors_used = l.ext(l.synth.call_ctors).in_obj
        || o.exports
            .iter()
            .chain(&o.exports_if_defined)
            .any(|e| e == "__wasm_call_ctors");
    if !ctors_used && o.entry.as_deref() != Some("__wasm_call_ctors") {
        if let Some(id) = l.syms.find("__wasm_call_dtors") {
            if let Def::Lazy { archive, member } = l.syms.get(id).def {
                l.syms.get_mut(id).def = Def::Undefined;
                l.load_member((archive, member))?;
            }
            if l.defined(id) {
                if l.ext(id).kind != Some(SYM_FUNCTION) {
                    return Err("__wasm_call_dtors must be a function".to_owned());
                }
                l.synth.call_dtors = Some(id);
            }
        }
    }
    for n in OPTIONAL {
        if let Some(id) = l.syms.find(n) {
            if !l.defined(id) {
                optional_data(&mut l, id, n);
            }
        }
    }
    if !o.shared_memory {
        if let Some(id) = l.syms.find("__tls_base") {
            if !l.defined(id) {
                let g = l.synth_global("__tls_base", false, HIDDEN);
                l.synth.tls_base = Some(g);
            }
        }
    }
    variants(&mut l).map_err(|e| warned(&l, e))?;
    for n in o.exports.iter().chain(&o.exports_if_defined) {
        if let Some(id) = l.syms.find(n) {
            if l.defined(id) {
                l.ext_mut(id).force_export = true;
            }
        }
    }
    weak_undefined(&mut l);
    if let Some(id) = entry {
        l.ext_mut(id).flags &= !HIDDEN;
    }
    let roots = roots(&mut l, entry);
    crate::gc::mark(&mut l.atoms, &l.syms, &roots, !o.gc);
    l.atoms[DEAD as usize].live = false;
    for (k, c) in l.chunks.iter().enumerate() {
        if matches!(c, Chunk::Custom { .. }) {
            l.atoms[k].live = false;
        }
    }
    mark_symbols(&mut l, &roots);
    if o.icf {
        fold_code(&mut l, &roots);
    }
    let text = write::write(&mut l, o).map_err(|e| warned(&l, e))?;
    // Freeing every atom and name takes as long as some links: let a
    // thread do it.
    let _ = std::thread::Builder::new().spawn(move || drop(l));
    Ok(text)
}

/// The atoms as identical code folding sees them.
struct Fold<'a> {
    l: &'a Link,
    canon: &'a HashMap<AtomId, AtomId>,
}
impl crate::icf::View for Fold<'_> {
    fn bytes(&self, a: AtomId) -> &[u8] {
        let at = &self.l.atoms[a as usize];
        &self.l.files[at.file as usize].data[at.off as usize..(at.off + at.size) as usize]
    }
    fn extra(&self, a: AtomId) -> u64 {
        match &self.l.chunks[a as usize] {
            Chunk::Func { sig, .. } => u64::from(*sig),
            _ => u64::MAX,
        }
    }
    fn dest(&self, t: Target) -> crate::icf::Dest {
        match t {
            Target::Sym(id) => match self.l.syms.get(id).def {
                Def::Defined { atom, off } => crate::icf::Dest::In {
                    atom: self.canon.get(&atom).copied().unwrap_or(atom),
                    off,
                    tag: 0,
                },
                _ => crate::icf::Dest::Other(u64::from(id)),
            },
            Target::Atom(_) => crate::icf::Dest::Other(u64::MAX),
        }
    }
    // A signature is the file's type index.
    fn addend(&self, a: AtomId, r: &crate::symtab::Reloc) -> i64 {
        if r.kind == R_TYPE_INDEX_LEB {
            let f = &self.l.files[self.l.atoms[a as usize].file as usize];
            return i64::from(f.types[r.addend as usize]);
        }
        r.addend
    }
}

/// Fold identical functions: ones never in the table or exported (only
/// `call` reaches them) or nabic's function value thunks, with the same
/// signature, body and relocations, become one, and nabic's string
/// literals with the same bytes. The
/// copies' symbols move to the kept one; debug info that described a
/// copy points at the tombstone, like dead code's.
fn fold_code(l: &mut Link, roots: &[Target]) {
    fn code(l: &Link, a: AtomId) -> &[u8] {
        let at = &l.atoms[a as usize];
        &l.files[at.file as usize].data[at.off as usize..(at.off + at.size) as usize]
    }
    let mut taken = crate::icf::address_taken(
        &l.atoms,
        &l.syms,
        |a| l.atoms[a as usize].file != u32::MAX,
        |a, r| {
            // `call` and `return_call`, not `ref.func`.
            r.kind == R_FUNCTION_INDEX_LEB
                && matches!(l.chunks[a as usize], Chunk::Func { body: None, .. })
                && r.off > 0
                && matches!(code(l, a).get(r.off as usize - 1), Some(0x10 | 0x12))
        },
    );
    // Function value thunks: in the table, never compared.
    for (a, t) in taken.iter_mut().enumerate() {
        if let Chunk::Func {
            name, body: None, ..
        } = &l.chunks[a]
        {
            if l.atoms[a].file != u32::MAX && crate::icf::fn_value_thunk(name) {
                *t = false;
            }
        }
    }
    for t in roots.iter().chain(l.atoms.iter().flat_map(|a| &a.keep)) {
        if let Some((a, _)) = l.syms.resolve(*t) {
            taken[a as usize] = true;
        }
    }
    for id in 0..l.syms.syms.len() as SymId {
        if let Def::Defined { atom, .. } = l.syms.get(id).def {
            if l.exported(id) || l.ext(id).pinned {
                taken[atom as usize] = true;
            }
        }
    }
    let cands: Vec<AtomId> = (1..l.atoms.len() as AtomId)
        .filter(|&a| {
            let at = &l.atoms[a as usize];
            at.live
                && !taken[a as usize]
                && at.file != u32::MAX
                && !l.discarded.contains(&a)
                && matches!(
                    &l.chunks[a as usize],
                    Chunk::Func {
                        body: None,
                        export_name: None,
                        ..
                    }
                )
        })
        .collect();
    let mut canon: HashMap<AtomId, AtomId> = HashMap::default();
    {
        let mut first: HashMap<&[u8], AtomId> = HashMap::default();
        for a in 1..l.atoms.len() as AtomId {
            let at = &l.atoms[a as usize];
            let Chunk::Data { name, merge, .. } = &l.chunks[a as usize] else {
                continue;
            };
            if !at.live || *merge || at.file == u32::MAX || !name.starts_with(".rodata.nabi.str.") {
                continue;
            }
            if !at.relocs.is_empty() {
                continue;
            }
            let c = *first.entry(code(l, a)).or_insert(a);
            if c != a {
                canon.insert(a, c);
            }
        }
    }
    let v = Fold { l, canon: &canon };
    let mut folds = crate::icf::fold(&l.atoms, &cands, &v);
    folds.extend(canon);
    if folds.is_empty() {
        return;
    }
    crate::icf::apply(&mut l.atoms, &folds);
    let to: HashMap<AtomId, AtomId> = folds.into_iter().collect();
    for id in 0..l.syms.syms.len() as SymId {
        if let Def::Defined { atom, off } = l.syms.get(id).def {
            if let Some(&k) = to.get(&atom) {
                l.syms.get_mut(id).def = Def::Defined { atom: k, off };
                l.moved.insert(id);
            }
        }
    }
}

/// Error `e` after the warnings so far.
fn warned(l: &Link, e: String) -> String {
    let mut s = warnings(l);
    s.push_str(&e);
    s
}

/// The warnings, one `warning:` line (or more) each.
#[must_use]
pub fn warnings(l: &Link) -> String {
    let mut s = String::new();
    for w in &l.warnings {
        s.push_str("warning: ");
        s.push_str(w);
        s.push('\n');
    }
    s
}

/// Define optional data symbol `name` (its address set at layout).
fn optional_data(l: &mut Link, id: SymId, name: &'static str) {
    let s = l.syms.get_mut(id);
    s.def = Def::Absolute(0);
    s.file = u32::MAX;
    s.private_extern = true;
    let x = l.ext_mut(id);
    x.kind = Some(SYM_DATA);
    x.flags = HIDDEN | ABSOLUTE;
    x.live = true;
    l.synth.optional.push((id, name));
}

/// Calls whose signature differs from the definition's: each file's
/// references become a trap with a warning; between undefined ones, an
/// error (wasm-ld's function variants).
fn variants(l: &mut Link) -> Result<(), String> {
    let mut stubs: HashMap<(SymId, u32), SymId> = HashMap::default();
    let mut errors = Vec::new();
    for fi in 0..l.files.len() {
        let undef = std::mem::take(&mut l.files[fi].undef_funcs);
        for &(si, sig, called) in &undef {
            let id = l.files[fi].syms[si as usize];
            let Some(want) = l.ext(id).sig else { continue };
            if !called || want == sig || l.ext(id).kind != Some(SYM_FUNCTION) {
                continue;
            }
            let name = l.name(id).to_owned();
            if !matches!(l.syms.get(id).def, Def::Defined { .. }) {
                let s = l.syms.get(id);
                errors.push(format!(
                    "function signature mismatch: {name}\n>>> defined as {} in {}\n>>> defined as {} in {}",
                    l.sig_text(want),
                    l.file_name(s.file),
                    l.sig_text(sig),
                    l.file_name(fi as u32)
                ));
                continue;
            }
            let def_file = l.syms.get(id).file;
            l.warnings.push(format!(
                "function signature mismatch: {name}\n>>> defined as {} in {}\n>>> defined as {} in {}",
                l.sig_text(sig),
                l.file_name(fi as u32),
                l.sig_text(want),
                l.file_name(def_file)
            ));
            let stub = if let Some(&s) = stubs.get(&(id, sig)) {
                s
            } else {
                let s = l.stub(&format!("signature_mismatch:{name}"), sig);
                stubs.insert((id, sig), s);
                s
            };
            l.files[fi].syms[si as usize] = stub;
            let atoms: Vec<AtomId> = l.files[fi]
                .funcs
                .iter()
                .chain(&l.files[fi].segs)
                .chain(&l.files[fi].customs)
                .copied()
                .collect();
            for a in atoms {
                for r in &mut l.atoms[a as usize].relocs {
                    if r.target == Target::Sym(id) {
                        r.target = Target::Sym(stub);
                    }
                }
            }
        }
        l.files[fi].undef_funcs = undef;
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

/// Weak undefined functions called from objects trap; their address is
/// the null pointer.
fn weak_undefined(l: &mut Link) {
    for id in 0..l.syms.syms.len() as SymId {
        let s = l.syms.get(id);
        let undef = matches!(s.def, Def::Undefined | Def::Lazy { .. });
        if !undef || !s.weak_ref || !s.referenced || !l.ext(id).in_obj {
            continue;
        }
        if l.ext(id).kind != Some(SYM_FUNCTION) {
            continue;
        }
        let Some(sig) = l.ext(id).sig else { continue };
        let name = format!("undefined_weak:{}", s.name);
        let stub = l.stub(&name, sig);
        let a = l.atom_of(stub);
        let s = l.syms.get_mut(id);
        s.def = Def::Defined { atom: a, off: 0 };
        let x = l.ext_mut(id);
        x.stub = true;
        x.flags = LOCAL | HIDDEN;
        x.local = Some(name);
    }
    // A stub's address is never taken: it is the null pointer.
    for a in &mut l.atoms {
        for r in &mut a.relocs {
            if let Target::Sym(id) = r.target {
                if is_table_index(r.kind) && l.ext.get(id as usize).is_some_and(|x| x.stub) {
                    r.target = Target::Atom(DEAD);
                }
            }
        }
    }
}

/// Garbage collection roots: the entry, exported and no-strip symbols,
/// `__wasm_call_dtors`, and the constructors and retained segments of the
/// command line's objects; an archive member's join once any of its
/// definitions is live.
fn roots(l: &mut Link, entry: Option<SymId>) -> Vec<Target> {
    let mut roots: Vec<Target> = entry.into_iter().map(Target::Sym).collect();
    for id in 0..l.syms.syms.len() as SymId {
        let x = l.ext(id);
        let local = x.flags & LOCAL != 0;
        if (x.flags & NO_STRIP != 0 && !local) || l.exported(id) {
            roots.push(Target::Sym(id));
        }
    }
    if let Some(d) = l.synth.call_dtors {
        roots.push(Target::Sym(d));
    }
    for fi in 0..l.files.len() {
        let f = &l.files[fi];
        let mut deps: Vec<Target> = Vec::new();
        for &(_, si) in &f.init_funcs {
            let Some(&id) = f.syms.get(si as usize) else {
                continue;
            };
            let discarded = l
                .syms
                .resolve(Target::Sym(id))
                .is_some_and(|(a, _)| l.discarded.contains(&a));
            if !discarded {
                deps.push(Target::Sym(id));
            }
        }
        for &a in &f.segs {
            if let Chunk::Data { flags, .. } = &l.chunks[a as usize] {
                if flags & SEG_RETAIN != 0 {
                    deps.push(Target::Atom(a));
                }
            }
        }
        if deps.is_empty() {
            continue;
        }
        if f.explicit {
            roots.extend(deps);
            continue;
        }
        // Every chunk a symbol of this file resolves to.
        let mut defs: Vec<AtomId> = f
            .syms
            .iter()
            .filter_map(|&id| match l.syms.get(id).def {
                Def::Defined { atom, .. } if l.atoms[atom as usize].file == fi as u32 => Some(atom),
                _ => None,
            })
            .collect();
        defs.sort_unstable();
        defs.dedup();
        for a in defs {
            l.atoms[a as usize].keep.extend(deps.iter().copied());
        }
    }
    roots
}

/// Symbol liveness after marking: what live chunks and the roots
/// reference.
fn mark_symbols(l: &mut Link, roots: &[Target]) {
    let mut used: Vec<SymId> = Vec::new();
    for a in l.atoms.iter().filter(|a| a.live) {
        for r in &a.relocs {
            if let Target::Sym(id) = r.target {
                used.push(id);
            }
        }
        for &k in &a.keep {
            if let Target::Sym(id) = k {
                used.push(id);
            }
        }
    }
    for &t in roots {
        if let Target::Sym(id) = t {
            used.push(id);
        }
    }
    for id in used {
        l.ext_mut(id).live = true;
    }
    for id in 0..l.ext.len() {
        if l.ext[id].pinned {
            l.ext[id].live = true;
        }
    }
    // `__wasm_call_ctors` runs the live constructors.
    let ctors = l.atom_of(l.synth.call_ctors);
    let any_init = l.files.iter().any(|f| {
        f.init_funcs.iter().any(|&(_, si)| {
            f.syms
                .get(si as usize)
                .and_then(|&id| l.syms.resolve(Target::Sym(id)))
                .is_some_and(|(a, _)| l.atoms[a as usize].live && !l.discarded.contains(&a))
        })
    });
    if any_init {
        l.atoms[ctors as usize].live = true;
    }
}
