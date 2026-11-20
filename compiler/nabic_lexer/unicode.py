#!/usr/bin/env python3
"""Generate compiler/nabic_lexer/src/unicode/tables.rs.

Reads the UCD 18.0.0 files checked in under `ucd/` (from
https://www.unicode.org/Public/18.0.0/ucd/; emoji-data.txt from its
`emoji/`), so regeneration is offline and reproducible:
- DerivedCoreProperties.txt: XID_Start / XID_Continue
- PropList.txt: ID_Compat_Math_Start / ID_Compat_Math_Continue (UAX31 7.1)
- emoji-data.txt: Extended_Pictographic
- UnicodeData.txt: canonical combining classes, decompositions, assigned
  code points, marks and the names diagnostics print
- DerivedNormalizationProps.txt: Full_Composition_Exclusion

Usage: python3 compiler/nabic_lexer/unicode.py
To update, re-download those five files into `ucd/` and rerun.
"""

import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "src", "unicode", "tables.rs")


def read(name):
    with open(os.path.join(HERE, "ucd", name), encoding="utf-8") as f:
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


derived = read("DerivedCoreProperties.txt")
VER = re.search(r"DerivedCoreProperties-([0-9.]+)\.txt", derived).group(1)
proplist = read("PropList.txt")
normprops = read("DerivedNormalizationProps.txt")
emoji = read("emoji-data.txt")
for text in (proplist, normprops):
    assert VER in text.splitlines()[0], "UCD files from different versions"
assert f"Version: {VER}" in emoji, "emoji-data.txt version mismatch"

xid_start = ucd_prop(derived, "XID_Start")
xid_continue = ucd_prop(derived, "XID_Continue")
math_start = ucd_prop(proplist, "ID_Compat_Math_Start")
math_continue = ucd_prop(proplist, "ID_Compat_Math_Continue")
ext_pict = ucd_prop(emoji, "Extended_Pictographic")
excluded = ucd_prop(normprops, "Full_Composition_Exclusion")
assert xid_start and math_start and math_continue and ext_pict and excluded

ccc = []  # runs of one non-zero canonical combining class
decomp = []  # one decomposition level (Hangul syllables are algorithmic)
data = []
compose = []
for line in read("UnicodeData.txt").splitlines():
    f = line.split(";")
    c, k, d = int(f[0], 16), int(f[3]), f[5]
    if k:
        if ccc and ccc[-1][1] == c - 1 and ccc[-1][2] == k:
            ccc[-1][1] = c
        else:
            ccc.append([c, c, k])
    if not d:
        continue
    compat = d.startswith("<")
    parts = [int(x, 16) for x in d.split() if not x.startswith("<")]
    decomp.append((c, len(data), len(parts), compat))
    data.extend(parts)
    if not compat and len(parts) == 2 and c not in excluded:
        compose.append((parts[0], parts[1], c))
compose.sort()
assert len(data) < 1 << 16

# Assigned code points (any category but Cn), marks, and the names of the
# characters diagnostics name: operators and punctuation, spaces, format
# characters, common combining marks and the confusable letters.
NAMED_MARKS = [(0x300, 0x36F), (0x1AB0, 0x1AFF), (0x1DC0, 0x1DFF), (0x20D0, 0x20FF),
               (0xFE00, 0xFE0F), (0xFE20, 0xFE2F)]
NAMED_LETTERS = "аеорсухіјѕһԁԛԝӏνοριАВЕКМНОРСТХУІЈЅΑΒΕΚΜΗΟΡΤΧΥΙΖΝ"
assigned = set()
marks = set()
names = {}
first = None
for line in read("UnicodeData.txt").splitlines():
    f = line.split(";")
    c, name, gc = int(f[0], 16), f[1], f[2]
    if name.endswith(", First>"):
        first = c
        continue
    if name.endswith(", Last>"):
        assigned.update(range(first, c + 1))
        continue
    assigned.add(c)
    if gc[0] == "M":
        marks.add(c)
    if name.startswith("<"):
        continue
    want = (
        gc in ("Sm", "Zs", "Zl", "Zp", "Cf")
        or (gc[0] == "P" and c < 0x10000)
        or (gc[0] == "S" and c < 0x2C00)
        or (gc[0] == "M" and any(lo <= c <= hi for lo, hi in NAMED_MARKS))
        or chr(c) in NAMED_LETTERS
        or c in (0x115F, 0x1160, 0x3164, 0xFFA0)
    )
    if want:
        names[c] = name
words = sorted({w for n in names.values() for w in n.split(" ")})
word_ix = {w: i for i, w in enumerate(words)}
assert len(words) < 1 << 16
name_rows = []
name_data = []
for c in sorted(names):
    ws = [word_ix[w] for w in names[c].split(" ")]
    name_rows.append((c, len(name_data), len(ws)))
    name_data.extend(ws)
assert len(name_data) < 1 << 16

out = [
    f"//! Generated by `compiler/nabic_lexer/unicode.py` from UCD {VER}.\n",
    "//! Do not edit by hand.\n\n",
    "#![allow(clippy::unreadable_literal)]\n\n",
    f'pub const UNICODE_VERSION: &str = "{VER}";\n\n',
    fmt_ranges("XID_START", xid_start),
    fmt_ranges("XID_CONTINUE", xid_continue),
    fmt_ranges("MATH_START", math_start),
    fmt_ranges("MATH_CONTINUE", math_continue),
    fmt_ranges("EXT_PICT", ext_pict),
    "/// `(lo, hi, ccc)`: runs with one non-zero canonical combining class.\n",
    "pub static CCC: &[(u32, u32, u8)] = &[\n",
    rows(f"(0x{a:X}, 0x{b:X}, {k})," for a, b, k in ccc),
    "];\n\n",
    "/// `(cp, start, len, compat)` into `DECOMP_DATA`: one decomposition level.\n",
    "pub static DECOMP: &[(u32, u16, u8, bool)] = &[\n",
    rows((f"(0x{c:X}, {s}, {n}, {str(k).lower()})," for c, s, n, k in decomp), 4),
    "];\n\n",
    "pub static DECOMP_DATA: &[u32] = &[\n",
    rows((f"0x{x:X}," for x in data), 10),
    "];\n\n",
    "/// `(first, second, composite)`, sorted: canonical primary composites.\n",
    "pub static COMPOSE: &[(u32, u32, u32)] = &[\n",
    rows((f"(0x{a:X}, 0x{b:X}, 0x{c:X})," for a, b, c in compose), 4),
    "];\n\n",
    fmt_ranges("ASSIGNED", assigned),
    fmt_ranges("MARK", marks),
    "/// Words of the character names in `NAMES`.\n",
    "pub static NAME_WORDS: &[&str] = &[\n",
    rows((f'"{w}",' for w in words), 8),
    "];\n\n",
    "/// `(cp, start, len)` into `NAME_DATA`, sorted: a name's words.\n",
    "pub static NAMES: &[(u32, u16, u8)] = &[\n",
    rows((f"(0x{c:X}, {s}, {n})," for c, s, n in name_rows), 5),
    "];\n\n",
    "pub static NAME_DATA: &[u16] = &[\n",
    rows((f"{x}," for x in name_data), 14),
    "];\n",
]
with open(OUT, "w") as f:
    f.write("".join(out))
print(f"wrote {OUT} (UCD {VER})", file=sys.stderr)
