#!/bin/sh
# The wasm32 corpora: every integration and differential case built for
# each wasm32 target at -O0 and -O2 and run under the target's host, then
# the CLI tests that run wasm modules.
#
#   scripts/test/wasm.sh [<target>..]   (default: all three)
#
# wasm32-wasip2 (a preview 2 component) and wasm32-wasip1-threads (preview 1
# with wasi-threads) run under wasmtime v46.0.3, the last release with
# wasi-threads, fetched (SHA-256 checked) into the cache of
# targets/build.sh, as is wac v0.12.0, which composes the components of
# the `wasm-plug` cases; wasm32-unknown-unknown runs under node (18+) with the
# shipped nabi.mjs. The targets' runtimes are built first
# (targets/build.sh). Needs rustup, curl and, for the browser, node.
# The browser target in real browsers: docs/ci.md.
set -eu
cd "$(dirname "$0")/../.."
cache=${NABI_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/nabi}
V=v46.0.3

case $(uname -m) in
  arm64 | aarch64) arch=aarch64 ;;
  *) arch=x86_64 ;;
esac
case $(uname -s) in
  Darwin) os=macos ext=tar.xz ;;
  Linux) os=linux ext=tar.xz ;;
  *) os=windows ext=zip ;;
esac
name=wasmtime-$V-$arch-$os
case $name in
  wasmtime-"$V"-aarch64-linux) sha=99aa698281ce7c18cb174899df86296d730bf39d6e4826ae6e56a773e0cbe53f ;;
  wasmtime-"$V"-aarch64-macos) sha=a5ae4844eedc9bce875b443ab07e3154f0ff06cb2959bbbbb734c0cde62fa39f ;;
  wasmtime-"$V"-aarch64-windows) sha=92b78fce601267c4ed22c1e18383e8d7d44fd2cc67f862a705068087ca6ae8cb ;;
  wasmtime-"$V"-x86_64-linux) sha=a13087bea96394e183a205d653e7337d50b596aca3090d496330efc770a54121 ;;
  wasmtime-"$V"-x86_64-windows) sha=35411bda40177b63e6ef3c4564a5d59eb29c031a1f42cb7201330e1819b57442 ;;
esac

file=$cache/$name.$ext
if [ ! -f "$file" ]; then
  mkdir -p "$cache"
  curl -fsSL --retry 3 -o "$file.part" "https://github.com/bytecodealliance/wasmtime/releases/download/$V/$name.$ext"
  mv "$file.part" "$file"
fi
got=$( (shasum -a 256 "$file" 2> /dev/null || sha256sum "$file") | cut -d' ' -f1)
if [ "$got" != "$sha" ]; then
  echo "checksum mismatch for $file: $got" >&2
  exit 1
fi
if [ ! -d "$cache/$name" ]; then
  case $ext in
    zip) (cd "$cache" && unzip -q "$file") ;;
    *) tar -xJf "$file" -C "$cache" ;;
  esac
fi
runner=$cache/$name/wasmtime
[ "$os" = windows ] && runner=$runner.exe

# wac, which composes the `wasm-plug` cases' components (none ships for
# Windows on arm64: those cases are skipped there).
WAC=v0.12.0
case $arch-$os in
  aarch64-macos) wac=aarch64-apple-darwin wac_sha=891af0e6d591481daed3a7c52a3dc20fd34cc326e6e955d4c8fd85c4b1835e3d ;;
  aarch64-linux) wac=aarch64-unknown-linux-musl wac_sha=80fe01c4cbf1e59526069d46f51a8a396be6f8df09ecc060d949181a317c08f5 ;;
  x86_64-linux) wac=x86_64-unknown-linux-musl wac_sha=7cd490edbd452221ef89e6750fc43eacc9b71b1242095eb577cb9a3bf0d3beda ;;
  x86_64-windows) wac=x86_64-pc-windows-gnu wac_sha=33d92a2a73db1f8de69538038e594a14cc8f9e6feb8138bf29fdaf2598d425d9 ;;
  *) wac= ;;
esac
NABI_TEST_WAC=
if [ -n "$wac" ]; then
  NABI_TEST_WAC=$cache/wac-$WAC-$wac
  [ "$os" = windows ] && NABI_TEST_WAC=$NABI_TEST_WAC.exe
  if [ ! -f "$NABI_TEST_WAC" ]; then
    curl -fsSL --retry 3 -o "$NABI_TEST_WAC.part" "https://github.com/bytecodealliance/wac/releases/download/$WAC/wac-cli-$wac"
    mv "$NABI_TEST_WAC.part" "$NABI_TEST_WAC"
  fi
  got=$( (shasum -a 256 "$NABI_TEST_WAC" 2> /dev/null || sha256sum "$NABI_TEST_WAC") | cut -d' ' -f1)
  if [ "$got" != "$wac_sha" ]; then
    echo "checksum mismatch for $NABI_TEST_WAC: $got" >&2
    exit 1
  fi
  chmod +x "$NABI_TEST_WAC"
fi

[ $# -gt 0 ] || set -- wasm32-wasip2 wasm32-wasip1-threads wasm32-unknown-unknown
targets/build.sh "$@"

export NABI_TEST_WASM_RUNNER="$runner" NABI_TEST_WAC
# wasmtime under load is slow; a hung case still fails, not the job.
export NABI_TEST_CASE_TIMEOUT="${NABI_TEST_CASE_TIMEOUT:-900}"
for t in "$@"; do
  for o in "" -O2; do
    echo "== $t ${o:--O0}"
    NABI_TEST_TARGET=$t NABI_TEST_OPT=$o scripts/test/filter.sh -p nabic --test main -- integration:: differential:: \
      native::abi:: native::ffi::
  done
done
scripts/test/filter.sh -p nabic --test main -- cli::targets::cross_targets_link_from_the_sysroot \
  cli::targets::browser_module_exports_to_js cli::deps::wit_prebuilt_dependency_carries_its_wit
