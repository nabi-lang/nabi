#!/bin/sh
# NABI_TEST_WASM_RUNNER for wasm32-wasip1-threads under WAMR's iwasm
# (a release build: wasi-threads, shared memory, libc-wasi).
#
#   iwasm.sh <module> [args...]
#
# IWASM names the program (default: iwasm on PATH). The module gets
# its root as /, 8 MiB of engine stack, room for the corpus's threads
# (iwasm's default cluster is 4) and of the host's environment what a
# case reads: iwasm's command line takes at most 8 variables, so only
# PATH, HOME, TZ, TMPDIR and the NABI* ones (not the harness's
# NABI_TEST_* nor nabic's build-time NABI_COMMIT_INFO and NABI_HOST).
# More than 8 run under IWASM_WIDE, a build of the same release with
# room for 64 (interpreter only; scripts/test/wasi.sh makes it), or
# fail without one.
#
# IWASM_MODE picks the engine (default --llvm-jit; --interp also
# passes). Not the release build's default, its fast JIT (also the
# first tier of --multi-tier-jit): 2.4.6's x86-64 fast JIT clobbers an
# i32 it holds in eax across an i64.atomic.rmw.and/or/xor (the
# cmpxchg loop's rax), which the GC's mark bitmap does, so GC cases
# trap or hang. The build compiles each function with the fast JIT too
# and logs (to stdout) those it cannot, SIMD ones: -v=0 keeps the log
# to fatal errors; traps are still reported.
set -euf
iwasm=${IWASM:-iwasm}
[ "${1:-}" != --invoke ] || {
  echo "iwasm.sh: no --invoke (components only)" >&2
  exit 2
}
n=$#
nl='
'
# A variable whose value spans lines is left out: `env` cannot tell
# its lines from the next variable's.
IFS=$nl
k=0
for kv in $(env | grep -E '^(PATH|HOME|TZ|TMPDIR|NABI[A-Z0-9_]*)=' |
  grep -v -E '^(NABI_TEST_[A-Z0-9_]*|NABI_COMMIT_INFO|NABI_HOST)='); do
  set -- "$@" "--env=$kv"
  k=$((k + 1))
done
unset IFS
mode=${IWASM_MODE:---llvm-jit}
if [ "$k" -gt 8 ]; then
  [ -n "${IWASM_WIDE:-}" ] || {
    echo "iwasm.sh: $k variables to forward, iwasm takes at most 8 (no IWASM_WIDE)" >&2
    exit 2
  }
  iwasm=$IWASM_WIDE mode=--interp
fi
# Rotate the module and its arguments behind the flags.
while [ "$n" -gt 0 ]; do
  set -- "$@" "$1"
  shift
  n=$((n - 1))
done
exec "$iwasm" "$mode" -v=0 --max-threads=256 --stack-size=8388608 --dir=/ "$@"
