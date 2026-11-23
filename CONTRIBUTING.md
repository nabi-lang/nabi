# Contributing to Nabi

Thanks for your interest in improving Nabi. This repository holds the compiler
(`nabic`), the toolchain front end (`nabi`), the runtime, and the standard
library.

## Repository layout

- `compiler/`: the compiler (`nabic`) and the crates every tool shares
  (`nabic_tools`: formatter, linter, doc generator, completion and hover)
- `cc/`: built-in C compiler (`nabicc`, `nabi cc`)
- `link/`: nabi's own linker (`nabild`) for every target
- `pm/`: the `nabi` command: manifests, dependency resolution, toolchains
- `lsp/`: language server (`nabils`, `nabi lsp`) and its protocol crate
- `repl/`: REPL, its line editor and the Jupyter kernel (`nabi repl`,
  `nabi jupyter`)
- `runtime/`: the runtime linked into every program (`nabir`, its
  collector and ABI)
- `library/`: the standard library (`core`, `std`, `bare`) and debugger
  support
- `tests/`: the end-to-end corpus (`tests/cases`, run by
  `compiler/nabic/tests/main`) and its fixtures
- `targets/`: the C libraries and stubs programs link with on each target
- `tools/`: repository tools (`layout`, the file layout check)
- `scripts/`: release, dist and check scripts
- `bench/`: benchmarks and their budgets
- `assets/`: logo and icons
- `docs/`: CI and release notes for maintainers
- `licenses/`, `nix/`: third-party licenses, Nix sources

## Building

You need the Rust toolchain pinned in `rust-toolchain.toml` and the C
compiler Rust links with. Run `cargo build`, then on Linux
`targets/build.sh <host triple>` for the C library programs link with, as
the README's "Building from source" shows.

The targets in progress (the spec's 0.6.2) stay in the code but `--target`
refuses them. To build or test one, set `NABI_IN_PROGRESS_TARGETS=1`; the
test harness sets it on its own for a `NABI_TEST_TARGET` in progress.

## Checks

Run the gates CI runs. `just` lists every task.

```sh
just fmt-check     # cargo fmt + shfmt
just lint          # clippy -D warnings, shellcheck, PSScriptAnalyzer
just check-layout  # file and module layout rules (tools/layout)
cargo test         # compiler and runtime tests
"${CARGO_TARGET_DIR:-target}/debug/nabi" fmt --check library    # the stdlib
```

Format before sending a change (`just fmt`).

`cargo test` includes the end-to-end corpus (`tests/cases`,
`cargo test -p nabic --test main integration::`), which builds and runs every
case.
To run a subset, set `NABI_TEST_FILTER` to a comma-separated list of name
parts: `NABI_TEST_FILTER=affine,std_201 cargo test -p nabic --test main
integration::`.
Empty or unset runs every case; a filter that selects no case fails, so a
typo never passes. CI runs the whole corpus.

`just fuzz [secs]` runs every fuzz target (`compiler/nabic/tests/main/fuzz.rs`)
for that long, including the slow differential one that compares random
programs at `-O0` with `-O2` and `-Os`; `NABI_FUZZ_SEED` picks other inputs.
A new crash fails the run and leaves a minimized repro in
`tests/crashes/`, a known open bug until the fix
deletes it.

## Changes that affect behavior

The language and standard library are defined by the spec (`nabi-lang/spec`,
English, Korean, Simplified Chinese and Traditional Chinese editions). A change
in observable behavior updates the spec in all four editions in the same change.

## Comments and style

Comments explain *why*, in the terse style of the surrounding code: no banners
or decorative headers, ASCII only. Match the conventions of the file you edit.

## Submitting

Open a pull request against `main` with the checks above passing. End its
description with a `changelog:` block, one line per user-visible change as it
should read in `CHANGELOG.md`, each starting with its kind (`added:`,
`changed:`, `fixed:`, `removed:`), or `silent:` when nobody using Nabi can see
it; the pull request template shows the form and the `Check / PR changelog block` check reads it.
Leave `CHANGELOG.md` alone: it is generated at release time from those blocks,
and its section for the version becomes the GitHub release notes
(`docs/releasing.md`). By contributing you agree that your work is licensed
under both MIT and Apache-2.0, the project's dual license.

## Sign your work

Nabi uses the [Developer Certificate of Origin 1.1](https://developercertificate.org/)
(DCO) instead of a CLA. By adding a `Signed-off-by` line to a commit you
certify that you wrote the change, or otherwise have the right to submit it
under this repository's license. `git commit -s` adds the line:

```text
Signed-off-by: Your Name <you@example.com>
```

Use your name and an email you can be reached at; the email must be the
commit's author email. The `DCO / Check sign-off` check fails a pull request with an
unsigned commit. To fix it, run `git commit --amend -s` (last commit) or
`git rebase --signoff main` (every commit), then push again. The full text
of the DCO is in the
[organization's CONTRIBUTING.md](https://github.com/nabi-lang/.github/blob/main/CONTRIBUTING.md#developer-certificate-of-origin).
