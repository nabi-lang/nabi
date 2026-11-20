#!/bin/sh
# NABI_CC of the iOS corpus (xctest.sh): links a case program as a dylib
# the XCTest harness loads, with shim.c's object (NABI_IOS_SHIM, built
# for the same SDK) taking exit, abort, atexit and mmap. Only `main` and
# the shim's hooks are exported. nabic's link arguments pass through.
set -eu
sdk=iphoneos
out=
prev=
for a in "$@"; do
  case $a in --target=*-simulator) sdk=iphonesimulator ;; esac
  [ "$prev" = -o ] && out=$a
  prev=$a
done
[ -n "$out" ] || {
  echo "cc.sh: no -o" >&2
  exit 2
}
shim=${NABI_IOS_SHIM:?set NABI_IOS_SHIM to the directory of shim-<sdk>.o}/shim-$sdk.o
# A release link exports nothing; the dylib keeps its two exports.
for a in "$@"; do
  shift
  case $a in -Wl,-x,-no_exported_symbols) a=-Wl,-x ;; esac
  set -- "$@" "$a"
done
exec xcrun -sdk "$sdk" clang "$@" "$shim" -dynamiclib \
  -install_name "@rpath/$(basename "$out").dylib" \
  -Wl,-exported_symbol,_main -Wl,-exported_symbol,_nabi_corpus_hooks
