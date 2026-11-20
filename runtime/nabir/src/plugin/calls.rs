//! The calls `std::plugin` makes: `str` in and out, the last failure
//! kept for `nabi_plugin_error*`. Failures are rare, so one slot under
//! a lock serves every thread; no allocation or blocking call happens
//! while it is held.

use crate::plugin::{PluginErr, nabi_plugin_state, raw_find, raw_info, raw_load, raw_unload};
use crate::str::{NabiStr, build};
use core::cell::UnsafeCell;
use nabir_abi::typedesc::Str;
use nabir_common::sys::Lock;

struct Last(UnsafeCell<PluginErr>);
// SAFETY: only touched under `LAST_LOCK`.
unsafe impl Sync for Last {}

static LAST_LOCK: Lock = Lock::new();
static LAST: Last = Last(UnsafeCell::new(EMPTY));

const EMPTY: PluginErr = PluginErr {
    kind: 0,
    reserved: 0,
    a: Str::EMPTY,
    b: Str::EMPTY,
    list: Str::EMPTY,
};

/// Run `f` with a fresh failure record, then keep it as the last one
/// when `f` failed.
fn record<R>(f: impl FnOnce(*mut PluginErr) -> R) -> R {
    let mut e = EMPTY;
    let r = f(&raw mut e);
    if e.kind != 0 {
        LAST_LOCK.lock();
        // SAFETY: under the lock.
        unsafe { *LAST.0.get() = e };
        LAST_LOCK.unlock();
    }
    r
}

/// The last failure's kind and texts.
fn last() -> (u32, [Str; 3]) {
    LAST_LOCK.lock();
    // SAFETY: under the lock.
    let e = unsafe { &*LAST.0.get() };
    let r = (e.kind, [e.a, e.b, e.list]);
    LAST_LOCK.unlock();
    r
}

fn text(s: Str) -> NabiStr {
    // SAFETY: failure and module texts stay allocated.
    build(&[unsafe { s.bytes() }])
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_plugin_load(path: NabiStr) -> usize {
    // SAFETY: a Nabi `str`.
    record(|e| unsafe { raw_load(path.ptr, path.len, e) })
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_plugin_find(h: usize, path: NabiStr, sig: u64, kind: usize) -> usize {
    // SAFETY: as above.
    record(|e| unsafe { raw_find(h, path.ptr, path.len, sig, kind, e) })
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_plugin_unload(h: usize) -> i32 {
    // SAFETY: a writable record.
    record(|e| unsafe { raw_unload(h, e) })
}

/// 1 once plugin `h` is unmapped, 0 when `ms` pass first.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_plugin_wait(h: usize, ms: u64) -> i32 {
    let ns = i64::try_from(ms.saturating_mul(1_000_000)).unwrap_or(i64::MAX);
    let end = nabir_common::sys::monotonic_ns().saturating_add(ns);
    loop {
        if nabi_plugin_state(h) == 2 {
            return 1;
        }
        if nabir_common::sys::monotonic_ns() >= end {
            return 0;
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_plugin_info(h: usize, what: usize) -> NabiStr {
    let mut s = Str::EMPTY;
    // SAFETY: `s` is writable.
    if unsafe { raw_info(h, what, &raw mut s) } != 0 {
        return build(&[]);
    }
    text(s)
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_plugin_error() -> u32 {
    last().0
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_plugin_error_text(part: usize) -> NabiStr {
    let (_, t) = last();
    text(t[part.min(2)])
}
