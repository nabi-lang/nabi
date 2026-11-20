"""gdb pretty-printers for Nabi programs built with `-g`.

gdb itself shows the active variant of a sum, an enum or an `any`. These
show `str` as its text, and `List`, `Dict`, `Set`, `SortedDict`,
`SortedSet`, `Slice` and `Array` as their size (an array's shape) with
the elements as children, a map's as key and value, in iteration order.

    source <sysroot>/lib/nabi/gdb/nabi.py

(`nabic --print gdbinit` prints that line.) It also maps the standard
library's /nabi/library/... paths to the sources beside it.
"""

import importlib.util
import os
import sys

import gdb

_library = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
_spec = importlib.util.spec_from_file_location(
    "nabi_layout", os.path.join(_library, "debug", "layout.py")
)
layout = importlib.util.module_from_spec(_spec)
# No __pycache__ in the toolchain's library.
_keep, sys.dont_write_bytecode = sys.dont_write_bytecode, True
_spec.loader.exec_module(layout)
sys.dont_write_bytecode = _keep


def at(addr, t):
    """The `t` at `addr`."""
    return gdb.Value(addr).cast(t.pointer()).dereference()


def items(ptr):
    """The elements of the `GcBuf<T>` object `ptr` points at: their
    address and `T`, or None for a null reference."""
    if int(ptr) == 0:
        return None
    it = ptr.dereference()["items"]
    t = it.type.strip_typedefs()
    if t.code == gdb.TYPE_CODE_ARRAY:
        t = t.target()
    return int(it.address), t


def head(v):
    """The `ListHead` / `DictHead` the handle `h` of `v` points at."""
    it = items(v["h"])
    return None if it is None else at(it[0], it[1])


def list_items(lst):
    h = head(lst)
    return None if h is None else items(h["buf"])


def run(ptr, first, count, stride):
    it = items(ptr)
    if it is None:
        return
    addr, t = it
    size = t.sizeof
    for i in range(min(count, layout.MAX_CHILDREN)):
        yield "[%d]" % i, at(addr + (first + i * stride) * size, t)


def entries(v):
    """A dict's live entries: (key, value)."""
    h = head(v)
    if h is None or not layout.sane(int(h["n"]), int(h["cap"])):
        return
    keys, vals, meta = items(h["keys"]), items(h["vals"]), items(h["meta"])
    if keys is None or vals is None or meta is None:
        return
    u64 = lambda a: int(at(a, meta[1]))
    for i in layout.dict_live(int(h["n"]), meta[0], u64):
        yield at(keys[0] + i * keys[1].sizeof, keys[1]), at(vals[0] + i * vals[1].sizeof, vals[1])


def tree(v):
    """A SortedDict's (key, value) in key order."""
    lists = [list_items(v[f]) for f in ("meta", "kids", "lens", "leaf", "keys", "vals")]
    if any(x is None for x in lists):
        return
    meta, kids, lens, leaf, keys, vals = lists
    ws = meta[1].sizeof
    word = lambda a: int(at(a, meta[1]))
    byte = lambda a: int(at(a, leaf[1]))
    slots = layout.sorted_slots(
        word(meta[0]), word(meta[0] + ws), kids[0], lens[0], leaf[0], word, byte, ws
    )
    for s in slots:
        yield at(keys[0] + s * keys[1].sizeof, keys[1]), at(vals[0] + s * vals[1].sizeof, vals[1])


def sorted_len(v):
    meta = list_items(v["meta"])
    if meta is None:
        return 0
    return int(at(meta[0] + meta[1].sizeof, meta[1]))


class Str:
    def __init__(self, v):
        self.v = v

    def to_string(self):
        n = int(self.v["len"])
        if n == 0:
            return '""'
        ptr = int(self.v["ptr"])
        try:
            data = gdb.selected_inferior().read_memory(ptr, min(n, layout.MAX_STR))
        except gdb.MemoryError:
            return "<unreadable str at 0x%x>" % ptr
        return layout.text(bytes(data), n)


class Seq:
    def __init__(self, v, kind):
        self.v = v
        self.kind = kind

    def display_hint(self):
        if self.kind in ("dict", "sorted"):
            return "map"
        # A matrix's children keep their `[i, j]` names.
        if self.kind == "array" and self.v["shape"].type.strip_typedefs().range()[1] > 0:
            return None
        return "array"

    def to_string(self):
        v, k = self.v, self.kind
        if k in ("list", "dict", "set"):
            h = head(v if k != "set" else v["d"])
            if h is None:
                return "size=0"
            if not layout.sane(int(h["len"]), int(h["cap"])):
                return "<invalid>"
            return "size=%d" % int(h["len"])
        if k in ("sorted", "sortedset"):
            return "size=%d" % sorted_len(v if k == "sorted" else v["d"])
        if k == "slice":
            return "size=%d" % int(v["len"])
        shape = v["shape"]
        n = shape.type.strip_typedefs().range()[1] + 1
        return "shape=[%s]" % ", ".join(str(int(shape[i])) for i in range(n))

    def children(self):
        v, k = self.v, self.kind
        if k == "list":
            h = head(v)
            if h is not None and layout.sane(int(h["len"]), int(h["cap"])):
                yield from run(h["buf"], 0, int(h["len"]), 1)
        elif k == "slice":
            yield from run(v["buf"], int(v["start"]), int(v["len"]), int(v["stride"]))
        elif k == "array":
            it = items(v["buf"])
            if it is None:
                return
            addr, t = it
            n = v["shape"].type.strip_typedefs().range()[1] + 1
            dims = [int(v["shape"][i]) for i in range(n)]
            steps = [int(v["strides"][i]) for i in range(n)]
            for idx, off in layout.array_order(dims, steps, int(v["off"])):
                yield layout.index_name(idx), at(addr + off * t.sizeof, t)
        elif k in ("dict", "sorted"):
            pairs = entries(v) if k == "dict" else tree(v)
            for i, (key, val) in enumerate(pairs):
                yield "[%d]" % (2 * i), key
                yield "[%d]" % (2 * i + 1), val
        else:
            pairs = entries(v["d"]) if k == "set" else tree(v["d"])
            for i, (key, _) in enumerate(pairs):
                yield "[%d]" % i, key


def lookup(v):
    t = v.type.strip_typedefs()
    if t.code != gdb.TYPE_CODE_STRUCT:
        return None
    k = layout.kind(t.name)
    if k is None:
        return None
    return Str(v) if k == "str" else Seq(v, k)


gdb.pretty_printers.append(lookup)
# Programs name the standard library's files /nabi/library/...; this
# file is <library>/gdb/nabi.py.
gdb.execute("set substitute-path /nabi/library %s" % _library)
