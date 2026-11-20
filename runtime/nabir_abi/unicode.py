#!/usr/bin/env python3
"""Generate runtime/nabir_abi/src/unicode/tables.rs (`char` / `str`).

Reads the UCD files checked in under `compiler/nabic_lexer/ucd/` (shared
with the lexer's `unicode.py`; UCD 18.0.0 from
https://www.unicode.org/Public/18.0.0/ucd/), so regeneration is offline:
- DerivedCoreProperties.txt: Alphabetic, Lowercase, Uppercase, Cased,
  Case_Ignorable
- PropList.txt: White_Space
- UnicodeData.txt: numeric categories (Nd / Nl / No), simple case mappings
  (upper, lower, title)
- SpecialCasing.txt: unconditional full case mappings (upper, lower, title)
- UnicodeData.txt, DerivedNormalizationProps.txt: canonical combining
  classes, decompositions, composition exclusions and quick checks
- CaseFolding.txt: full case folding (statuses C and F)
- GraphemeBreakProperty.txt, WordBreakProperty.txt, emoji-data.txt,
  DerivedCoreProperties.txt (InCB): UAX #29 segmentation

Usage: python3 runtime/nabir_abi/unicode.py
"""

import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
UCD = os.path.join(HERE, "..", "..", "compiler", "nabic_lexer", "ucd")
OUT = os.path.join(HERE, "src", "unicode", "tables.rs")


def read(name):
    with open(os.path.join(UCD, name), encoding="utf-8") as f:
        return f.read()


def ucd_prop(text, name):
    cps = set()
    for line in text.splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        rng, prop = (s.strip() for s in line.split(";")[:2])
        if prop != name:
            continue
        lo, _, hi = rng.partition("..")
        cps.update(range(int(lo, 16), int(hi or lo, 16) + 1))
    return cps


def ranges(cps):
    out = []
    for c in sorted(cps):
        if out and out[-1][1] == c - 1:
            out[-1][1] = c
        else:
            out.append([c, c])
    return out


def rows(items, per=6):
    items = list(items)
    return "".join(
        "    " + " ".join(items[i : i + per]) + "\n" for i in range(0, len(items), per)
    )


def fmt_ranges(name, cps):
    body = rows(f"(0x{lo:X}, 0x{hi:X})," for lo, hi in ranges(cps))
    return f"pub static {name}: &[(u32, u32)] = &[\n{body}];\n\n"


def fmt_map(name, pairs):
    body = rows((f"(0x{a:X}, 0x{b:X})," for a, b in sorted(pairs)), 6)
    return f"pub static {name}: &[(u32, u32)] = &[\n{body}];\n\n"


def fmt_special(name, entries):
    def three(xs):
        xs = xs + [0] * (3 - len(xs))
        return "[" + ", ".join(f"0x{x:X}" for x in xs) + "]"

    body = rows((f"(0x{c:X}, {three(m)})," for c, m in sorted(entries.items())), 3)
    return f"pub static {name}: &[(u32, [u32; 3])] = &[\n{body}];\n\n"


derived = read("DerivedCoreProperties.txt")
VER = re.search(r"DerivedCoreProperties-([0-9.]+)\.txt", derived).group(1)
proplist = read("PropList.txt")
special = read("SpecialCasing.txt")
for text in (proplist, special):
    assert VER in text.splitlines()[0], "UCD files from different versions"

numeric = set()
lower, upper, title = [], [], []
for line in read("UnicodeData.txt").splitlines():
    f = line.split(";")
    c, cat = int(f[0], 16), f[2]
    if cat in ("Nd", "Nl", "No"):
        numeric.add(c)
    if f[12]:
        upper.append((c, int(f[12], 16)))
    if f[13]:
        lower.append((c, int(f[13], 16)))
    # Simple titlecase defaults to simple uppercase: keep only the others.
    up = int(f[12], 16) if f[12] else c
    if f[14] and int(f[14], 16) != up:
        title.append((c, int(f[14], 16)))
# `First>` / `Last>` range rows: only CJK / Hangul / ... letters, none numeric.

full_lower, full_upper, full_title = {}, {}, {}
for line in special.splitlines():
    line = line.split("#", 1)[0].strip()
    if not line:
        continue
    f = [x.strip() for x in line.split(";")]
    if f[4]:  # a condition (Final_Sigma, locales): handled in code or skipped
        continue
    c = int(f[0], 16)
    lo = [int(x, 16) for x in f[1].split()]
    ti = [int(x, 16) for x in f[2].split()]
    up = [int(x, 16) for x in f[3].split()]
    assert len(lo) <= 3 and len(ti) <= 3 and len(up) <= 3
    if lo != [c] and len(lo) > 1:
        full_lower[c] = lo
    if up != [c] and len(up) > 1:
        full_upper[c] = up
    if len(ti) > 1:
        full_title[c] = ti


def ucd_values(text, name):
    """`{cp: value}` of a `cp ; name ; value` property."""
    out = {}
    for line in text.splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        f = [x.strip() for x in line.split(";")]
        if len(f) < 3 or f[1] != name:
            continue
        lo, _, hi = f[0].partition("..")
        for c in range(int(lo, 16), int(hi or lo, 16) + 1):
            out[c] = f[2]
    return out


def ucd_classes(text):
    """`{cp: value}` of a `cp ; value` property file."""
    out = {}
    for line in text.splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        rng, val = (x.strip() for x in line.split(";")[:2])
        lo, _, hi = rng.partition("..")
        for c in range(int(lo, 16), int(hi or lo, 16) + 1):
            out[c] = val
    return out


def breakpoints(vals, bits=8):
    """`(start << bits) | value` where the value changes (0 where unset)."""
    out, prev = [0], 0
    for c in sorted(set(vals) | {c + 1 for c in vals}):
        v = vals.get(c, 0)
        assert v < 1 << bits
        if v != prev:
            out.append((c << bits) | v)
            prev = v
    return out


def check_breakpoints(table, vals, bits=8):
    j = 0
    for c in range(0x110000):
        while j + 1 < len(table) and (table[j + 1] >> bits) <= c:
            j += 1
        assert table[j] & ((1 << bits) - 1) == vals.get(c, 0), hex(c)


def fmt_u32s(name, xs, per=8):
    body = rows((f"0x{x:X}," for x in xs), per)
    return f"pub static {name}: &[u32] = &[\n{body}];\n\n"


def fmt_text(name, t):
    """`t` as a `&str` literal, non-ASCII escaped."""
    lines, cur = [], ""
    for ch in t:
        x = ord(ch)
        # A continued line drops its leading blanks: escape a first space.
        plain = 0x20 <= x < 0x7F and ch not in '"\\' and (cur or ch != " ")
        cur += ch if plain else f"\\u{{{x:X}}}"
        if len(cur) >= 88:
            lines.append(cur)
            cur = ""
    if cur:
        lines.append(cur)
    body = "".join(f"    {l}\\\n" for l in lines)
    return f'pub static {name}: &str = "\\\n{body}";\n\n'


normprops = read("DerivedNormalizationProps.txt")
folding = read("CaseFolding.txt")
gcb_text = read("GraphemeBreakProperty.txt")
wb_text = read("WordBreakProperty.txt")
emoji = read("emoji-data.txt")
for text in (normprops, folding, gcb_text, wb_text):
    assert VER in text.splitlines()[0], "UCD files from different versions"
assert f"Version: {VER}" in emoji, "emoji-data.txt version mismatch"

HANGUL = range(0xAC00, 0xD7A4)

# Normalization.
excluded = ucd_prop(normprops, "Full_Composition_Exclusion")
ccc, dmap = {}, {}
for line in read("UnicodeData.txt").splitlines():
    f = line.split(";")
    c = int(f[0], 16)
    if int(f[3]):
        ccc[c] = int(f[3])
    if f[5]:
        dmap[c] = (f[5].startswith("<"), [int(x, 16) for x in f[5].split() if x[0] != "<"])


def hangul(c):
    s = c - 0xAC00
    out = [0x1100 + s // 588, 0x1161 + s % 588 // 28]
    if s % 28:
        out.append(0x11A7 + s % 28)
    return out


def full(c, compat):
    if c in HANGUL:
        return hangul(c)
    d = dmap.get(c)
    if not d or (d[0] and not compat):
        return [c]
    return [x for p in d[1] for x in full(p, compat)]


QC_BITS = {("NFD_QC", "N"): 1, ("NFKD_QC", "N"): 2, ("NFC_QC", "N"): 4,
           ("NFC_QC", "M"): 8, ("NFKC_QC", "N"): 16, ("NFKC_QC", "M"): 32}
qc = {}
for (prop, val), bit in QC_BITS.items():
    for c, v in ucd_values(normprops, prop).items():
        if v == val:
            qc[c] = qc.get(c, 0) | bit
for c in HANGUL:
    assert qc[c] == 3
normv = {c: ccc.get(c, 0) | (qc.get(c, 0) << 8) for c in set(ccc) | set(qc)}
norm_bp = breakpoints(normv, 16)
check_breakpoints(norm_bp, normv, 16)

text = ""
spans = {}


def put(cps):
    global text
    b = "".join(map(chr, cps))
    if b not in spans:
        at = text.find(b)
        if at < 0:
            at = len(text)
            text += b
        n = len(b.encode("utf-8"))
        at = len(text[:at].encode("utf-8"))
        assert at < 1 << 14 and n < 1 << 7
        spans[b] = (at << 7) | n
    return spans[b]


decomp = []
for c in sorted(dmap):
    canon = full(c, False)
    compat = full(c, True)
    v = put(canon) if canon != [c] else 0
    k = put(compat) if compat != [c] else 0
    assert k, hex(c)
    decomp.append((c << 42) | (v << 21) | k)

decomp_max = max(len(full(c, True)) for c in dmap)
compose = []
for c, (is_compat, parts) in dmap.items():
    if not is_compat and len(parts) == 2 and c not in excluded:
        compose.append((parts[0] << 42) | (parts[1] << 21) | c)
        # A second element is never below U+0300 and always QC Maybe.
        assert parts[1] >= 0x300 and qc.get(parts[1], 0) & 8, hex(c)
compose.sort()

# Full case folding: kept where it is not the simple lowercase mapping.
fold = {}
for c, status, m in (
    (int(f[0], 16), f[1].strip(), [int(x, 16) for x in f[2].split()])
    for f in (l.split(";") for l in folding.splitlines() if l and l[0] != "#")
):
    if status in ("C", "F"):
        fold[c] = m
simple_lower = dict(lower)
fold_x = {}
for c in set(fold) | set(simple_lower):
    f = fold.get(c, [c])
    if f != [simple_lower.get(c, c)]:
        assert len(f) <= 3
        fold_x[c] = f

# Segmentation.
GCB = ["Other", "CR", "LF", "Control", "Extend", "ZWJ", "Regional_Indicator", "Prepend",
       "SpacingMark", "L", "V", "T", "LV", "LVT"]
gcb = {c: GCB.index(v) for c, v in ucd_classes(gcb_text).items()}
for c in HANGUL:
    assert gcb[c] == (12 if (c - 0xAC00) % 28 == 0 else 13)
    gcb[c] = 12
pict = ucd_prop(emoji, "Extended_Pictographic")
INCB = {"Consonant": 1, "Extend": 2, "Linker": 3}
incb = {c: INCB[v] for c, v in ucd_values(derived, "InCB").items()}
gvals = {}
for c in set(gcb) | pict | set(incb):
    v = gcb.get(c, 0) | (16 if c in pict else 0) | (incb.get(c, 0) << 5)
    if v:
        gvals[c] = v
gcb_bp = breakpoints(gvals)
check_breakpoints(gcb_bp, gvals)

WB = ["Other", "CR", "LF", "Newline", "Extend", "ZWJ", "Regional_Indicator", "Format",
      "Katakana", "Hebrew_Letter", "ALetter", "Single_Quote", "Double_Quote", "MidNumLet",
      "MidLetter", "MidNum", "Numeric", "ExtendNumLet", "WSegSpace"]
wb = {c: WB.index(v) for c, v in ucd_classes(wb_text).items()}
wvals = {}
for c in set(wb) | pict:
    v = wb.get(c, 0) | (32 if c in pict else 0)
    if v:
        wvals[c] = v
wb_bp = breakpoints(wvals)
check_breakpoints(wb_bp, wvals)

out = [
    f"//! Generated by `runtime/nabir_abi/unicode.py` from UCD {VER}.\n",
    "//! Do not edit by hand.\n\n",
    "#![allow(clippy::unreadable_literal)]\n\n",
    fmt_ranges("ALPHABETIC", ucd_prop(derived, "Alphabetic")),
    fmt_ranges("LOWERCASE", ucd_prop(derived, "Lowercase")),
    fmt_ranges("UPPERCASE", ucd_prop(derived, "Uppercase")),
    fmt_ranges("CASED", ucd_prop(derived, "Cased")),
    fmt_ranges("CASE_IGNORABLE", ucd_prop(derived, "Case_Ignorable")),
    fmt_ranges("WHITE_SPACE", ucd_prop(proplist, "White_Space")),
    fmt_ranges("NUMERIC", numeric),
    "/// `(c, simple lowercase)`, sorted.\n",
    fmt_map("TO_LOWER", lower),
    "/// `(c, simple uppercase)`, sorted.\n",
    fmt_map("TO_UPPER", upper),
    "/// `(c, simple titlecase)` where it differs from the simple uppercase,\n",
    "/// sorted.\n",
    fmt_map("TO_TITLE", title),
    "/// Unconditional multi-char lowercase mappings (0-padded).\n",
    fmt_special("FULL_LOWER", full_lower),
    "/// Unconditional multi-char uppercase mappings (0-padded).\n",
    fmt_special("FULL_UPPER", full_upper),
    "/// Unconditional multi-char titlecase mappings (0-padded).\n",
    fmt_special("FULL_TITLE", full_title),
    "/// Breakpoints `(start << 16) | (qc << 8) | ccc` up to the next start.\n",
    "///\n",
    "/// `ccc` is the canonical combining class, `qc` the quick check answers\n",
    "/// other than Yes: 1 `NFD_QC=N`, 2 `NFKD_QC=N`, 4 `NFC_QC=N`,\n",
    "/// 8 `NFC_QC=M`, 16 `NFKC_QC=N`, 32 `NFKC_QC=M`.\n",
    "pub static NORM: &[u64] = &[\n",
    rows((f"0x{x:X}," for x in norm_bp), 6),
    "];\n\n",
    "/// `(cp << 42) | (canonical << 21) | compatibility`, sorted.\n",
    "///\n",
    "/// The full decompositions of `cp` as `(offset << 7) | len` into\n",
    "/// `DECOMP_TEXT` (canonical 0: none). Hangul syllables are algorithmic.\n",
    "pub static DECOMP: &[u64] = &[\n",
    rows((f"0x{x:X}," for x in decomp), 4),
    "];\n\n",
    "/// UTF-8 text of the decompositions.\n",
    fmt_text("DECOMP_TEXT", text),
    f"/// The most code points one character decomposes to.\n",
    f"pub const DECOMP_MAX: usize = {decomp_max};\n\n",
    "/// `(first << 42) | (second << 21) | composite`, sorted: primary\n",
    "/// composites (no exclusions, no Hangul).\n",
    "pub static COMPOSE: &[u64] = &[\n",
    rows((f"0x{x:X}," for x in compose), 4),
    "];\n\n",
    "/// Full case folding (C + F) where it is not the simple lowercase\n",
    "/// mapping (0-padded), sorted.\n",
    fmt_special("FOLD", fold_x),
    "/// Breakpoints `(start << 8) | v` of the grapheme break properties.\n",
    "///\n",
    "/// `v & 15` is the `Grapheme_Cluster_Break` (0 Other, 1 CR, 2 LF,\n",
    "/// 3 Control, 4 Extend, 5 ZWJ, 6 RI, 7 Prepend, 8 `SpacingMark`, 9 L,\n",
    "/// 10 V, 11 T, 12 LV, 13 LVT; Hangul syllables are all 12), 16 is\n",
    "/// `Extended_Pictographic`, `v >> 5` the `InCB` (1 Consonant, 2 Extend,\n",
    "/// 3 Linker).\n",
    fmt_u32s("GRAPHEME", gcb_bp),
    "/// Breakpoints `(start << 8) | v` of the word break properties.\n",
    "///\n",
    "/// `v & 31` is the `Word_Break` (0 Other, 1 CR, 2 LF, 3 Newline,\n",
    "/// 4 Extend, 5 ZWJ, 6 RI, 7 Format, 8 Katakana, 9 `Hebrew_Letter`,\n",
    "/// 10 `ALetter`, 11 `Single_Quote`, 12 `Double_Quote`, 13 `MidNumLet`,\n",
    "/// 14 `MidLetter`, 15 `MidNum`, 16 Numeric, 17 `ExtendNumLet`,\n",
    "/// 18 `WSegSpace`), 32 is `Extended_Pictographic`.\n",
    fmt_u32s("WORD", wb_bp),
    "/// `WORD` values of ASCII.\n",
    "pub static WORD_ASCII: [u8; 128] = [\n",
    rows((f"{wvals.get(c, 0)}," for c in range(128)), 16),
    "];\n",
]
with open(OUT, "w") as f:
    f.write("".join(out).rstrip("\n") + "\n")
print(f"wrote {OUT} (UCD {VER})", file=sys.stderr)
print(
    f"norm {len(norm_bp)} decomp {len(decomp)} text {len(text)} "
    f"compose {len(compose)} fold {len(fold_x)} grapheme {len(gcb_bp)} word {len(wb_bp)}",
    file=sys.stderr,
)
