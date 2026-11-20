"""A reproducible zip of a staged toolchain, for dist/native.sh on Windows.

    python -I scripts/dist/zip.py <dir> <name> <out.zip>

Zips <dir>/<name> with entries in sorted order, each with the time of
SOURCE_DATE_EPOCH (UTC), Unix modes 755 or 644, and nothing that names
the build machine or its time zone.
"""

import os
import sys
import time
import zipfile


def main():
    root, name, out = sys.argv[1:4]
    when = time.gmtime(max(int(os.environ["SOURCE_DATE_EPOCH"]), 315532800))[:6]
    paths = []
    for d, dirs, files in os.walk(os.path.join(root, name)):
        rel = os.path.relpath(d, root).replace(os.sep, "/")
        paths.append(rel + "/")
        paths.extend(rel + "/" + f for f in files)
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for p in sorted(paths):
            info = zipfile.ZipInfo(p, when)
            info.create_system = 3
            if p.endswith("/"):
                info.external_attr = (0o40755 << 16) | 0x10
                z.writestr(info, b"")
                continue
            full = os.path.join(root, *p.split("/"))
            exe = p.endswith(".exe") or os.access(full, os.X_OK) and not os.name == "nt"
            info.external_attr = (0o100755 if exe else 0o100644) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            with open(full, "rb") as f:
                z.writestr(info, f.read(), compresslevel=9)


main()
