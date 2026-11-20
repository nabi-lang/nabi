#!/bin/sh
# Rewrite the archive <in> as <out>, one relocatable object in which only
# `nabi_*` stays global (ELF: also compiler_builtins' `__*`, weak), so
# libnabir.a links next to another Rust staticlib. <in> is built with fat
# LTO (dist/native.sh, targets/build.sh).
#
#   runtime/nabir/localize.sh <in.a> <out.a>   (or <in.lib> <out.lib>)
#
# Tools of the Rust toolchain: rust-lld (ships with rustc) and llvm-ar,
# llvm-nm, llvm-objcopy (rustup's llvm-tools). ELF: rust-lld -r, then
# llvm-objcopy. Mach-O (macOS hosts): cc -r. COFF: llvm-nm, llvm-objcopy.
set -eu

in=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
out=$2
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

case $(uname -s) in MINGW* | MSYS* | CYGWIN*) exe=.exe ;; *) exe="" ;; esac
host=$(rustc -vV | sed -n 's/^host: //p')
rustup component add llvm-tools > /dev/null
bin=$(rustc --print sysroot)/lib/rustlib/$host/bin

mkdir "$tmp/x"
(cd "$tmp/x" && "$bin/llvm-ar$exe" x "$in")
first=$(ls "$tmp"/x/*.o | head -n 1)
case $in in *.lib) magic=COFF ;; *) magic=$(od -An -c -N4 "$first" | tr -d ' ') ;; esac
case $magic in
  COFF)
    # No relocatable link (lld-link has no -r), and llvm-objcopy cannot
    # make a COFF symbol local. Fat LTO leaves nabir's Rust code one
    # object defining only `nabi*` globals; every other global definition
    # (compiler_builtins' members, which rustc keeps out of LTO) becomes
    # `nabir.<name>` in every member, references included, so it never
    # meets another staticlib's. The intrinsics are weak externals
    # already and keep their names. x86 spells C names `_nabi*`. No
    # CodeView either: its build info names the build machine's paths
    # (rustc's command line, the remap flags included).
    "$bin/llvm-nm$exe" --defined-only --extern-only "$tmp"/x/*.o |
      awk '$2 ~ /^[TDRB]$/ && $3 !~ /^_?nabi/ { print $3, "nabir." $3 }' | sort -u > "$tmp/names"
    for o in "$tmp"/x/*.o; do
      "$bin/llvm-objcopy$exe" --strip-debug --redefine-syms="$tmp/names" "$o"
    done
    rm -f "$out"
    "$bin/llvm-ar$exe" rcs --format=coff "$out" "$tmp"/x/*.o
    exit 0
    ;;
  177ELF)
    "$bin/rust-lld$exe" -flavor gnu -r --whole-archive "$in" -o "$tmp/nabir.o"
    # compiler_builtins' intrinsics stay global for the C library
    # (aarch64 musl's long double code calls `__netf2`), but weak: they
    # yield to another staticlib's copy. No debug info, as the release
    # profile strips nabir's own: the precompiled core / compiler_builtins
    # units of code the final link drops would sit at the DWARF tombstone.
    "$bin/llvm-objcopy$exe" --strip-debug --wildcard \
      --keep-global-symbol='nabi_*' --keep-global-symbol='__*' \
      --weaken-symbol='__*' "$tmp/nabir.o"
    format=gnu
    ;;
  *)
    # Mach-O. ld64 -r ignores archive members even under -all_load: pass
    # the objects. -S: no debug info either. The exports are listed by
    # name: ld64 refuses to export a private extern (`_nabi_tls_addr`)
    # that a `_nabi_*` pattern matches; private externs become local.
    nm -gmU "$tmp"/x/*.o | awk '$0 !~ /private external/ && $NF ~ /^_nabi_/ { print $NF }' |
      sort -u > "$tmp/exports"
    # iOS objects: the link names their platform, as ld checks it, and
    # the result the deployment target (rustup's compiler_builtins
    # objects say iOS 17.2).
    ios=${IPHONEOS_DEPLOYMENT_TARGET:-13.4}
    case $(vtool -show-build "$first" | awk '$1 == "platform" { print $2 }') in
      IOS) target="-target arm64-apple-ios$ios" platform=ios ;;
      IOSSIMULATOR) target="-target arm64-apple-ios$ios-simulator" platform=iossim ;;
      *) target="-arch $(lipo -archs "$first")" platform="" ;;
    esac
    # shellcheck disable=SC2086 # two words
    cc -r -nostdlib $target "$tmp"/x/*.o \
      -Wl,-exported_symbols_list,"$tmp/exports" -Wl,-S -o "$tmp/nabir.o" 2> "$tmp/err" ||
      {
        cat "$tmp/err" >&2
        exit 1
      }
    grep -v -e 'incompatible-sysroot' -e 'was built for newer .iOS' "$tmp/err" >&2 || true
    if [ -n "$platform" ]; then
      vtool -set-build-version "$platform" "$ios" "$ios" -replace \
        -output "$tmp/nabir.v.o" "$tmp/nabir.o"
      mv "$tmp/nabir.v.o" "$tmp/nabir.o"
    fi
    format=darwin
    ;;
esac

rm -f "$out"
"$bin/llvm-ar$exe" rcs --format=$format "$out" "$tmp/nabir.o"
