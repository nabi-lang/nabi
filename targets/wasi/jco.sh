#!/bin/sh
# NABI_TEST_WASM_RUNNER for wasm32-wasip2 components under jco: the
# component transpiled to JS and core modules (`jco transpile`), run on
# node with preview2-shim as `jco run` runs it.
#
#   jco.sh <component> [args...]
#   jco.sh --invoke '<export>(<arg>, ...)' <component>
#
# JCO names the program (default: jco on PATH), installed with
# @bytecodealliance/preview2-shim and preview3-shim beside it; node is
# the one on PATH. The component gets the host's environment, its root
# as / and the stack of nabi.mjs's runs; its argv[0] is its path, as
# under wasmtime. The transpiled files go to a directory under TMPDIR
# that node removes once it has loaded them. node replaces this script,
# so a timeout's kill reaches it (`jco run` leaves its own node child
# running).
#
# --invoke calls the export with WAVE arguments and prints its result
# as WAVE (invoke.mjs, typed by the component's WIT from WASM_TOOLS,
# default: wasm-tools on PATH) instead of running it, as
# `wasmtime run --invoke` does.
set -eu
call=
if [ "${1:-}" = --invoke ]; then
  call=$2
  shift 2
fi
jco=${JCO:-$(command -v jco)}
bin=$(dirname "$jco")
mods=$bin/../@bytecodealliance
[ -d "$mods" ] || mods=$bin/../lib/node_modules/@bytecodealliance
dir=$(mktemp -d "${TMPDIR:-/tmp}/jco.XXXXXX")
trap 'rm -rf "$dir"' EXIT
"$jco" transpile "$1" --name main --no-typescript --quiet -o "$dir" > /dev/null
# preview3-shim: what an async component imports.
mkdir -p "$dir/node_modules/@bytecodealliance"
for m in preview2-shim preview3-shim; do
  ln -s "$mods/$m" "$dir/node_modules/@bytecodealliance/$m"
done
echo '{"type":"module"}' > "$dir/package.json"
if [ -n "$call" ]; then
  "${WASM_TOOLS:-wasm-tools}" component wit "$1" > "$dir/main.wit"
  trap - EXIT
  exec node "$(dirname "$0")/invoke.mjs" "$dir/main.js" "$dir/main.wit" "$call"
fi
cat > "$dir/run.js" << 'EOF'
import { rmSync } from 'node:fs';
import { dirname } from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';
import { _setArgs } from '@bytecodealliance/preview2-shim/cli';
import { _setPreopens } from '@bytecodealliance/preview2-shim/filesystem';
_setArgs(process.argv.slice(2));
_setPreopens({ '/': '/' });
const mod = await import('./main.js');
rmSync(dirname(fileURLToPath(import.meta.url)), { recursive: true, force: true });
let code = 0;
try {
  await mod.run.run();
} catch (e) {
  console.error(e);
  code = 1;
}
await Promise.all([process.stdout, process.stderr].map(
  (s) => new Promise((ok, err) => s.write('', (e) => (e ? err(e) : ok())))));
// Not process.exit: its platform shutdown hangs now and then on node
// 24 too (wasi.sh), after all the output.
process.exitCode = code;
EOF
trap - EXIT
exec node --stack-size=8000 "$dir/run.js" "$@"
