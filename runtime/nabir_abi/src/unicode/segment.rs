//! Text segmentation (UAX #29): extended grapheme clusters and word
//! boundaries.
//!
//! Both scan forward from a boundary, so a caller walks a text piece by
//! piece without looking back. Bytes that are not UTF-8 count one by one
//! as characters with no break property.

use crate::unicode::norm::Runs;
use crate::unicode::tables as t;

type Table = Runs<u32, 8>;

/// The char at byte `i` of `b` and its length; an ill-formed byte is a
/// one-byte U+FFFD.
#[inline]
#[allow(clippy::many_single_char_names)]
fn decode(b: &[u8], i: usize) -> (u32, usize) {
    let x = b[i];
    if x < 0x80 {
        return (u32::from(x), 1);
    }
    let (n, min, lead) = match x {
        0xC2..=0xDF => (2, 0x80, x & 0x1F),
        0xE0..=0xEF => (3, 0x800, x & 0x0F),
        0xF0..=0xF4 => (4, 0x1_0000, x & 0x07),
        _ => return (0xFFFD, 1),
    };
    let Some(tail) = b.get(i + 1..i + n) else {
        return (0xFFFD, 1);
    };
    let mut c = u32::from(lead);
    for &y in tail {
        if y & 0xC0 != 0x80 {
            return (0xFFFD, 1);
        }
        c = (c << 6) | u32::from(y & 0x3F);
    }
    if c < min || c > 0x10_FFFF || (0xD800..0xE000).contains(&c) {
        return (0xFFFD, 1);
    }
    (c, n)
}

// Grapheme_Cluster_Break values (`tables::GRAPHEME`).
const G_CR: u32 = 1;
const G_LF: u32 = 2;
const G_CONTROL: u32 = 3;
const G_EXTEND: u32 = 4;
const G_ZWJ: u32 = 5;
const G_RI: u32 = 6;
const G_PREPEND: u32 = 7;
const G_SPACING: u32 = 8;
const G_L: u32 = 9;
const G_V: u32 = 10;
const G_T: u32 = 11;
const G_LV: u32 = 12;
const G_LVT: u32 = 13;
const G_PICT: u32 = 16;

/// `GRAPHEME` bits of `c`, Hangul syllables resolved to LV / LVT.
#[inline]
fn grapheme_bits(c: u32, table: &mut Table) -> u32 {
    if c < 0x7F {
        return match c {
            0x0D => G_CR,
            0x0A => G_LF,
            0..0x20 => G_CONTROL,
            _ => 0,
        };
    }
    let v = table.get(c);
    if v & 15 == G_LV && (c.wrapping_sub(0xAC00)) % 28 != 0 {
        return v - G_LV + G_LVT;
    }
    v
}

/// Byte offset of the grapheme cluster boundary after the one at `pos`
/// (`b.len()` at the end).
#[must_use]
pub fn grapheme_end(b: &[u8], pos: usize) -> usize {
    grapheme_end_in(b, pos, &mut Table::new(t::GRAPHEME))
}

// The arms follow the rules one by one, in their order.
#[allow(clippy::many_single_char_names, clippy::match_same_arms)]
fn grapheme_end_in(b: &[u8], pos: usize, table: &mut Table) -> usize {
    if pos >= b.len() {
        return b.len();
    }
    let (c, n) = decode(b, pos);
    let mut prev = grapheme_bits(c, table);
    // Odd count of regional indicators so far.
    let mut ri_odd = prev & 15 == G_RI;
    // Extended_Pictographic Extend*: 1; then ZWJ: 2.
    let mut emoji = u8::from(prev & G_PICT != 0);
    // After InCB Linker Extend*.
    let mut linked = prev >> 5 == 3;
    let mut i = pos + n;
    while i < b.len() {
        let (c, n) = decode(b, i);
        let cur = grapheme_bits(c, table);
        let (p, q) = (prev & 15, cur & 15);
        let keep = match (p, q) {
            (G_CR, G_LF) => true,
            (G_CR | G_LF | G_CONTROL, _) | (_, G_CR | G_LF | G_CONTROL) => false,
            (G_L, G_L | G_V | G_LV | G_LVT) | (G_LV | G_V, G_V | G_T) | (G_LVT | G_T, G_T) => true,
            (_, G_EXTEND | G_ZWJ | G_SPACING) | (G_PREPEND, _) => true,
            (G_ZWJ, _) if cur & G_PICT != 0 && emoji == 2 => true,
            (G_RI, G_RI) => ri_odd,
            _ => cur >> 5 == 1 && linked,
        };
        if !keep {
            return i;
        }
        ri_odd = q == G_RI && !ri_odd;
        emoji = match q {
            _ if cur & G_PICT != 0 => 1,
            G_EXTEND if emoji == 1 => 1,
            G_ZWJ if emoji == 1 => 2,
            _ => 0,
        };
        linked = cur >> 5 == 3 || (linked && cur >> 5 == 2);
        prev = cur;
        i += n;
    }
    b.len()
}

/// Byte offset of the char that ends at `end` (`end > 0` a char
/// boundary), as `decode` reads `b` from the front.
fn char_before(b: &[u8], end: usize) -> usize {
    let lo = end.saturating_sub(4);
    let mut i = end - 1;
    while i > lo && b[i] & 0xC0 == 0x80 {
        i -= 1;
    }
    // A lead byte always starts a char; its sequence either ends at
    // `end` or the bytes before `end` are bad ones of their own.
    if decode(b, i).1 == end - i {
        i
    } else {
        end - 1
    }
}

/// Byte offset of the grapheme cluster boundary before the one at `end`
/// (0 at the start). The same boundaries as `grapheme_end` walking
/// from 0: each rule's state is read back from the chars before.
#[must_use]
pub fn grapheme_start(b: &[u8], end: usize) -> usize {
    let table = &mut Table::new(t::GRAPHEME);
    let end = end.min(b.len());
    if end == 0 {
        return 0;
    }
    let mut i = char_before(b, end);
    while i > 0 && !grapheme_break(b, i, table) {
        i = char_before(b, i);
    }
    i
}

/// A boundary between the char before byte `i` and the one at `i`
/// (`0 < i < b.len()`); `grapheme_end_in`'s arms.
#[allow(clippy::match_same_arms)]
fn grapheme_break(b: &[u8], i: usize, table: &mut Table) -> bool {
    let j = char_before(b, i);
    let prev = grapheme_bits(decode(b, j).0, table);
    let cur = grapheme_bits(decode(b, i).0, table);
    let keep = match (prev & 15, cur & 15) {
        (G_CR, G_LF) => true,
        (G_CR | G_LF | G_CONTROL, _) | (_, G_CR | G_LF | G_CONTROL) => false,
        (G_L, G_L | G_V | G_LV | G_LVT) | (G_LV | G_V, G_V | G_T) | (G_LVT | G_T, G_T) => true,
        (_, G_EXTEND | G_ZWJ | G_SPACING) | (G_PREPEND, _) => true,
        (G_ZWJ, _) if cur & G_PICT != 0 && pict_before(b, j, table) => true,
        (G_RI, G_RI) => ri_odd(b, j, table),
        _ => cur >> 5 == 1 && linked(b, j, table),
    };
    !keep
}

/// `Extended_Pictographic Extend*` ends right before the ZWJ at `j`.
fn pict_before(b: &[u8], mut j: usize, table: &mut Table) -> bool {
    while j > 0 {
        j = char_before(b, j);
        let v = grapheme_bits(decode(b, j).0, table);
        if v & G_PICT != 0 {
            // A control breaks before the Extend after it.
            return !matches!(v & 15, G_CR | G_LF | G_CONTROL);
        }
        if v & 15 != G_EXTEND {
            return false;
        }
    }
    false
}

/// An odd run of regional indicators ends with the one at `j`.
fn ri_odd(b: &[u8], mut j: usize, table: &mut Table) -> bool {
    let mut odd = true;
    while j > 0 {
        j = char_before(b, j);
        if grapheme_bits(decode(b, j).0, table) & 15 != G_RI {
            break;
        }
        odd = !odd;
    }
    odd
}

/// `InCB=Linker InCB=Extend*` ends with the char at `j`. `InCB=Extend`
/// chars are Extend or ZWJ, so the run is one cluster unless a control
/// comes before it, and a control is no Linker.
fn linked(b: &[u8], mut j: usize, table: &mut Table) -> bool {
    loop {
        match grapheme_bits(decode(b, j).0, table) >> 5 {
            3 => return true,
            2 if j > 0 => j = char_before(b, j),
            _ => return false,
        }
    }
}

/// The number of grapheme clusters in `b`.
#[must_use]
pub fn grapheme_count(b: &[u8]) -> usize {
    let mut table = Table::new(t::GRAPHEME);
    let mut n = 0;
    let mut i = 0;
    while i < b.len() {
        i = grapheme_end_in(b, i, &mut table);
        n += 1;
    }
    n
}

// Word_Break values (`tables::WORD`).
const W_CR: u32 = 1;
const W_LF: u32 = 2;
const W_NEWLINE: u32 = 3;
const W_EXTEND: u32 = 4;
const W_ZWJ: u32 = 5;
const W_RI: u32 = 6;
const W_FORMAT: u32 = 7;
const W_KATAKANA: u32 = 8;
const W_HEBREW: u32 = 9;
const W_ALETTER: u32 = 10;
const W_SQUOTE: u32 = 11;
const W_DQUOTE: u32 = 12;
const W_MIDNUMLET: u32 = 13;
const W_MIDLETTER: u32 = 14;
const W_MIDNUM: u32 = 15;
const W_NUMERIC: u32 = 16;
const W_EXTENDNUMLET: u32 = 17;
const W_WSEGSPACE: u32 = 18;
const W_PICT: u32 = 32;

#[inline]
fn word_bits(c: u32, table: &mut Table) -> u32 {
    match t::WORD_ASCII.get(c as usize) {
        Some(&v) => u32::from(v),
        None => table.get(c),
    }
}

const fn ah_letter(p: u32) -> bool {
    matches!(p, W_ALETTER | W_HEBREW)
}

const fn mid_letter(p: u32) -> bool {
    matches!(p, W_MIDLETTER | W_MIDNUMLET | W_SQUOTE)
}

const fn mid_num(p: u32) -> bool {
    matches!(p, W_MIDNUM | W_MIDNUMLET | W_SQUOTE)
}

const fn ignored(p: u32) -> bool {
    matches!(p, W_EXTEND | W_FORMAT | W_ZWJ)
}

/// The `Word_Break` of the first char at or after byte `i` that the
/// ignore rule does not skip (0 at the end).
#[allow(clippy::many_single_char_names)]
fn next_word(b: &[u8], mut i: usize, table: &mut Table) -> u32 {
    while i < b.len() {
        let (c, n) = decode(b, i);
        let p = word_bits(c, table) & 31;
        if !ignored(p) {
            return p;
        }
        i += n;
    }
    0
}

/// Byte offset of the word boundary after the one at `pos` (`b.len()`
/// at the end).
// The arms follow the rules one by one, in their order.
#[must_use]
#[allow(clippy::many_single_char_names, clippy::match_same_arms)]
pub fn word_end(b: &[u8], pos: usize) -> usize {
    if pos >= b.len() {
        return b.len();
    }
    let table = &mut Table::new(t::WORD);
    let (c, n) = decode(b, pos);
    // The raw previous char's bits, and the last two the ignore rule
    // keeps (0 before the start).
    let mut raw = word_bits(c, table);
    let mut prev = raw & 31;
    let mut prev2 = 0;
    let mut ri_odd = prev == W_RI;
    let mut i = pos + n;
    while i < b.len() {
        let (c, n) = decode(b, i);
        let bits = word_bits(c, table);
        let p = bits & 31;
        let r = raw & 31;
        let keep = if r == W_CR && p == W_LF {
            true
        } else if matches!(r, W_CR | W_LF | W_NEWLINE) || matches!(p, W_CR | W_LF | W_NEWLINE) {
            false
        } else if (r == W_ZWJ && bits & W_PICT != 0) || (r == W_WSEGSPACE && p == W_WSEGSPACE) {
            true
        } else if ignored(p) {
            raw = bits;
            i += n;
            continue;
        } else {
            match (prev, p) {
                (a, z) if ah_letter(a) && ah_letter(z) => true,
                (W_HEBREW, W_SQUOTE) => true,
                (a, m) if ah_letter(a) && mid_letter(m) => ah_letter(next_word(b, i + n, table)),
                (m, z) if mid_letter(m) && ah_letter(z) => ah_letter(prev2),
                (W_HEBREW, W_DQUOTE) => next_word(b, i + n, table) == W_HEBREW,
                (W_DQUOTE, W_HEBREW) => prev2 == W_HEBREW,
                (W_NUMERIC, W_NUMERIC) => true,
                (a, W_NUMERIC) if ah_letter(a) => true,
                (W_NUMERIC, z) if ah_letter(z) => true,
                (m, W_NUMERIC) if mid_num(m) => prev2 == W_NUMERIC,
                (W_NUMERIC, m) if mid_num(m) => next_word(b, i + n, table) == W_NUMERIC,
                (W_KATAKANA, W_KATAKANA) => true,
                (
                    W_ALETTER | W_HEBREW | W_NUMERIC | W_KATAKANA | W_EXTENDNUMLET,
                    W_EXTENDNUMLET,
                ) => true,
                (W_EXTENDNUMLET, W_ALETTER | W_HEBREW | W_NUMERIC | W_KATAKANA) => true,
                (W_RI, W_RI) => ri_odd,
                _ => false,
            }
        };
        if !keep {
            return i;
        }
        ri_odd = p == W_RI && !ri_odd;
        raw = bits;
        prev2 = prev;
        prev = p;
        i += n;
    }
    b.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces(s: &str, end: fn(&[u8], usize) -> usize) -> Vec<&str> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < s.len() {
            let j = end(s.as_bytes(), i);
            out.push(&s[i..j]);
            i = j;
        }
        out
    }

    /// The pieces walking back with `grapheme_start`, in text order.
    fn back(s: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut j = s.len();
        while j > 0 {
            let i = grapheme_start(s.as_bytes(), j);
            out.push(&s[i..j]);
            j = i;
        }
        out.reverse();
        out
    }

    #[test]
    fn graphemes() {
        let g = |s| {
            let fwd = pieces(s, grapheme_end);
            assert_eq!(back(s), fwd, "{s:?}");
            fwd
        };
        assert_eq!(g("e\u{301}x"), ["e\u{301}", "x"]);
        assert_eq!(g("\r\n\n"), ["\r\n", "\n"]);
        assert_eq!(
            g("\u{1F469}\u{200D}\u{1F4BB}!"),
            ["\u{1F469}\u{200D}\u{1F4BB}", "!"]
        );
        assert_eq!(
            g("\u{1F1F0}\u{1F1F7}\u{1F1EF}"),
            ["\u{1F1F0}\u{1F1F7}", "\u{1F1EF}"]
        );
        assert_eq!(
            g("\u{1100}\u{1161}\u{11A8}\u{AC00}"),
            ["\u{1100}\u{1161}\u{11A8}", "\u{AC00}"]
        );
        assert_eq!(g("\u{915}\u{94D}\u{937}"), ["\u{915}\u{94D}\u{937}"]);
        assert_eq!(grapheme_count("a\u{301}b".as_bytes()), 2);
        assert_eq!(grapheme_end(b"a\xffb", 1), 2);
        // A surrogate and an overlong form are one bad byte each.
        assert_eq!(grapheme_count(b"\xed\xa0\x80"), 3);
        assert_eq!(grapheme_count(b"\xc0\xaf"), 2);
    }

    #[test]
    fn graphemes_back() {
        // ZWJ sequences, with skin tones and a variation selector.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        let tone = "\u{1F44B}\u{1F3FD}\u{200D}\u{2642}\u{FE0F}";
        let s = format!("a{family}{tone}\u{200D}b");
        let tone_zwj = format!("{tone}\u{200D}");
        assert_eq!(back(&s), ["a", family, &tone_zwj, "b"]);
        // A ZWJ after a non-pictograph does not join.
        assert_eq!(back("x\u{200D}\u{1F600}"), ["x\u{200D}", "\u{1F600}"]);
        // Regional indicators pair from the start of their run.
        let ri = "\u{1F1F0}\u{1F1F7}\u{1F1EF}\u{1F1F5}\u{1F1FA}";
        assert_eq!(
            back(ri),
            ["\u{1F1F0}\u{1F1F7}", "\u{1F1EF}\u{1F1F5}", "\u{1F1FA}"]
        );
        assert_eq!(
            back(&format!("a{ri}")),
            ["a", "\u{1F1F0}\u{1F1F7}", "\u{1F1EF}\u{1F1F5}", "\u{1F1FA}"]
        );
        // Hangul: syllables, and conjoining jamo L V T.
        assert_eq!(
            back("\u{D55C}\u{AE00}\u{1112}\u{1161}\u{11AB}\u{1100}\u{1173}"),
            [
                "\u{D55C}",
                "\u{AE00}",
                "\u{1112}\u{1161}\u{11AB}",
                "\u{1100}\u{1173}"
            ]
        );
        assert_eq!(
            back("\u{AC00}\u{11A8}\u{1100}"),
            ["\u{AC00}\u{11A8}", "\u{1100}"]
        );
        // Indic conjuncts, CR LF and controls.
        assert_eq!(
            back("\u{915}\u{94D}\u{200D}\u{937}\u{93F}a"),
            ["\u{915}\u{94D}\u{200D}\u{937}\u{93F}", "a"]
        );
        assert_eq!(
            back("a\r\n\r\r\u{301}"),
            ["a", "\r\n", "\r", "\r", "\u{301}"]
        );
        // Bad bytes count one by one, as from the front.
        let bad: &[u8] = b"a\xe0\x80\x80\xf0\x9f\x98\x80\x80\xc0\xaf\xcc\x81";
        let mut fwd = vec![0];
        while fwd.last() != Some(&bad.len()) {
            fwd.push(grapheme_end(bad, *fwd.last().unwrap()));
        }
        let mut rev = vec![bad.len()];
        while rev.last() != Some(&0) {
            rev.push(grapheme_start(bad, *rev.last().unwrap()));
        }
        rev.reverse();
        assert_eq!(rev, fwd);
        assert_eq!(grapheme_start(b"", 0), 0);
    }

    #[test]
    fn words() {
        let w = |s| pieces(s, word_end);
        assert_eq!(
            w("can't stop, 3.14!"),
            ["can't", " ", "stop", ",", " ", "3.14", "!"]
        );
        assert_eq!(w("a.b. c"), ["a.b", ".", " ", "c"]);
        assert_eq!(w("e\u{301}t\u{e9}  x"), ["e\u{301}t\u{e9}", "  ", "x"]);
        assert_eq!(
            w("\u{30AB}\u{30BF}\u{30AB}\u{30CA}\u{6F22}"),
            ["\u{30AB}\u{30BF}\u{30AB}\u{30CA}", "\u{6F22}"]
        );
    }
}
