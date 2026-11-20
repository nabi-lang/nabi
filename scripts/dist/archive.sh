#!/bin/sh
# Archive a staged toolchain reproducibly:
#
#   scripts/dist/archive.sh <out> <name>
#
# packs <out>/<name> into <out>/<name>.tar.gz and .tar.xz (Windows: .zip,
# with scripts/dist/zip.py). Every entry has the time SOURCE_DATE_EPOCH
# (required), user and group 0, mode 755 (directories, and files the
# owner may run) or 644, no extended attributes, ACLs or macOS metadata,
# in sorted order; gzip writes no name or time. dist/native.sh runs it;
# scripts/check/repro.sh checks it.
set -eu

[ $# -eq 2 ] || {
  echo "usage: scripts/dist/archive.sh <out> <name>" >&2
  exit 2
}
out=$1 name=$2
stage=$out/$name
: "${SOURCE_DATE_EPOCH:?dist/archive.sh: set SOURCE_DATE_EPOCH (git log -1 --format=%ct)}"
cd "$(dirname "$0")/../.."
# This machine's form of path $1 (Git Bash: C:\..., as Python sees it).
native() {
  case $(uname -s) in
    MINGW* | MSYS* | CYGWIN*) cygpath -w "$1" ;;
    *) printf '%s\n' "$1" ;;
  esac
}

# Reproducible entries: the commit's time, user and group 0, rwx for the
# owner, r-x for the rest where the owner has x, nothing else.
find "$stage" -type d -exec chmod 755 {} +
find "$stage" -type f -perm -u+x -exec chmod 755 {} +
find "$stage" -type f ! -perm -u+x -exec chmod 644 {} +
stamp=$(date -u -d "@$SOURCE_DATE_EPOCH" +%Y%m%d%H%M.%S 2> /dev/null ||
  date -u -r "$SOURCE_DATE_EPOCH" +%Y%m%d%H%M.%S)
find "$stage" -exec env TZ=UTC touch -h -t "$stamp" {} +
(cd "$out" && find "$name" | LC_ALL=C sort) > "$out/$name.list"
case $(uname -s) in
  # Git Bash's tar writes no zip; Python's zipfile does (scripts/dist/zip.py).
  MINGW* | MSYS* | CYGWIN*)
    zip_py=$(native "$(pwd)/scripts/dist/zip.py")
    (cd "$out" && "$(command -v python3 || command -v python)" -I "$zip_py" . "$name" "$name.zip")
    ;;
  *)
    if tar --version 2> /dev/null | grep -q 'GNU tar'; then
      set -- --format=gnu --owner=0 --group=0 --numeric-owner --mtime="@$SOURCE_DATE_EPOCH" \
        --no-recursion --no-xattrs --no-acls
    else
      # bsdtar (macOS, FreeBSD).
      set -- --uid 0 --gid 0 --uname "" --gname "" --numeric-owner -n --no-xattrs --no-acls --no-fflags
      [ "$(uname -s)" != Darwin ] || set -- "$@" --no-mac-metadata
    fi
    tar -C "$out" "$@" -cf "$out/$name.tar" -T "$out/$name.list"
    # nabi toolchain install reads the .tar.gz; install.sh prefers the
    # .tar.xz. gzip -n: no name or time in the header.
    gzip -n -9 < "$out/$name.tar" > "$out/$name.tar.gz"
    xz -9 -T0 < "$out/$name.tar" > "$out/$name.tar.xz"
    rm "$out/$name.tar"
    ;;
esac
rm "$out/$name.list"
