#!/usr/bin/env python3
"""Link-only stub of a glibc shared library (the Zig model).

    stubs.py <lib.so.N> <out.so>

Reads the exported symbols and their versions from a 32- or 64-bit
ELF shared library of either byte order and writes a shared object of
the same machine that defines each one under its version (objects keep
their size, aliases of one object share its storage), with no code and
no relocations: an input a linker links against as it would the real
library. Its soname is the input's file name. GLIBC_PRIVATE symbols are
left out, and so are versions above glibc 2.34 (what executables may
need): a symbol whose default is newer binds to its previous version
(x86_64's hypot@@GLIBC_2.35 to GLIBC_2.2.5), one that is new there is
not defined.
"""

import os
import struct
import sys

SHT_PROGBITS, SHT_STRTAB, SHT_HASH, SHT_DYNAMIC, SHT_NOBITS, SHT_DYNSYM = 1, 3, 5, 6, 8, 11
SHT_GNU_VERDEF = 0x6FFFFFFD
SHT_GNU_VERSYM = 0x6FFFFFFF
SHF_WRITE, SHF_ALLOC, SHF_EXECINSTR, SHF_TLS = 1, 2, 4, 0x400
PT_LOAD, PT_DYNAMIC, PT_PHDR, PT_TLS = 1, 2, 6, 7
PF_X, PF_W, PF_R = 1, 2, 4
STT_OBJECT, STT_FUNC, STT_TLS = 1, 2, 6
STB_GLOBAL, STB_WEAK = 1, 2
VER_FLG_BASE = 1
DT_NULL, DT_HASH, DT_STRTAB, DT_SYMTAB, DT_STRSZ, DT_SYMENT, DT_SONAME = 0, 4, 5, 6, 10, 11, 14
DT_VERSYM, DT_VERDEF, DT_VERDEFNUM = 0x6FFFFFF0, 0x6FFFFFFC, 0x6FFFFFFD
# The largest page of any target (ppc64le, 64K aarch64 kernels).
PAGE = 0x10000


def cstr(b, off):
    return b[off : b.index(b"\0", off)].decode()


def read(path):
    b = open(path, "rb").read()
    if b[:4] != b"\x7fELF" or b[4] not in (1, 2) or b[5] not in (1, 2):
        sys.exit(f"{path}: not an ELF file")
    elf64 = b[4] == 2
    e = "<" if b[5] == 1 else ">"
    machine, = struct.unpack_from(e + "H", b, 18)
    if elf64:
        flags, = struct.unpack_from(e + "I", b, 0x30)
        shoff, = struct.unpack_from(e + "Q", b, 0x28)
        shentsize, shnum = struct.unpack_from(e + "HH", b, 0x3A)
        shdr, symsize = e + "IIQQQQIIQQ", 24
    else:
        flags, = struct.unpack_from(e + "I", b, 0x24)
        shoff, = struct.unpack_from(e + "I", b, 0x20)
        shentsize, shnum = struct.unpack_from(e + "HH", b, 0x2E)
        shdr, symsize = e + "IIIIIIIIII", 16
    secs = [struct.unpack_from(shdr, b, shoff + i * shentsize) for i in range(shnum)]
    by_type = {s[1]: s for s in secs}
    dynsym, verdef, versym = (by_type[t] for t in (SHT_DYNSYM, SHT_GNU_VERDEF, SHT_GNU_VERSYM))
    strtab = secs[dynsym[6]][4]

    names = {}
    off = verdef[4]
    while True:
        _, vflags, ndx, _, _, aux, nxt = struct.unpack_from(e + "HHHHIII", b, off)
        names[ndx] = (cstr(b, strtab + struct.unpack_from(e + "I", b, off + aux)[0]), vflags & 1)
        if not nxt:
            break
        off += nxt

    syms = []
    for i in range(1, dynsym[5] // symsize):
        at = dynsym[4] + i * symsize
        if elf64:
            name, info, _, shndx, value, size = struct.unpack_from(e + "IBBHQQ", b, at)
        else:
            name, value, size, info, _, shndx = struct.unpack_from(e + "IIIBBH", b, at)
        ver, = struct.unpack_from(e + "H", b, versym[4] + i * 2)
        bind, typ = info >> 4, info & 0xF
        vname, base = names.get(ver & 0x7FFF, (None, 1))
        if shndx == 0 or bind not in (STB_GLOBAL, STB_WEAK) or base or vname == "GLIBC_PRIVATE":
            continue
        syms.append((cstr(b, strtab + name), vname, not ver & 0x8000, bind, typ, value, size))
    versions = [n for k, (n, base) in sorted(names.items()) if not base and n != "GLIBC_PRIVATE"]
    return (elf64, e, machine, flags), syms, versions


def number(ver):
    """GLIBC_2.2.5 as (2, 2, 5); () for one without a number."""
    tail = ver.removeprefix("GLIBC_")
    return tuple(int(x) for x in tail.split(".")) if tail[:1].isdigit() else ()


def cap(syms, versions, newest=(2, 34)):
    syms = [s for s in syms if number(s[1]) <= newest]
    versions = [v for v in versions if number(v) <= newest]
    defaults = {s[0] for s in syms if s[2]}
    best = {}
    for i, s in enumerate(syms):
        j = best.get(s[0])
        if s[0] not in defaults and (j is None or number(s[1]) > number(syms[j][1])):
            best[s[0]] = i
    for i in best.values():
        syms[i] = syms[i][:2] + (True,) + syms[i][3:]
    return syms, versions


def elf_hash(name):
    h = 0
    for c in name.encode():
        h = ((h << 4) + c) & 0xFFFFFFFF
        g = h & 0xF0000000
        h ^= g >> 24
        h &= ~g
    return h


def align(n, a):
    return (n + a - 1) & -a


class Strtab:
    def __init__(self):
        self.data = bytearray(b"\0")
        self.at = {"": 0}

    def add(self, s):
        if s not in self.at:
            self.at[s] = len(self.data)
            self.data += s.encode() + b"\0"
        return self.at[s]


# Section order: the header indices.
ORDER = [".dynsym", ".gnu.version", ".gnu.version_d", ".hash", ".dynstr",
         ".text", ".dynamic", ".data", ".tbss", ".shstrtab"]
NDX = {n: i + 1 for i, n in enumerate(ORDER)}


def place_symbols(syms):
    """Storage per symbol: functions one byte each in .text, objects in
    .data, thread-locals in .tbss; an alias (same kind and address in
    the library) shares the first one's."""
    size_of = {"text": 0, "data": 0, "tbss": 0}
    align_of = {"data": 1, "tbss": 1}
    storage = {}
    placed = []
    for name, ver, default, bind, typ, value, size in syms:
        if typ in (STT_OBJECT, STT_TLS):
            sec = "tbss" if typ == STT_TLS else "data"
            at = storage.get((typ, value))
            if at is None:
                a = min(value & -value or 1, 64)
                at = align(size_of[sec], a)
                size_of[sec] = at + max(size, 1)
                align_of[sec] = max(align_of[sec], a)
                storage[(typ, value)] = at
        else:
            # IFUNCs too: a stub resolves nothing.
            sec, at, typ, size = "text", size_of["text"], STT_FUNC, 0
            size_of["text"] += 1
        placed.append((name, ver, default, bind, typ, sec, at, size))
    return placed, size_of, align_of


def write(path, soname, head, syms, versions):
    elf64, e, machine, flags = head
    word = "Q" if elf64 else "I"
    ehsize, phentsize, shentsize = (64, 56, 64) if elf64 else (52, 32, 40)
    symsize, dynsize = (24, 16) if elf64 else (16, 8)
    wsize = 8 if elf64 else 4

    placed, size_of, align_of = place_symbols(syms)
    text = max(size_of["text"], 1)
    data, tbss = size_of["data"], size_of["tbss"]

    dynstr = Strtab()
    for s in [soname] + [p[0] for p in placed] + versions:
        dynstr.add(s)
    vindex = {v: i + 2 for i, v in enumerate(versions)}
    shstr = Strtab()
    for n in ORDER:
        shstr.add(n)

    nsyms = len(placed) + 1
    nverdef = len(versions) + 1
    nbucket = max(1, nsyms // 2) | 1
    ndyn = 10
    nphdr = 6 if tbss else 5

    # File offsets and addresses. Read-only: headers and the dynamic
    # linking tables at address 0. The executable and the writable
    # parts each start a new page in memory, but not in the file
    # (offset and address agree modulo PAGE).
    lay = {}
    off = ehsize + nphdr * phentsize
    for name, size, a in [(".dynsym", nsyms * symsize, wsize), (".gnu.version", nsyms * 2, 2),
                          (".gnu.version_d", nverdef * 28, 4), (".hash", (2 + nbucket + nsyms) * 4, 4),
                          (".dynstr", len(dynstr.data), 1)]:
        off = align(off, a)
        lay[name] = (off, off, size)
        off += size
    ro_end = off
    off = align(off, 16)
    text_va = align(ro_end, PAGE) + off % PAGE
    lay[".text"] = (off, text_va, text)
    off += text
    off = align(off, wsize)
    rw_off = off
    rw_va = align(text_va + text, PAGE) + rw_off % PAGE
    lay[".dynamic"] = (off, rw_va, ndyn * dynsize)
    off += ndyn * dynsize
    off = align(off, align_of["data"])
    lay[".data"] = (off, rw_va + off - rw_off, data)
    off += data
    rw_size = off - rw_off
    tbss_off = align(off, align_of["tbss"])
    lay[".tbss"] = (tbss_off, rw_va + tbss_off - rw_off, tbss)

    def va(name):
        return lay[name][1]

    out = bytearray(off)

    def put(fmt, at, *v):
        struct.pack_into(e + fmt, out, at, *v)

    # .dynsym and .gnu.version (entry 0 stays zero in both).
    o = lay[".dynsym"][0] + symsize
    vo = lay[".gnu.version"][0] + 2
    for name, ver, default, bind, typ, sec, at, size in placed:
        # A thread-local's value is its offset in the TLS segment.
        value = at if sec == "tbss" else va("." + sec) + at
        info = bind << 4 | typ
        if elf64:
            put("IBBHQQ", o, dynstr.add(name), info, 0, NDX["." + sec], value, size)
        else:
            put("IIIBBH", o, dynstr.add(name), value, size, info, 0, NDX["." + sec])
        put("H", vo, vindex[ver] | (0 if default else 0x8000))
        o += symsize
        vo += 2

    # .gnu.version_d: the base (the soname), then each version.
    o = lay[".gnu.version_d"][0]
    for i, v in enumerate([soname] + versions):
        nxt = 0 if i == len(versions) else 28
        put("HHHHIII", o, 1, VER_FLG_BASE if i == 0 else 0, i + 1, 1, elf_hash(v), 20, nxt)
        put("II", o + 20, dynstr.add(v), 0)
        o += 28

    # .hash.
    buckets = [0] * nbucket
    chains = [0] * nsyms
    for i, p in enumerate(placed, 1):
        b = elf_hash(p[0]) % nbucket
        chains[i] = buckets[b]
        buckets[b] = i
    put(f"{2 + nbucket + nsyms}I", lay[".hash"][0], nbucket, nsyms, *buckets, *chains)

    o = lay[".dynstr"][0]
    out[o : o + len(dynstr.data)] = dynstr.data

    o = lay[".dynamic"][0]
    for tag, val in [(DT_SONAME, dynstr.add(soname)), (DT_SYMTAB, va(".dynsym")), (DT_SYMENT, symsize),
                     (DT_STRTAB, va(".dynstr")), (DT_STRSZ, len(dynstr.data)),
                     (DT_HASH, va(".hash")), (DT_VERSYM, va(".gnu.version")),
                     (DT_VERDEF, va(".gnu.version_d")), (DT_VERDEFNUM, nverdef), (DT_NULL, 0)]:
        put(word * 2, o, tag, val)
        o += dynsize

    shstr_off = len(out)
    out += shstr.data
    shoff = align(len(out), wsize)
    out += bytes(shoff - len(out))

    # Section headers: type, flags, link, info, alignment, entry size.
    info = {
        ".dynsym": (SHT_DYNSYM, SHF_ALLOC, NDX[".dynstr"], 1, wsize, symsize),
        ".gnu.version": (SHT_GNU_VERSYM, SHF_ALLOC, NDX[".dynsym"], 0, 2, 2),
        ".gnu.version_d": (SHT_GNU_VERDEF, SHF_ALLOC, NDX[".dynstr"], nverdef, 4, 0),
        ".hash": (SHT_HASH, SHF_ALLOC, NDX[".dynsym"], 0, 4, 4),
        ".dynstr": (SHT_STRTAB, SHF_ALLOC, 0, 0, 1, 0),
        ".text": (SHT_PROGBITS, SHF_ALLOC | SHF_EXECINSTR, 0, 0, 16, 0),
        ".dynamic": (SHT_DYNAMIC, SHF_ALLOC | SHF_WRITE, NDX[".dynstr"], 0, wsize, dynsize),
        ".data": (SHT_PROGBITS, SHF_ALLOC | SHF_WRITE, 0, 0, align_of["data"], 0),
        ".tbss": (SHT_NOBITS, SHF_ALLOC | SHF_WRITE | SHF_TLS, 0, 0, align_of["tbss"], 0),
    }
    shdr = e + ("IIQQQQIIQQ" if elf64 else "IIIIIIIIII")
    out += struct.pack(shdr, *(0,) * 10)
    for n in ORDER:
        if n == ".shstrtab":
            out += struct.pack(shdr, shstr.add(n), SHT_STRTAB, 0, 0, shstr_off, len(shstr.data), 0, 0, 1, 0)
            continue
        ty, fl, link, inf, al, es = info[n]
        o, addr, size = lay[n]
        out += struct.pack(shdr, shstr.add(n), ty, fl, addr, o, size, link, inf, al, es)

    # Program headers: type, flags, offset, address, file size, memory
    # size, alignment.
    phdrs = [
        (PT_PHDR, PF_R, ehsize, ehsize, nphdr * phentsize, nphdr * phentsize, wsize),
        (PT_LOAD, PF_R, 0, 0, ro_end, ro_end, PAGE),
        (PT_LOAD, PF_R | PF_X, lay[".text"][0], text_va, text, text, PAGE),
        (PT_LOAD, PF_R | PF_W, rw_off, rw_va, rw_size, rw_size, PAGE),
        (PT_DYNAMIC, PF_R | PF_W, rw_off, rw_va, ndyn * dynsize, ndyn * dynsize, wsize),
    ]
    if tbss:
        phdrs.append((PT_TLS, PF_R, tbss_off, va(".tbss"), 0, tbss, align_of["tbss"]))
    o = ehsize
    for ty, fl, po, pva, filesz, memsz, al in phdrs:
        if elf64:
            put("IIQQQQQQ", o, ty, fl, po, pva, pva, filesz, memsz, al)
        else:
            put("IIIIIIII", o, ty, po, pva, pva, filesz, memsz, fl, al)
        o += phentsize

    # ELF header: ET_DYN for the input's machine and flags.
    out[:16] = b"\x7fELF" + bytes([2 if elf64 else 1, 1 if e == "<" else 2, 1]) + bytes(9)
    put("HHI" + word * 3 + "IHHHHHH", 16, 3, machine, 1, 0, ehsize, shoff, flags,
        ehsize, phentsize, nphdr, shentsize, len(ORDER) + 1, NDX[".shstrtab"])
    open(path, "wb").write(out)


def main():
    lib, out = sys.argv[1:]
    head, syms, versions = read(lib)
    syms, versions = cap(syms, versions)
    write(out, os.path.basename(lib), head, syms, versions)


main()
