//! The OS helpers behind `std` and `core::io`: one module
//! per platform family, its items re-exported here.

#[cfg(target_os = "none")]
mod none;
#[cfg(not(target_os = "none"))]
mod openhow;
#[cfg(not(any(windows, target_os = "none")))]
mod posix;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "none")]
pub use none::*;
#[cfg(not(any(windows, target_os = "none")))]
pub use posix::*;
#[cfg(windows)]
pub use windows::*;
