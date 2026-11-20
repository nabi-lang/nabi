//! COFF links as plugin linking sees them. No GOT: plugin code reads
//! the process's data through `__imp_X`, which is the slot itself.

use crate::coff::{Arch, Link};
use crate::plugin::{Image, Use};
use crate::symtab::{Atom, AtomId, Fetch, SymbolTable};

const AMD64_ADDR64: u16 = 1;
const AMD64_SECREL: u16 = 0xb;
const ARM64_ADDR64: u16 = 0xe;
const ARM64_SECREL: u16 = 8;
const ARM64_SECREL_LOW12L: u16 = 0xb;
const I386_DIR32: u16 = 6;
const I386_SECREL: u16 = 0xb;

impl Image for Link {
    fn syms(&mut self) -> &mut SymbolTable {
        &mut self.syms
    }

    fn atoms(&mut self) -> &mut Vec<Atom> {
        &mut self.atoms
    }

    fn prefix(&self) -> &'static str {
        if self.arch == Arch::X86 { "_" } else { "" }
    }

    fn code(&self, a: AtomId) -> bool {
        self.atoms[a as usize].file != u32::MAX && self.sect(a).code()
    }

    fn fetch(&mut self, f: Fetch) -> Result<(), String> {
        self.drain(vec![f])
    }

    fn add(&mut self, name: &str, data: Vec<u8>) -> Result<(), String> {
        let f = self.add_bytes(name.to_owned(), String::new(), data)?;
        self.drain(f)
    }

    fn classify(&self, kind: u16) -> Use {
        let tls = match self.arch {
            Arch::X86_64 => kind == AMD64_SECREL,
            Arch::Aarch64 => (ARM64_SECREL..=ARM64_SECREL_LOW12L).contains(&kind),
            Arch::X86 => kind == I386_SECREL,
        };
        if tls { Use::Tls } else { Use::Plain }
    }

    fn abs(&self) -> (u16, u8) {
        match self.arch {
            Arch::X86_64 => (AMD64_ADDR64, 3),
            Arch::Aarch64 => (ARM64_ADDR64, 3),
            Arch::X86 => (I386_DIR32, 2),
        }
    }

    fn ptr(&self) -> u32 {
        if self.arch == Arch::X86 { 4 } else { 8 }
    }

    fn coff(&self) -> bool {
        true
    }
}
