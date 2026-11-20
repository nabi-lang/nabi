#!/usr/bin/env python3
"""Benchmark runner: see bench/README.md.

Builds bench/*.nabi at -O2 (release nabic + runtime) and bench/c/*.c with
clang -O2, runs each best-of-N under /usr/bin/time, records instructions
retired, user CPU and peak RSS in bench/history.jsonl keyed by the git
commit, and compares against the previous record and against the LLVM
backend's numbers in bench/llvm.json. Exit 1 when a benchmark regresses
past the threshold (instructions, or user time where instructions are not
available), or when a --compile, --scaling or --pauses result misses its
budget in bench/budgets.json.
"""

import argparse
import datetime
import json
import math
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BENCH = os.path.join(ROOT, "bench")
HISTORY = os.path.join(BENCH, "history.jsonl")
LLVM = os.path.join(BENCH, "llvm.json")
BUDGETS = os.path.join(BENCH, "budgets.json")
MAC = platform.system() == "Darwin"
HOST = f"{platform.system()}-{platform.machine()}"
HELLO = 'fn main() {\n    println("hello");\n}\n'
SCALING = (1, 2, 4, 8)
PAUSE = os.path.join(BENCH, "pause")
# `--pauses`: name -> (probe, arguments). Each run logs its collections
# (`NABIR_GC_LOG=1`); see `pause_metrics`.
PAUSES = {
    "pause/live-100": ("live", ["ptr", "100", "4096", "1"]),
    "pause/live-1000": ("live", ["ptr", "1000", "4096", "1"]),
    "pause/cache-64": ("cache", ["64", "8192", "1"]),
    "pause/frame": ("frame", ["1200", "200", "4096", "busy"]),
    "pause/ttsp-count": ("ttsp", ["count", "2048"]),
    "pause/ttsp-find": ("ttsp", ["find", "2048"]),
    "pause/ttsp-grow": ("ttsp", ["grow", "2048"]),
    "pause/ttsp-hand": ("ttsp", ["hand", "2048"]),
    "pause/satb": ("satb", ["8", "4096"]),
}
# At least this many runs per probe: the gate takes the median.
PAUSE_RUNS = 5
GC_LINE = re.compile(r"nabi gc: #\d+ (.+?): (\d+)\.(\d+) ms pause, \d+ objects marked, \d+ -> (\d+) KiB; "
                     r"ttsp (\d+) us")


def sh(args, **kw):
    return subprocess.run(args, check=True, **kw)


def git(*args):
    r = subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True)
    return r.stdout.strip() if r.returncode == 0 else ""


def target_dir(a):
    t = a.target_dir or os.environ.get("CARGO_TARGET_DIR") or os.path.join(ROOT, "target")
    return os.path.abspath(t)


def build_toolchain(a):
    """The release nabic, built in the target directory."""
    env = dict(os.environ, CARGO_TARGET_DIR=target_dir(a))
    sh(["cargo", "build", "--release", "-q", "-p", "nabir", "-p", "nabic"], cwd=ROOT, env=env)
    return os.path.join(target_dir(a), "release", "nabic")


def build(a, nabic, names):
    """{name: exe} for the nabi and C programs whose name matches."""
    out = os.path.join(target_dir(a), "bench")
    os.makedirs(out, exist_ok=True)
    exes = {}
    for f in sorted(os.listdir(BENCH)):
        name = f[:-5]
        if f.endswith(".nabi") and names(name):
            exe = os.path.join(out, name)
            r = subprocess.run([nabic, "-q", "build", os.path.join(BENCH, f), "-O2", "-o", exe], cwd=ROOT)
            if r.returncode != 0:
                print(f"BUILD FAILED: {f}", file=sys.stderr)
                continue
            exes[name] = exe
    cdir = os.path.join(BENCH, "c")
    for f in sorted(os.listdir(cdir)):
        name = f[:-2]
        if f.endswith(".c") and names(name):
            exe = os.path.join(out, "c_" + name)
            sh(["clang", "-O2", "-o", exe, os.path.join(cdir, f), "-lm"])
            exes["c/" + name] = exe
    return exes


def perf():
    """`perf stat` counts this user's instructions (Linux, where allowed)."""
    if MAC or not shutil.which("perf"):
        return False
    r = subprocess.run(["perf", "stat", "-x,", "-e", "instructions:u", "true"], capture_output=True, text=True)
    return r.returncode == 0 and re.search(r"^\d+,", r.stderr, re.M) is not None


def measure(cmd, env=None, cwd=None):
    """(stdout, {insns, user, rss_kb, wall}) of one run of `cmd`."""
    if MAC:
        full = ["/usr/bin/time", "-l", *cmd]
    else:
        full = ["/usr/bin/time", "-f", "%U user\n%M maxrss", *cmd]
        if PERF:
            full = ["perf", "stat", "-x,", "-e", "instructions:u", "--", *full]
    t = time.perf_counter()
    r = subprocess.run(full, capture_output=True, text=True, env=env, cwd=cwd)
    wall = time.perf_counter() - t
    if r.returncode != 0:
        raise SystemExit(f"{' '.join(cmd)} exited {r.returncode}:\n{r.stderr[-2000:]}")
    err = r.stderr
    stats = {"wall": round(wall, 4)}
    m = re.search(r"([\d.]+) user", err)
    stats["user"] = float(m.group(1)) if m else None
    m = re.search(r"(\d+)\s+instructions retired", err) or re.search(r"^(\d+),[^,]*,instructions", err, re.M)
    stats["insns"] = int(m.group(1)) if m else None
    m = re.search(r"(\d+)\s+maximum resident set size", err) or re.search(r"(\d+) maxrss", err)
    if m:
        # macOS reports bytes, GNU time KiB.
        stats["rss_kb"] = int(m.group(1)) // (1024 if MAC else 1)
    return r.stdout, stats


def keep_best(into, stats):
    for k, v in stats.items():
        if v is not None and (k not in into or v < into[k]):
            into[k] = v


def best(cmd, runs):
    out, b = None, {}
    for _ in range(runs):
        out, s = measure(cmd)
        keep_best(b, s)
    return out, b


def compile_times(nabic, runs):
    """A hello built by a fresh copy of nabic (whose cache sits beside it)
    at -O0 and -O2: first with the cache empty (`-cold`), then after an
    edit, the standard library code taken from the cache. Best of `runs`."""
    out = {}
    rt = os.path.join(os.path.dirname(nabic), "libnabir.a")
    for level in ("0", "2"):
        cold, warm = {}, {}
        for _ in range(runs):
            with tempfile.TemporaryDirectory(prefix="nabi-bench-") as tmp:
                bin_dir = os.path.join(tmp, "bin")
                os.makedirs(bin_dir)
                shutil.copy2(nabic, bin_dir)
                if os.path.exists(rt):
                    shutil.copy2(rt, bin_dir)
                src = os.path.join(tmp, "hello.nabi")
                env = dict(os.environ, NABI_INCREMENTAL="0", NABI_TARGET_DIR=os.path.join(tmp, "target"))
                cmd = [os.path.join(bin_dir, "nabic"), "-q", "build", src, f"-O{level}",
                       "-o", os.path.join(tmp, "hello")]
                for n, into in enumerate((cold, warm)):
                    # An edit, so the second build is not `Fresh`.
                    with open(src, "w") as f:
                        f.write(HELLO + "// " * n + "\n")
                    keep_best(into, measure(cmd, env=env, cwd=tmp)[1])
        out[f"compile/hello-cold-O{level}"] = cold
        out[f"compile/hello-O{level}"] = warm
    return out


def scaling(a, nabic, runs):
    """bench/threads.nabi's work split over 1, 2, 4, 8 threads, best of `runs`."""
    exe = os.path.join(target_dir(a), "bench", "threads")
    os.makedirs(os.path.dirname(exe), exist_ok=True)
    sh([nabic, "-q", "build", os.path.join(BENCH, "threads.nabi"), "-O2", "-o", exe], cwd=ROOT)
    out = {}
    for kind in ("cpu", "alloc"):
        for n in SCALING:
            out[f"threads/{kind}-{n}"] = best([exe, str(n), kind], runs)[1]
    return out


def percentile(xs, p):
    """The `p`th percentile of `xs` (nearest rank), 0 when empty."""
    if not xs:
        return 0.0
    xs = sorted(xs)
    return xs[min(len(xs) - 1, max(0, math.ceil(len(xs) * p / 100) - 1))]


def pause_metrics(err, out):
    """One `--pauses` run's numbers, in ms: the longest pause and time to
    safepoint, steady minor p50 / p99 (once the heap first reached 90% of
    its largest live size: the build-up left out), the count of
    stop-the-world full collections, and from the program's own output
    the frame time p99.9 (`frame <ns>` lines) or the longest first store
    after a collection (`first <ns>`)."""
    events = []
    for line in err.splitlines():
        m = GC_LINE.search(line)
        if m:
            events.append((m.group(1), int(m.group(2)) + int(m.group(3)) / 1000,
                           int(m.group(4)), int(m.group(5)) / 1000))
    top = max((e[2] for e in events), default=0)
    start = next((i for i, e in enumerate(events) if e[2] >= top * 0.9), 0)
    minors = [e[1] for e in events[start:] if e[0] == "minor"]
    r = {
        "max": max((e[1] for e in events), default=0.0),
        "ttsp_max": max((e[3] for e in events), default=0.0),
        "minor_p50": percentile(minors, 50),
        "minor_p99": percentile(minors, 99),
        "full_stw": sum(1 for e in events if e[0] == "full"),
    }
    frames = [int(l.split()[1]) / 1e6 for l in out.splitlines() if l.startswith("frame ")]
    if frames:
        r["frame_p999"] = percentile(frames, 99.9)
    firsts = [int(l.split()[1]) / 1e6 for l in out.splitlines() if l.startswith("first ")]
    if firsts:
        r["first_store_max"] = max(firsts)
    return r


def pauses(a, nabic, runs):
    """The pause probes of bench/pause, each run `runs` times (at least
    PAUSE_RUNS); per metric the median over the runs."""
    out_dir = os.path.join(target_dir(a), "bench", "pause")
    os.makedirs(out_dir, exist_ok=True)
    exes = {}
    for probe in sorted({p for p, _ in PAUSES.values()}):
        exes[probe] = os.path.join(out_dir, probe)
        sh([nabic, "-q", "build", os.path.join(PAUSE, probe + ".nabi"), "-O2", "-o", exes[probe]], cwd=ROOT)
    env = dict(os.environ, NABIR_GC_LOG="1")
    for k in ("NABIR_GC_CONCURRENT", "NABIR_GC_EVAC", "NABIR_GC_STRESS", "NABIR_GC_YOUNG"):
        env.pop(k, None)
    results = {}
    for name, (probe, args) in PAUSES.items():
        if a.filter not in name:
            continue
        samples = []
        for _ in range(max(runs, PAUSE_RUNS)):
            r = subprocess.run([exes[probe], *args], capture_output=True, text=True, env=env, cwd=out_dir)
            if r.returncode != 0:
                raise SystemExit(f"{name} exited {r.returncode}:\n{r.stderr[-2000:]}")
            samples.append(pause_metrics(r.stderr, r.stdout))
        results[name] = {k: round(percentile(sorted(s[k] for s in samples), 50), 3) for k in samples[0]}
        print(f"  {name}: " + ", ".join(f"{k} {v}" for k, v in results[name].items()), file=sys.stderr)
    return results


def speedups(results):
    """{kind: {n: wall on 1 thread / wall on n}}."""
    out = {}
    for kind in ("cpu", "alloc"):
        one = results.get(f"threads/{kind}-1", {}).get("wall")
        if not one:
            continue
        out[kind] = {}
        for n in SCALING:
            w = results.get(f"threads/{kind}-{n}", {}).get("wall")
            if w:
                out[kind][n] = one / w
    return out


def load(path):
    try:
        with open(path) as f:
            return json.load(f)
    except FileNotFoundError:
        return None


def previous():
    try:
        with open(HISTORY) as f:
            lines = [l for l in f if l.strip()]
        return json.loads(lines[-1]) if lines else None
    except FileNotFoundError:
        return None


def commit_of(nabic):
    """(commit, dirty): git's; else NABI_COMMIT_HASH or the commit nabic
    names, dirtiness unknown."""
    c = git("rev-parse", "--short=12", "HEAD")
    if c:
        return c, bool(git("status", "--porcelain", "--untracked-files=no"))
    c = os.environ.get("NABI_COMMIT_HASH", "")
    if not c:
        v = subprocess.run([nabic, "--version"], capture_output=True, text=True).stdout
        m = re.search(r"\(([0-9a-f]{7,})[ )]", v)
        c = m.group(1) if m else ""
    return c, None


def pct(new, old):
    if new is None or not old:
        return None
    return (new - old) * 100.0 / old


def fmt(v, unit=""):
    if v is None:
        return "-"
    if isinstance(v, float):
        return f"{v:.3f}{unit}"
    return f"{v:,}{unit}"


def over_budget(results, budgets):
    """Lines for the results that miss bench/budgets.json."""
    misses = []
    for name, b in sorted(budgets.get("compile", {}).items()):
        got = results.get(name, {}).get("insns")
        if b.get("host") != HOST or got is None:
            continue
        if got > b["insns"] * (1 + b["tolerance"] / 100):
            misses.append(f"{name}: {got / 1e9:.2f}G instructions, budget {b['insns'] / 1e9:.2f}G "
                          f"+ {b['tolerance']}%")
    for name, b in sorted(budgets.get("pauses", {}).items()):
        got = results.get(name)
        if b.get("host") != HOST or got is None:
            continue
        for metric, limit in sorted(b.get("ms", {}).items()):
            v = got.get(metric)
            if v is not None and v > limit * (1 + b["tolerance"] / 100):
                misses.append(f"{name}: {metric} {v:.3f} ms, budget {limit} ms + {b['tolerance']}%")
        for metric, limit in sorted(b.get("count", {}).items()):
            v = got.get(metric)
            if v is not None and v > limit:
                misses.append(f"{name}: {metric} {v:g}, at most {limit}")
    got = speedups(results)
    cores = os.cpu_count() or 1
    for kind, floors in sorted(budgets.get("scaling", {}).items()):
        for n, floor in floors.items():
            s = got.get(kind, {}).get(int(n))
            # Not more threads than cores: the floor assumes they run at once.
            if s is not None and int(n) <= cores and s < floor:
                misses.append(f"threads/{kind}: {s:.2f}x on {n} threads, at least {floor}x")
    return misses


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("filter", nargs="?", default="", help="only benchmarks containing this")
    ap.add_argument("--runs", type=int, default=3, help="best of N (default 3)")
    ap.add_argument("--threshold", type=float, default=3.0, help="regression threshold in %% (default 3)")
    ap.add_argument("--no-record", action="store_true", help="do not append to history.jsonl")
    ap.add_argument("--target-dir", help="build directory (default: $CARGO_TARGET_DIR, else target/)")
    ap.add_argument("--compile", action="store_true", help="also time hello builds, with an empty cache and a warm one")
    ap.add_argument("--scaling", action="store_true", help="also time bench/threads.nabi on 1, 2, 4, 8 threads")
    ap.add_argument("--pauses", action="store_true",
                    help="also run the GC pause probes of bench/pause (median of at least 5 runs)")
    ap.add_argument("--only", action="store_true", help="with --compile / --scaling / --pauses: no other benchmarks")
    a = ap.parse_args()

    prev = previous()
    llvm = load(LLVM) or {}
    budgets = load(BUDGETS) or {}
    nabic = build_toolchain(a)
    results, outputs = {}, {}
    if not a.only:
        for name, exe in build(a, nabic, lambda n: a.filter in n).items():
            outputs[name], results[name] = best([exe], a.runs)
            print(f"  {name}: {fmt(results[name].get('insns'))} insns, {fmt(results[name].get('user'), 's')} user",
                  file=sys.stderr)
    if a.compile:
        results.update(compile_times(nabic, a.runs))
    if a.scaling:
        results.update(scaling(a, nabic, a.runs))
    if a.pauses:
        results.update(pauses(a, nabic, a.runs))

    commit, dirty = commit_of(nabic)
    entry = {
        "commit": commit,
        "dirty": dirty,
        "subject": git("log", "-1", "--format=%s"),
        "date": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "host": HOST,
        "runs": a.runs,
        "results": results,
    }

    # Counts depend on the CPU and the C compiler: compare one host's only.
    base = prev["results"] if prev and prev.get("host") == HOST else {}
    ref = llvm.get("results", {}) if llvm.get("host") == HOST else {}
    head = f"\ncommit {commit or '?'}{' (dirty)' if dirty else ''}"
    if base:
        head += f" vs {prev['commit'] or '?'}{' (dirty)' if prev.get('dirty') else ''}"
    else:
        head += " (no previous record)"
    if ref:
        head += f"; LLVM: {llvm['commit']} of {llvm['date'][:10]}"
    print(head)
    print(f"{'benchmark':<18} {'instructions':>16} {'Δ%':>7} {'user s':>8} {'Δ%':>7} {'rss KiB':>9} "
          f"{'vs C':>6} {'vs LLVM':>8}")
    regressions, mismatches = [], []
    for name in sorted(results):
        if name.startswith("pause/"):
            continue
        r, o = results[name], base.get(name, {})
        di, du = pct(r.get("insns"), o.get("insns")), pct(r.get("user"), o.get("user"))
        # Wall-clock rows: too noisy for the threshold; budgets.json gates them.
        if name.startswith("threads/"):
            di = du = None
        c = results.get("c/" + name)
        ratio = None
        if c and c.get("insns") and r.get("insns"):
            ratio = r["insns"] / c["insns"]
        elif c and c.get("user") and r.get("user"):
            ratio = r["user"] / c["user"]
        lv = ref.get(name, {})
        vs_llvm = r["insns"] / lv["insns"] if lv.get("insns") and r.get("insns") else None
        if c and outputs.get("c/" + name) != outputs.get(name):
            mismatches.append(name)
        key = di if di is not None else du
        flag = ""
        if key is not None and key > a.threshold:
            flag = "  REGRESSION"
            regressions.append(name)
        print(f"{name:<18} {fmt(r.get('insns')):>16} {fmt(di) if di is None else f'{di:+.1f}':>7} "
              f"{fmt(r.get('user')):>8} {fmt(du) if du is None else f'{du:+.1f}':>7} "
              f"{fmt(r.get('rss_kb')):>9} {'' if ratio is None else f'{ratio:.2f}x':>6} "
              f"{'' if vs_llvm is None else f'{vs_llvm:.2f}x':>8}{flag}")
    for name in sorted(n for n in results if n.startswith("pause/")):
        r, o = results[name], base.get(name, {})
        print(f"{name:<18} " + ", ".join(
            f"{k} {v:g}" + (f" (was {o[k]:g})" if k in o else "") for k, v in r.items()))
    for kind, s in speedups(results).items():
        print(f"threads/{kind} speedup: " + ", ".join(f"{n}: {v:.2f}x" for n, v in s.items())
              + f" ({os.cpu_count()} cores)")

    if not a.no_record:
        with open(HISTORY, "a") as f:
            f.write(json.dumps(entry, sort_keys=True) + "\n")
        print(f"\nrecorded in {os.path.relpath(HISTORY, ROOT)}")
    for n in mismatches:
        print(f"OUTPUT MISMATCH: {n} prints differently from c/{n}")
    if regressions:
        print(f"{len(regressions)} regression(s) over {a.threshold}%: {', '.join(regressions)}")
    misses = over_budget(results, budgets)
    for m in misses:
        print(f"OVER BUDGET: {m}")
    return 1 if regressions or mismatches or misses else 0


PERF = perf()

if __name__ == "__main__":
    sys.exit(main())
