#!/usr/bin/env python3
# Regenerate nabic's built-in intrinsic headers (include/*intrin*.h,
# arm_*.h, ..) from clang's: each one reduced to declarations. Inline
# intrinsics become prototypes; bodies, attribute macros and function-like
# intrinsic macros go; types, constants, arch conditionals and #errors
# stay. Only the little-endian NEON branch and the C (not __cplusplus)
# branches are kept, and fp8 (__mfp8) declarations are dropped. AVX-512,
# AVX10 and AMX headers contribute their types only, inlined where they
# are included.
#
#   python3 -I cc/nabicc_pp/intrin.py <clang include dir> [--check]
#
# <clang include dir> is clang's resource include directory, e.g.
# "$(clang -print-resource-dir)/include". The tree is generated from
# LLVM 21.1.8 (Homebrew llvm@21, Ubuntu clang 21.1.8 and Apple clang 21
# ship the same files). After an LLVM update, rerun against the new
# version, review the diff, run the nabicc tests and update the
# version above and include/LICENSE-LLVM.txt (clang/LICENSE.TXT of
# llvmorg-<version>).
# --check writes nothing and fails if a header would change.
#
# The output keeps each source's license comment; the files are LLVM's,
# Apache-2.0 WITH LLVM-exception (arm_neon.h and arm_fp16.h: the MIT
# notice their generator writes).

import os
import re
import sys

X86 = ['mmintrin.h', 'xmmintrin.h', 'emmintrin.h', 'pmmintrin.h', 'tmmintrin.h',
       'smmintrin.h', 'nmmintrin.h', 'wmmintrin.h', '__wmmintrin_aes.h',
       '__wmmintrin_pclmul.h', 'popcntintrin.h', 'crc32intrin.h', 'avxintrin.h',
       'avx2intrin.h', 'fmaintrin.h', 'f16cintrin.h', 'bmiintrin.h', 'bmi2intrin.h',
       'lzcntintrin.h', 'ia32intrin.h', 'adcintrin.h', 'adxintrin.h', 'immintrin.h',
       'x86intrin.h', 'x86gprintrin.h']
MS = ['intrin.h', 'intrin0.h', 'armintr.h', 'arm64intr.h']
ARM = ['arm_neon.h', 'arm_vector_types.h', 'arm_bf16.h', 'arm_fp16.h', 'arm_acle.h']
ALL = X86 + MS + ARM
DESC = {**{n: 'the x86 intrinsics' for n in X86},
        **{n: "Microsoft's intrinsics" for n in MS},
        **{n: 'the Arm intrinsics' for n in ARM}}
# Kept as #include; nabic or the C library has them.
KEEP = ('mm_malloc.h', 'stdint.h', 'setjmp.h', 'stdlib.h', 'malloc.h')
# Types only.
TYPES = re.compile(r'(avx512|avx10|amx)\w*intrin\.h')

CXX_IF = re.compile(r'(defined\s*\(?\s*__cplusplus\s*\)?|__cplusplus)(\s*&&.*)?')
C_IF = re.compile(r'!\s*defined\s*\(?\s*__cplusplus\s*\)?')
ATTR_VAL = re.compile(r'__attribute__|\bstatic\b|__inline|\bconstexpr\b|__builtin')
INLINE = re.compile(r'\bstatic\b|\b__ai\b|__inline')
SPECIFIERS = re.compile(r'\b(static|__inline__|__inline|inline|constexpr|__ai|extern)\b')


def strip_comments(s):
    out = []
    i, n = 0, len(s)
    while i < n:
        if s.startswith('/*', i):
            i = s.find('*/', i + 2) + 2
            out.append(' ')
        elif s.startswith('//', i):
            j = s.find('\n', i)
            i = j if j >= 0 else n
        elif s[i] in '"\'':
            q, j = s[i], i + 1
            while s[j] != q:
                j += 2 if s[j] == '\\' else 1
            out.append(s[i:j + 1])
            i = j + 1
        else:
            out.append(s[i])
            i += 1
    return ''.join(out)


def strip_attrs(t):
    while True:
        m = re.search(r'__attribute__\s*\(\(', t)
        if not m:
            return t
        d, j = 0, m.end() - 2
        while True:
            if t[j] == '(':
                d += 1
            elif t[j] == ')':
                d -= 1
                if d == 0:
                    break
            j += 1
        t = t[:m.start()] + ' ' + t[j + 1:]


class Macros:
    """The attribute macros seen so far, as one word regex."""

    def __init__(self):
        self.names = set()
        self.re = None

    def add(self, name):
        self.names.add(name)
        self.re = None

    def _re(self):
        if self.re is None:
            alt = '|'.join(re.escape(m) for m in sorted(self.names, key=len, reverse=True))
            self.re = re.compile(r'\b(?:' + alt + r')\b')
        return self.re

    def used(self, s):
        return bool(self.names) and self._re().search(s) is not None

    def drop(self, s):
        return self._re().sub(' ', s) if self.names else s


def convert(src, drop_fn, drop_line, include):
    """The lines of header text `src` as declarations."""
    lines = strip_comments(src).replace('\\\n', ' ').split('\n')
    macros = Macros()
    out = []
    # 'le': #ifdef __LITTLE_ENDIAN__, 'cxx' / 'c': a __cplusplus
    # conditional, 'keep': any other.
    conds = []
    skip = False
    buf = ''
    body = 0
    seen = set()
    for line in lines:
        s = line.strip()
        if body:
            body += s.count('{') - s.count('}')
            continue
        if s.startswith('#'):
            d = re.match(r'#\s*(\w+)\s*(.*)', s)
            kw, rest = d.group(1), d.group(2)
            if kw in ('if', 'ifdef', 'ifndef'):
                if kw == 'ifdef' and rest == '__LITTLE_ENDIAN__':
                    conds.append('le')
                    continue
                if (CXX_IF.fullmatch(rest) and kw != 'ifndef') or \
                        (kw == 'ifdef' and rest == '__cplusplus'):
                    conds.append('cxx')
                    skip = True
                    continue
                if (kw == 'ifndef' and rest == '__cplusplus') or C_IF.fullmatch(rest):
                    conds.append('c')
                    continue
                conds.append('keep')
            elif kw == 'else' and conds and conds[-1] == 'le':
                skip = True
                continue
            elif kw == 'else' and conds and conds[-1] in ('cxx', 'c'):
                skip = conds[-1] == 'c'
                continue
            elif kw == 'endif':
                if conds.pop() in ('le', 'cxx', 'c'):
                    skip = False
                    continue
            if skip:
                continue
            if kw == 'define':
                name = re.match(r'(\w+)', rest).group(1)
                val = rest[len(name):]
                if not val.strip():
                    out.append(s)
                    continue
                fnlike = val.startswith('(')
                attr = ATTR_VAL.search(val) or macros.used(val)
                if (attr and name.startswith('__')) or \
                        (fnlike and not name[:3].isupper() and not name.startswith('_MM')):
                    macros.add(name)
                    continue
            if kw == 'undef' and rest in macros.names:
                continue
            if kw == 'pragma':
                continue
            if kw == 'include':
                m = re.match(r'[<"]([^>"]+)[>"]', rest)
                if m:
                    out.extend(include(m.group(1), s))
                    continue
            out.append(s)
            continue
        if skip or not s:
            continue
        if s in ('extern "C" {', '}') and not buf.strip():
            continue
        buf += ' ' + s
        while True:
            b = buf.strip()
            if not b:
                buf = ''
                break
            # The first '{' or ';' outside parentheses.
            d, k = 0, None
            for j, ch in enumerate(b):
                if ch == '(':
                    d += 1
                elif ch == ')':
                    d -= 1
                elif d == 0 and ch in '{;':
                    k = j
                    break
            if k is None:
                break
            head = b[:k]
            if b[k] == '{' and head.rstrip().endswith(')') and \
                    (INLINE.search(head) or macros.used(head)):
                # An inline definition: its prototype.
                p = macros.drop(strip_attrs(head))
                p = ' '.join(SPECIFIERS.sub(' ', p).split())
                name = re.search(r'(\w+)\s*\(', p).group(1)
                rem = b[k + 1:]
                dd, jj = 1, 0
                while jj < len(rem) and dd:
                    if rem[jj] == '{':
                        dd += 1
                    elif rem[jj] == '}':
                        dd -= 1
                    jj += 1
                if dd:
                    body, buf = dd, ''
                else:
                    buf = rem[jj:]
                if name in seen or drop_fn(name, p):
                    continue
                seen.add(name)
                out.append(p + ';')
                if body:
                    break
                continue
            if b[k] == '{':
                # A struct or enum: through its closing brace and ';'.
                dd, end = 0, None
                for j in range(k, len(b)):
                    if b[j] == '{':
                        dd += 1
                    elif b[j] == '}':
                        dd -= 1
                        if dd == 0:
                            e = b.find(';', j)
                            if e >= 0:
                                end = e
                            break
                if end is None:
                    break
                st = ' '.join(b[:end + 1].split())
                if not drop_line(st):
                    out.append(st)
                buf = b[end + 1:]
                continue
            st = ' '.join(b[:k + 1].split())
            if not drop_line(st):
                out.append(st)
            buf = b[k + 1:]
    # Drop the conditionals left empty.
    changed = True
    while changed:
        changed = False
        for i in range(len(out) - 1):
            a, b = out[i], out[i + 1]
            if re.match(r'#\s*if', a) and re.match(r'#\s*endif', b):
                del out[i:i + 2]
            elif re.match(r'#\s*else', a) and re.match(r'#\s*endif', b):
                del out[i]
            elif re.match(r'#\s*elif', a) and re.match(r'#\s*(endif|elif|else)', b):
                del out[i]
            else:
                continue
            changed = True
            break
    return out


def read(path):
    with open(path, encoding='ascii') as f:
        return f.read()


def generate(src):
    """{name: text} of every header, from clang include dir `src`."""
    types_seen = set()

    def types_of(name):
        out = []
        path = os.path.join(src, name)
        if not os.path.exists(path):
            return out
        for line in convert(read(path), lambda n, p: True, lambda l: False, include_types):
            if line.startswith('typedef') and line not in types_seen:
                types_seen.add(line)
                out.append(line)
            elif line.startswith('#include'):
                out.append(line)
        return out

    def include_types(name, line):
        return types_of(name) if TYPES.match(name) else []

    def include(name, line):
        if name in ALL or name in KEEP:
            return [line]
        if TYPES.match(name):
            return types_of(name)
        return []

    def fp8(s):
        return 'mfloat8' in s or '__mfp8' in s

    res = {}
    for n in ALL:
        text = read(os.path.join(src, n))
        lines = convert(text, lambda f, p: fp8(p) or f.startswith('__noswap_'), fp8, include)
        notice = text[:text.index('*/') + 2]
        head = (f"{notice}\n"
                f"/* {n}: nabic's. Clang's, reduced to declarations by intrin.py:\n"
                f" * {DESC[n]} as prototypes, their types and constants; bindgen binds\n"
                " * none of them. */\n")
        res[n] = head + '\n'.join(lines) + '\n'
    return res


def main(argv):
    args = [a for a in argv if a != '--check']
    if len(args) != 1:
        sys.exit('usage: intrin.py <clang include dir> [--check]')
    dst = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'include')
    stale = []
    for n, text in generate(args[0]).items():
        path = os.path.join(dst, n)
        old = read(path) if os.path.exists(path) else None
        if old == text:
            continue
        stale.append(n)
        if '--check' not in argv:
            with open(path, 'w', encoding='ascii', newline='\n') as f:
                f.write(text)
    for n in stale:
        print(('stale: ' if '--check' in argv else 'wrote: ') + n)
    if stale and '--check' in argv:
        sys.exit(1)


if __name__ == '__main__':
    main(sys.argv[1:])
