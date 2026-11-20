//! Hot reload under `nabi dev`: the dev channel, the versions of each
//! hot package, and applying a new one at a stop of the world.
//!
//! A hot package is a plugin built with `--hot`: its calls to hot
//! functions go through `HOT_FN` slots. `nabi dev` sends `load <path>`
//! for each new version; the version applies when the world is stopped:
//! every slot of every loaded version is bound to the newest code with
//! the same signature. A change to a type's layout also needs no frame
//! of old code to depend on the type, and migrates the type's objects
//! in a full collection (`nabir_gc::migrate`). What `std::reload` calls
//! is here; on targets without plugins it reports "not active".

#[cfg(not(any(target_family = "wasm", target_os = "none")))]
mod channel;
#[cfg(not(any(target_family = "wasm", target_os = "none")))]
mod frames;
#[cfg(not(any(target_family = "wasm", target_os = "none")))]
mod swap;

use crate::str::{NabiStr, build};
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

/// `std::reload`'s hook table (a GC buffer): a root word, registered
/// before anything is stored in it.
static HOOKS: AtomicUsize = AtomicUsize::new(0);
/// 0: not rooted yet, 1: being rooted, 2: rooted.
static ROOTED: AtomicU8 = AtomicU8::new(0);

/// The hook table: `table` (not 0) becomes it unless one is set already;
/// the table now, 0 for none. The caller keeps `table` alive across the
/// call.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_hot_hooks(table: usize) -> usize {
    if table != 0 {
        if ROOTED
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            nabir_gc::world::add_root(HOOKS.as_ptr().cast_const());
            ROOTED.store(2, Ordering::Release);
        }
        while ROOTED.load(Ordering::Acquire) != 2 {
            core::hint::spin_loop();
        }
        let _ = HOOKS.compare_exchange(0, table, Ordering::AcqRel, Ordering::Acquire);
    }
    HOOKS.load(Ordering::Acquire)
}

/// 1 while a `nabi dev` channel is connected.
#[unsafe(no_mangle)]
#[cfg_attr(
    any(target_family = "wasm", target_os = "none"),
    allow(clippy::missing_const_for_fn)
)]
pub extern "C" fn nabi_hot_active() -> i32 {
    #[cfg(not(any(target_family = "wasm", target_os = "none")))]
    return i32::from(channel::connected());
    #[cfg(any(target_family = "wasm", target_os = "none"))]
    0
}

/// Connect to `nabi dev` at `endpoint`: "" on success, else why not.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_hot_connect(endpoint: NabiStr) -> NabiStr {
    #[cfg(not(any(target_family = "wasm", target_os = "none")))]
    match channel::connect(endpoint.bytes()) {
        Ok(()) => build(&[]),
        Err(m) => build(&[m.as_slice()]),
    }
    #[cfg(any(target_family = "wasm", target_os = "none"))]
    {
        let _ = endpoint;
        build(&[b"hot reload needs a target with plugins"])
    }
}

/// The changes `point()` would apply now, as `std::reload` parses them
/// (`package \t version \t functions \t types` lines).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_hot_ready() -> NabiStr {
    #[cfg(not(any(target_family = "wasm", target_os = "none")))]
    return swap::ready();
    #[cfg(any(target_family = "wasm", target_os = "none"))]
    build(&[])
}

/// Apply what is ready, on this thread.
#[unsafe(no_mangle)]
#[cfg_attr(
    any(target_family = "wasm", target_os = "none"),
    allow(clippy::missing_const_for_fn)
)]
pub extern "C" fn nabi_hot_point() {
    #[cfg(not(any(target_family = "wasm", target_os = "none")))]
    swap::point();
}

/// The changes applied since the last call, as `nabi_hot_ready` writes
/// them.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_hot_events() -> NabiStr {
    #[cfg(not(any(target_family = "wasm", target_os = "none")))]
    return swap::events();
    #[cfg(any(target_family = "wasm", target_os = "none"))]
    build(&[])
}

/// While on (a `before_reload` hook is registered), changes apply only
/// in `point()`.
#[unsafe(no_mangle)]
#[cfg_attr(
    any(target_family = "wasm", target_os = "none"),
    allow(clippy::missing_const_for_fn)
)]
pub extern "C" fn nabi_hot_hold(on: i32) {
    #[cfg(not(any(target_family = "wasm", target_os = "none")))]
    swap::hold(on != 0);
    #[cfg(any(target_family = "wasm", target_os = "none"))]
    let _ = on;
}
