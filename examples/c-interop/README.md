# c-interop

Nabi calling C and C calling back into Nabi, built by the toolchain
alone: `nabi build` compiles `c/ring.c` with the built-in C compiler
(`nabicc`) and links it into the program. No system C compiler is used.

- `c/ring.h`, `c/ring.c`: a fixed-capacity ring of `u32` with error
  codes, a `for_each` that calls a function pointer, and an FNV-1a
  checksum.
- `nabi.nabion`: the `c` field lists the C sources and include
  directories.
- `src/ring.nabi`: the `extern "C"` block and a safe `Ring` that owns
  the C handle. `Drop` frees it, C error codes become a `RingError`
  thrown with `throws`, and `to_list` passes an `extern "C" fn` to C,
  with the Nabi list travelling through `void *ctx` in a `Handle`.
  Every `unsafe` block is here.
- `src/main.nabi`: uses `Ring` with no `unsafe`, and checks C's
  checksum against a pure-Nabi FNV-1a.

## Run

```sh
nabi run
```

Output:

```
push 50: ring is full
pop: 10
push 60: ring is full
len: 4, values: [20, 30, 40, 50]
fnv1a: c 0x684d2b55, nabi 0x684d2b55, match: true
new(0): capacity must be between 1 and SIZE_MAX / 4
```

The C is portable C11 (`stdlib.h`, `stdint.h`, `stddef.h` only), so
the same package builds on Linux, macOS and Windows.
