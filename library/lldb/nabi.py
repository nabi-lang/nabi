"""lldb formatters for Nabi programs built with `-g`.

lldb reads a DWARF variant part (a sum, an enum, an `any`) as a union of
every variant, `$variants$`, each `$variant$<discriminant>` holding the
discriminant `$discr$` and the variant's struct `value`. These show the
active variant only: its name as the summary, its fields as the children
(a boxed `any` payload through its box).

`str` shows as its text; `List`, `Dict`, `Set`, `SortedDict`,
`SortedSet`, `Slice` and `Array` as their size (an array's shape) with
the elements as children, a map's named by key, in iteration order.

    command script import <sysroot>/lib/nabi/lldb/nabi.py

(`nabic --print lldbinit` prints that line.) It also maps the standard
library's /nabi/library/... paths to the sources beside it.
"""

import importlib.util
import os
import sys

import lldb

# The collections' layouts, shared with the gdb formatters.
_spec = importlib.util.spec_from_file_location(
    "nabi_layout",
    os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "debug", "layout.py"),
)
layout = importlib.util.module_from_spec(_spec)
# No __pycache__ in the toolchain's library.
_keep, sys.dont_write_bytecode = sys.dont_write_bytecode, True
_spec.loader.exec_module(layout)
sys.dont_write_bytecode = _keep


def is_sum(t, _dict):
    t = t.GetCanonicalType()
    return t.GetNumberOfFields() > 0 and t.GetFieldAtIndex(0).GetName() == "$variants$"


def active(v):
    """The active variant: its `$variant$<n>` member and the type of its
    `value`, or None. Read from the types and the bytes: walking the
    children here would leave the synthetic ones empty."""
    data = v.GetNonSyntheticValue().GetData()
    variants = v.GetType().GetCanonicalType().GetFieldAtIndex(0).GetType()
    err = lldb.SBError()
    for i in range(variants.GetNumberOfFields()):
        f = variants.GetFieldAtIndex(i)
        fields = [f.GetType().GetFieldAtIndex(k) for k in range(f.GetType().GetNumberOfFields())]
        by_name = {x.GetName(): x for x in fields}
        d, value = by_name.get("$discr$"), by_name.get("value")
        if d is None or value is None:
            continue
        at = f.GetOffsetInBytes() + d.GetOffsetInBytes()
        if d.GetType().GetByteSize() == 1:
            discr = data.GetUnsignedInt8(err, at)
        else:
            discr = data.GetUnsignedInt32(err, at)
        if err.Success() and f.GetName() == "$variant$%d" % discr:
            return f.GetName(), value.GetType()
    return None


def variant_name(sum_name, name):
    """`Shape::Rect` of `Shape` as `Rect`."""
    return name[len(sum_name) + 2 :] if name.startswith(sum_name + "::") else name


def child(v, c):
    """Field `c` of the active variant as a child of `v`; a pointer to a
    `box<T>` object as its value."""
    t = c.GetType()
    if t.IsPointerType() and t.GetPointeeType().GetName().startswith("box<") and c.GetValueAsUnsigned():
        b = c.Dereference().GetChildMemberWithName("value")
        if b.IsValid():
            return v.CreateValueFromAddress(c.GetName(), b.GetLoadAddress(), b.GetType())
    return v.CreateChildAtOffset(c.GetName(), c.GetLoadAddress() - v.GetLoadAddress(), t)


class Sum:
    def __init__(self, v, _dict):
        self.v = v
        self.kids = []

    def update(self):
        self.kids = []
        a = active(self.v)
        if a is not None:
            variants = self.v.GetNonSyntheticValue().GetChildAtIndex(0)
            value = variants.GetChildMemberWithName(a[0]).GetChildMemberWithName("value")
            self.kids = [
                child(self.v, value.GetChildAtIndex(i)) for i in range(value.GetNumChildren())
            ]
        return False

    def num_children(self):
        return len(self.kids)

    def get_child_index(self, name):
        for i, c in enumerate(self.kids):
            if c.GetName() == name:
                return i
        return -1

    def get_child_at_index(self, i):
        return self.kids[i] if 0 <= i < len(self.kids) else None

    def has_children(self):
        return bool(self.kids)


def summary(v, _dict):
    a = active(v)
    if a is None:
        return "<invalid discriminant>"
    return variant_name(v.GetType().GetCanonicalType().GetName(), a[1].GetName())


def read_uint(v, addr, size):
    err = lldb.SBError()
    n = v.GetProcess().ReadUnsignedFromMemory(addr, size, err)
    return n if err.Success() else 0


def items(ptr):
    """The elements of the `GcBuf<T>` object `ptr` points at: their
    address and `T`, or None for a null reference."""
    if not ptr.IsValid() or ptr.GetValueAsUnsigned() == 0:
        return None
    buf = ptr.GetType().GetPointeeType()
    for k in range(buf.GetNumberOfFields()):
        f = buf.GetFieldAtIndex(k)
        if f.GetName() == "items":
            t = f.GetType()
            if t.IsArrayType():
                t = t.GetArrayElementType()
            return ptr.GetValueAsUnsigned() + f.GetOffsetInBytes(), t
    return None


def head(v):
    """The `ListHead` / `DictHead` the handle `h` of `v` points at."""
    it = items(v.GetChildMemberWithName("h"))
    if it is None:
        return None
    return v.CreateValueFromAddress("head", it[0], it[1])


def list_items(lst):
    """The elements of `List` `lst`: their address and type, or None."""
    h = head(lst)
    if h is None:
        return None
    return items(h.GetChildMemberWithName("buf"))


def field(v, name):
    return v.GetChildMemberWithName(name).GetValueAsUnsigned()


def plain(v):
    """A value's text, for a map child's name."""
    s = v.GetSummary()
    if s:
        return s
    s = v.GetValue()
    return s if s is not None else "?"


class Seq:
    """`List`, `Slice`, `Array`, `Dict`, `Set`, `SortedDict` and
    `SortedSet`: the elements as children."""

    def __init__(self, v, _dict):
        self.v = v
        self.kids = []

    def update(self):
        self.kids = []
        v = self.v.GetNonSyntheticValue()
        try:
            k = layout.kind(v.GetType().GetCanonicalType().GetName())
            self.kids = getattr(self, "of_" + k)(v)
        except Exception:  # a half-built value: show none rather than fail
            self.kids = []
        return False

    def at(self, name, addr, t):
        return self.v.CreateValueFromAddress(name, addr, t)

    def run(self, ptr, first, count, stride, name=None):
        it = items(ptr)
        if it is None:
            return []
        addr, t = it
        size = t.GetByteSize()
        n = min(count, layout.MAX_CHILDREN)
        return [
            self.at(name(i) if name else "[%d]" % i, addr + (first + i * stride) * size, t)
            for i in range(n)
        ]

    def of_list(self, v):
        h = head(v)
        if h is None or not layout.sane(field(h, "len"), field(h, "cap")):
            return []
        return self.run(h.GetChildMemberWithName("buf"), 0, field(h, "len"), 1)

    def of_slice(self, v):
        stride = v.GetChildMemberWithName("stride").GetValueAsSigned()
        return self.run(v.GetChildMemberWithName("buf"), field(v, "start"), field(v, "len"), stride)

    def of_array(self, v):
        it = items(v.GetChildMemberWithName("buf"))
        if it is None:
            return []
        addr, t = it
        shape = v.GetChildMemberWithName("shape")
        strides = v.GetChildMemberWithName("strides")
        dims = [shape.GetChildAtIndex(i).GetValueAsUnsigned() for i in range(shape.GetNumChildren())]
        steps = [strides.GetChildAtIndex(i).GetValueAsSigned() for i in range(strides.GetNumChildren())]
        size = t.GetByteSize()
        return [
            self.at(layout.index_name(idx), addr + at * size, t)
            for idx, at in layout.array_order(dims, steps, field(v, "off"))
        ]

    def entries(self, v):
        """A dict's live entries: (key, value) children."""
        h = head(v)
        if h is None or not layout.sane(field(h, "n"), field(h, "cap")):
            return []
        keys = items(h.GetChildMemberWithName("keys"))
        vals = items(h.GetChildMemberWithName("vals"))
        meta = items(h.GetChildMemberWithName("meta"))
        if keys is None or vals is None or meta is None:
            return []
        live = layout.dict_live(field(h, "n"), meta[0], lambda a: read_uint(v, a, 8))
        ks, vs = keys[1].GetByteSize(), vals[1].GetByteSize()
        return [
            (self.at("key", keys[0] + i * ks, keys[1]), vals[0] + i * vs, vals[1]) for i in live
        ]

    def of_dict(self, v):
        return [self.at("[%s]" % plain(k), a, t) for k, a, t in self.entries(v)]

    def of_set(self, v):
        d = v.GetChildMemberWithName("d")
        return [self.at("[%d]" % i, k.GetLoadAddress(), k.GetType()) for i, (k, _, _) in enumerate(self.entries(d))]

    def tree(self, v):
        """A SortedDict's key slots in order, and its key and value lists'
        elements."""
        meta = list_items(v.GetChildMemberWithName("meta"))
        kids = list_items(v.GetChildMemberWithName("kids"))
        lens = list_items(v.GetChildMemberWithName("lens"))
        leaf = list_items(v.GetChildMemberWithName("leaf"))
        keys = list_items(v.GetChildMemberWithName("keys"))
        vals = list_items(v.GetChildMemberWithName("vals"))
        if None in (meta, kids, lens, leaf, keys, vals):
            return [], None, None
        ws = meta[1].GetByteSize()
        word = lambda a: read_uint(v, a, ws)
        slots = layout.sorted_slots(
            word(meta[0]), word(meta[0] + ws), kids[0], lens[0], leaf[0], word,
            lambda a: read_uint(v, a, 1), ws,
        )
        return slots, keys, vals

    def of_sorted(self, v):
        slots, keys, vals = self.tree(v)
        out = []
        for s in slots:
            k = self.at("key", keys[0] + s * keys[1].GetByteSize(), keys[1])
            out.append(self.at("[%s]" % plain(k), vals[0] + s * vals[1].GetByteSize(), vals[1]))
        return out

    def of_sortedset(self, v):
        slots, keys, _ = self.tree(v.GetChildMemberWithName("d"))
        size = keys[1].GetByteSize() if keys else 0
        return [self.at("[%d]" % i, keys[0] + s * size, keys[1]) for i, s in enumerate(slots)]

    def num_children(self):
        return len(self.kids)

    def get_child_index(self, name):
        for i, c in enumerate(self.kids):
            if c.GetName() == name:
                return i
        return -1

    def get_child_at_index(self, i):
        return self.kids[i] if 0 <= i < len(self.kids) else None

    def has_children(self):
        return bool(self.kids)


def size_summary(v, _dict):
    """`size=N`; an array's `shape=[..]`."""
    v = v.GetNonSyntheticValue()
    k = layout.kind(v.GetType().GetCanonicalType().GetName())
    try:
        if k in ("list", "dict", "set"):
            h = head(v if k != "set" else v.GetChildMemberWithName("d"))
            if h is None:
                return "size=0"
            if not layout.sane(field(h, "len"), field(h, "cap")):
                return "<invalid>"
            return "size=%d" % field(h, "len")
        if k in ("sorted", "sortedset"):
            d = v if k == "sorted" else v.GetChildMemberWithName("d")
            meta = list_items(d.GetChildMemberWithName("meta"))
            if meta is None:
                return "size=0"
            ws = meta[1].GetByteSize()
            return "size=%d" % read_uint(v, meta[0] + ws, ws)
        if k == "slice":
            return "size=%d" % field(v, "len")
        if k == "array":
            shape = v.GetChildMemberWithName("shape")
            dims = [str(shape.GetChildAtIndex(i).GetValueAsUnsigned()) for i in range(shape.GetNumChildren())]
            return "shape=[%s]" % ", ".join(dims)
    except Exception:
        pass
    return None


def str_summary(v, _dict):
    v = v.GetNonSyntheticValue()
    ptr = v.GetChildMemberWithName("ptr").GetValueAsUnsigned()
    n = v.GetChildMemberWithName("len").GetValueAsUnsigned()
    if n == 0:
        return '""'
    err = lldb.SBError()
    data = v.GetProcess().ReadMemory(ptr, min(n, layout.MAX_STR), err)
    if not err.Success():
        return "<unreadable str at 0x%x>" % ptr
    return layout.text(data, n)


def __lldb_init_module(debugger, _dict):
    m = __name__
    debugger.HandleCommand(
        "type synthetic add -w nabi -l %s.Sum --recognizer-function %s.is_sum" % (m, m)
    )
    debugger.HandleCommand(
        "type summary add -w nabi -e -F %s.summary --recognizer-function %s.is_sum" % (m, m)
    )
    for k, r in layout.KINDS:
        if k == "str":
            continue
        debugger.HandleCommand('type synthetic add -w nabi -l %s.Seq -x "%s"' % (m, r.pattern))
        debugger.HandleCommand('type summary add -w nabi -e -F %s.size_summary -x "%s"' % (m, r.pattern))
    debugger.HandleCommand("type summary add -w nabi -F %s.str_summary str" % m)
    debugger.HandleCommand("type category enable nabi")
    # Programs name the standard library's files /nabi/library/...; this
    # file is <library>/lldb/nabi.py.
    library = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    debugger.HandleCommand('settings append target.source-map /nabi/library "%s"' % library)
