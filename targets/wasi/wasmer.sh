#!/bin/sh
# NABI_TEST_WASM_RUNNER for wasm32-wasip1-threads under wasmer (its
# WASI implements wasi-threads' `wasi::thread-spawn`).
#
#   wasmer.sh <module> [args...]
#
# WASMER names the program (default: wasmer on PATH); WASMER_DIR, as
# wasmer reads it, its home. The module gets the host's environment,
# its root as / and 8 MiB of engine stack; no code cache, as every case
# is a fresh module.
set -eu
[ "${1:-}" != --invoke ] || {
  echo "wasmer.sh: no --invoke (components only)" >&2
  exit 2
}
m=$1
shift
exec "${WASMER:-wasmer}" run -q --disable-cache --forward-host-env --volume /:/ \
  --stack-size 8388608 "$m" -- "$@"
