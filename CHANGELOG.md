# Changelog

## 0.1.7 (2026-11-28)

<!-- begin summary -->
Summary: Operators can bind `->`, so unit conversions read `bag -> g` with no Unicode arrow to type.
<!-- end summary -->

### Added

- `#[infix("->", like = "|")]` binds `->` between two values, so `bag -> g` converts; a return type keeps `->` (#65)

<!-- silent: #62 #63 #64 -->

## 0.1.6 (2026-11-27)

<!-- begin summary -->
Summary: Clearer errors for mixing units, and the `units` package 0.1.1 bundled.
<!-- end summary -->

<!-- begin highlights -->
### Highlights

- `10kg + 3m` now says that `Mass` and `Length` are two different types, instead of suggesting a `Quantity<f64, 0, 1, 0, 0, 0, 0, 0>` to write. Help notes name types by the aliases the message uses.
- The bundled `units` package is 0.1.1: converting to `g`, `cm`, `mm` or `ms` is exact where the value allows, so `10.54kg → g` is `10540 g`.
<!-- end highlights -->

### Fixed

- Adding two values whose types differ only in const parameters (`10kg + 3m`) no longer suggests changing an element type; the help says they are different types (#60)

<!-- silent: #56 #57 #59 -->

## 0.1.5 (2026-11-24)

<!-- begin summary -->
Summary: Fixes the 0.1.4 regression that stopped the `table` package from building.
<!-- end summary -->

<!-- begin highlights -->
### Highlights

- In 0.1.4, `x ?? d` inside a branch where `x is none` or `x != none` had narrowed `x` failed with E0064, which 0.1.3 accepted. The `table` package hits this, so it and every project that depends on it failed to build. It compiles again; update with `nabi toolchain update`.
<!-- end highlights -->

### Fixed

- `x ?? d` compiles again inside a branch where `x is none` or `x != none` narrowed `x`, so the `table` package builds on 0.1.5 (#54)

<!-- silent: #53 -->

## 0.1.4 (2026-11-24)

<!-- begin summary -->
Summary: Pattern and inference fixes, and errors that say what to write instead.
<!-- end summary -->

<!-- begin highlights -->
### Highlights

- An f-string or print hole whose type cannot be inferred, such as a recursive call, is now an error; it used to print nothing. Holes are also formatted left to right.
- `..` works in every tuple and tuple struct pattern (`(a, ..)`, `(h, ..rest)`, `Pair(a, ..)`) in `match`, `if let` and `let`.
- `x is none` narrows like `x == none`, and a catch-all arm after `none =>` has the remaining type.
- Errors for habits from other languages (truthiness, `for` over a `str`, tuple indexing, `'text'`, a keyword as a name, `self::f`, refutable `let`, `?` in a `()` function) say what to write instead, and a mistyped option of any `nabi` command names the closest one.
<!-- end highlights -->

### Fixed

- `nix profile install github:nabi-lang/nabi` installs the current release (#18)
- Tuple rest patterns (`(a, ..)`, `(h, ..rest)`) work in `match` and `if let`, not only `let` (#22)
- A keyword used as a name gives E0021 with the `r#name` help for every keyword (#22)
- `self::f` reports that paths start at the name and suggests `f` or `pkg::f` (#22)
- A bare type pattern (`i32 =>`) is no longer warned as an unused variable (#22)
- An f-string or print hole whose type cannot be inferred is an error instead of printing nothing (#45)
- F-string holes are formatted left to right, so a later hole that changes a value no longer changes an earlier one (#45)
- `x is none` narrows `x` like `x == none` (#45)
- A plain value passed to a `T | none` parameter infers `T` (#45)
- A catch-all binding after a `none =>` arm has the remaining type (`i32`, not `i32 | none`) (#45)
- `..` works in tuple struct patterns (`Pair(a, ..)`) in `match` and `let` (#46)
- A closure with the wrong number of parameters is one error, suggesting `|(k, v)|` when the one argument is a tuple (#46)
- A type pattern (`while let x: i32 = it.next()`) fixes the type of an unsuffixed literal (#46)
- A refutable type pattern in `for` or `let` explains that it must always match (#46)
- `ParseError::position` is the byte where parsing failed (#46)
- Inf and NaN are right-aligned under a width, like other numbers (#46)
- A malformed dynamic format spec panics with its source location (#46)
- Clearer help for ambiguous `consume`, `let _ = File::open(..)`, `Set<f64>`, `fs::read_to_str` on bad UTF-8 and `fs::copy` into a directory (#46)
- `nabi init --script` writes `nabi_version` and the toolchain pin, runs `git init` and prints the next command (#46)
- A mistyped option of any `nabi` command names the closest one, including `run`, `test`, `bench`, `check` and the global options (#47)
- `nabi version` and options before the command report unknown options instead of ignoring them (#47)
- `nabi run x.nabi arg` says how to pass the argument to the program (#47)
- In `-> some Iterator<Item = i32>`, literals of the returned value (`0..3`, `[1, 2].iter()`) are `i32` (#49)
- Errors for common habits from other languages say what to write instead (truthiness, `for` over a `str`, tuple indexing, single-quoted strings, `f"{}"`, untyped `const`, `1_u8`, a dropped last value, `if` without `else`, run-time array lengths) (#49)
- The help for a shift count of another type converts the count, not the shifted value (#49)
- Refutable `let`, chained `if let`, `?` in a function returning `()`, an empty `try`, the orphan rule, mixed value enums, a missing `await`, a wrong trait path and `[v; N]` for a `List` get help that says what to write (#49)

<!-- silent: #19 #1 #21 #23 #48 #51 -->

## 0.1.3 (2026-11-22)

<!-- begin summary -->
Summary: `nabi audit` checks the packages pinned in `nabi.lock` against the registry's security advisories.
<!-- end summary -->

<!-- begin highlights -->
### Highlights

- `nabi audit` reads `nabi.lock` and reports each pinned package that a packages.nabi-lang.org advisory affects, with the advisory ID, summary, severity and the version to update to (for example `nabi update json`). It exits 0 when nothing is affected, 1 when something is, and 2 when it cannot check, so CI can run it as a gate.
- `--json` prints machine-readable output, `--ignore <ID>` skips an advisory, and `--offline` uses only the cached advisory list. The list is cached for an hour; without a network, an older cache is used with a warning. Path and git dependencies are skipped and listed as not checked.
<!-- end highlights -->

## 0.1.2 (2026-11-22)

<!-- begin summary -->
Summary: A fix release: packages whose types go through `any`, such as `spatial` and `sparse`, build again as dependencies.
<!-- end summary -->

<!-- begin highlights -->
### Highlights

- 0.1.1 failed to build a project that depends on `spatial` ("cannot find type `spatial::euler::EulerOrder`"), or on `spatial` and `sparse` together ("cannot find trait `sparse::Preconditioner`"). The code `nabic` writes for a package's types stored in `any` now names them as that package sees them, and a package no longer carries that code for packages it does not depend on.
- `any ::dep::Trait` parses as an `any` bound again; with a renamed dependency, `any al::Trait` failed with "cannot find type `any::alpha::Trait`".
- Changing only which dependencies a package sees now rebuilds it; before, the build reused a stale result.
<!-- end highlights -->

## 0.1.1 (2026-11-22)

<!-- begin summary -->
Summary: A fix release: each package now sees only its own dependencies, and the toolchain bundles the updated official packages.
<!-- end summary -->

<!-- begin highlights -->
### Highlights

- A package sees only the packages it depends on, under the names it gives them. Before, a bare `x::` path could resolve to any package in the build graph, so `table` 0.1.0 failed to build next to the `csv` package. When a dependency and a child module share a name, `x::` is now an error that names both and says to write `::x::..` or `pkg::x::..` (#11).
- The bundled official packages: `table` 0.1.3 (builds in any graph, docs without a `plot` cycle), and `autodiff`, `linalg`, `ode`, `optimize`, `signal`, `sparse` and `stats` 0.1.1, whose tutorials moved into their API docs at docs.nabi-lang.org/packages.
<!-- end highlights -->

## 0.1.0 (2026-11-22)

<!-- begin summary -->
Summary: The first release of Nabi: the compiler, the `nabi` package tool, the language server, the runtime, the standard library and 20 official packages, with Nabi's own code generator and linker for 17 supported targets and release archives for 8 hosts.
<!-- end summary -->

<!-- begin highlights -->
### Highlights

- One toolchain with no LLVM and no system linker: `nabic` generates machine code and links it itself, and each archive ships the runtime of every supported target (the Apple ones in the macOS archive), so `--target <triple>` cross-compiles to the Linux, FreeBSD, Windows, Android and wasm32 targets from any host (Android links with the NDK).
- 17 supported build targets, and release archives for 8 hosts: macOS on Apple Silicon, Windows 11 on x86_64 and aarch64, Linux on x86_64 and aarch64 (glibc, and static musl), and FreeBSD 14 on x86_64.
- `nabi` runs the whole workflow: `new`, `build`, `run`, `test`, `bench`, `fmt`, `lint`, `doc`, `repl`, dependencies with `nabi.lock`, `publish` to the package registry at packages.nabi-lang.org, `toolchain` to install and switch versions, and `bindgen` for C headers.
- 20 official packages on packages.nabi-lang.org (`json`, `http`, `regex`, `toml`, `csv`, `markdown`, `table`, `stats`, `linalg`, `plot` and more) come with the toolchain: code that uses one gets it added to its manifest on first use, with no network.
- `nabils` (`nabi lsp`) for editors, with extensions for VS Code, Vim and Zed; a REPL and a Jupyter kernel (`nabi jupyter install`).
- A generational, moving garbage collector that works with threads; async tasks; a standard library with collections, files, processes, and networking with TLS.

### Platforms

- Supported build targets: x86_64 and aarch64 Linux (glibc, and static musl), macOS on Apple Silicon, x86_64 and aarch64 Windows, x86_64 FreeBSD, i686 Linux and Windows, armv7 Linux (glibc, and static musl), `wasm32-wasip2`, `wasm32-wasip1-threads` and `wasm32-unknown-unknown`, Android on x86_64 (`x86_64-linux-android`, the emulator) and the iOS simulator (`aarch64-apple-ios-sim`).
- Six targets are in progress and come in a later release: Android and iOS devices (`aarch64-linux-android`, `aarch64-apple-ios`), riscv64, powerpc64le and s390x Linux, and `riscv32imac-unknown-none-elf`. `--target` refuses them and names what to build for now.
- Apple targets build only on macOS, with the Xcode Command Line Tools SDK (the iOS simulator with Xcode's iOS SDK).

### Compatibility notes

- Before 1.0 the minor version is the breaking slot: 0.2.0 may break code written for 0.1, while 0.1.x releases add features and fixes without breaking it. Only the latest series gets fixes.
- Programs linked for the `-gnu` Linux targets need glibc 2.34 or newer. On an older system the installer picks the static musl archive.

### Known limits

- The TLS stack and the cryptography in `core::crypto` are Nabi's own code and have had no external audit. Treat them as experimental.
- Releases are signed: `SHA256SUMS` with minisign (key `B33E6FE61BE5314C`, `keys/release.pub`), which `nabi toolchain install` / `update` require and `install.sh` / `install.ps1` check when `minisign` is on PATH, plus GitHub build provenance and SBOM attestations. The binaries themselves are not code-signed (no macOS notarization, no Windows Authenticode): Gatekeeper and SmartScreen warn about an archive downloaded with a browser; the `curl | sh` and `irm | iex` installers are not affected.
- Only the registry's maintainers can publish packages for now; anyone can sign in and use them.
- The toolchain's build cache is capped at 5 GiB; `NABI_CACHE_MAX_SIZE` changes the cap (`off`: none).
<!-- end highlights -->

