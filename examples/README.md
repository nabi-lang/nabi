# Examples

Whole programs, each its own package: build one with `nabi run` in its
directory. They use the file system, threads, sockets and C, so they run
on your machine rather than in the [playground](https://playground.nabi-lang.org),
whose examples are the short ones. CI builds, formats, lints and tests
every one against the current release on Linux, macOS and Windows
(`.github/workflows/examples.yml`).

| Example | What it is | What it shows |
|---|---|---|
| [grep](grep) | a parallel recursive search | threads and channels, `regex`, a walk over the file system, exit codes |
| [todo-api](todo-api) | a JSON REST server | `http`, `json` derives, an error sum type mapped to status codes, a shared store behind a `Mutex` |
| [chat](chat) | an async TCP chat room | `async` tasks, channels, one task owning the state, `using` and `Drop` for cleanup |
| [csv-report](csv-report) | a sales report from a CSV file | `csv`, `stats` and `table`, row validation with line numbers, `--json` |
| [kv-store](kv-store) | a crash-safe key-value store | an append-only log with checksums, recovery of a torn tail, atomic compaction, tests |
| [interp](interp) | a tiny scripting language | a generator lexer, a Pratt parser, sum types and exhaustive `match`, errors with carets |
| [args-derive](args-derive) | a CLI parser from a user derive | `@derive` meta blocks, `@for_fields`, reflection, "did you mean" errors |
| [raytracer](raytracer) | a path tracer writing a PPM image | operators (`⋅`, `×`, `.*`, a postfix `°`), sum types, threads |
| [orbit](orbit) | the Moon's orbit, checked against Kepler | `ode`, `units` (`363300km`, `->`), dotted operators, a `plot` SVG |
| [c-interop](c-interop) | C called from Nabi and back | C sources in a package built by nabicc, `extern "C"`, a safe wrapper with `Drop` |

```sh
cd examples/grep
nabi run -- 'fn \w+' ../../library
```
