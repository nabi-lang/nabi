//! ELF links as plugin linking sees them.

use crate::elf::arch::{a64, i386, riscv, x64};
use crate::elf::{Arch, Link, object};
use crate::plugin::{Image, Use};
use crate::symtab::{Atom, AtomId, Fetch, SymbolTable};

impl Image for Link {
    fn syms(&mut self) -> &mut SymbolTable {
        &mut self.syms
    }

    fn atoms(&mut self) -> &mut Vec<Atom> {
        &mut self.atoms
    }

    fn prefix(&self) -> &'static str {
        ""
    }

    fn code(&self, a: AtomId) -> bool {
        let at = &self.atoms[a as usize];
        self.files
            .get(at.file as usize)
            .and_then(|f| f.sects.get(at.sect as usize))
            .is_some_and(|s| s.flags & object::SHF_EXECINSTR != 0)
    }

    fn fetch(&mut self, f: Fetch) -> Result<(), String> {
        self.drain(vec![f])
    }

    fn add(&mut self, name: &str, data: Vec<u8>) -> Result<(), String> {
        let fi = self.files.len() as u32;
        let f = object::parse(self, fi, name.to_owned(), data)?;
        self.drain(f)
    }

    fn classify(&self, kind: u16) -> Use {
        let tls = crate::elf::reloc::needs_symbol(self.arch, kind);
        match self.arch {
            Arch::X86_64 => match kind {
                x64::GOTPCREL | x64::GOTPCRELX | x64::REX_GOTPCRELX => Use::Got(x64::PC32),
                x64::GOT32 | x64::GOT64 | x64::GOTPCREL64 => Use::BadGot,
                _ if tls => Use::Tls,
                x64::TPOFF32 | x64::TPOFF64 | x64::DTPOFF32 | x64::DTPOFF64 | x64::TLSLD => {
                    Use::Tls
                }
                _ => Use::Plain,
            },
            Arch::Aarch64 => match kind {
                a64::ADR_GOT_PAGE => Use::Got(a64::ADR_PREL_PG_HI21),
                a64::LD64_GOT_LO12_NC => Use::Got(a64::LDST64_ABS_LO12_NC),
                a64::GOT_LD_PREL19 => Use::Got(a64::LD_PREL_LO19),
                310 | 313 => Use::BadGot,
                _ if tls => Use::Tls,
                512..=575 => Use::Tls,
                _ => Use::Plain,
            },
            Arch::I386 => match kind {
                i386::GOT32 | i386::GOT32X => Use::Got(i386::GOTOFF),
                _ if tls => Use::Tls,
                14..=19 | 32..=41 => Use::Tls,
                _ => Use::Plain,
            },
            Arch::Riscv64 | Arch::Riscv32 => match kind {
                riscv::GOT_HI20 => Use::Got(riscv::PCREL_HI20),
                _ if tls => Use::Tls,
                riscv::TPREL_HI20..=riscv::TPREL_ADD => Use::Tls,
                _ => Use::Plain,
            },
            _ if tls => Use::BadGot,
            _ => Use::Plain,
        }
    }

    fn abs(&self) -> (u16, u8) {
        let k = match self.arch {
            Arch::X86_64 => x64::R_64,
            Arch::Aarch64 => a64::ABS64,
            Arch::I386 => i386::R_32,
            Arch::Riscv64 => riscv::R_64,
            Arch::Riscv32 => riscv::R_32,
            _ => 0,
        };
        (k, 0)
    }

    fn ptr(&self) -> u32 {
        self.arch.ptr() as u32
    }
}
