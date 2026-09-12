"""Copy-only sandbox: a frozen subtree in the scratchpad instead of a worktree.

A worktree bundles three things -- a second working tree, a second environment,
and somewhere to build a commit. A run that only needs *a frozen copy of some
directories* pays for all three, and then the tree outlives the run. This
exports exactly the paths a run imports out of the object database with
``git archive``, so nothing is registered with Git, nothing needs reaping, and
deleting the directory is the whole cleanup story.

Not for building commits: those belong on a branch in the shared checkout under
a narrow claim (``KB-GOV-01``). Take a real worktree only when a *different
commit* must be checked out at the same time.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def default_root() -> Path:
    """Where sandboxes live: the session scratchpad if the harness set one."""
    named = os.environ.get("SANDBOX_ROOT") or os.environ.get("CLAUDE_SCRATCHPAD_DIR")
    # tempfile.gettempdir() honours TMPDIR and falls back per-platform, so the
    # temp directory is never a literal in the source (bandit B108).
    return Path(named) if named else Path(tempfile.gettempdir()) / "llm-sandboxes"


def export(repo: Path, commit: str, paths: list[str], dest: Path) -> Path:
    """Extract ``paths`` at ``commit`` into ``dest``; fail loudly on any error."""
    if not paths:
        raise ValueError("a sandbox must name the directories the run imports")
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    archive = subprocess.Popen(
        ["git", "archive", "--format=tar", commit, "--", *paths],
        cwd=repo,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert archive.stdout is not None
    extract = subprocess.run(
        ["tar", "-x", "-C", str(dest)], stdin=archive.stdout, check=False
    )
    archive.stdout.close()
    failure = (archive.stderr.read() if archive.stderr else b"").decode().strip()
    if archive.wait() or extract.returncode:
        shutil.rmtree(dest, ignore_errors=True)
        raise RuntimeError(f"git archive {commit} failed: {failure or 'tar error'}")
    return dest


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="conductor.sandbox")
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--commit", default="HEAD")
    parser.add_argument("--name", required=True, help="sandbox directory name")
    parser.add_argument("--root", type=Path, default=None)
    parser.add_argument(
        "paths", nargs="+", help="repository directories the run imports"
    )
    args = parser.parse_args(argv)
    root = args.root or default_root()
    try:
        dest = export(args.repo.resolve(), args.commit, args.paths, root / args.name)
    except (OSError, RuntimeError, ValueError) as exc:
        print(f"sandbox: {exc}", file=sys.stderr)
        return 2
    print(dest)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
