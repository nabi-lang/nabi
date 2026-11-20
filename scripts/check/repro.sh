#!/bin/sh
# Build this host's release archives twice and compare them byte for byte.
#
#   scripts/check/repro.sh [--clean] [<dir>]
#
# Both runs are dist/native.sh of the same commit into <dir>/a and <dir>/b
# (default: a temporary directory). The second reuses the cargo target
# directory, so it checks the archiving (staging, tar, gzip, xz, zip);
# --clean gives it a fresh CARGO_TARGET_DIR, so it also checks that the
# compiler, the runtime and the target runtimes build the same. On a
# difference it lists the entries that differ. Environment as
# dist/native.sh (NABI_DIST_PACKAGES=0 leaves out the packages, which
# otherwise come from the network and may change between the runs).
set -eu

cd "$(dirname "$0")/../.."
clean=""
dir=""
for a; do
  case $a in
    --clean) clean=1 ;;
    -*)
      echo "usage: scripts/check/repro.sh [--clean] [<dir>]" >&2
      exit 2
      ;;
    *) dir=$a ;;
  esac
done
[ -n "$dir" ] || dir=$(mktemp -d "${TMPDIR:-/tmp}/nabi-repro.XXXXXX")
mkdir -p "$dir"
dir=$(cd "$dir" && pwd)

sha256_of() {
  { sha256sum "$1" 2> /dev/null || shasum -a 256 "$1"; } | awk '{ print $1 }'
}

scripts/dist/native.sh --out "$dir/a" > "$dir/a.log" 2>&1 || {
  tail -n 30 "$dir/a.log" >&2
  echo "check/repro.sh: the first build failed ($dir/a.log)" >&2
  exit 1
}
if [ -n "$clean" ]; then
  CARGO_TARGET_DIR=$dir/target scripts/dist/native.sh --out "$dir/b" > "$dir/b.log" 2>&1
else
  scripts/dist/native.sh --out "$dir/b" > "$dir/b.log" 2>&1
fi || {
  tail -n 30 "$dir/b.log" >&2
  echo "check/repro.sh: the second build failed ($dir/b.log)" >&2
  exit 1
}

bad=0
for f in "$dir"/a/nabi-*.tar.gz "$dir"/a/nabi-*.tar.xz "$dir"/a/nabi-*.zip; do
  [ -f "$f" ] || continue
  n=${f##*/}
  x=$(sha256_of "$f")
  y=$(sha256_of "$dir/b/$n")
  if [ "$x" = "$y" ]; then
    echo "same      $n $x"
    continue
  fi
  echo "differs   $n ($x, $y)"
  bad=1
  case $n in
    *.zip) continue ;;
  esac
  mkdir -p "$dir/x/a" "$dir/x/b"
  tar -xf "$f" -C "$dir/x/a"
  tar -xf "$dir/b/$n" -C "$dir/x/b"
  (cd "$dir/x" && diff -rq a b) || true
  tar -tvf "$f" > "$dir/x/a.list"
  tar -tvf "$dir/b/$n" > "$dir/x/b.list"
  diff "$dir/x/a.list" "$dir/x/b.list" | head -n 20 || true
  rm -rf "$dir/x"
done
[ "$bad" -eq 0 ] || {
  echo "check/repro.sh: the two builds differ; the builds are in $dir" >&2
  exit 1
}
echo "check/repro.sh: both builds give the same archives ($dir)"
