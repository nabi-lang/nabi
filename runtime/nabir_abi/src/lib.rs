//! What the compiler and the runtime must agree on, in one place: the
//! object header and TLS block layout, the module and type descriptors
//! plugins and reflection read, and the pure functions behind
//! the runtime's format, `str` and `char` entries, which the compiler's
//! constant evaluator runs too. Uses `core` only and depends on nothing,
//! so both sides can build it.

#![cfg_attr(not(test), no_std)]

pub mod float;
pub mod fmt;
pub mod gc;
pub mod half;
pub mod module;
pub mod str;
pub mod tls;
pub mod typedesc;
pub mod unicode;
