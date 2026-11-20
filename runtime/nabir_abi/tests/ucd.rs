//! UCD conformance: `NormalizationTest.txt`, `GraphemeBreakTest.txt` and
//! `WordBreakTest.txt`. `tests/ucd/` holds the break tests whole and a
//! sample of the normalization test; with `NABI_UCD_TESTS=<dir>` the
//! upstream files there are checked instead, including the rule for the
//! code points Part 1 of the normalization test does not list.

use nabir_abi::unicode::norm::{self, Buf, Form};
use nabir_abi::unicode::segment;
use std::collections::HashSet;
use std::path::PathBuf;

struct Items(Vec<u64>);

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

fn data(name: &str) -> (String, bool) {
    if let Some(dir) = std::env::var_os("NABI_UCD_TESTS") {
        let p = PathBuf::from(dir).join(name);
        return (
            std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())),
            true,
        );
    }
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/ucd")
        .join(name);
    (std::fs::read_to_string(p).expect("tests/ucd"), false)
}

fn cps(field: &str) -> String {
    field
        .split_whitespace()
        .map(|h| char::from_u32(u32::from_str_radix(h, 16).expect("hex")).expect("scalar"))
        .collect()
}

fn nf(s: &str, form: Form) -> String {
    let mut out = String::new();
    norm::normalize(s, form, &mut Items(Vec::new()), |p| out.push_str(p));
    out
}

fn normal(s: &str, form: Form) -> bool {
    norm::is_normalized(s, form, &mut Items(Vec::new()))
}

#[test]
fn normalization() {
    let (text, full) = data("NormalizationTest.txt");
    let mut part1 = HashSet::new();
    let mut part = "";
    let mut checked = 0;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('@') {
            part = line;
            continue;
        }
        let f: Vec<String> = line.split(';').take(5).map(cps).collect();
        let [c1, c2, c3, c4, c5] = &f[..] else {
            panic!("{line}")
        };
        if part.starts_with("@Part1") {
            part1.insert(c1.chars().next().expect("char"));
        }
        for (src, want_c, want_d, want_kc, want_kd) in [
            (c1, c2, c3, c4, c5),
            (c2, c2, c3, c4, c5),
            (c3, c2, c3, c4, c5),
            (c4, c4, c5, c4, c5),
            (c5, c4, c5, c4, c5),
        ] {
            for (form, want) in [
                (Form::Nfc, want_c),
                (Form::Nfd, want_d),
                (Form::Nfkc, want_kc),
                (Form::Nfkd, want_kd),
            ] {
                assert_eq!(&nf(src, form), want, "{form:?} of {line}");
                assert_eq!(
                    normal(src, form),
                    src == want,
                    "is_normalized {form:?} of {line}"
                );
            }
        }
        checked += 1;
    }
    assert!(checked > 700, "{checked} lines");
    if full {
        // Every other code point is its own normal form in all four.
        for c in (0..0x11_0000)
            .filter_map(char::from_u32)
            .filter(|c| !part1.contains(c))
        {
            let s = c.to_string();
            for form in [Form::Nfc, Form::Nfd, Form::Nfkc, Form::Nfkd] {
                assert_eq!(nf(&s, form), s, "{form:?} of U+{:04X}", c as u32);
                assert!(
                    normal(&s, form),
                    "is_normalized {form:?} of U+{:04X}",
                    c as u32
                );
            }
        }
    }
}

/// Each `÷` / `×` line: the pieces `end` cuts must match.
/// With `start`, walking back from the end must cut the same pieces.
fn breaks(name: &str, end: fn(&[u8], usize) -> usize, start: Option<fn(&[u8], usize) -> usize>) {
    let (text, _) = data(name);
    let mut checked = 0;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut s = String::new();
        let mut want = Vec::new();
        for tok in line.split_whitespace() {
            match tok {
                "÷" => want.push(s.len()),
                "×" => {}
                h => s.push(
                    char::from_u32(u32::from_str_radix(h, 16).expect("hex")).expect("scalar"),
                ),
            }
        }
        let mut got = vec![0];
        let mut i = 0;
        while i < s.len() {
            i = end(s.as_bytes(), i);
            got.push(i);
        }
        assert_eq!(got, want, "{name}: {line}");
        if let Some(start) = start {
            let mut got = vec![s.len()];
            let mut j = s.len();
            while j > 0 {
                j = start(s.as_bytes(), j);
                got.push(j);
            }
            got.reverse();
            assert_eq!(got, want, "{name} backward: {line}");
        }
        checked += 1;
    }
    assert!(checked > 800, "{checked} lines");
}

#[test]
fn graphemes() {
    breaks(
        "GraphemeBreakTest.txt",
        segment::grapheme_end,
        Some(segment::grapheme_start),
    );
}

#[test]
fn words() {
    breaks("WordBreakTest.txt", segment::word_end, None);
}
