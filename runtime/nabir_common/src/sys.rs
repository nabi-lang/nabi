//! The OS layer: one module per platform family, its items re-exported
//! here.

#[cfg(target_os = "none")]
mod none;
#[cfg(any(unix, target_os = "wasi"))]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "none")]
pub use none::*;
#[cfg(any(unix, target_os = "wasi"))]
pub use unix::*;
#[cfg(windows)]
pub use windows::*;
