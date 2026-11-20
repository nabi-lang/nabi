//! Unicode normalization: NFC, NFD, NFKC and NFKD, and the quick check.
//!
//! Text is decomposed into a scratch `Buf` of code points tagged with
//! their combining class, put in canonical order and, for the composed
//! forms, recomposed in place. The buffer is flushed at segment starts
//! (a starter nothing before it can combine with), so it stays small
//! unless a single segment is long.

use crate::unicode::tables as t;

/// A normalization form.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Form {
    Nfc,
    Nfd,
    Nfkc,
    Nfkd,
}

impl Form {
    /// The form numbered as `nabi_str_normalize` takes it: 0 NFC, 1 NFD,
    /// 2 NFKC, 3 NFKD.
    #[must_use]
    pub const fn from_i32(n: i32) -> Option<Self> {
        Some(match n {
            0 => Self::Nfc,
            1 => Self::Nfd,
            2 => Self::Nfkc,
            3 => Self::Nfkd,
            _ => return None,
        })
    }

    const fn compat(self) -> bool {
        matches!(self, Self::Nfkc | Self::Nfkd)
    }

    const fn composed(self) -> bool {
        matches!(self, Self::Nfc | Self::Nfkc)
    }

    /// `QC` bits meaning No and Maybe for this form.
    const fn qc_bits(self) -> (u32, u32) {
        match self {
            Self::Nfd => (1, 0),
            Self::Nfkd => (2, 0),
            Self::Nfc => (4, 8),
            Self::Nfkc => (16, 32),
        }
    }
}

/// Growable scratch of tagged code points (`cp | ccc << 24`, the bits
/// from 32 free for sorting).
pub trait Buf {
    fn push(&mut self, x: u64);
    fn items(&mut self) -> &mut [u64];
    fn clear(&mut self);
}

/// Flush the scratch at a segment start once it holds this many.
const FLUSH: usize = 32;

const S_BASE: u32 = 0xAC00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT;
const S_COUNT: u32 = 19 * N_COUNT;

/// Lookups in a breakpoint table (`(start << SHIFT) | value`, sorted,
/// the first start 0) that remember the last run found: text tends to
/// stay in one script.
pub(crate) struct Runs<T: Copy + Into<u64> + 'static, const SHIFT: u32> {
    table: &'static [T],
    lo: u32,
    hi: u32,
    v: u32,
}

impl<T: Copy + Into<u64> + 'static, const SHIFT: u32> Runs<T, SHIFT> {
    pub(crate) const fn new(table: &'static [T]) -> Self {
        Self {
            table,
            lo: 1,
            hi: 0,
            v: 0,
        }
    }

    /// The value at `c`.
    #[inline]
    pub(crate) fn get(&mut self, c: u32) -> u32 {
        if self.lo <= c && c < self.hi {
            return self.v;
        }
        let start = |e: T| (e.into() >> SHIFT) as u32;
        let i = self.table.partition_point(|&e| start(e) <= c).max(1);
        let e = self.table[i - 1];
        self.lo = start(e);
        self.hi = self.table.get(i).map_or(u32::MAX, |&n| start(n));
        self.v = (e.into() & ((1 << SHIFT) - 1)) as u32;
        self.v
    }
}

/// `NORM` lookups: `ccc | qc << 8`.
pub(crate) struct Props(Runs<u64, 16>);

impl Props {
    pub(crate) const fn new() -> Self {
        Self(Runs::new(t::NORM))
    }

    #[inline]
    pub(crate) fn get(&mut self, c: u32) -> u32 {
        if c < 0xA0 { 0 } else { self.0.get(c) }
    }
}

/// `NORM` bits of `c`, without a remembered run.
fn props(c: u32) -> u32 {
    Props::new().get(c)
}

/// Canonical combining class of `c`.
#[inline]
#[must_use]
pub fn ccc(c: u32) -> u32 {
    props(c) & 0xFF
}

/// The end of the ASCII run of `b` from `i`.
#[inline]
pub(crate) fn ascii_end(b: &[u8], mut i: usize) -> usize {
    while let Some(w) = b.get(i..i + 8) {
        let w = u64::from_le_bytes([w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7]]);
        if w & 0x8080_8080_8080_8080 != 0 {
            break;
        }
        i += 8;
    }
    while i < b.len() && b[i] < 0x80 {
        i += 1;
    }
    i
}

/// Set in a scratch item that may compose with a starter before it.
const SECOND: u64 = 1 << 21;

/// `c` (`p` its `props`) as a scratch item: `cp | SECOND? | ccc << 24`.
#[inline]
fn item(c: u32, p: u32) -> u64 {
    let second = if p & (8 << 8) != 0 { SECOND } else { 0 };
    u64::from(c) | second | (u64::from(p & 0xFF) << 24)
}

/// The full decomposition of `c` from the table, if it has one there.
fn table_decomp(c: u32, compat: bool) -> Option<&'static str> {
    let i = t::DECOMP
        .binary_search_by_key(&c, |&e| (e >> 42) as u32)
        .ok()?;
    let e = t::DECOMP[i];
    let v = if compat { e } else { e >> 21 } & 0x1F_FFFF;
    if v == 0 {
        return None;
    }
    let at = (v >> 7) as usize;
    t::DECOMP_TEXT.get(at..at + (v & 0x7F) as usize)
}

/// `c`'s full decomposition (itself when it has none) into `out`;
/// returns the count. `p` is `props(c)`.
#[allow(clippy::many_single_char_names)]
fn decompose(c: u32, p: u32, compat: bool, out: &mut [u32; t::DECOMP_MAX]) -> usize {
    // Quick check NFD / NFKD No: exactly the chars that decompose.
    let no = if compat { 2 } else { 1 };
    if (p >> 8) & no == 0 {
        out[0] = c;
        return 1;
    }
    let s = c.wrapping_sub(S_BASE);
    if s < S_COUNT {
        out[0] = L_BASE + s / N_COUNT;
        out[1] = V_BASE + s % N_COUNT / T_COUNT;
        if s % T_COUNT == 0 {
            return 2;
        }
        out[2] = T_BASE + s % T_COUNT;
        return 3;
    }
    let Some(d) = table_decomp(c, compat) else {
        out[0] = c;
        return 1;
    };
    let mut n = 0;
    for x in d.chars() {
        out[n] = x as u32;
        n += 1;
    }
    n
}

/// The primary composite of `a` and `b`, if any.
#[allow(clippy::many_single_char_names)]
fn compose_pair(a: u32, b: u32) -> Option<u32> {
    let l = a.wrapping_sub(L_BASE);
    let v = b.wrapping_sub(V_BASE);
    if l < 19 && v < V_COUNT {
        return Some(S_BASE + (l * V_COUNT + v) * T_COUNT);
    }
    let s = a.wrapping_sub(S_BASE);
    let tt = b.wrapping_sub(T_BASE);
    if s < S_COUNT && s % T_COUNT == 0 && tt > 0 && tt < T_COUNT {
        return Some(a + tt);
    }
    let key = (u64::from(a) << 21) | u64::from(b);
    let i = t::COMPOSE.binary_search_by_key(&key, |&e| e >> 21).ok()?;
    Some((t::COMPOSE[i] & 0x1F_FFFF) as u32)
}

/// Stable sort of each run of non-starters by combining class.
fn reorder(items: &mut [u64]) {
    let ccc_of = |x: u64| (x >> 24) & 0xFF;
    let mut i = 0;
    while i < items.len() {
        if ccc_of(items[i]) == 0 {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut sorted = true;
        while j < items.len() && ccc_of(items[j]) != 0 {
            sorted &= ccc_of(items[j - 1]) <= ccc_of(items[j]);
            j += 1;
        }
        if !sorted {
            let run = &mut items[i..j];
            for (k, x) in run.iter_mut().enumerate() {
                *x |= (k as u64) << 32;
            }
            run.sort_unstable_by_key(|&x| (ccc_of(x), x >> 32));
            for x in run.iter_mut() {
                *x &= 0xFFFF_FFFF;
            }
        }
        i = j;
    }
}

/// Canonical composition of `items` in place; returns the new length.
fn recompose(items: &mut [u64]) -> usize {
    let mut out = 0;
    let mut starter: Option<usize> = None;
    let mut last = 0;
    for i in 0..items.len() {
        let x = items[i];
        let (c, cc) = ((x & 0x1F_FFFF) as u32, (x >> 24) & 0xFF);
        if let Some(s) = starter.filter(|_| x & SECOND != 0) {
            let blocked = out > s + 1 && (last == 0 || last >= cc);
            if !blocked {
                if let Some(m) = compose_pair((items[s] & 0x1F_FFFF) as u32, c) {
                    items[s] = u64::from(m);
                    continue;
                }
            }
        }
        if cc == 0 {
            starter = Some(out);
        }
        last = cc;
        items[out] = x;
        out += 1;
    }
    out
}

fn flush(buf: &mut impl Buf, form: Form, put: &mut impl FnMut(&str)) {
    let items = buf.items();
    if items.is_empty() {
        return;
    }
    reorder(items);
    let n = if form.composed() {
        recompose(items)
    } else {
        items.len()
    };
    for &x in &items[..n] {
        let c = char::from_u32((x & 0x1F_FFFF) as u32).unwrap_or('\u{FFFD}');
        put(c.encode_utf8(&mut [0; 4]));
    }
    buf.clear();
}

/// Push `c`'s decomposition (`p` its `props`) to `buf`; the count.
fn push(buf: &mut impl Buf, props: &mut Props, c: u32, p: u32, compat: bool) -> usize {
    let mut d = [0; t::DECOMP_MAX];
    let n = decompose(c, p, compat, &mut d);
    if n == 1 && d[0] == c {
        buf.push(item(c, p));
        return 1;
    }
    for &x in &d[..n] {
        buf.push(item(x, props.get(x)));
    }
    n
}

/// The char starting at byte `i` of `text`.
#[inline]
fn char_at(text: &str, i: usize) -> u32 {
    text[i..].chars().next().map_or(0, |c| c as u32)
}

/// `text` in normalization form `form`, piece by piece into `put`.
///
/// A run of stable starters (Yes, class 0: nothing before or after
/// changes them) is passed through as one slice, all but its last char,
/// which marks after it may still change.
#[allow(clippy::many_single_char_names)]
pub fn normalize(text: &str, form: Form, buf: &mut impl Buf, mut put: impl FnMut(&str)) {
    let (no, maybe) = form.qc_bits();
    let fast = fast(form);
    let compat = form.compat();
    let mut props = Props::new();
    // Start of the current run of stable starters, and of its last char.
    let mut run: Option<(usize, usize)> = None;
    let mut held = 0;
    buf.clear();
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let (c, p, n) = if b[i] < 0x80 {
            let end = ascii_end(b, i);
            if end - i > 1 {
                // All but the last are stable and stay as they are.
                if run.is_none() {
                    flush(buf, form, &mut put);
                    held = 0;
                    run = Some((i, i));
                }
                i = end - 1;
            }
            (u32::from(b[i]), 0, 1)
        } else {
            let c = char_at(text, i);
            let p = if c < fast { 0 } else { props.get(c) };
            (c, p, char::from_u32(c).map_or(1, char::len_utf8))
        };
        if p & (((no | maybe) << 8) | 0xFF) == 0 {
            if let Some((_, last)) = &mut run {
                *last = i;
            } else {
                flush(buf, form, &mut put);
                held = 0;
                run = Some((i, i));
            }
            i += n;
            continue;
        }
        if let Some((start, last)) = run.take() {
            put(&text[start..last]);
            let s = char_at(text, last);
            let sp = props.get(s);
            held = push(buf, &mut props, s, sp, compat);
        }
        // A char whose decomposition starts with a starter that composes
        // with nothing before it begins a segment.
        if held >= FLUSH {
            let mut d = [0; t::DECOMP_MAX];
            decompose(c, p, compat, &mut d);
            if props.get(d[0]) & ((maybe << 8) | 0xFF) == 0 {
                flush(buf, form, &mut put);
                held = 0;
            }
        }
        held += push(buf, &mut props, c, p, compat);
        i += n;
    }
    match run {
        Some((start, _)) => put(&text[start..]),
        None => flush(buf, form, &mut put),
    }
}

/// Below this every char is Yes with class 0 in `form`.
const fn fast(form: Form) -> u32 {
    match form {
        Form::Nfc => 0x300,
        Form::Nfd => 0xC0,
        Form::Nfkc | Form::Nfkd => 0xA0,
    }
}

/// Quick check answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Qc {
    Yes,
    No,
    Maybe,
}

/// The quick check of `text` for `form`, and the byte offset up to which
/// `text` is known to be normalized and unaffected by what follows (the
/// last stable starter before the first char that is not Yes).
#[must_use]
#[allow(clippy::many_single_char_names)]
pub fn quick_check(text: &str, form: Form) -> (Qc, usize) {
    let (no, maybe) = form.qc_bits();
    let fast = fast(form);
    let mut props = Props::new();
    let mut last = 0;
    let mut answer = Qc::Yes;
    let mut safe = 0;
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] < 0x80 {
            i = ascii_end(b, i);
            last = 0;
            if answer == Qc::Yes {
                safe = i - 1;
            }
            continue;
        }
        let c = char_at(text, i);
        let at = i;
        i += char::from_u32(c).map_or(1, char::len_utf8);
        if c < fast {
            last = 0;
            if answer == Qc::Yes {
                safe = at;
            }
            continue;
        }
        let p = props.get(c);
        let (cc, q) = (p & 0xFF, p >> 8);
        if last > cc && cc != 0 {
            return (Qc::No, safe);
        }
        if q & no != 0 {
            return (Qc::No, safe);
        }
        if q & maybe != 0 {
            answer = Qc::Maybe;
        } else if cc == 0 && answer == Qc::Yes {
            safe = at;
        }
        last = cc;
    }
    if answer == Qc::Yes {
        safe = text.len();
    }
    (answer, safe)
}

/// Whether `text` is in form `form`: the quick check, then a full
/// comparison where it answers Maybe.
pub fn is_normalized(text: &str, form: Form, buf: &mut impl Buf) -> bool {
    let (answer, safe) = quick_check(text, form);
    match answer {
        Qc::Yes => true,
        Qc::No => false,
        Qc::Maybe => {
            let mut rest = &text[safe..];
            let mut same = true;
            normalize(rest, form, buf, |piece| {
                same &= rest.starts_with(piece);
                rest = rest.get(piece.len()..).unwrap_or("");
            });
            same && rest.is_empty()
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) struct Items(pub(crate) Vec<u64>);

    impl Buf for Items {
        fn push(&mut self, x: u64) {
            self.0.push(x);
        }
        fn items(&mut self) -> &mut [u64] {
            &mut self.0
        }
        fn clear(&mut self) {
            self.0.clear();
        }
    }

    pub(crate) fn norm(s: &str, form: Form) -> String {
        let mut out = String::new();
        normalize(s, form, &mut Items(Vec::new()), |p| out.push_str(p));
        out
    }

    #[test]
    fn forms() {
        assert_eq!(norm("e\u{301}", Form::Nfc), "\u{e9}");
        assert_eq!(norm("\u{e9}", Form::Nfd), "e\u{301}");
        assert_eq!(norm("\u{fb01}", Form::Nfkc), "fi");
        assert_eq!(norm("\u{fb01}", Form::Nfc), "\u{fb01}");
        assert_eq!(norm("\u{1e9b}\u{323}", Form::Nfkd), "s\u{323}\u{307}");
        assert_eq!(norm("\u{1100}\u{1161}\u{11a8}", Form::Nfc), "\u{ac01}");
        assert_eq!(norm("\u{ac01}", Form::Nfd), "\u{1100}\u{1161}\u{11a8}");
        // Canonical order: dot below (220) before acute (230).
        assert_eq!(norm("a\u{301}\u{323}", Form::Nfd), "a\u{323}\u{301}");
        // A long run of marks keeps the order of equal classes.
        let marks: String = (0..100)
            .map(|i| if i % 2 == 0 { '\u{301}' } else { '\u{316}' })
            .collect();
        let want: String = "a"
            .chars()
            .chain((0..50).map(|_| '\u{316}'))
            .chain((0..50).map(|_| '\u{301}'))
            .collect();
        assert_eq!(norm(&(String::from("a") + &marks), Form::Nfd), want);
        // Exclusions never recompose.
        assert_eq!(norm("\u{915}\u{93c}", Form::Nfc), "\u{915}\u{93c}");
    }

    #[test]
    fn quick() {
        assert_eq!(quick_check("plain", Form::Nfc), (Qc::Yes, 5));
        assert_eq!(quick_check("ab\u{e9}", Form::Nfd), (Qc::No, 1));
        assert_eq!(quick_check("xe\u{301}", Form::Nfc), (Qc::Maybe, 1));
        assert_eq!(quick_check("a\u{301}\u{323}", Form::Nfc).0, Qc::No);
        let mut b = Items(Vec::new());
        assert!(is_normalized("caf\u{e9}", Form::Nfc, &mut b));
        assert!(!is_normalized("cafe\u{301}", Form::Nfc, &mut b));
        assert!(is_normalized("\u{915}\u{93c}", Form::Nfc, &mut b));
    }
}
