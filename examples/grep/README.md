# grep

A small ripgrep-like search tool: a regex over every file under the given
paths, searched by one worker thread per CPU.

```sh
cd examples/grep && nabi run -- 'fn \w+' ../../library
```

```
../../library/bare/src/alloc.nabi:11:    fn nabi_alloc_raw(size: usize, align: usize, zeroed: i32) -> usize;
../../library/bare/src/alloc.nabi:12:    fn nabi_realloc_raw(ptr: usize, old_size: usize, align: usize, new_size: usize) -> usize;
../../library/bare/src/alloc.nabi:13:    fn nabi_free(ptr: usize);
```

## Usage

```
grep [-i] [-c] [-l] <pattern> [path..]
```

| Flag | |
|---|---|
| `-i` | ignore case |
| `-c` | print `path:count` per file |
| `-l` | print only the names of matching files |
| `--` | end of flags, for a pattern that starts with `-` |

The paths default to `.`. Hidden directories, `target/` and binary files
(a NUL byte in the first 8 KiB) are skipped. Output is `path:line:text`,
sorted by path; on a terminal the path and each match are colored
(`NO_COLOR` turns that off). Exit status: 0 on a match, 1 on none, 2 on an
error (a bad pattern, an unreadable path).

```sh
$ nabi run -- -c pipe ../../../spec/en/refs-lang/003-expressions
../../../spec/en/refs-lang/003-expressions/302-precedence-and-associativity.md:1
../../../spec/en/refs-lang/003-expressions/318-pipe-expression.md:5
../../../spec/en/refs-lang/003-expressions/README.md:1
$ nabi run -- '(ab' .
grep: regex parse error at byte 0: unclosed group: add `)`, or write `\(` to match a parenthesis
    (ab
    ^
```

## What it shows

- `src/args.nabi`: a typed error, `throws (UsageError)`, and
  `pattern ?? throw ...` on a `str | none`.
- `src/main.nabi`: `try .. catch e: RegexError { .. }` turning errors into
  exit codes, a pipe into the parser, a `[ref failed]` closure shared by
  the walker and the workers' reports, and an MPMC channel feeding
  `std::thread` workers. A `Regex` is not `Sync`, so each worker owns one
  (`[move re]`).
- `src/walk.nabi`: `try for` over `fs::read_dir`, with the error of an
  unreadable directory caught and reported while the walk goes on.
- `src/search.nabi`: a `Found | none` result, iterator adapters, and
  match highlighting from `regex` byte ranges.
- `src/tty.nabi`: `#[cfg]`-selected `extern "C"` declarations for the
  terminal check, since std has none yet.
