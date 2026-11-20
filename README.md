<p align="center">
  <img src="assets/logo.svg" alt="Nabi" width="96">
</p>

# Nabi

[![CI](https://github.com/nabi-lang/nabi/actions/workflows/ci.yml/badge.svg)](https://github.com/nabi-lang/nabi/actions/workflows/ci.yml) [![release](https://img.shields.io/github/v/release/nabi-lang/nabi?include_prereleases&sort=semver)](https://github.com/nabi-lang/nabi/releases) [![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

*Clear rules, free as a butterfly.*

The implementation of the Nabi language: the compiler `nabic`, the package / build
front end `nabi`, the language server `nabils`, the runtime `libnabir.a`, and the
standard library (`library/{core,std,bare}`).

- Site: https://nabi-lang.org
- Docs (guide, reference, standard library API): https://docs.nabi-lang.org
- Language specification: https://github.com/nabi-lang/spec

## Install

macOS, Linux and FreeBSD:

```sh
curl -fsSL https://nabi.sh | sh
```

Windows (x64 or arm64), in PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://nabi.sh/install.ps1 | iex"
```

The installer puts `~/.nabi/bin` on `PATH` the way rustup does: `install.sh` writes
`~/.nabi/env` and sources it from `~/.profile`, `~/.bashrc`, `~/.bash_profile` and
`~/.zshenv` (and fish's `conf.d/nabi.fish`), and `install.ps1` sets the user `PATH`. It
also installs shell completion for `nabi` (`nabi completions <shell>` prints the
script). Open a new shell afterwards. To leave your shell files alone, pass
`--no-modify-path` (`curl -fsSL https://nabi.sh | sh -s -- --no-modify-path`)
or `-NoModifyPath`, or set `NABI_NO_MODIFY_PATH=1`.
The toolchain carries the official packages, so code that uses them builds
without a network. To leave them out, pass `--no-packages` (`-NoPackages`), or
set `NABI_NO_PACKAGES=1` (`nabi component remove packages` does it later).

Nix:

```sh
nix profile install github:nabi-lang/nabi
```

The [Download page](https://nabi-lang.org/download.html) has the release archives,
their checksums, and the cross-compilation targets. Each release's `SHA256SUMS` is
signed with minisign (`keys/release.pub`, key `B33E6FE61BE5314C`); `nabi toolchain
update` installs nothing that does not verify, and the installers check it when
`minisign` is on PATH (docs/releasing.md, "Signing"). Nothing else is needed: a
toolchain has its own code generator and linker and ships every target's
runtime, so `--target <triple>` cross-compiles from any host.

## Quick start

```sh
nabi new hello        # hello/{src/main.nabi, nabi.nabion, .gitignore} + git init
cd hello
nabi run              # Hello, world!
nabi repl .           # a REPL session with the package's bindings
nabi test
nabi jupyter install  # Nabi kernel for Jupyter / VS Code
```

## Building from source

Building needs rustup with the Rust toolchain pinned in `rust-toolchain.toml`
and the C compiler and linker Rust itself uses; the runtime step below also
needs `curl`, `python3` and `tar` with xz (it adds rustup's `llvm-tools`
component for `llvm-ar`).

```sh
cargo build --release                                     # target/release/{nabi,nabic,nabils} and the host runtime libnabir.a
targets/build.sh "$(rustc -vV | sed -n 's/^host: //p')"   # the host's C library (Linux), so nabic can link programs
```

`cargo build` produces the binaries and the host runtime
(`target/<profile>/libnabir.a`). A target whose C library the toolchain
ships (Linux, wasm32, FreeBSD) and every non-host target need their files
under `target/lib/nabi/targets/<triple>/`, which `targets/build.sh <triple>`
builds; without them `nabic` reports `no runtime for <triple>`. On macOS
and Windows the host needs nothing more. Build any other shipped triple to
cross-compile to it (no triple builds every one the host can).

`nabic` generates code itself (`compiler/nabic_codegen_native`) and links
every target with its own linker (`link/nabild`); `NABI_CC` names a C
compiler driver to link with instead. `nabi bindgen` reads C headers with
the compiler's own C front end (`cc/`): no C compiler or libclang is
needed.

Checks:

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --no-fail-fast
./target/debug/nabi fmt --check library
```

`NABI_TEST_FILTER=a,b cargo test -p nabic --test main integration::` runs only
the corpus cases whose name contains `a` or `b`; empty or unset runs every case.

Packaging (`dist-*.sh`), the cross-target and wasm corpus, and the CI tiers are
documented in [docs/ci.md](docs/ci.md).

## License

`MIT OR Apache-2.0` ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)).
The specification repository is CC-BY-4.0.

nabic embeds third-party files under their own licenses, listed below. The
release archives also ship C runtime pieces for their cross targets (musl,
glibc crt objects, wasi-libc) and the Rust standard library linked into the
binaries and the runtime; [THIRD-PARTY-NOTICES](THIRD-PARTY-NOTICES) lists
everything an archive carries, with the license texts in
[licenses/](licenses/).

- the intrinsic headers in `cc/nabicc_pp/include/` (`immintrin.h`,
  `arm_neon.h`, `intrin.h`, ...) are LLVM's clang headers reduced to
  declarations by `cc/nabicc_pp/intrin.py`:
  `Apache-2.0 WITH LLVM-exception`
  ([LICENSE-LLVM.txt](cc/nabicc_pp/include/LICENSE-LLVM.txt);
  `arm_neon.h` and `arm_fp16.h` carry the MIT notice clang generates them
  with);
- the Windows module definitions in `link/nabild/windows/` are
  mingw-w64's
  ([COPYING.mingw-w64](link/nabild/windows/COPYING.mingw-w64)).
