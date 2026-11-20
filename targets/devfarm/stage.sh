#!/bin/sh
# Builds the end-to-end corpus for a device target without running it
# (integration.rs, NABI_TEST_STAGE): <out>/bin/<case> and
# <out>/cases/<case>.json, what targets/android/apk.sh and
# targets/ios/xctest.sh pack.
#
#   targets/devfarm/stage.sh <triple> <out>
#
# NABI_TEST_OPT, NABI_TEST_FILTER and NABI_TEST_JOBS pass through. Build
# failures and the cases checked at build time (expect-error, lints) fail
# here, as in any corpus run; the cases that built are staged all the same
# and the exit status is the corpus run's.
set -eu

triple=${1:?triple}
out=${2:?out dir}
root=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)
rm -rf "${out:?}/bin" "${out:?}/cases" "${out:?}/tmp"
mkdir -p "$out/tmp"
cd "$root"
st=0
NABI_TEST_TARGET=$triple NABI_TEST_STAGE=$out NABI_TEST_TMP=$out/tmp \
  NABI_TEST_FILTER=${NABI_TEST_FILTER:-} \
  cargo test -p nabic --test main -- integration::end_to_end_cases || st=$?
rm -rf "$out/tmp"
echo "$out: $(ls "$out/cases" | wc -l | tr -d ' ') cases staged"
[ "$st" = 0 ] || echo "stage.sh: the corpus failed on the host (above)" >&2
exit "$st"
