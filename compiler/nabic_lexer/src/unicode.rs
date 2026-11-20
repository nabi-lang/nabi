//! Identifier character classes and normalization, over the
//! generated tables in `unicode/tables.rs`.

#[rustfmt::skip]
mod tables;

pub use t::UNICODE_VERSION;

use crate::unicode::tables as t;

fn in_ranges(table: &[(u32, u32)], c: char) -> bool {
    let c = c as u32;
    table
        .binary_search_by(|&(lo, hi)| {
            if hi < c {
                std::cmp::Ordering::Less
            } else if lo > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// `Extended_Pictographic` above U+00FF (`©` / `®` are not identifiers).
#[must_use]
pub fn is_pictographic(c: char) -> bool {
    c as u32 > 0xFF && in_ranges(t::EXT_PICT, c)
}

/// Emoji components allowed after the first character. ZWJ is the
/// lexer's (between pictographs).
#[must_use]
pub const fn is_emoji_component(c: char) -> bool {
    matches!(c,
        '\u{FE0E}' | '\u{FE0F}'
        | '\u{1F3FB}'..='\u{1F3FF}'
        | '\u{1F1E6}'..='\u{1F1FF}'
        | '\u{E0020}'..='\u{E007F}')
}

/// `_`, `XID_Start`, the UAX31 math profile start set plus
/// `∑ ∏ ∫`, and pictographs.
#[must_use]
pub fn is_ident_start(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphabetic() || c == '_';
    }
    in_ranges(t::XID_START, c)
        || in_ranges(t::MATH_START, c)
        || matches!(c, '∑' | '∏' | '∫')
        || is_pictographic(c)
}

/// `XID_Continue`, the math profile set, primes, pictographs
/// and emoji components; not ZWJ (the lexer's).
#[must_use]
pub fn is_ident_continue(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || c == '_';
    }
    // ZWJ is XID_Continue since Unicode 15.1, but Nabi allows it only
    // between two pictographs (the lexer checks the neighbours).
    (c != '\u{200D}' && in_ranges(t::XID_CONTINUE, c))
        || in_ranges(t::MATH_CONTINUE, c)
        || matches!(c, '∑' | '∏' | '∫' | '\u{2032}'..='\u{2034}' | '\u{2057}')
        || is_pictographic(c)
        || is_emoji_component(c)
}

fn ccc(c: char) -> u8 {
    let c = c as u32;
    t::CCC
        .binary_search_by(|&(lo, hi, _)| {
            if hi < c {
                std::cmp::Ordering::Less
            } else if lo > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .map_or(0, |i| t::CCC[i].2)
}

const S_BASE: u32 = 0xAC00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT;
const S_COUNT: u32 = L_COUNT * N_COUNT;

fn decompose_into(c: char, compat: bool, out: &mut Vec<char>) {
    let cp = c as u32;
    if (S_BASE..S_BASE + S_COUNT).contains(&cp) {
        let s = cp - S_BASE;
        let push = |out: &mut Vec<char>, v: u32| out.extend(char::from_u32(v));
        push(out, L_BASE + s / N_COUNT);
        push(out, V_BASE + (s % N_COUNT) / T_COUNT);
        if s % T_COUNT != 0 {
            push(out, T_BASE + s % T_COUNT);
        }
        return;
    }
    match t::DECOMP.binary_search_by_key(&cp, |e| e.0) {
        Ok(i) if compat || !t::DECOMP[i].3 => {
            let (_, start, len, _) = t::DECOMP[i];
            let start = usize::from(start);
            for &d in &t::DECOMP_DATA[start..start + usize::from(len)] {
                decompose_into(char::from_u32(d).unwrap_or(c), compat, out);
            }
        }
        _ => out.push(c),
    }
}

fn compose_pair(a: char, b: char) -> Option<char> {
    let (a32, b32) = (a as u32, b as u32);
    if (L_BASE..L_BASE + L_COUNT).contains(&a32) && (V_BASE..V_BASE + V_COUNT).contains(&b32) {
        let s = S_BASE + ((a32 - L_BASE) * V_COUNT + (b32 - V_BASE)) * T_COUNT;
        return char::from_u32(s);
    }
    if (S_BASE..S_BASE + S_COUNT).contains(&a32)
        && (a32 - S_BASE) % T_COUNT == 0
        && (T_BASE + 1..T_BASE + T_COUNT).contains(&b32)
    {
        return char::from_u32(a32 + (b32 - T_BASE));
    }
    t::COMPOSE
        .binary_search_by_key(&(a32, b32), |e| (e.0, e.1))
        .ok()
        .and_then(|i| char::from_u32(t::COMPOSE[i].2))
}

fn normalize(s: &str, compat: bool) -> String {
    if s.is_ascii() {
        return s.to_owned();
    }
    let mut d = Vec::with_capacity(s.len());
    for c in s.chars() {
        decompose_into(c, compat, &mut d);
    }
    // Canonical ordering: stable-sort each run of non-starters by class.
    let mut i = 0;
    while i < d.len() {
        if ccc(d[i]) == 0 {
            i += 1;
            continue;
        }
        let start = i;
        while i < d.len() && ccc(d[i]) != 0 {
            i += 1;
        }
        d[start..i].sort_by_key(|&c| ccc(c));
    }
    // Canonical composition (UAX #15 sample). `last`: class of the last
    // kept char after the starter (0 adjacent, 256 no starter yet).
    let mut out: Vec<char> = Vec::with_capacity(d.len());
    let mut starter: Option<usize> = None;
    let mut last: u16 = 256;
    for c in d {
        let k = u16::from(ccc(c));
        if let Some(si) = starter {
            if last == 0 || last < k {
                if let Some(comp) = compose_pair(out[si], c) {
                    out[si] = comp;
                    continue;
                }
            }
        }
        if k == 0 {
            starter = Some(out.len());
        }
        last = k;
        out.push(c);
    }
    out.into_iter().collect()
}

/// NFC.
#[must_use]
pub fn nfc(s: &str) -> String {
    normalize(s, false)
}

/// NFKC (confusable skeletons only).
#[must_use]
pub fn nfkc(s: &str) -> String {
    normalize(s, true)
}

/// An identifier's canonical spelling - NFC, then the emoji
/// variation selectors U+FE0E / U+FE0F removed.
#[must_use]
pub fn normalize_ident(s: &str) -> String {
    if s.is_ascii() {
        return s.to_owned();
    }
    nfc(s).replace(['\u{FE0E}', '\u{FE0F}'], "")
}

/// Bidirectional formatting controls (U+202A-202E, U+2066-2069): never
/// allowed in source text.
#[must_use]
pub const fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Assigned in the tables' Unicode version (any category but `Cn`).
#[must_use]
pub fn is_assigned(c: char) -> bool {
    c.is_ascii() || in_ranges(t::ASSIGNED, c)
}

/// A combining mark (`Mn`, `Mc`, `Me`): it changes the character
/// before it and cannot start a token.
#[must_use]
pub fn is_mark(c: char) -> bool {
    !c.is_ascii() && in_ranges(t::MARK, c)
}

/// Characters that show nothing: zero-width and format characters,
/// fillers, variation selectors and tags (bidi controls are their own).
#[must_use]
pub const fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{AD}' | '\u{34F}' | '\u{61C}' | '\u{115F}' | '\u{1160}' | '\u{17B4}' | '\u{17B5}'
        | '\u{180B}'..='\u{180F}' | '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}'
        | '\u{206A}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}'
        | '\u{FFA0}' | '\u{1D173}'..='\u{1D17A}' | '\u{E0000}'..='\u{E007F}'
        | '\u{E0100}'..='\u{E01EF}')
}

/// A space other than ASCII space and tab (`U+00A0`, `U+3000`, ...).
#[must_use]
pub const fn is_other_space(c: char) -> bool {
    matches!(
        c,
        '\u{85}' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// The character's Unicode name, for the characters diagnostics name
/// (operators, punctuation, spaces, format characters, common marks).
#[must_use]
pub fn char_name(c: char) -> Option<String> {
    let cp = c as u32;
    if (0xE0100..=0xE01EF).contains(&cp) {
        return Some(format!("VARIATION SELECTOR-{}", cp - 0xE0100 + 17));
    }
    let i = t::NAMES.binary_search_by_key(&cp, |e| e.0).ok()?;
    let (_, start, len) = t::NAMES[i];
    let start = usize::from(start);
    let words: Vec<&str> = t::NAME_DATA[start..start + usize::from(len)]
        .iter()
        .map(|&w| t::NAME_WORDS[usize::from(w)])
        .collect();
    Some(words.join(" "))
}

/// `U+200B ZERO WIDTH SPACE`; `U+XXXX` alone when the name is not
/// in the tables.
#[must_use]
pub fn code_point(c: char) -> String {
    char_name(c).map_or_else(
        || format!("U+{:04X}", c as u32),
        |n| format!("U+{:04X} {n}", c as u32),
    )
}

/// What a diagnostic shows for `c`: the character in backticks, then
/// its code point and name; invisible ones and marks by code point only.
#[must_use]
pub fn describe(c: char) -> String {
    if is_invisible(c) || is_bidi_control(c) || is_other_space(c) || c.is_control() {
        return code_point(c);
    }
    if is_mark(c) {
        return format!("`\u{25CC}{c}` ({})", code_point(c));
    }
    format!("`{c}` ({})", code_point(c))
}

/// The token a non-ASCII character looks like, in code: U+2212 MINUS
/// SIGN is `-`, curly quotes are `"`, fullwidth forms their ASCII twins.
#[must_use]
pub const fn lookalike(c: char) -> Option<&'static str> {
    Some(match c {
        '\u{2010}'..='\u{2015}' | '\u{2212}' | '\u{FE63}' | '\u{FF0D}' => "-",
        '\u{2DC}' | '\u{2053}' | '\u{223C}' | '\u{FF5E}' => "~",
        '\u{2216}' | '\u{29F5}' | '\u{FE68}' | '\u{FF3C}' => "\\",
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{FF02}' => "\"",
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{FF07}' => "'",
        '\u{2217}' | '\u{2731}' | '\u{FF0A}' => "*",
        '\u{2215}' | '\u{2044}' | '\u{FF0F}' => "/",
        '\u{2715}' | '\u{2716}' | '\u{2A2F}' => "×",
        '\u{2219}' | '\u{2022}' => "⋅",
        '\u{2223}' | '\u{FF5C}' => "|",
        '\u{2236}' | '\u{FF1A}' => ":",
        '\u{FF1B}' | '\u{37E}' => ";",
        '\u{FF0C}' | '\u{3001}' => ",",
        '\u{FF0E}' | '\u{3002}' => ".",
        '\u{FF08}' => "(",
        '\u{FF09}' => ")",
        '\u{FF3B}' => "[",
        '\u{FF3D}' => "]",
        '\u{FF5B}' => "{",
        '\u{FF5D}' => "}",
        '\u{FF1D}' | '\u{A78A}' => "=",
        '\u{FF0B}' => "+",
        '\u{FF1C}' | '\u{2039}' => "<",
        '\u{FF1E}' | '\u{203A}' => ">",
        '\u{FF01}' | '\u{1C3}' => "!",
        '\u{FF1F}' => "?",
        '\u{FF06}' => "&",
        '\u{FF03}' => "#",
        '\u{FF05}' | '\u{66A}' => "%",
        '\u{FF3E}' => "^",
        '\u{FF20}' => "@",
        '\u{FF04}' => "$",
        '\u{2A75}' => "==",
        _ => return None,
    })
}

/// The Latin letter a Cyrillic or Greek look-alike folds to in
/// `skeleton`, if any.
#[must_use]
pub fn latin_twin(c: char) -> Option<char> {
    if c.is_ascii() || script(c).is_none() {
        return None;
    }
    let mut buf = [0u8; 4];
    let s = skeleton(c.encode_utf8(&mut buf));
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(l), None) if l.is_ascii_alphabetic() => Some(l),
        _ => None,
    }
}

/// Cheap confusable skeleton (not full UTS #39): NFKC, then Latin
/// look-alike Cyrillic / Greek letters mapped to Latin.
#[must_use]
pub fn skeleton(s: &str) -> String {
    nfkc(s)
        .chars()
        .map(|c| match c {
            'а' => 'a',
            'е' => 'e',
            'о' | 'ο' => 'o',
            'р' | 'ρ' => 'p',
            'с' => 'c',
            'у' => 'y',
            'х' => 'x',
            'і' | 'ι' => 'i',
            'ј' => 'j',
            'ѕ' => 's',
            'һ' => 'h',
            'ԁ' => 'd',
            'ԛ' => 'q',
            'ԝ' => 'w',
            'ӏ' => 'l',
            'ν' => 'v',
            'А' | 'Α' => 'A',
            'В' | 'Β' => 'B',
            'Е' | 'Ε' => 'E',
            'К' | 'Κ' => 'K',
            'М' | 'Μ' => 'M',
            'Н' | 'Η' => 'H',
            'О' | 'Ο' => 'O',
            'Р' | 'Ρ' => 'P',
            'С' => 'C',
            'Т' | 'Τ' => 'T',
            'Х' | 'Χ' => 'X',
            'У' | 'Υ' => 'Y',
            'І' | 'Ι' => 'I',
            'Ј' => 'J',
            'Ѕ' => 'S',
            'Ζ' => 'Z',
            'Ν' => 'N',
            c => c,
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Script {
    Latin,
    Greek,
    Cyrillic,
}

const fn script(c: char) -> Option<Script> {
    match c {
        'A'..='Z' | 'a'..='z' | '\u{C0}'..='\u{24F}' | '\u{1E00}'..='\u{1EFF}' => {
            Some(Script::Latin)
        }
        '\u{370}'..='\u{3FF}' | '\u{1F00}'..='\u{1FFF}' => Some(Script::Greek),
        '\u{400}'..='\u{52F}'
        | '\u{1C80}'..='\u{1C8F}'
        | '\u{2DE0}'..='\u{2DFF}'
        | '\u{A640}'..='\u{A69F}' => Some(Script::Cyrillic),
        _ => None,
    }
}

/// A risky script mix inside one identifier: Cyrillic with Latin or
/// Greek. (Latin + Greek is ordinary math notation - `Δt`, `σx`.)
#[must_use]
pub fn mixed_script(s: &str) -> bool {
    let has = |sc: Script| s.chars().any(|c| script(c) == Some(sc));
    has(Script::Cyrillic) && (has(Script::Latin) || has(Script::Greek))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        for c in [
            'a', 'Z', '_', 'é', 'σ', '가', '∂', '∇', '∞', '𝛁', '∑', '∫', '🦀',
        ] {
            assert!(is_ident_start(c), "{c:?} should start an identifier");
        }
        for c in [
            '1',
            '²',
            '₀',
            '⁺',
            '′',
            '″',
            '\u{301}',
            '\u{1F3FB}',
            '\u{FE0F}',
        ] {
            assert!(!is_ident_start(c) || c == '1', "{c:?}");
            assert!(is_ident_continue(c), "{c:?} should continue an identifier");
        }
        for c in ['©', '®', '×', '√', '∈', '+', '\u{20E3}', '\u{200D}'] {
            assert!(!is_ident_start(c) && !is_ident_continue(c), "{c:?}");
        }
    }

    #[test]
    fn normalization() {
        // Expected values from python `unicodedata.normalize` (Unicode 16).
        let cases = [
            ("e\u{301}", "é", "é"),
            ("\u{1E0B}\u{323}", "\u{1E0D}\u{307}", "\u{1E0D}\u{307}"),
            (
                "a\u{328}\u{301}\u{328}",
                "\u{105}\u{328}\u{301}",
                "\u{105}\u{328}\u{301}",
            ),
            ("\u{1100}\u{1161}\u{11A8}", "각", "각"),
            ("\u{212B}", "Å", "Å"),
            ("σ²", "σ²", "σ2"),
            ("ﬁ", "ﬁ", "fi"),
            ("\u{0344}", "\u{308}\u{301}", "\u{308}\u{301}"),
            ("\u{0F73}", "\u{F71}\u{F72}", "\u{F71}\u{F72}"),
            ("\u{1E9B}\u{323}", "\u{1E9B}\u{323}", "\u{1E69}"),
        ];
        for (src, want_nfc, want_nfkc) in cases {
            assert_eq!(nfc(src), want_nfc, "nfc {src:?}");
            assert_eq!(nfkc(src), want_nfkc, "nfkc {src:?}");
        }
        assert_eq!(normalize_ident("❤\u{FE0F}x"), "❤x");
        assert_eq!(skeleton("σ²"), skeleton("σ2"));
        assert_eq!(skeleton("pаy"), "pay");
        assert!(mixed_script("pаy"));
        assert!(!mixed_script("Δt"));
    }

    /// Check against `NABI_NFC_VECTORS=<file>` (`input;nfc;nfkc` hex lines,
    /// e.g. from Python's `unicodedata`); skipped when unset.
    #[test]
    fn normalization_vectors() {
        let Ok(path) = std::env::var("NABI_NFC_VECTORS") else {
            return;
        };
        let text = std::fs::read_to_string(path).unwrap();
        let dec = |f: &str| -> String {
            f.split_whitespace()
                .map(|h| char::from_u32(u32::from_str_radix(h, 16).unwrap()).unwrap())
                .collect()
        };
        let mut bad = 0;
        for line in text.lines() {
            let f: Vec<&str> = line.split(';').collect();
            let src = dec(f[0]);
            if nfc(&src) != dec(f[1]) || nfkc(&src) != dec(f[2]) {
                bad += 1;
                if bad < 10 {
                    eprintln!("mismatch: {line}");
                }
            }
        }
        assert_eq!(bad, 0);
    }
}
