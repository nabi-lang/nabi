//! The linker tests, one binary with a module per format: `cargo test -p
//! nabild --test main elf::riscv::` runs one of them.

mod coff;
mod elf;
mod link;
mod malformed;
mod wasm;
