# Development tasks. Run `just` to list them.
# Needs: shfmt, shellcheck, actionlint, pwsh + PSScriptAnalyzer (shell/PowerShell),
# and the Rust toolchain (rustfmt, clippy).

_default:
    @just --list

# Format Rust and shell in place.
fmt: fmt-rust fmt-sh

fmt-rust:
    cargo fmt --all

# shfmt reads indent width and options from .editorconfig.
fmt-sh:
    find . -name '*.sh' -not -path './target/*' -exec shfmt -w {} +

# Fail if anything is unformatted (for CI).
fmt-check: fmt-check-sh
    cargo fmt --all --check

# shfmt check only; the CI lint job reuses this (it has no Rust toolchain).
fmt-check-sh:
    find . -name '*.sh' -not -path './target/*' -exec shfmt -d {} +

# The nabi standard library, formatted by `nabi fmt` (needs a built nabi).
fmt-nabi:
    "${CARGO_TARGET_DIR:-target}/debug/nabi" fmt library

# Lint everything.
lint: lint-sh lint-ps lint-actions clippy

# The workflows: a YAML error stops GitHub from loading the whole file.
lint-actions:
    actionlint -shellcheck="shellcheck -e SC2015"

lint-sh:
    find . -name '*.sh' -not -path './target/*' -exec shellcheck --severity=warning {} +

# PSScriptAnalyzer over the .ps1 scripts; exits non-zero on any finding.
lint-ps:
    #!/usr/bin/env pwsh
    $found = Get-ChildItem -Recurse -Filter *.ps1 |
        Where-Object { $_.FullName -notmatch '[/\\]target[/\\]' } |
        ForEach-Object { Invoke-ScriptAnalyzer -Path $_.FullName -Settings ./PSScriptAnalyzerSettings.psd1 }
    $found | Format-Table -AutoSize
    if ($found) { exit 1 }

clippy:
    cargo clippy --all-targets -- -D warnings

# Every fuzz target for `secs` seconds (the front end, the NABIHIR and
# NABION decoders, the object readers, -O0 against -O2 and -Os builds);
# NABI_FUZZ_SEED picks other inputs. A new crash fails and is recorded
# under tests/crashes/.
fuzz secs="60":
    NABI_FUZZ_SECS={{secs}} cargo test --release -p nabic --test main -- fuzz:: --include-ignored --test-threads=1

# Fail on a file or module layout rule (A, B, D, E) over the Rust and Nabi
# sources; `cargo run -p layout` lists every violation.
check-layout:
    cargo run -q -p layout -- --quiet --deny ABDE

# Fail if the release version disagrees across Cargo.toml and the install scripts.
version-check:
    scripts/check/version.sh

# Fail if the release target triples disagree across release-targets.json and
# the install scripts.
check-targets:
    scripts/check/targets.sh

# release/publish.sh --publish against a stubbed gh: -rc.N is a prerelease, and an
# archive that breaks the installer contract stops the release.
test-release:
    scripts/test/release.sh

# release/prep.sh, changelog/lint.sh, changelog/pr.sh and release/ready.sh's
# expected-jobs check against a fake history and a stubbed gh.
test-release-prep:
    scripts/test/prep.sh

# Fail if nabi doc's copy of the web kit's styles, scripts and fonts
# differs from ../web and ../nabi-lang.org (`scripts/check/kit.sh --update`).
check-kit:
    scripts/check/kit.sh

# Licenses, advisories, bans and sources of Cargo.lock (deny.toml).
deny:
    cargo deny --locked check

# Build this host's release archives twice and compare them.
check-repro:
    scripts/check/repro.sh

# Fail on gitlinks .gitmodules does not list, or anything under .claude/.
check-tree:
    scripts/check/tree.sh
