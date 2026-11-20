#!/bin/sh
# The release's signed checksums.
#
#   scripts/release/sign.sh sums <dir>
#       write <dir>/SHA256SUMS: `<sha256>  <name>` for every archive
#       (nabi-*.tar.gz, .tar.xz, .zip) and the SBOM (nabi-*.cdx.json),
#       sorted, the form `sha256sum -c` reads
#   scripts/release/sign.sh sign <dir> <secret-key-file>
#       write <dir>/SHA256SUMS.minisig with minisign, trusted comment
#       "nabi <version> SHA256SUMS"; Ed25519 is deterministic, so a rerun
#       writes the same file
#   scripts/release/sign.sh verify <dir> [<public-key-file>]
#       the signature verifies with the key (default keys/release.pub) and
#       every file SHA256SUMS lists is in <dir> with that SHA-256
#
# sign and verify need minisign (.github/actions/setup-minisign in CI). The
# release key has no password; release.yml writes it from the
# NABI_MINISIGN_KEY secret to a file only the job reads (docs/releasing.md).
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)

die() {
  echo "release/sign.sh: $*" >&2
  exit 1
}

sha256_of() {
  { sha256sum "$1" 2> /dev/null || shasum -a 256 "$1"; } | awk '{ print $1 }'
}

need_minisign() {
  command -v minisign > /dev/null 2>&1 ||
    die "minisign is needed (https://jedisct1.github.io/minisign/; brew install minisign, apt install minisign)"
}

sums() {
  dir=$1
  [ -f "$dir/nabi-version" ] || die "no $dir/nabi-version; build the archives into $dir first"
  n=0
  : > "$dir/SHA256SUMS.new"
  for f in "$dir"/nabi-*.tar.gz "$dir"/nabi-*.tar.xz "$dir"/nabi-*.zip "$dir"/nabi-*.cdx.json; do
    [ -f "$f" ] || continue
    printf '%s  %s\n' "$(sha256_of "$f")" "${f##*/}" >> "$dir/SHA256SUMS.new"
    n=$((n + 1))
  done
  [ "$n" -gt 0 ] || die "no archives in $dir"
  LC_ALL=C sort -k 2 "$dir/SHA256SUMS.new" > "$dir/SHA256SUMS"
  rm "$dir/SHA256SUMS.new"
  echo "release/sign.sh: $dir/SHA256SUMS lists $n files"
}

sign() {
  dir=$1 key=$2
  need_minisign
  [ -f "$dir/SHA256SUMS" ] || die "no $dir/SHA256SUMS; run: scripts/release/sign.sh sums $dir"
  [ -s "$key" ] || die "the secret key file $key is empty (is the NABI_MINISIGN_KEY secret set?)"
  version=$(cat "$dir/nabi-version")
  rm -f "$dir/SHA256SUMS.minisig"
  minisign -S -s "$key" -m "$dir/SHA256SUMS" -x "$dir/SHA256SUMS.minisig" \
    -c "signature from the nabi release key" -t "nabi $version SHA256SUMS" < /dev/null ||
    die "minisign could not sign; the key must be a minisign secret key without a password"
}

verify() {
  dir=$1 pub=${2:-$root/keys/release.pub}
  need_minisign
  for f in SHA256SUMS SHA256SUMS.minisig; do
    [ -f "$dir/$f" ] || die "no $dir/$f; sign the set: scripts/release/sign.sh sums|sign $dir ..."
  done
  version=$(cat "$dir/nabi-version")
  out=$(minisign -V -p "$pub" -m "$dir/SHA256SUMS" -x "$dir/SHA256SUMS.minisig" 2>&1) ||
    die "SHA256SUMS.minisig does not verify with $pub: $out"
  printf '%s\n' "$out" | grep -qxF "Trusted comment: nabi $version SHA256SUMS" ||
    die "SHA256SUMS.minisig is not for nabi $version: $out"
  bad=0
  while read -r want name; do
    if [ ! -f "$dir/$name" ]; then
      echo "missing   $name (in SHA256SUMS)" >&2
      bad=1
    elif [ "$(sha256_of "$dir/$name")" != "$want" ]; then
      echo "bad sha   $name (SHA256SUMS)" >&2
      bad=1
    fi
  done < "$dir/SHA256SUMS"
  [ "$bad" -eq 0 ] || die "$dir does not match its signed SHA256SUMS"
  echo "release/sign.sh: SHA256SUMS is signed by $(sed -n 's/^untrusted comment: //p' "$pub") and matches $dir"
}

cmd=${1:-}
[ $# -eq 0 ] || shift
case $cmd in
  sums)
    [ $# -eq 1 ] || die "usage: sums <dir>"
    sums "$1"
    ;;
  sign)
    [ $# -eq 2 ] || die "usage: sign <dir> <secret-key-file>"
    sign "$1" "$2"
    ;;
  verify)
    [ $# -ge 1 ] && [ $# -le 2 ] || die "usage: verify <dir> [<public-key-file>]"
    verify "$@"
    ;;
  *) die "usage: release/sign.sh sums|sign|verify ..." ;;
esac
