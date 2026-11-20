//! Stack overflow report: one module per platform family, its items
//! re-exported here.

#[cfg(not(any(windows, target_family = "wasm", target_os = "none")))]
mod posix;
#[cfg(any(target_family = "wasm", target_os = "none"))]
mod wasm;
#[cfg(windows)]
mod windows;

#[cfg(not(any(windows, target_family = "wasm", target_os = "none")))]
pub use posix::*;
#[cfg(any(target_family = "wasm", target_os = "none"))]
pub use wasm::*;
#[cfg(windows)]
pub use windows::*;
