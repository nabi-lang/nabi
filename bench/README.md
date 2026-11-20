# Benchmarks

`bench/*.nabi` are Nabi benchmarks; `bench/c/*.c` are C counterparts of
some of them (same algorithm, same output). Not run by CI.

| benchmark        | what it stresses                                         | C |
|------------------|----------------------------------------------------------|---|
| `nbody`          | f64 arithmetic over parallel `List<f64>`                 | yes |
| `fannkuch`       | small integer lists, permutations, prefix reversals       | yes |
| `spectralnorm`   | calls + f64 loops                                         | yes |
| `binarytrees`    | short-lived trait objects, GC throughput                  | yes |
| `jsontokenizer`  | building a 6 MB `str` with `StrBuf`, byte-wise scanning   | yes |
| `wordcount`      | `split_whitespace` + `Dict<str, i64>` counting            | yes |
| `normalize`      | NFC / NFD of mostly-ASCII and of mixed text: the quick check, decomposition, composition | |
| `throwparse`     | recursive throwing calls (`try`), errors caught per expr  | yes |
| `throwwide`      | throwing calls with a `str` result and a two-word error   |   |
| `channelcontended` | 4 producer threads into one receiver, unbounded and bounded channels | |
| `filerw`         | 16-byte `File` writes, reads and seeks (syscall-bound)    |   |
| `matchdispatch`  | bytecode interpreter: a 12-arm `match` on an enum per step |   |
| `datetime`       | calendar text both ways: RFC 3339 `Display` / `parse`, strftime `format` / `parse_with`, a POSIX rule zone |   |
| `floatmath`      | `floor` / `ceil` / `round` / `trunc`, `abs`, `min` / `max`, `copysign`, `mul_add` |   |
| `vecsum`         | sums of a `List<i32>` (checked) and a `List<u8>` (wrapping): vector reductions | yes |
| `vecdot`         | dot products: `f64` (its sum stays in order: scalar) and wrapping `i32` | yes |
| `vecsaxpy`       | `a * x + y` over `List<f64>`, an `i16` map: vector maps    | yes |
| `vecbytes`       | a byte count and a newline search over 8 MB: vector tests and early exits | yes |
| `vecscan`        | early-exit byte searches from an offset to a bound passed in, and two lists or slices tested with `&&` and `&` | |
| `vecsumfor`, `vecbytesfor` | `vecsum` / `vecbytes` written with `for x in xs` and `enumerate`: iterator loops made counted | |
| `vecminmax`      | min / max reductions, a ReLU and a clip storing on both ways of a test: vector reductions and merged stores | |
| `vecwiden`       | sums of bytes and halves into wider accumulators, a byte map into `u16`, words narrowed to bytes: widened loads, narrowed stores | |
| `vecfminmax`     | float min / max reductions (`min` / `max` and a test), `f64` and `f32`: vector reductions that leave NaNs and zeros to the scalar loop | |
| `veccheckedmul`  | checked products, `i16` and `u32` by a constant, of two `i16` lists: vector multiplies testing each product's high half | |
| `vecdot8`        | dot products of `i8` (checked `i32` sum) and `u8` (wrapping `u32`): byte lanes, four products to a lane with arm64 `dotprod` | |
| `arrayindex`     | `Array` elements by index: `a[i]` of rank 1 and `a[i, j]` of rank 2, `usize` counters | |
| `arrayiter`      | `Array` walks: `for x in a`, `sum`, `to_list`, `map`, `copy`, `==`, a transposed view | |
| `arrayreduce`    | `Array` reductions: `sum`, `mean`, `var`, `min` / `max` / `argmax`, whole and along each axis, `cumsum` | |
| `sorteddict`     | `SortedDict` (B-tree): scattered inserts, lookups, range walks, ordered iteration, removals | |
| `simd`           | `core::simd`: an `f32x4` dot product, `a * x + y` and a ReLU through a mask, beside the scalar dot product | |
| `simdlanes`      | `core::simd` vectors of struct fields: `from_array`, a product and `reduce_sum`, `f32` and `f64`, and an accumulator vector through a loop | |
| `slptile`        | a matrix product in 4 x 4 tiles of C, `f64` and `f32`, each row's four elements updated lane by lane: straight-line vectors | |
| `sliceindex`     | indexed loops over a list's `Slice` and over a list at an offset (`x[k + m]`) | |
| `digitspush`     | decimal digits of `u64`s by `% 10` / `/ 10` into a `StrBuf` with `push_byte` | yes |
| `ryufmt`         | the Ryu shortest-digits core of an `f64`: high products, `/ 10`, `/ 100`, short loops | yes |
| `byteparse`      | a number parser in one large function indexing a `Slice<u8>` outside loops | yes |
| `wordload`       | words read a byte at a time (one load), shifts by masked amounts | yes |
| `bigstruct`      | calls on a 61-word struct: by value, `ref self` writing one field or none, an iterator's `next(ref self)` over a 54-word struct | |
| `strtable`       | `[str; 100]` tables indexed at run time, `const` and `static`, beside a `[u64; 100]` | |
| `structf32`      | `f32` / `f64` three-vector structs read from a list, inline and through a by-value call | |
| `elemfield`      | one-field stores into list elements of a ten-word struct, a `str` field among them | |
| `threads`        | one amount of work over 4 threads (`threads <n> cpu\|alloc` for others): `--scaling` | |
| `alloc*`, `gc*`, `storeheavy`, `wbthreads`, `timers`, `fstringheavy` | allocation, collector, write barriers, timers, f-strings | |

## Running

```sh
python3 bench/bench.py                 # all, best of 3, record + compare
python3 bench/bench.py nbody --runs 5  # only names containing "nbody"
python3 bench/bench.py --no-record     # measure and compare, keep no record
python3 bench/bench.py --threshold 5   # regression threshold in % (default 3)
python3 bench/bench.py --compile --scaling --only   # the gates only (CI nightly)
python3 bench/bench.py --pauses --only     # the GC pause gate
python3 bench/bench.py --target-dir /tmp/t  # build elsewhere than $CARGO_TARGET_DIR / target/
```

The runner builds the release `nabic` and runtime, compiles each
`bench/*.nabi` at `-O2` and each `bench/c/*.c` with `clang -O2` into
`<target>/bench/`, and runs every program N times under `/usr/bin/time -l`
(macOS; on Linux GNU `time`, under `perf stat` where it may count
instructions, else user time and RSS only). Per benchmark it
keeps the best (lowest) of the N runs of:

- **instructions retired**: the primary metric; nearly noise-free, so
  small regressions show;
- **user CPU seconds**;
- **peak RSS** (KiB).

Each run appends one JSON line to `bench/history.jsonl`: the git commit
(and whether the tree was dirty), commit subject, date, host, run count
and the results by benchmark (`c/<name>` for the C programs). The table
printed at the end compares against the previous line of the history:
the change in instructions (user time when no instruction count is
available) above the threshold is flagged `REGRESSION`; `vs C` is the
Nabi / C instruction ratio. A Nabi program whose output differs from its C
counterpart is flagged `OUTPUT MISMATCH`. Either makes the exit status 1.

Compare only records from the same host: instruction counts depend on
the CPU and the C compiler. Outside a git checkout the commit is
`NABI_COMMIT_HASH`, else the one `nabic --version` names, and `dirty` is
`null`.

## Gates

`--compile` builds a hello at `-O0` and `-O2` with a fresh copy of
`nabic`, so its cache is empty: `compile/hello-cold-O<n>` is that first
build (the standard library code it reaches compiled too), and
`compile/hello-O<n>` the build after an edit, which takes that code from
the function cache.
`--scaling` runs `threads.nabi`, one amount of work split over 1, 2, 4
and 8 threads, arithmetic only (`threads/cpu-<n>`) and boxing
(`threads/alloc-<n>`), and prints each kind's speedup (wall time on one
thread over wall time on n).

`--pauses` runs the GC pause probes of `bench/pause` (built at `-O2`,
`NABIR_GC_LOG=1`, the other `NABIR_GC_*` cleared), each at least 5 times,
and keeps per metric the median over the runs, in ms:

| probe | what it measures |
|---|---|
| `pause/live-100`, `pause/live-1000` | short-lived churn beside 100 MB / 1 GB of live boxes (`live ptr`): steady minor p50 / p99 (after the heap first reached 90% of its live size), longest pause |
| `pause/cache-64` | a 64 MB ring of middle-lived boxes (`cache`): the stop-the-world full collections (`full_stw`; concurrent marks must not turn into them) |
| `pause/frame` | a 60 fps loop at full speed, 200 MB of assets, 4 MB of garbage per frame (`frame`): frame time p99.9, longest pause |
| `pause/ttsp-count`, `-find`, `-grow`, `-hand` | a churning thread beside one that counts / searches a 256 MB string, grows a `StrBuf` to 256 MB, or hands a 128 MB list to a new thread (`ttsp`): longest time to safepoint and pause |
| `pause/satb` | stores into an 8 Mi-element old list during concurrent marks (`satb`): the longest first store after a collection |

Every metric of a run: `max` (longest pause), `ttsp_max`, `minor_p50`,
`minor_p99`, `full_stw` (a count), `frame_p999`, `first_store_max`. Pauses
are wall-clock times: run the gate on an idle machine. The probes take a
few GB of memory (`live-1000` about 2.5 GB).

`budgets.json` holds the limits; a miss prints `OVER BUDGET` and exits 1:

- `compile`: instructions retired, with the date and host they were
  measured on and a tolerance in percent; checked on that host only.
- `scaling`: the least speedup per kind and thread count, checked where
  the machine has at least that many cores; a kind without floors is
  only printed.
- `pauses`: per probe, on its host only, ms limits (`ms`) with a
  tolerance in percent and counts (`count`) that may not be exceeded.
  The tolerance is wide (pauses are noisier than instructions); the
  limits catch a pause that grows by a factor, such as a thread that
  holds every stop back.

CI runs the gates nightly on macOS arm64 (`bench` in ci.yml).

## LLVM baseline

`llvm.json` is the last record of the same benchmarks built by the LLVM
backend the compiler had before its own (commit b12067d4649d of the
repository before the rename, 2026-10-13, Darwin-arm64, best of 3; the
sources are the same, renamed since). The table's `vs LLVM` is the instruction ratio against it, on
that host only. It cannot be rebuilt from this tree. The C programs
were measured with it, so their rows show how far the two hosts agree.

`NABIR_GC_STATS=1` makes a Nabi program print collections, pause totals
and collector CPU, then blocks / resident KiB / evacuations / bytes
moved, and per kind of pause its p50 / p90 / p99 / longest and time to
safepoint at exit (`NABIR_GC_LOG=1`: a line per collection):

```sh
NABIR_GC_STATS=1 target/bench/gcoldheap
```
