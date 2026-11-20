//! errno and `GetLastError` as the last `#[errno]` C call left them.
//!
//! Compiled code calls `nabi_errno_save` right after such a call returns,
//! before leaving native state (which may wait out a collection) or
//! unpinning arguments: both may run code that changes errno.

use nabir_common::tls;

#[cfg(windows)]
unsafe extern "C" {
    /// The UCRT's errno accessor, `_errno`.
    #[link_name = "_errno"]
    fn ucrt_errno() -> *mut i32;
}

/// The calling thread's errno location.
#[cfg(not(target_os = "none"))]
fn location() -> *mut i32 {
    // SAFETY: the libc accessors have no preconditions.
    #[cfg(windows)]
    unsafe {
        ucrt_errno()
    }
    #[cfg(any(target_vendor = "apple", target_os = "freebsd"))]
    unsafe {
        libc::__error()
    }
    #[cfg(target_os = "android")]
    unsafe {
        libc::__errno()
    }
    #[cfg(not(any(
        windows,
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "android"
    )))]
    unsafe {
        libc::__errno_location()
    }
}

/// Save errno (and `GetLastError` on Windows) into this thread's
/// slots. A leaf: no heap, no lock, no safepoint.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_errno_save() {
    let (e, w) = tls::errno_slots();
    // First: the errno accessor may set the last error.
    #[cfg(windows)]
    // SAFETY: this thread's own slot; no preconditions.
    unsafe {
        w.write(nabir_common::win::GetLastError());
    }
    #[cfg(not(windows))]
    let _ = w;
    #[cfg(not(target_os = "none"))]
    // SAFETY: this thread's own slot; the location is the thread's errno.
    unsafe {
        e.write(*location());
    }
    #[cfg(target_os = "none")]
    let _ = e;
}

/// `bare::ffi::errno()`: the saved errno.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_errno_get() -> i32 {
    // SAFETY: this thread's own slot.
    unsafe { tls::errno_slots().0.read() }
}

/// `bare::ffi::set_errno(v)`: the saved slot and the C library's errno.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_errno_set(v: i32) {
    // SAFETY: this thread's own slot and errno.
    unsafe {
        tls::errno_slots().0.write(v);
        #[cfg(not(target_os = "none"))]
        location().write(v);
    }
}

/// `bare::ffi::last_error()`: the saved `GetLastError` (0 off Windows).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_last_error_get() -> u32 {
    // SAFETY: this thread's own slot.
    unsafe { tls::errno_slots().1.read() }
}

/// `bare::ffi::set_last_error(v)`: the saved slot and, on Windows,
/// `SetLastError`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_last_error_set(v: u32) {
    // SAFETY: this thread's own slot.
    unsafe { tls::errno_slots().1.write(v) };
    #[cfg(windows)]
    // SAFETY: no preconditions.
    unsafe {
        nabir_common::win::SetLastError(v);
    }
}
