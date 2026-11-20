//! Mach-O links as plugin linking sees them.

use crate::macho::{Link, object};
use crate::plugin::{Image, Use};
use crate::symtab::{Atom, AtomId, Fetch, SymbolTable};

const ARM64_RELOC_PAGE21: u16 = 3;
const ARM64_RELOC_PAGEOFF12: u16 = 4;
const ARM64_RELOC_GOT_LOAD_PAGE21: u16 = 5;
const ARM64_RELOC_GOT_LOAD_PAGEOFF12: u16 = 6;
const ARM64_RELOC_POINTER_TO_GOT: u16 = 7;
const ARM64_RELOC_TLVP_LOAD_PAGE21: u16 = 8;
const ARM64_RELOC_TLVP_LOAD_PAGEOFF12: u16 = 9;

impl Image for Link {
    fn syms(&mut self) -> &mut SymbolTable {
        &mut self.syms
    }

    fn atoms(&mut self) -> &mut Vec<Atom> {
        &mut self.atoms
    }

    fn prefix(&self) -> &'static str {
        "_"
    }

    fn code(&self, a: AtomId) -> bool {
        let at = &self.atoms[a as usize];
        self.files
            .get(at.file as usize)
            .and_then(|f| f.sects.get(at.sect as usize))
            .is_some_and(object::InSect::code)
    }

    fn fetch(&mut self, f: Fetch) -> Result<(), String> {
        self.drain(vec![f])
    }

    fn add(&mut self, name: &str, data: Vec<u8>) -> Result<(), String> {
        let fi = self.files.len() as u32;
        let f = object::parse(self, fi, name.to_owned(), name.to_owned(), data)?;
        self.drain(f)
    }

    fn classify(&self, kind: u16) -> Use {
        match kind {
            ARM64_RELOC_GOT_LOAD_PAGE21 => Use::Got(ARM64_RELOC_PAGE21),
            ARM64_RELOC_GOT_LOAD_PAGEOFF12 => Use::Got(ARM64_RELOC_PAGEOFF12),
            ARM64_RELOC_POINTER_TO_GOT => Use::BadGot,
            ARM64_RELOC_TLVP_LOAD_PAGE21 | ARM64_RELOC_TLVP_LOAD_PAGEOFF12 => Use::Tls,
            _ => Use::Plain,
        }
    }

    fn abs(&self) -> (u16, u8) {
        (0, 3)
    }

    fn ptr(&self) -> u32 {
        8
    }
}
