#!/usr/bin/env python3
"""Bundle linked libraries from a trusted, locally compiled application."""
import pathlib
import re
import shutil
import subprocess
import sys

appdir = pathlib.Path(sys.argv[1]).resolve()
libdir = appdir / "usr/lib"
pending = []
for path in appdir.rglob("*"):
    if path.is_file():
        with path.open("rb") as stream:
            if stream.read(4) == b"\x7fELF":
                pending.append(path)

seen = set()
while pending:
    path = pending.pop()
    if path in seen:
        continue
    seen.add(path)
    result = subprocess.run(["ldd", str(path)], text=True, capture_output=True, check=True)
    if "not found" in result.stdout:
        raise SystemExit(f"Unresolved dependencies for {path}:\n{result.stdout}")
    for line in result.stdout.splitlines():
        match = re.search(r"(?:=>\s+)?(/\S+)\s+\(", line)
        if not match:
            continue
        source = pathlib.Path(match[1])
        destination = libdir / source.name
        if destination.exists():
            if source.read_bytes() != destination.read_bytes():
                raise SystemExit(f"Conflicting library name: {source.name}")
            continue
        shutil.copy2(source, destination, follow_symlinks=True)
        pending.append(source)

print(f"Bundled dependencies of {len(seen)} ELF files")
