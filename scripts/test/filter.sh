#!/bin/sh
# cargo test whose name filters must each match a test: libtest runs zero
# tests and exits 0 when a filter matches nothing, so a renamed test or
# module silently drops out of CI. This fails then instead.
#
#   scripts/test/filter.sh -p nabic --test main -- cli::targets::x --exact
set -eu

# The filters: the words after `--` that are neither options nor their values.
filters() {
  seen=false skip=false
  for a in "$@"; do
    if [ "$seen" = false ]; then
      [ "$a" = -- ] && seen=true
      continue
    fi
    if [ "$skip" = true ]; then
      skip=false
      continue
    fi
    case $a in
      --skip | --test-threads | --color | --format | --logfile | -Z) skip=true ;;
      -*) ;;
      *) printf '%s\n' "$a" ;;
    esac
  done
}
filters=$(filters "$@")
exact=false
for a in "$@"; do [ "$a" = --exact ] && exact=true; done

out=$(mktemp)
st=$(mktemp)
trap 'rm -f "$out" "$st"' EXIT

# Streamed: a corpus run takes hours. sh has no pipefail.
{
  status=0
  cargo test "$@" || status=$?
  echo "$status" > "$st"
} | tee "$out"
status=$(cat "$st")
[ "$status" -eq 0 ] || exit "$status"

# libtest prints `test <name> ... <result>` for each test it runs.
names=$(sed -n 's/^test \([^ ]*\) \.\.\. .*/\1/p' "$out")
if [ -z "$names" ]; then
  echo "error: no test ran: cargo test $*" >&2
  echo "List the names with \`cargo test <args> -- --list\`." >&2
  exit 1
fi
bad=0
while IFS= read -r f; do
  [ -n "$f" ] || continue
  if [ "$exact" = true ]; then
    printf '%s\n' "$names" | grep -qxF -- "$f" && continue
  else
    printf '%s\n' "$names" | grep -qF -- "$f" && continue
  fi
  echo "error: the filter \`$f\` matched no test; list the names with \`cargo test <args> -- --list\`" >&2
  bad=1
done << EOF
$filters
EOF
[ "$bad" -eq 0 ] || exit 1
echo "test/filter.sh: $(printf '%s\n' "$names" | wc -l | tr -d ' ') test(s) ran"
