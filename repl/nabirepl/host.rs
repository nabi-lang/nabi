//! The REPL host's `main` (`build.rs` compiles it alone): the host is the
//! runtime's, linked whole beside it.

#![no_std]

unsafe extern "C" {
    fn nabi_repl_host_main(argc: i32, argv: *const *const u8) -> i32;
}

/// # Safety
/// The C runtime's call.
#[cfg(not(windows))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn main(argc: i32, argv: *const *const u8) -> i32 {
    // SAFETY: what the C runtime passed.
    unsafe { nabi_repl_host_main(argc, argv) }
}

/// nabi's own Windows startup calls `nabi_main` with no arguments: the
/// runtime reads the command line itself.
///
/// # Safety
/// The startup's call.
#[cfg(windows)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_main() -> i32 {
    // SAFETY: the Windows host ignores argc / argv.
    unsafe { nabi_repl_host_main(0, core::ptr::null()) }
}
