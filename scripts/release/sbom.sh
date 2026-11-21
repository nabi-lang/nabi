#!/bin/sh
# The release's software bill of materials, CycloneDX 1.6 JSON:
#
#   scripts/release/sbom.sh <version> > nabi-<version>.cdx.json
#
# One document for every toolchain archive: the crates Cargo.lock pulls
# from a registry (name, version, its checksum) and each third-party part
# THIRD-PARTY-NOTICES lists (name, the version its title names, where it
# ships, its license as written there). The workspace's own crates are the
# toolchain itself. Reads only the checkout: no network, no cargo. The
# timestamp is SOURCE_DATE_EPOCH (default: the HEAD commit's time), so
# the same commit always gives the same file. Needs jq.
set -eu

[ $# -eq 1 ] || {
  echo "usage: scripts/release/sbom.sh <version>" >&2
  exit 2
}
version=$1
root=$(cd "$(dirname "$0")/../.." && pwd)
epoch=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --format=%ct)}
stamp=$(jq -nr --argjson e "$epoch" '$e | todate')
# serialNumber, which attest-sbom requires of CycloneDX: a UUID derived
# from the version and the commit, so a rerun gives the same file.
commit=$(git -C "$root" rev-parse HEAD)
h=$(printf 'nabi %s %s' "$version" "$commit" | { sha256sum 2> /dev/null || shasum -a 256; } | cut -c1-32)
serial=$(printf '%s' "$h" | sed -E 's/^(.{8})(.{4}).(.{3})(.)(.{3})(.{12})$/urn:uuid:\1-\2-5\3-\4\5-\6/')
serial=$(printf '%s' "$serial" | awk -F- '{ v = substr($4, 1, 1); n = index("0123456789abcdef", v) - 1; printf "%s-%s-%s-%s%s-%s", $1, $2, $3, substr("89ab", n % 4 + 1, 1), substr($4, 2), $5 }')

# Registry crates: "name<TAB>version<TAB>source<TAB>checksum".
crates() {
  awk '
    function flush() { if (src ~ /^"?registry\+/) printf "%s\t%s\t%s\t%s\n", name, ver, src, sum }
    /^\[\[package\]\]/ { flush(); name = ver = src = sum = ""; next }
    /^name = / { name = $3 }
    /^version = / { ver = $3 }
    /^source = / { src = $3 }
    /^checksum = / { sum = $3 }
    END { flush() }
  ' "$root/Cargo.lock" | tr -d '"'
}

# THIRD-PARTY-NOTICES entries: an unindented title line, then indented
# "Where:", "License:" and "Text:" fields that may wrap.
# "title<TAB>where<TAB>license".
notices() {
  awk '
    function flush() { if (title != "") printf "%s\t%s\t%s\n", title, f["Where"], f["License"]; title = "" }
    /^[^ ]/ { flush(); last = $0; next }
    /^  [A-Z][a-z]*: / && last != "" {
      title = last; last = ""
    }
    /^  [A-Z][a-z]*: / && title != "" {
      key = $1; sub(/:$/, "", key); val = $0; sub(/^  [A-Z][a-z]*: */, "", val)
      f[key] = val; cur = key; next
    }
    /^   +[^ ]/ && title != "" { val = $0; sub(/^ +/, "", val); f[cur] = f[cur] " " val; next }
    /^$/ { flush(); last = "" }
    END { flush() }
  ' "$root/THIRD-PARTY-NOTICES"
}

{
  crates | jq -Rc 'split("\t") | {
    type: "library", "bom-ref": "pkg:cargo/\(.[0])@\(.[1])", name: .[0], version: .[1],
    purl: "pkg:cargo/\(.[0])@\(.[1])", hashes: [{alg: "SHA-256", content: .[3]}],
    externalReferences: [{type: "distribution", url: "https://crates.io/crates/\(.[0])/\(.[1])"}]
  }'
  notices | jq -Rc 'split("\t") | (.[0] | [scan("[0-9]+\\.[0-9]+(?:\\.[0-9]+)?")]) as $v | {
    type: "library", "bom-ref": "notice:\(.[0])", name: .[0],
    description: "Where: \(.[1])",
    licenses: [{license: {name: .[2]}}]
  } + (if ($v | length) == 1 then {version: $v[0]} else {} end)'
} | jq -s --arg v "$version" --arg t "$stamp" --arg s "$serial" '
  sort_by(.["bom-ref"]) as $c |
  {
    bomFormat: "CycloneDX",
    specVersion: "1.6",
    serialNumber: $s,
    version: 1,
    metadata: {
      timestamp: $t,
      tools: {components: [{type: "application", name: "scripts/release/sbom.sh"}]},
      component: {
        type: "application", "bom-ref": "pkg:github/nabi-lang/nabi@v\($v)",
        name: "nabi", version: $v, purl: "pkg:github/nabi-lang/nabi@v\($v)",
        licenses: [{expression: "MIT OR Apache-2.0"}],
        externalReferences: [{type: "vcs", url: "https://github.com/nabi-lang/nabi"}]
      }
    },
    components: $c,
    dependencies: [{ref: "pkg:github/nabi-lang/nabi@v\($v)", dependsOn: [$c[] | .["bom-ref"]]}]
  }'
