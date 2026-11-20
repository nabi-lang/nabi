#!/bin/sh
# Fail if nabi doc's copy of the web kit (compiler/nabic_tools/kit/)
# differs from the web kit and nabi-lang.org it comes from; --update
# copies them over. NABI_WEB / NABI_SITE name the checkouts (default:
# ../web and ../nabi-lang.org beside this repository).
set -eu

cd "$(dirname "$0")/../.."
web=${NABI_WEB:-../web}
site=${NABI_SITE:-../nabi-lang.org}
kit=compiler/nabic_tools/kit

# copy name, source
pairs="nabi.css $web/css/nabi.css
theme.js $web/js/theme.js
keys.js $web/js/keys.js
fonts/Geist-Variable.woff2 $web/assets/fonts/Geist-Variable.woff2
fonts/GeistMono-Variable.woff2 $web/assets/fonts/GeistMono-Variable.woff2
fonts/LICENSE-Geist.txt $web/assets/fonts/LICENSE-Geist.txt
style.css $site/static/style.css"

for d in "$web" "$site"; do
  [ -d "$d" ] || {
    echo "check/kit.sh: no checkout at $d; set NABI_WEB / NABI_SITE" >&2
    exit 1
  }
done

bad=0
while read -r name src; do
  if ! cmp -s "$kit/$name" "$src"; then
    if [ "${1:-}" = --update ]; then
      cp "$src" "$kit/$name"
      echo "updated $kit/$name"
    else
      echo "$kit/$name differs from $src" >&2
      bad=1
    fi
  fi
done << END
$pairs
END

if [ "$bad" = 1 ]; then
  echo "check/kit.sh: run 'scripts/check/kit.sh --update' (build the kit's scripts first: npm run build in $web)" >&2
  exit 1
fi
echo "nabi doc's kit matches $web and $site"
