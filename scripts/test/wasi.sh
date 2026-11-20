#!/bin/sh
# The WASI corpora under runtimes other than wasmtime (wasm.sh's): every
# integration and differential case at -O0 and -O2, wasm32-wasip2 under
# jco (the component transpiled to JS, on node) and wasm32-wasip1-threads
# under WAMR's iwasm and under wasmer, through the targets/wasi/
# wrappers. Run on a maintainer's machine before a release, not in CI.
#
#   scripts/test/wasi.sh [<runtime>..]   (default: jco iwasm wasmer)
#
# Pinned releases, fetched (SHA-256 checked) into the cache of
# targets/build.sh: node v24.22.3 (v26.13.1 deadlocks now and then in
# process.exit: its platform shutdown joins a V8 background compile job
# that waits for a GC only the exiting main thread could run; v24 does
# too, more rarely, so targets/wasi/jco.sh leaves without it) with jco
# 1.42.0, preview2-shim 0.31.1 and preview3-shim 0.14.1 from npm (exact
# versions; npm checks the registry's hashes) and wasm-tools 1.263.0,
# WAMR 2.4.6's iwasm (its release build has wasi-threads; x86_64 only;
# and one built from its source with room for more environment
# variables) and wasmer 7.5.0, and wac (as wasm.sh) for the
# `wasm-plug` cases. Linux x86_64 or aarch64, or macOS arm64 for jco
# and wasmer. Needs rustup and curl, for iwasm also cmake and a C
# compiler.
set -eu
cd "$(dirname "$0")/../.."
cache=${NABI_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/nabi}
mkdir -p "$cache"

case $(uname -s)-$(uname -m) in
  Linux-x86_64) plat=linux-x64 ;;
  Linux-aarch64 | Linux-arm64) plat=linux-arm64 ;;
  Darwin-arm64) plat=darwin-arm64 ;;
  *)
    echo "wasi.sh: no pinned runtimes for $(uname -s) $(uname -m)" >&2
    exit 1
    ;;
esac

# fetch <url> <file> <sha256>
fetch() {
  if [ ! -f "$cache/$2" ]; then
    curl -fsSL --retry 3 -o "$cache/$2.part" "$1"
    mv "$cache/$2.part" "$cache/$2"
  fi
  got=$( (shasum -a 256 "$cache/$2" 2> /dev/null || sha256sum "$cache/$2") | cut -d' ' -f1)
  if [ "$got" != "$3" ]; then
    echo "checksum mismatch for $cache/$2: $got" >&2
    exit 1
  fi
}

NODE=v24.22.3
JCO=1.42.0
WASM_TOOLS=1.263.0
jco() {
  case $plat in
    linux-x64) sha=4f2b175a6d53a5c617a80dba2676238a0688cef28c6da4eba35c7d844e66fcfb ;;
    linux-arm64) sha=6e51a2ada00f0011cc266f65d98922b403d7ad88f809ca2aa615a5fe1ce6e86c ;;
    darwin-arm64) sha=1d64136c8a43d648d1b3bad10797a4c995b1ca032d2bde8799de43b37f058fe5 ;;
  esac
  name=node-$NODE-$plat
  fetch "https://nodejs.org/dist/$NODE/$name.tar.xz" "$name.tar.xz" "$sha"
  [ -d "$cache/$name" ] || tar -xJf "$cache/$name.tar.xz" -C "$cache"
  dir=$cache/jco-$JCO
  if [ ! -x "$dir/node_modules/.bin/jco" ]; then
    mkdir -p "$dir"
    echo '{"name":"nabi-jco","private":true}' > "$dir/package.json"
    (cd "$dir" && PATH=$cache/$name/bin:$PATH npm_config_cache=$cache/npm npm install \
      --save-exact --no-audit --no-fund "@bytecodealliance/jco@$JCO" \
      @bytecodealliance/preview2-shim@0.31.1 @bytecodealliance/preview3-shim@0.14.1)
  fi
  # The WIT of a component, which `jco.sh --invoke` reads the export's
  # types from (jco's `wit` prints the world alone).
  case $plat in
    linux-x64) a=x86_64-linux sha=21c036c1740a0552124c79c6e2170cb61c5b016005671a6d5bc06cf642165431 ;;
    linux-arm64) a=aarch64-linux sha=773b5e6bf957f0d93a9a337c9c5fc4c35509706301a80799f57a76fa367fd1ca ;;
    darwin-arm64) a=aarch64-macos sha=b568f04746a699c4068b8d751779b60b501cf2cab6d259a3bc973eb5eeb1f62b ;;
  esac
  fetch "https://github.com/bytecodealliance/wasm-tools/releases/download/v$WASM_TOOLS/wasm-tools-$WASM_TOOLS-$a.tar.gz" \
    "wasm-tools-$WASM_TOOLS-$a.tar.gz" "$sha"
  [ -x "$cache/wasm-tools-$WASM_TOOLS-$a/wasm-tools" ] || tar -xzf "$cache/wasm-tools-$WASM_TOOLS-$a.tar.gz" -C "$cache"
  export PATH="$cache/$name/bin:$PATH" JCO="$dir/node_modules/.bin/jco" \
    WASM_TOOLS="$cache/wasm-tools-$WASM_TOOLS-$a/wasm-tools"
}

WAMR=2.4.6
iwasm() {
  [ "$plat" = linux-x64 ] || {
    echo "wasi.sh: WAMR ships iwasm for x86_64 only" >&2
    exit 1
  }
  name=iwasm-$WAMR-x86_64-ubuntu-22.04
  fetch "https://github.com/bytecodealliance/wasm-micro-runtime/releases/download/WAMR-$WAMR/$name.tar.gz" \
    "$name.tar.gz" 90f00a2c01fe4525c98d22874bffee6418f4435126170d1a19bf246171df9e31
  mkdir -p "$cache/iwasm-$WAMR"
  [ -x "$cache/iwasm-$WAMR/iwasm" ] || tar -xzf "$cache/$name.tar.gz" -C "$cache/iwasm-$WAMR"
  # iwasm's command line takes at most 8 environment variables; a case
  # setting more runs under this build of the release's source with
  # room for 64 (fast interpreter, SIMD through SIMDe, wasi-threads).
  fetch "https://github.com/bytecodealliance/wasm-micro-runtime/archive/refs/tags/WAMR-$WAMR.tar.gz" \
    "wamr-$WAMR.tar.gz" 6a8de3f74671a72698b4bd61a96c1ce8966db077b2adad7b720757cfd9caff62
  wide=$cache/iwasm-$WAMR-env64
  if [ ! -x "$wide/iwasm" ]; then
    rm -rf "$wide.src"
    mkdir -p "$wide.src"
    tar -xzf "$cache/wamr-$WAMR.tar.gz" -C "$wide.src"
    src=$wide.src/wasm-micro-runtime-WAMR-$WAMR
    sed -i.orig 's/env_list\[8\]/env_list[64]/' "$src/product-mini/platforms/common/libc_wasi.c"
    grep -q 'env_list\[64\]' "$src/product-mini/platforms/common/libc_wasi.c"
    cmake -S "$src/product-mini/platforms/linux" -B "$wide.src/build" -DCMAKE_BUILD_TYPE=Release \
      -DWAMR_BUILD_INTERP=1 -DWAMR_BUILD_FAST_INTERP=1 -DWAMR_BUILD_AOT=0 -DWAMR_BUILD_JIT=0 \
      -DWAMR_BUILD_FAST_JIT=0 -DWAMR_BUILD_LIBC_WASI=1 -DWAMR_BUILD_LIB_WASI_THREADS=1 \
      -DWAMR_BUILD_SHARED_MEMORY=1 -DWAMR_BUILD_THREAD_MGR=1 -DWAMR_BUILD_BULK_MEMORY=1 \
      -DWAMR_BUILD_REF_TYPES=1 -DWAMR_BUILD_SIMD=1 -DWAMR_BUILD_LIB_SIMDE=1 \
      -DWAMR_BUILD_TAIL_CALL=1 > "$wide.src/log" 2>&1 &&
      cmake --build "$wide.src/build" --target iwasm --parallel >> "$wide.src/log" 2>&1 || {
      cat "$wide.src/log" >&2
      exit 1
    }
    mkdir -p "$wide"
    cp -L "$wide.src/build/iwasm" "$wide/iwasm"
    rm -rf "$wide.src"
  fi
  export IWASM="$cache/iwasm-$WAMR/iwasm" IWASM_WIDE="$wide/iwasm"
}

WASMER=7.5.0
wasmer() {
  case $plat in
    linux-x64) a=linux-amd64 sha=23f99818deaad93859f8bb8292af81f0c39f25aea19eaccfb72fc7c9e8a3fae9 ;;
    linux-arm64) a=linux-aarch64 sha=903dadb34ba984d981a9bdd2ed1e456dab862c4489d65d2498ad5e7cf1a1fe87 ;;
    darwin-arm64) a=darwin-arm64 sha=fd6526fdce3f8c68a8c3139e9b0e438991d04bafda3d0f091bf98c1600ea0401 ;;
  esac
  fetch "https://github.com/wasmerio/wasmer/releases/download/v$WASMER/wasmer-$a.tar.gz" \
    "wasmer-$WASMER-$a.tar.gz" "$sha"
  mkdir -p "$cache/wasmer-$WASMER"
  [ -x "$cache/wasmer-$WASMER/bin/wasmer" ] || tar -xzf "$cache/wasmer-$WASMER-$a.tar.gz" -C "$cache/wasmer-$WASMER"
  # Its home (and the registry's cache) stays out of ~/.wasmer.
  export WASMER="$cache/wasmer-$WASMER/bin/wasmer" WASMER_DIR="$cache/wasmer-home"
}

# wac composes the `wasm-plug` cases' components.
WAC=v0.12.0
case $plat in
  linux-x64) wac=x86_64-unknown-linux-musl wac_sha=7cd490edbd452221ef89e6750fc43eacc9b71b1242095eb577cb9a3bf0d3beda ;;
  linux-arm64) wac=aarch64-unknown-linux-musl wac_sha=80fe01c4cbf1e59526069d46f51a8a396be6f8df09ecc060d949181a317c08f5 ;;
  darwin-arm64) wac=aarch64-apple-darwin wac_sha=891af0e6d591481daed3a7c52a3dc20fd34cc326e6e955d4c8fd85c4b1835e3d ;;
esac
fetch "https://github.com/bytecodealliance/wac/releases/download/$WAC/wac-cli-$wac" "wac-$WAC-$wac" "$wac_sha"
chmod +x "$cache/wac-$WAC-$wac"
export NABI_TEST_WAC="$cache/wac-$WAC-$wac"

[ $# -gt 0 ] || set -- jco iwasm wasmer
for rt in "$@"; do
  case $rt in
    jco) jco ;;
    iwasm) iwasm ;;
    wasmer) wasmer ;;
    *)
      echo "wasi.sh: unknown runtime $rt (jco, iwasm, wasmer)" >&2
      exit 1
      ;;
  esac
done
targets/build.sh wasm32-wasip2 wasm32-wasip1-threads

export NABI_TEST_CASE_TIMEOUT="${NABI_TEST_CASE_TIMEOUT:-900}"
for rt in "$@"; do
  case $rt in
    jco) t=wasm32-wasip2 ;;
    *) t=wasm32-wasip1-threads ;;
  esac
  for o in "" -O2; do
    echo "== $rt $t ${o:--O0}"
    NABI_TEST_WASM_RUNNER=$PWD/targets/wasi/$rt.sh NABI_TEST_TARGET=$t NABI_TEST_OPT=$o \
      scripts/test/filter.sh -p nabic --test main -- integration:: differential:: native::abi:: native::ffi::
  done
done
