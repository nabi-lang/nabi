//! Sockets behind `std::net` and the I/O driver's readiness
//! poller: one module per platform family, its items re-exported here.

#[cfg(not(windows))]
mod posix;
#[cfg(windows)]
mod windows;

#[cfg(not(windows))]
#[cfg_attr(target_family = "wasm", allow(unused_imports))]
pub use posix::*;
#[cfg(windows)]
pub use windows::*;
