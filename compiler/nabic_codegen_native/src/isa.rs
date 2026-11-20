//! LIR -> machine code, one module per instruction set.

pub(crate) mod a64;
pub(crate) mod arm32;
pub(crate) mod i386;
pub(crate) mod ppc64;
pub(crate) mod riscv;
pub(crate) mod s390x;
pub(crate) mod wasm;
pub(crate) mod x64;
