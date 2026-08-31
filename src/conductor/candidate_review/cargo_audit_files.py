"""Run cargo-audit against lockfiles owning changed Rust candidate files."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path


def _owning_lockfile(path: Path, *, root: Path) -> Path | None:
    candidate = path if path.name == "Cargo.lock" else path.parent
    while candidate.is_relative_to(root):
        lockfile = (
            candidate if candidate.name == "Cargo.lock" else candidate / "Cargo.lock"
        )
        if lockfile.is_file():
            return lockfile
        if candidate == root:
            break
        candidate = candidate.parent
    return None


def main(argv: list[str] | None = None) -> int:
    root = Path.cwd().resolve()
    values = sys.argv[1:] if argv is None else argv
    lockfiles = {
        lockfile
        for value in values
        if (lockfile := _owning_lockfile(Path(value).resolve(), root=root)) is not None
    }
    if not lockfiles:
        print(
            "cargo-audit: no owning Cargo.lock found for changed Rust files",
            file=sys.stderr,
        )
        return 1

    for lockfile in sorted(lockfiles):
        completed = subprocess.run(
            ["cargo", "audit", "--file", str(lockfile)],
            check=False,
        )
        if completed.returncode:
            return completed.returncode
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
