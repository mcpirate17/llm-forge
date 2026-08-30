#!/usr/bin/env python3
"""Shared scan-root resolution for governance tools.

Several governance tools measure a caller-chosen tree but historically had no
argument to name one: they derived their root from
``Path(__file__).resolve().parents[N]``, so the scan target was decided by
where the module happened to be *imported from* -- not cwd, not any CLI
argument. Under an editable install (see ``pip show llm-workspace``), that
resolves to the shared checkout regardless of which tree the operator is
standing in, and the tool silently audits the wrong tree.

This module packages the fix (first applied in
``conductor/run_duplicate_audit.py``) so every tool that needs it resolves,
prints, and validates its scan root the same way instead of re-deriving the
logic per file.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path


class AuditRootError(RuntimeError):
    """The tool's scan target could not be resolved precisely."""


def _show_toplevel(cwd: Path) -> str | None:
    completed = subprocess.run(
        ["git", "-C", str(cwd), "rev-parse", "--show-toplevel"],
        check=False,
        capture_output=True,
        text=True,
        errors="replace",
    )
    resolved = completed.stdout.strip()
    if completed.returncode or not resolved:
        return None
    return resolved


def resolve_audit_root(
    explicit_root: str | Path | None, *, cwd: Path | None = None
) -> Path:
    """Resolve the checkout a governance tool must scan.

    An explicit root is authoritative. Otherwise require the Git worktree
    containing the process working directory -- never ``__file__``, which
    names the checkout that happened to supply the imported module, not the
    tree the operator is standing in. Refuses loudly (``AuditRootError``)
    rather than falling back to the module location.
    """
    invocation_cwd = (cwd or Path.cwd()).resolve()
    if explicit_root is not None:
        candidate = Path(explicit_root).expanduser()
        if not candidate.is_absolute():
            candidate = invocation_cwd / candidate
        try:
            root = candidate.resolve(strict=True)
        except OSError as exc:
            raise AuditRootError(
                f"explicit --root does not exist ({candidate}): {exc}"
            ) from exc
        if not root.is_dir():
            raise AuditRootError(f"explicit --root is not a directory: {root}")
        return root

    resolved = _show_toplevel(invocation_cwd)
    if resolved is None:
        raise AuditRootError(
            "cannot resolve a scan root: the current working directory "
            f"({invocation_cwd}) is not inside a Git worktree; pass --root explicitly"
        )
    root = Path(resolved).resolve()
    if not root.is_dir():
        raise AuditRootError(f"resolved Git root is not a directory: {root}")
    return root


def git_head(root: Path) -> str:
    completed = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "HEAD"],
        check=False,
        capture_output=True,
        text=True,
        errors="replace",
    )
    head = completed.stdout.strip()
    return head if completed.returncode == 0 and head else "unavailable"


def print_audit_provenance(
    tool: str, root: Path, *, cwd: Path | None = None, extra: str = ""
) -> None:
    """Print the resolved root and commit before any results, every run.

    Printed unconditionally -- an omitted ``--root`` flag must not fail
    silently, which is the entire failure mode this module exists to close.
    Also warns loudly (to stderr) when the resolved root differs from cwd's
    own Git toplevel: an operator standing in one tree whose results
    actually describe a different one needs to see that immediately, not
    discover it from a wrong report later.
    """
    suffix = f" | {extra}" if extra else ""
    print(f"{tool}: root={root} git-head={git_head(root)}{suffix}", flush=True)
    invocation_cwd = (cwd or Path.cwd()).resolve()
    cwd_toplevel = _show_toplevel(invocation_cwd)
    if cwd_toplevel is None:
        return
    cwd_root = Path(cwd_toplevel).resolve()
    if cwd_root != root:
        print(
            f"WARNING: {tool}: resolved root {root} differs from the current "
            f"working directory's Git worktree ({cwd_root}). Results describe "
            "--root, not the tree you are standing in.",
            file=sys.stderr,
            flush=True,
        )
