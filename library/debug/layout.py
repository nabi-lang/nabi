"""How the standard library's collections lie in memory, for the lldb and
gdb formatters (library/lldb/nabi.py, library/gdb/nabi.py). Pure Python:
the debugger side reads memory through the callables passed in.

The layouts are the library's own (library/core/src/collections/...):

- `str`: `{ptr: u8 *, len}`, UTF-8 bytes.
- a `GcBuf<T>` reference points at its object: a 16-byte header, then the
  `T`s (`items`).
- `List<T>`: `h` -> `ListHead { len, cap, buf: GcBuf<T>, ver }`.
- `Dict<K, V, S>`: `h` -> `DictHead { len, n, cap, slots, keys, vals, meta,
  .. }`; entries `0..n` in insertion order, `meta[i] == 0` a removed one.
- `Set<T, S>`: `d: Dict<T, bool, S>`.
- `SortedDict<K, V>`: a B-tree in flat lists; node `n` has its keys and
  values at `n * FULL ..`, children at `n * (FULL + 1) ..`, `lens[n]` keys,
  `leaf[n]`; `meta[0]` is the root, `meta[1]` the entry count.
- `SortedSet<T>`: `d: SortedDict<T, bool>`.
- `Slice<T>`: `{buf, start, len, stride}`.
- `Array<T, N>`: `{buf, off, shape: [usize; N], strides: [isize; N]}`.
"""

import re

# Keys a SortedDict node holds at most (2 * MIN - 1, MIN = 16).
FULL = 31

# Children shown at most; the summary still gives the true size.
MAX_CHILDREN = 10000

# Bytes of a string shown at most.
MAX_STR = 4096

KINDS = [
    ("list", re.compile(r"^core::collections::list::List<.+>$")),
    ("dict", re.compile(r"^core::collections::dict::Dict<.+>$")),
    ("set", re.compile(r"^core::collections::set::Set<.+>$")),
    ("sorted", re.compile(r"^core::collections::sorted::SortedDict<.+>$")),
    ("sortedset", re.compile(r"^core::collections::sorted::SortedSet<.+>$")),
    ("slice", re.compile(r"^core::collections::slice::Slice<.+>$")),
    ("array", re.compile(r"^core::array::Array<.+>$")),
    ("str", re.compile(r"^str$")),
]


def sane(length, cap):
    """Whether a length and capacity read from a value can be its own: a
    variable not yet assigned holds whatever was in its slot."""
    return 0 <= length <= cap < (1 << 40)


def kind(type_name):
    """Which collection a type of this name is, or None."""
    for k, r in KINDS:
        if r.match(type_name or ""):
            return k
    return None


def text(data, length):
    """A `str`'s bytes as a quoted literal, cut at `MAX_STR` bytes."""
    s = bytes(data[:MAX_STR]).decode("utf-8", "replace")
    out = []
    for c in s:
        if c == '"' or c == "\\":
            out.append("\\" + c)
        elif c == "\n":
            out.append("\\n")
        elif c == "\t":
            out.append("\\t")
        elif c == "\r":
            out.append("\\r")
        elif ord(c) < 0x20 or ord(c) == 0x7F:
            out.append("\\u{%x}" % ord(c))
        else:
            out.append(c)
    more = "..." if length > MAX_STR else ""
    return '"' + "".join(out) + '"' + more


def dict_live(n, meta, u64):
    """Entry numbers of a dict's live entries in insertion order: those
    of `0..n` whose hash in `meta` (the address of the u64s) is not 0."""
    out = []
    for i in range(n):
        if u64(meta + 8 * i) != 0:
            out.append(i)
            if len(out) >= MAX_CHILDREN:
                break
    return out


def sorted_slots(root, count, kids, lens, leaf, word, byte, ws):
    """Key slots of a SortedDict in key order. `kids` / `lens` / `leaf`
    are the addresses of those lists' elements; `word` reads a usize of
    `ws` bytes, `byte` a bool."""
    out = []
    want = min(count, MAX_CHILDREN)
    # (node, i): descend into child i, after emitting key i - 1.
    stack = [(root, 0)]
    steps = 0
    while stack and len(out) < want:
        steps += 1
        if steps > 4 * count + 64:
            break  # a broken tree: stop rather than loop
        n, i = stack.pop()
        keys = word(lens + ws * n)
        if byte(leaf + n) != 0:
            out.extend(n * FULL + k for k in range(keys))
            continue
        if i > 0:
            out.append(n * FULL + i - 1)
        if i < keys:
            stack.append((n, i + 1))
        stack.append((word(kids + ws * (n * (FULL + 1) + i)), 0))
    return out[:want]


def array_order(shape, strides, off):
    """`(index, element number)` of an Array's elements in row-major
    order, up to `MAX_CHILDREN`."""
    total = 1
    for s in shape:
        total *= s
    out = []
    for flat in range(min(total, MAX_CHILDREN)):
        idx = []
        rest = flat
        for s in reversed(shape):
            idx.append(rest % s if s else 0)
            rest = rest // s if s else 0
        idx.reverse()
        at = off + sum(i * st for i, st in zip(idx, strides))
        out.append((tuple(idx), at))
    return out


def index_name(idx):
    """`[1, 2]` of an element index."""
    return "[" + ", ".join(str(i) for i in idx) + "]"
