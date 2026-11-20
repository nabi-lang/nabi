//! Atoms, relocations and the global symbol table: what every object
//! format reads into and the resolution rules (strong over weak over
//! common, archive members loaded on demand, dylibs last).

use nabic_core::HashMap;

pub type SymId = u32;
pub type AtomId = u32;

/// What a relocation (or a live-support edge) points at.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Target {
    Sym(SymId),
    Atom(AtomId),
}

#[derive(Clone, Copy, Debug)]
pub struct Reloc {
    /// Offset in the atom.
    pub off: u32,
    /// Format specific.
    pub kind: u16,
    /// log2 of the field width.
    pub len: u8,
    pub pcrel: bool,
    pub target: Target,
    /// Target offset (atom targets) plus the addend.
    pub addend: i64,
    /// Subtrahend of a difference: `target - sub + addend`.
    pub sub: Option<Target>,
}

/// An indivisible piece of an input section: the unit of dead stripping
/// and placement.
#[derive(Debug)]
pub struct Atom {
    pub file: u32,
    /// Input section index in its file.
    pub sect: u32,
    /// Offset in the input section.
    pub off: u64,
    pub size: u64,
    /// log2.
    pub align: u8,
    pub relocs: Vec<Reloc>,
    /// Kept alive with this atom (unwind info's personality, LSDA).
    pub keep: Vec<Target>,
    /// Never dead stripped.
    pub root: bool,
    pub live: bool,
    /// Output section, `u32::MAX` before layout.
    pub out: u32,
    pub addr: u64,
}

impl Atom {
    #[must_use]
    pub const fn new(file: u32, sect: u32, off: u64, size: u64, align: u8) -> Self {
        Self {
            file,
            sect,
            off,
            size,
            align,
            relocs: Vec::new(),
            keep: Vec::new(),
            root: false,
            live: false,
            out: u32::MAX,
            addr: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Def {
    Undefined,
    /// In archive `archive`, member index `member`, not loaded yet.
    Lazy {
        archive: u32,
        member: u32,
    },
    Defined {
        atom: AtomId,
        off: u64,
    },
    Absolute(u64),
    /// A tentative definition: becomes zero-fill unless defined.
    Common {
        size: u64,
        align: u8,
    },
    /// Exported by dylib `lib` (index in link order).
    Dylib {
        lib: u32,
    },
    /// Made by the linker (the image header).
    Synthetic,
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct Symbol {
    pub name: String,
    pub def: Def,
    /// The file that defined it (or first referenced it).
    pub file: u32,
    pub weak_def: bool,
    /// Visible to the link only, local in the output.
    pub private_extern: bool,
    /// Every reference is weak.
    pub weak_ref: bool,
    pub referenced: bool,
    pub no_dead_strip: bool,
    /// The definition is a thread-local descriptor.
    pub tlv: bool,
}

#[derive(Default)]
pub struct SymbolTable {
    pub syms: Vec<Symbol>,
    map: HashMap<String, SymId>,
}

/// An archive member to load: `(archive, member)`.
pub type Fetch = (u32, u32);

impl SymbolTable {
    pub fn intern(&mut self, name: &str) -> SymId {
        if let Some(&id) = self.map.get(name) {
            return id;
        }
        let id = self.syms.len() as SymId;
        self.syms.push(Symbol {
            name: name.to_owned(),
            def: Def::Undefined,
            file: u32::MAX,
            weak_def: false,
            private_extern: false,
            weak_ref: true,
            referenced: false,
            no_dead_strip: false,
            tlv: false,
        });
        self.map.insert(name.to_owned(), id);
        id
    }

    #[must_use]
    pub fn find(&self, name: &str) -> Option<SymId> {
        self.map.get(name).copied()
    }

    #[must_use]
    pub fn get(&self, id: SymId) -> &Symbol {
        &self.syms[id as usize]
    }

    pub fn get_mut(&mut self, id: SymId) -> &mut Symbol {
        &mut self.syms[id as usize]
    }

    /// A definition from `file`. `Err` names the earlier definition of
    /// a duplicate strong symbol.
    pub fn define(
        &mut self,
        id: SymId,
        def: Def,
        file: u32,
        weak: bool,
        private_extern: bool,
    ) -> Result<(), u32> {
        let s = &mut self.syms[id as usize];
        let replace = match s.def {
            Def::Undefined | Def::Lazy { .. } | Def::Dylib { .. } | Def::Common { .. } => true,
            Def::Defined { .. } | Def::Absolute(_) | Def::Synthetic => {
                if s.weak_def && !weak {
                    true
                } else if weak {
                    false
                } else {
                    return Err(s.file);
                }
            }
        };
        if replace {
            s.def = def;
            s.file = file;
            s.weak_def = weak;
            s.private_extern = private_extern;
        } else {
            // A hidden weak copy anywhere keeps the symbol out of the
            // exports.
            s.private_extern &= private_extern;
        }
        Ok(())
    }

    /// A tentative definition: the largest size and alignment win, a real
    /// definition overrides it.
    pub fn common(&mut self, id: SymId, size: u64, align: u8, file: u32, private_extern: bool) {
        let s = &mut self.syms[id as usize];
        match s.def {
            Def::Undefined | Def::Lazy { .. } | Def::Dylib { .. } => {
                s.def = Def::Common { size, align };
                s.file = file;
                s.private_extern = private_extern;
            }
            Def::Common {
                size: s0,
                align: a0,
            } => {
                s.def = Def::Common {
                    size: s0.max(size),
                    align: a0.max(align),
                };
            }
            _ => {}
        }
    }

    /// A reference from `file`; the archive member to load if the
    /// symbol is lazy.
    pub fn reference(&mut self, id: SymId, file: u32, weak: bool) -> Option<Fetch> {
        let s = &mut self.syms[id as usize];
        if !s.referenced && s.def == Def::Undefined {
            s.file = file;
        }
        s.referenced = true;
        s.weak_ref &= weak;
        match s.def {
            Def::Lazy { archive, member } => {
                s.def = Def::Undefined;
                Some((archive, member))
            }
            _ => None,
        }
    }

    /// An archive symbol table entry: the member to load if the symbol is
    /// already wanted.
    pub fn lazy(&mut self, name: &str, archive: u32, member: u32) -> Option<Fetch> {
        let id = self.intern(name);
        let s = &mut self.syms[id as usize];
        match s.def {
            Def::Undefined if s.referenced => Some((archive, member)),
            Def::Undefined => {
                s.def = Def::Lazy { archive, member };
                None
            }
            _ => None,
        }
    }

    /// Bind each still undefined symbol to the first dylib that
    /// `exports`.
    pub fn bind_dylibs(&mut self, exports: impl Fn(&str) -> Option<u32>) {
        for s in &mut self.syms {
            if s.referenced && s.def == Def::Undefined {
                if let Some(lib) = exports(&s.name) {
                    s.def = Def::Dylib { lib };
                }
            }
        }
    }

    /// Referenced symbols nothing defines.
    #[must_use]
    pub fn undefined(&self) -> Vec<SymId> {
        (0..self.syms.len() as SymId)
            .filter(|&i| {
                let s = &self.syms[i as usize];
                s.referenced && matches!(s.def, Def::Undefined | Def::Lazy { .. })
            })
            .collect()
    }

    /// The atom and offset a target resolves to, if it is in the image.
    #[must_use]
    pub fn resolve(&self, t: Target) -> Option<(AtomId, u64)> {
        match t {
            Target::Atom(a) => Some((a, 0)),
            Target::Sym(id) => match self.syms[id as usize].def {
                Def::Defined { atom, off } => Some((atom, off)),
                _ => None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strong_beats_weak_and_common() {
        let mut t = SymbolTable::default();
        let x = t.intern("x");
        t.common(x, 8, 3, 0, false);
        t.define(x, Def::Defined { atom: 1, off: 0 }, 1, true, false)
            .unwrap();
        t.define(x, Def::Defined { atom: 2, off: 0 }, 2, true, false)
            .unwrap();
        assert_eq!(t.get(x).def, Def::Defined { atom: 1, off: 0 });
        t.define(x, Def::Defined { atom: 3, off: 0 }, 3, false, false)
            .unwrap();
        assert_eq!(t.get(x).def, Def::Defined { atom: 3, off: 0 });
        assert_eq!(
            t.define(x, Def::Defined { atom: 4, off: 0 }, 4, false, false),
            Err(3)
        );
        // A later common never replaces a definition.
        t.common(x, 64, 4, 5, false);
        assert_eq!(t.get(x).def, Def::Defined { atom: 3, off: 0 });
    }

    #[test]
    fn commons_merge_to_the_largest() {
        let mut t = SymbolTable::default();
        let c = t.intern("c");
        t.common(c, 4, 2, 0, false);
        t.common(c, 16, 1, 1, false);
        assert_eq!(t.get(c).def, Def::Common { size: 16, align: 2 });
    }

    #[test]
    fn archive_members_load_on_demand() {
        let mut t = SymbolTable::default();
        assert_eq!(t.lazy("f", 0, 7), None);
        let f = t.find("f").unwrap();
        assert_eq!(t.reference(f, 0, false), Some((0, 7)));
        // Already wanted: the next archive defining it is fetched at once.
        let g = t.intern("g");
        t.reference(g, 0, false);
        assert_eq!(t.lazy("g", 1, 2), Some((1, 2)));
        t.define(g, Def::Defined { atom: 0, off: 0 }, 1, false, false)
            .unwrap();
        assert_eq!(t.lazy("g", 2, 0), None);
    }

    #[test]
    fn dylibs_bind_what_is_left() {
        let mut t = SymbolTable::default();
        let a = t.intern("a");
        let b = t.intern("b");
        t.reference(a, 0, false);
        t.reference(b, 0, true);
        t.bind_dylibs(|n| (n == "a").then_some(3));
        assert_eq!(t.get(a).def, Def::Dylib { lib: 3 });
        assert_eq!(t.undefined(), vec![b]);
        assert!(t.get(b).weak_ref && !t.get(a).weak_ref);
    }
}
