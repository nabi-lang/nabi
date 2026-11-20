//! What of old code is still running or alive: frames, by the
//! `FrameInfo` address each one roots, and function values (closures,
//! coroutine resume words), by the code they point into.

use crate::hot::channel::push_num;
use nabir_abi::module::{FRAME_MAGIC, FrameInfo, Held, ModuleDesc};
use nabir_abi::typedesc::Slice;
use nabir_common::wtf8::MBuf;

/// The `FrameInfo`s of some modules.
pub(crate) struct Frames {
    /// Their addresses, sorted.
    at: MBuf<usize>,
    /// `(code, end, info)`, sorted by code.
    code: MBuf<(usize, usize, usize)>,
}

const fn items<T>(s: &Slice<T>) -> &'static [T] {
    if s.len == 0 || s.ptr.is_null() {
        return &[];
    }
    // SAFETY: descriptor tables live as long as their module.
    unsafe { core::slice::from_raw_parts(s.ptr, s.len) }
}

#[allow(clippy::trivially_copy_pass_by_ref)] // 8 bytes on 32-bit targets only
const fn text(s: &Slice<u8>) -> &'static [u8] {
    items(s)
}

impl Frames {
    /// The `FrameInfo`s `mods` list in their `functions`.
    pub(crate) fn of(mods: impl Iterator<Item = *const ModuleDesc>) -> Self {
        let mut at: MBuf<usize> = MBuf::new();
        let mut code: MBuf<(usize, usize, usize)> = MBuf::new();
        for m in mods {
            // SAFETY: a loaded descriptor.
            for f in items(unsafe { &(*m).functions }) {
                if f.frame.is_null() {
                    continue;
                }
                #[allow(clippy::cast_ptr_alignment)] // the compiler aligns it
                let fi = f.frame.cast::<FrameInfo>();
                // SAFETY: the compiler's FrameInfo.
                let (magic, start, len) = unsafe { ((*fi).magic, (*fi).code, (*fi).len) };
                if magic != FRAME_MAGIC {
                    continue;
                }
                at.push(fi as usize);
                code.push((start, start + len as usize, fi as usize));
            }
        }
        at.as_mut_slice().sort_unstable();
        code.as_mut_slice().sort_unstable();
        Self { at, code }
    }

    /// `[lo, hi)` around all their code.
    pub(crate) fn range(&self) -> (usize, usize) {
        let c = self.code.as_slice();
        let lo = c.first().map_or(0, |e| e.0);
        let hi = c.iter().map(|e| e.1).max().unwrap_or(0);
        (lo, hi)
    }

    /// With the world stopped: why a running frame of theirs holds a
    /// change back, `changed` saying which type keys changed.
    pub(crate) fn running(&self, changed: &dyn Fn(u64) -> bool) -> Option<MBuf<u8>> {
        let mut why = None;
        nabir_gc::world::for_each_root(|r| {
            if why.is_some() || self.at.as_slice().binary_search(&(r as usize)).is_err() {
                return;
            }
            // SAFETY: one of the FrameInfos above.
            #[allow(clippy::cast_ptr_alignment)] // one of the addresses above
            let fi = unsafe { &*r.cast::<FrameInfo>() };
            if let Some(h) = first(fi, changed) {
                let mut m: MBuf<u8> = MBuf::new();
                who(&mut m, fi, h);
                m.extend(b" is running and uses `");
                m.extend(text(&h.name));
                m.extend(b"`, which changed; keep the `");
                m.extend(text(&h.name));
                m.extend(
                    b"` in a `reload::State` made in another function, or move this work into a function it calls",
                );
                why = Some(m);
            }
        });
        why
    }

    /// Why a function value (a heap word at `word`) holds a change back.
    pub(crate) fn value(&self, word: usize, changed: &dyn Fn(u64) -> bool) -> Option<MBuf<u8>> {
        let c = self.code.as_slice();
        let k = c.partition_point(|e| e.0 <= word);
        let (start, end, fi) = *c.get(k.checked_sub(1)?)?;
        if word < start || word >= end {
            return None;
        }
        // SAFETY: one of the FrameInfos above.
        let fi = unsafe { &*(fi as *const FrameInfo) };
        let h = first(fi, changed)?;
        let mut m: MBuf<u8> = MBuf::new();
        m.extend(b"a closure or function value of ");
        who(&mut m, fi, h);
        m.extend(b" is alive and uses `");
        m.extend(text(&h.name));
        m.extend(b"`, which changed; let it finish or drop it, or restart with `r`");
        Some(m)
    }
}

/// The first changed type `fi` holds.
fn first<'a>(fi: &'a FrameInfo, changed: &dyn Fn(u64) -> bool) -> Option<&'a Held> {
    items(&fi.held).iter().find(|h| changed(h.key_hash))
}

/// `` `name` (file:line) ``, the line where it uses `h`.
fn who(m: &mut MBuf<u8>, fi: &FrameInfo, h: &Held) {
    m.push(b'`');
    m.extend(text(&fi.name));
    m.extend(b"` (");
    m.extend(text(&fi.file));
    m.push(b':');
    let line = if h.line != 0 {
        u64::from(h.line)
    } else {
        fi.line as u64
    };
    push_num(m, line);
    m.push(b')');
}
