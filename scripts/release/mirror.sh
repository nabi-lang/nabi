#!/bin/sh
# Copy a published release to the download mirror (dist.nabi-lang.org,
# a Cloudflare R2 bucket) in GitHub releases' layout, so NABI_DIST can
# point there:
#   download/v<version>/<every asset>
#   latest/download/nabi-version   (the latest release only, written last)
#
#   scripts/release/mirror.sh <version>
#
# Environment: GH_TOKEN, R2_ACCOUNT_ID, AWS_ACCESS_KEY_ID and
# AWS_SECRET_ACCESS_KEY (an R2 API token with write access to the bucket),
# NABI_DIST_BUCKET (default nabi-dist), NABI_DIST_URL (default
# https://dist.nabi-lang.org), checked after the upload. Uploading the
# same release again rewrites the same objects.
set -eu

die() {
  printf 'release/mirror.sh: %s\n' "$*" >&2
  exit 1
}

[ $# -eq 1 ] || die "usage: scripts/release/mirror.sh <version>"
version=$1
bucket=${NABI_DIST_BUCKET:-nabi-dist}
url=${NABI_DIST_URL:-https://dist.nabi-lang.org}
[ -n "${R2_ACCOUNT_ID:-}" ] || die "R2_ACCOUNT_ID is not set"
endpoint="https://$R2_ACCOUNT_ID.r2.cloudflarestorage.com"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

gh release download "v$version" --dir "$tmp/v$version"
[ -s "$tmp/v$version/SHA256SUMS" ] || die "v$version has no SHA256SUMS asset"
# Only the release GitHub calls latest moves latest/: mirroring an older
# one again must not roll it back.
latest=$(gh release view --json tagName --jq .tagName)

s3() {
  aws s3 "$@" --endpoint-url "$endpoint" --region auto --only-show-errors
}

# A version's files never change: cache them for a year.
s3 cp --recursive "$tmp/v$version" "s3://$bucket/download/v$version/" \
  --cache-control "public, max-age=31536000, immutable"

# Clients check every archive against SHA256SUMS, so compare it as served.
curl -fsSL "$url/download/v$version/SHA256SUMS" | cmp -s - "$tmp/v$version/SHA256SUMS" ||
  die "$url/download/v$version/SHA256SUMS differs from the release's"

if [ "$latest" != "v$version" ]; then
  echo "release/mirror.sh: v$version mirrored (latest is $latest: latest/ unchanged)"
  exit 0
fi
s3 cp "$tmp/v$version/nabi-version" "s3://$bucket/latest/download/nabi-version" \
  --cache-control "no-cache" --content-type "text/plain"
echo "release/mirror.sh: v$version mirrored, latest is $(curl -fsSL -H 'Cache-Control: no-cache' "$url/latest/download/nabi-version")"
