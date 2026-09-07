"""File iteration, report shaping and tool invocation for the reuse analyzers.

Ported from ``audit/orchestrator/{files,reporting,process}.py`` when the
reuse analyzers moved into conductor. Only the helpers the analyzers actually call
came across; the shell/git wrappers stayed behind with the audit loop.
"""

from __future__ import annotations

import subprocess
from collections.abc import Callable
from datetime import datetime, timezone
from pathlib import Path
from typing import TypeVar

T = TypeVar("T")


def iter_files(
    roots: list[Path],
    *,
    suffixes: set[str] | None = None,
    skip_parts: set[str] | None = None,
    sort_paths: bool = False,
) -> list[Path]:
    """Collect files under roots, optionally filtered by suffix and excluded parts.

    `skip_parts` is matched against each path's components RELATIVE to its root, never
    the absolute prefix. Otherwise a root under e.g. ``/tmp/...`` (a disposable snapshot
    worktree) would have every file skipped by an exclude entry like ``"tmp"``, silently
    profiling zero files -- the dirty-tree no-op bug fixed 2026-07-11.
    """
    skip = skip_parts or set()
    out: list[Path] = []
    seen: set[Path] = set()
    for root in roots:
        if not root.exists():
            continue
        for path in root.rglob("*"):
            if not path.is_file():
                continue
            if suffixes is not None and path.suffix.lower() not in suffixes:
                continue
            if any(part in skip for part in path.relative_to(root).parts):
                continue
            if path in seen:
                continue
            seen.add(path)
            out.append(path)
    if sort_paths:
        out.sort()
    return out


def generated_output(
    *,
    base: dict,
    items_key: str,
    items: list[T],
    serialize: Callable[[T], dict],
) -> dict:
    return {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        **base,
        items_key: [serialize(item) for item in items],
    }


def plus_more_list(items: list[str], *, show: int) -> str:
    shown = items[:show]
    extra = len(items) - len(shown)
    text = ", ".join(shown)
    if extra > 0:
        text += f", +{extra} more"
    return text


def write_markdown_table(
    path: Path,
    *,
    title: str,
    summary: str,
    columns: list[str],
    rows: list[str],
) -> None:
    lines = [
        title,
        "",
        summary,
        "",
        "| " + " | ".join(columns) + " |",
        "|" + "|".join("---" for _ in columns) + "|",
        *rows,
    ]
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def run_tool_capture(
    argv: list[str],
    cwd: Path,
    *,
    timeout: int,
    tool_name: str,
    ok_returncodes: set[int] | None = None,
    not_found_detail: str | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ``argv`` and turn every failure mode into a named, loud RuntimeError."""
    try:
        proc = subprocess.run(
            argv,
            cwd=cwd,
            text=True,
            timeout=timeout,
            capture_output=True,
        )
    except FileNotFoundError as exc:
        detail = not_found_detail or f"{argv[0]} was not found"
        raise RuntimeError(f"{tool_name} unavailable: {detail}") from exc
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(f"{tool_name} timed out after {timeout} seconds") from exc
    if ok_returncodes is not None and proc.returncode not in ok_returncodes:
        raise RuntimeError(
            f"{tool_name} failed ({proc.returncode}): {(proc.stderr or '')[:500]}"
        )
    return proc
