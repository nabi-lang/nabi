//! Above `-O0`: the shadow-stack frame's root list in runs, or a static
//! descriptor.
//!
//! Lowering lists every root word's address (`Addr` into the frame, one
//! per word, after the count). Once the frame is laid out, consecutive
//! root words are known: a run of `n` of them becomes the entry
//! `(n << 1) | 1` and the first one's address, two stores instead of `n`
//! (the collector reads both forms). Words listed twice (slots sharing
//! memory) are listed once. The roots cleared on entry are cleared
//! together, adjacent ones as one range.
//!
//! With two entries or more the list goes to read-only data instead
//! (`frame`): the frame holds a count of 0 and the descriptor's address,
//! the descriptor a `u16` count and the same entries in `u16`s, each
//! address as its offset from the frame in words, doubled. Descriptors
//! are named by content, one kept by the link however many functions
//! share it.

use crate::lir::{Func, Inst, Mem, Slot, V};
use crate::obj::{Obj, Sect};
use nabic_core::HashMap;

/// The shortest run worth an entry of its own (two words).
const MIN_RUN: u32 = 3;

/// The fewest entries a descriptor replaces (its two stores against
/// the count's and the entries').
const MIN_DESC: usize = 2;

/// Instructions of `f` replaced: by index, what goes in their place.
pub struct Runs(HashMap<usize, Vec<Inst>>);

impl Runs {
    /// What to emit for instruction `n`: `None` to emit it as it is.
    #[must_use]
    pub fn at(&self, n: usize) -> Option<&[Inst]> {
        self.0.get(&n).map(Vec::as_slice)
    }
}

/// `f`'s root list in runs, with slot offsets `off` and words of `word`
/// bytes, as a static descriptor in `obj` when it has `MIN_DESC`
/// entries or more; `None` when lowering's list is no longer.
pub fn frame(
    obj: &mut Obj,
    f: &Func,
    off: &[u32],
    word: u32,
    gone: &dyn Fn(Slot) -> bool,
) -> Option<Runs> {
    frame_from(obj, f, off, word, gone, MIN_DESC)
}

/// `frame`, a descriptor from `min` entries (where taking its address
/// costs more than two stores).
#[allow(clippy::too_many_lines)]
pub fn frame_from(
    obj: &mut Obj,
    f: &Func,
    off: &[u32],
    word: u32,
    gone: &dyn Fn(Slot) -> bool,
    min: usize,
) -> Option<Runs> {
    // A hot build's frame says where its `FrameInfo` is: always a
    // descriptor, the word before it pointing there (or 0).
    let info = obj.hot_frames.get(&f.sym).copied();
    if !f.opt && info.is_none() {
        return None;
    }
    let (fs, roots) = f.gc_frame.as_ref()?;
    let fs = *fs;
    let at = f.insts.iter().position(|i| {
        matches!(i, Inst::Mov { dst: Mem::Frame(s, o), src: V::I(_), .. } if *s == fs && *o == word)
    })?;
    let Inst::Mov {
        k, src: V::I(n), ..
    } = f.insts[at]
    else {
        return None;
    };
    let n = usize::try_from(n).ok()?;
    let end = at + 1 + n;
    // Each listed word: its frame offset and where lowering took it.
    let mut words: Vec<(u32, Slot, u32)> = Vec::with_capacity(n);
    for (j, i) in f.insts.get(at + 1..end)?.iter().enumerate() {
        let Inst::Addr {
            dst: Mem::Frame(d, o),
            src: Mem::Frame(s, so),
        } = *i
        else {
            return None;
        };
        if d != fs || o != word * (2 + j as u32) || !roots.contains(&s) || gone(s) {
            return None;
        }
        words.push((off[s as usize] + so, s, so));
    }
    words.sort_unstable_by_key(|w| w.0);
    words.dedup_by_key(|w| w.0);
    // Runs of consecutive words.
    let mut runs: Vec<Vec<(u32, Slot, u32)>> = Vec::new();
    for w in words {
        match runs.last_mut() {
            Some(r) if r[r.len() - 1].0 + word == w.0 => r.push(w),
            _ => runs.push(vec![w]),
        }
    }
    let long = |r: &[(u32, Slot, u32)]| r.len() as u32 >= MIN_RUN;
    let entries: usize = runs.iter().map(|r| if long(r) { 2 } else { r.len() }).sum();
    let slot = |j: usize| Mem::Frame(fs, word * (2 + j as u32));
    // Each entry: a word's offset from the frame in words, doubled, or a
    // run's `(n << 1) | 1` before its first's; all in 16 bits.
    let base = i64::from(off[fs as usize]);
    let rel = |w: u32| (i64::from(w) - base) / i64::from(word) * 2;
    let mut d = vec![entries as i64];
    let mut fits = entries <= 0xffff;
    for r in &runs {
        if long(r) {
            d.push(((r.len() as i64) << 1) | 1);
            fits &= r.len() < 0x8000;
        }
        // A run lists its first word; the rest list their own.
        let listed = if long(r) { &r[..1] } else { &r[..] };
        for w in listed {
            let e = rel(w.0);
            fits &= i64::from(e as i16) == e;
            d.push(e);
        }
    }
    if let Some(info) = info.filter(|_| fits) {
        let pw = word as usize;
        let mut bytes = vec![0; pw];
        bytes.extend(d.iter().flat_map(|&e| obj.int_bytes(e as u64, 2)));
        let name = format!("{}.gcframe", obj.syms[f.sym as usize].name);
        let sym = obj.sym(&name);
        let sect = if info.is_some() {
            Sect::DataConst
        } else {
            Sect::Const
        };
        let off = obj.define_data(sym, sect, &bytes, word, false);
        if let Some(i) = info {
            obj.abs64(sect, off, i);
        }
        let insts = vec![
            Inst::Mov {
                k,
                dst: Mem::Frame(fs, word),
                src: V::I(0),
            },
            Inst::Sym {
                dst: slot(0),
                sym,
                got: false,
            },
            Inst::Bin {
                op: crate::lir::Bin::Add,
                k,
                signed: false,
                dst: slot(0),
                a: V::M(slot(0)),
                b: V::I(i64::from(word)),
            },
        ];
        let mut edits: HashMap<usize, Vec<Inst>> = (at + 1..end).map(|j| (j, Vec::new())).collect();
        edits.insert(at, insts);
        return Some(Runs(edits));
    }
    if entries >= min && fits {
        let bytes: Vec<u8> = d.iter().flat_map(|&e| obj.int_bytes(e as u64, 2)).collect();
        let name = format!("nabi.gcframe.{:016x}", nabic_core::hash::fnv(&bytes));
        let defined = obj.named(&name);
        let sym = obj.sym(&name);
        if !defined {
            obj.define_data(sym, Sect::Const, &bytes, 2, true);
            let y = &mut obj.syms[sym as usize];
            y.weak = true;
            y.hidden = true;
        }
        let insts = vec![
            Inst::Mov {
                k,
                dst: Mem::Frame(fs, word),
                src: V::I(0),
            },
            Inst::Sym {
                dst: slot(0),
                sym,
                got: false,
            },
        ];
        let mut edits: HashMap<usize, Vec<Inst>> = (at + 1..end).map(|j| (j, Vec::new())).collect();
        edits.insert(at, insts);
        zeros(f, at, off, roots, gone, &mut edits);
        return Some(Runs(edits));
    }
    if entries >= n {
        return None;
    }
    let mut insts = vec![Inst::Mov {
        k,
        dst: Mem::Frame(fs, word),
        src: V::I(entries as i64),
    }];
    let mut j = 0;
    for r in runs {
        if long(&r) {
            insts.push(Inst::Mov {
                k,
                dst: slot(j),
                src: V::I(((r.len() as i64) << 1) | 1),
            });
            j += 1;
        }
        // A run lists its first word; the rest list their own.
        let listed = if long(&r) { &r[..1] } else { &r[..] };
        for &(_, s, o) in listed {
            insts.push(Inst::Addr {
                dst: slot(j),
                src: Mem::Frame(s, o),
            });
            j += 1;
        }
    }
    let mut edits: HashMap<usize, Vec<Inst>> = (at + 1..end).map(|j| (j, Vec::new())).collect();
    edits.insert(at, insts);
    zeros(f, at, off, roots, gone, &mut edits);
    Some(Runs(edits))
}

/// The roots lowering clears before linking the frame (`Zero`s back
/// from `at - 1`, between them only clears of the roots' cached copies)
/// cleared at the first of them, adjacent ones as one range.
fn zeros(
    f: &Func,
    at: usize,
    off: &[u32],
    roots: &[Slot],
    gone: &dyn Fn(Slot) -> bool,
    edits: &mut HashMap<usize, Vec<Inst>>,
) {
    let mut found: Vec<(usize, u32, Slot, u32, u32)> = Vec::new();
    for (j, i) in f.insts[..at.saturating_sub(1)].iter().enumerate().rev() {
        match *i {
            Inst::Zero {
                dst: Mem::Frame(s, o),
                size,
            } if roots.contains(&s) && !gone(s) => found.push((j, off[s as usize] + o, s, o, size)),
            Inst::Mov {
                dst: Mem::Frame(t, _),
                src: V::I(0),
                ..
            } if !roots.contains(&t) => {}
            Inst::Line(..) => {}
            _ => break,
        }
    }
    if found.len() < 2 {
        return;
    }
    let first = found.iter().map(|z| z.0).min().unwrap_or(0);
    found.sort_unstable_by_key(|z| z.1);
    // Ranges: (address, slot, offset, size).
    let mut ranges: Vec<(u32, Slot, u32, u32)> = Vec::new();
    for &(_, a, s, o, size) in &found {
        match ranges.last_mut() {
            Some(r) if a <= r.0 + r.3 => r.3 = r.3.max(a + size - r.0),
            _ => ranges.push((a, s, o, size)),
        }
    }
    for z in &found {
        edits.insert(z.0, Vec::new());
    }
    let zs = ranges
        .into_iter()
        .map(|(_, s, o, size)| Inst::Zero {
            dst: Mem::Frame(s, o),
            size,
        })
        .collect();
    edits.insert(first, zs);
}
