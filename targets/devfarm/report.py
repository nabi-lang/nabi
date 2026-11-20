"""Summarizes a corpus run: a junit.xml (targets/android Corpus), an
xcodebuild test log (targets/ios Corpus.m) or Device Farm's own results
as run.sh lists them (tests.tsv: name, result, message):

    python3 report.py <junit.xml | xcodebuild.log | tests.tsv>

prints "N of M cases ran, K failed", then each failing case with the
first line of what differed and each skipped one with its reason. Exits 1
when a case failed.
"""

import re
import sys
import xml.etree.ElementTree as ET

RESULT = re.compile(r"^Test [Cc]ase '-\[\w+ test_(\w+)\]' (passed|failed|skipped)")
MESSAGE = re.compile(r"-\[\w+ test_(\w+)\] : (?:failed - |Test skipped - )(.*)")


def junit(path):
    suite = ET.parse(path).getroot()
    out = []
    for c in suite.iter("testcase"):
        f = c.find("failure")
        if f is None:
            f = c.find("error")
        s = c.find("skipped")
        if f is not None:
            out.append((c.get("name"), "failed", f.get("message") or ""))
        elif s is not None:
            out.append((c.get("name"), "skipped", s.get("message") or ""))
        else:
            out.append((c.get("name"), "passed", ""))
    return out


def xcodebuild(path):
    msgs, out = {}, []
    with open(path, errors="replace") as f:
        for line in f:
            r = RESULT.match(line)
            if r:
                out.append((r[1], r[2], msgs.get(r[1], "")))
                continue
            m = MESSAGE.search(line)
            if m and m[1] not in msgs:
                msgs[m[1]] = m[2].strip()
    return out


# Device Farm's results; its setup and teardown "tests" are not cases.
STATES = {"PASSED": "passed", "FAILED": "failed", "ERRORED": "failed", "SKIPPED": "skipped"}


def tsv(path):
    out = []
    with open(path, errors="replace") as f:
        for line in f:
            name, result, msg = (line.rstrip("\n").split("\t") + ["", ""])[:3]
            if not name.startswith("test_"):
                continue
            out.append((name[5:], STATES.get(result, "failed"), "" if msg == "None" else msg))
    return out


def main():
    path = sys.argv[1]
    with open(path, "rb") as f:
        xml = f.read(64).lstrip().startswith(b"<")
    if xml:
        cases = junit(path)
    elif path.endswith(".tsv"):
        cases = tsv(path)
    else:
        cases = xcodebuild(path)
    failed = [(n, m) for n, st, m in cases if st == "failed"]
    skipped = [(n, m) for n, st, m in cases if st == "skipped"]
    print(f"{len(cases) - len(skipped)} of {len(cases)} cases ran, {len(failed)} failed")
    for name, msg in failed:
        print(f"  FAIL {name}: {msg}")
    for name, msg in skipped:
        print(f"  skip {name}: {msg}")
    return 1 if failed or not cases else 0


if __name__ == "__main__":
    sys.exit(main())
