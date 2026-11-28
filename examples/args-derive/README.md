# args-derive

A command-line parser written as a user derive, and a small `wc` built
on it. `#[derive(Args)]` reads a struct at compile time and writes
`parse(argv)` and `help()` for it; no runtime schema is written by hand.

```
#[derive(Args, Inspect, Debug)]
struct Opts {
    #[arg(short = "l", help = "print the line count")]
    lines: bool;
    #[arg(short = "L", help = "also count lines longer than this")]
    longer: i64 | none;
    #[arg(help = "words per minute for --read-time")]
    wpm: f64 = 238.0;
    #[arg(help = "files to count; stdin when none")]
    files: List<str>;
}

let o = try Opts::parse(env::args()) catch e { e.exit() };
```

The field type picks the shape: `bool` is a flag, `str` / `i64` / `f64`
take a value (`--wpm 200` or `--wpm=200`), `T | none` may be left out,
`List<str>` takes the positionals, and `= expr` is the default. A field
named `read_time` is `--read-time`. `--` ends the options.

## Run

```sh
nabi run -- src/main.nabi src/args.nabi
nabi run -- -l -L 80 -r --wpm 200 src/args.nabi
nabi run -- --help
nabi run -- --dump -m fn src/main.nabi
nabi test
```

```
$ args-derive --lnes
args-derive: unknown option --lnes; did you mean --lines?
try `args-derive --help`
$ args-derive -m
args-derive: missing value for -m
try `args-derive --help`
$ args-derive --wpm=fast
args-derive: --wpm wants a number, not `fast`
try `args-derive --help`
$ args-derive --help
usage: args-derive [options] [files..]

options:
  -l, --lines           print the line count
  -w, --words           print the word count
  -c, --bytes           print the byte count
  -m, --matching <str>  count only lines containing this
  -L, --longer <i64>    also count lines longer than this
  -r, --read-time       also print the reading time
      --wpm <f64>       words per minute for --read-time (default 238.0)
      --total <str>     name of the total row (default "total")
      --dump            print the parsed options and exit
  -h, --help            print this help

files..: files to count; stdin when none
```

Usage errors exit 2, `--help` exits 0, an unreadable file exits 1.

## What it shows

- `src/args.nabi`: `#[impl_derive_attribute(Args, arg)]` with
  `@for_fields`, `@field.name`, `@field.type`, `@field.default` and
  `@apply_if(@has_derive_attr(arg))`. The attribute's `short = "v",
  help = ".."` reaches the template as tokens; `@derive_attr(arg)` passes
  them to a small `@macro` that rebuilds them as a `Meta { .. }` literal,
  so a misspelt key is a compile error with a "did you mean" help. A
  `Value` trait (with `impl<T: Value> Value for T | none`) maps each
  field type to its shape, and its `--help` hint comes from
  `T::type_name()`.
- `src/main.nabi`: the `wc`; `--dump` prints the `Inspect` field table
  (`Opts::fields()`) and the parsed value.
- `src/args/tests.nabi`: flags, options, defaults, missing values,
  bad numbers, required options, the "did you mean" suggestion and the
  `--help` text.

`nabi build --emit=expansions` writes what the derive generated to
`target/debug/build/args-derive.expanded.nabi`.
