#!/usr/bin/env python3
"""Qwen-compatible mark-and-flush automation for the workspace memory index.

Post-tool hooks create unique pending markers for indexed source files. Stop and
session-end hooks coalesce those markers behind one OS file lock, run one atomic
content-hash refresh, and retain every marker if the refresh fails.
"""

from __future__ import annotations

import argparse
import fcntl
import json
import os
import subprocess
import sys
import time
import uuid
from collections.abc import Callable
from datetime import datetime, timezone
from pathlib import Path

from conductor.atomic_json import write_json_atomic
from typing import Any, Final

from conductor.kb_retrieve import RetrieveError
from conductor.memory_index import ROOT, load_catalog, path_matches_source

STATE_SCHEMA_VERSION: Final[int] = 1
DEFAULT_TIMEOUT_SECONDS: Final[float] = 240.0
MAX_RECEIPTS: Final[int] = 128
PATH_KEYS: Final[tuple[str, ...]] = (
    "file_path",
    "filePath",
    "path",
    "target_file",
    "targetFile",
)


class AutoIndexError(RuntimeError):
    """Raised when automatic indexing cannot produce a valid receipt."""


IndexRunner = Callable[[Path, float, set[str] | None], subprocess.CompletedProcess[str]]


def _utc_now() -> str:
    return datetime.now(timezone.utc).isoformat()


def _state_dir(repo_root: Path) -> Path:
    override = os.environ.get("MEMORY_AUTO_INDEX_STATE_DIR")
    if override:
        return Path(override).expanduser().resolve()
    return repo_root / "research" / "cache" / "memory_auto_index"


def _tool_input(payload: dict[str, Any]) -> dict[str, Any]:
    value = payload.get("tool_input") or payload.get("toolInput")
    return value if isinstance(value, dict) else {}


def candidate_paths(payload: dict[str, Any], repo_root: Path = ROOT) -> list[Path]:
    """Extract normalized file paths from a Qwen-compatible tool payload."""

    tool_input = _tool_input(payload)
    cwd_raw = payload.get("cwd")
    cwd = Path(cwd_raw) if isinstance(cwd_raw, str) and cwd_raw else repo_root
    candidates: list[Path] = []
    for key in PATH_KEYS:
        value = tool_input.get(key)
        values = value if isinstance(value, list) else [value]
        for raw in values:
            if not isinstance(raw, str) or not raw:
                continue
            path = Path(raw).expanduser()
            if not path.is_absolute():
                path = cwd / path
            candidates.append(path.resolve())
    return list(dict.fromkeys(candidates))


def indexed_path_sources(
    payload: dict[str, Any],
    *,
    repo_root: Path = ROOT,
    catalog_path: Path | None = None,
) -> dict[Path, tuple[str, ...]]:
    """Map payload paths to the indexed catalog sources that contain them."""

    catalog = load_catalog(catalog_path or repo_root / "conductor/memory_sources.toml")
    entries = [entry for entry in catalog["source"] if entry.get("kind") == "index"]
    matches: dict[Path, tuple[str, ...]] = {}
    for path in candidate_paths(payload, repo_root):
        source_ids = tuple(
            sorted(
                str(entry["id"])
                for entry in entries
                if path_matches_source(entry, path)
            )
        )
        if source_ids:
            matches[path] = source_ids
    return matches


def indexed_paths(
    payload: dict[str, Any],
    *,
    repo_root: Path = ROOT,
    catalog_path: Path | None = None,
) -> list[Path]:
    """Return payload paths covered by at least one indexed catalog source."""

    return list(
        indexed_path_sources(
            payload,
            repo_root=repo_root,
            catalog_path=catalog_path,
        )
    )


def mark_pending(
    payload: dict[str, Any],
    *,
    repo_root: Path = ROOT,
    state_dir: Path | None = None,
    catalog_path: Path | None = None,
) -> Path | None:
    """Create one unique pending marker when a tool changed indexed files."""

    matches = indexed_path_sources(
        payload,
        repo_root=repo_root,
        catalog_path=catalog_path,
    )
    if not matches:
        return None
    state = state_dir or _state_dir(repo_root)
    token = f"{time.time_ns()}-{os.getpid()}-{uuid.uuid4().hex}"
    marker = state / "pending" / f"{token}.json"
    write_json_atomic(
        marker,
        {
            "schema_version": STATE_SCHEMA_VERSION,
            "token": token,
            "created_at": _utc_now(),
            "pid": os.getpid(),
            "hook_event_name": payload.get("hook_event_name"),
            "tool_name": payload.get("tool_name"),
            "paths": [str(path) for path in matches],
            "sources": sorted(
                {source for source_ids in matches.values() for source in source_ids}
            ),
        },
    )
    return marker


def _default_runner(
    repo_root: Path,
    timeout: float,
    source_ids: set[str] | None,
) -> subprocess.CompletedProcess[str]:
    python = repo_root / ".venv" / "bin" / "python"
    executable = python if python.is_file() else Path(sys.executable)
    command = [str(executable), "-m", "conductor.memory_index", "index"]
    if source_ids:
        command.extend(("--sources", ",".join(sorted(source_ids))))
    return subprocess.run(
        command,
        cwd=repo_root,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )


def _parse_index_receipt(stdout: str) -> dict[str, Any]:
    lines = [line for line in stdout.splitlines() if line.strip()]
    if len(lines) != 1:
        raise AutoIndexError("memory index must emit exactly one JSON receipt")
    try:
        payload = json.loads(lines[0])
    except json.JSONDecodeError as exc:
        raise AutoIndexError("memory index emitted malformed JSON") from exc
    if not isinstance(payload, dict):
        raise AutoIndexError("memory index receipt must be a JSON object")
    if not isinstance(payload.get("chunks"), int) or payload["chunks"] < 1:
        raise AutoIndexError("memory index receipt has invalid chunks")
    if not isinstance(payload.get("sources"), list):
        raise AutoIndexError("memory index receipt has invalid sources")
    if not isinstance(payload.get("updated"), bool):
        raise AutoIndexError("memory index receipt has invalid updated flag")
    return payload


def _marker_paths(markers: list[Path]) -> list[str]:
    paths: set[str] = set()
    for marker in markers:
        try:
            payload = json.loads(marker.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        for path in payload.get("paths", []):
            if isinstance(path, str):
                paths.add(path)
    return sorted(paths)


def _marker_sources(markers: list[Path]) -> set[str] | None:
    sources: set[str] = set()
    for marker in markers:
        try:
            payload = json.loads(marker.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            return None
        values = payload.get("sources")
        if not isinstance(values, list) or not values:
            return None
        for source in values:
            if not isinstance(source, str) or not source:
                return None
            sources.add(source)
    return sources or None


def _write_receipt(state_dir: Path, payload: dict[str, Any]) -> Path:
    receipts = state_dir / "receipts"
    receipt = receipts / f"{time.time_ns()}-{os.getpid()}.json"
    write_json_atomic(receipt, payload)
    write_json_atomic(state_dir / "latest.json", payload)
    old = sorted(receipts.glob("*.json"))
    for path in old[:-MAX_RECEIPTS]:
        path.unlink(missing_ok=True)
    return receipt


def flush_pending(
    *,
    repo_root: Path = ROOT,
    state_dir: Path | None = None,
    runner: IndexRunner = _default_runner,
    timeout: float = DEFAULT_TIMEOUT_SECONDS,
    scan_if_clean: bool = False,
) -> list[dict[str, Any]]:
    """Coalesce pending markers and refresh behind one process-wide file lock."""

    state = state_dir or _state_dir(repo_root)
    pending_dir = state / "pending"
    if not scan_if_clean and not any(pending_dir.glob("*.json")):
        return []
    state.mkdir(parents=True, exist_ok=True)
    receipts: list[dict[str, Any]] = []
    with (state / "flush.lock").open("a+", encoding="utf-8") as lock:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        force_once = scan_if_clean
        while True:
            markers = sorted(pending_dir.glob("*.json"))
            if not markers and not force_once:
                break
            force_once = False
            started = time.monotonic()
            started_at = _utc_now()
            changed_paths = _marker_paths(markers)
            selected_sources = _marker_sources(markers)
            try:
                completed = runner(repo_root, timeout, selected_sources)
                if completed.returncode != 0:
                    raise AutoIndexError(
                        f"memory index exited {completed.returncode}: "
                        f"{completed.stderr.strip()[-4000:]}"
                    )
                index_receipt = _parse_index_receipt(completed.stdout)
            except (AutoIndexError, subprocess.TimeoutExpired, OSError) as exc:
                receipt = {
                    "schema_version": STATE_SCHEMA_VERSION,
                    "status": "FAIL-CLOSED",
                    "is_valid": False,
                    "started_at": started_at,
                    "completed_at": _utc_now(),
                    "elapsed_seconds": round(time.monotonic() - started, 3),
                    "pending_markers": len(markers),
                    "changed_paths": changed_paths,
                    "selected_sources": sorted(selected_sources or []),
                    "error": str(exc),
                }
                _write_receipt(state, receipt)
                raise AutoIndexError(str(exc)) from exc
            receipt = {
                "schema_version": STATE_SCHEMA_VERSION,
                "status": "PASS",
                "is_valid": True,
                "started_at": started_at,
                "completed_at": _utc_now(),
                "elapsed_seconds": round(time.monotonic() - started, 3),
                "pending_markers": len(markers),
                "changed_paths": changed_paths,
                "selected_sources": sorted(selected_sources or []),
                "index": index_receipt,
            }
            _write_receipt(state, receipt)
            for marker in markers:
                marker.unlink(missing_ok=True)
            receipts.append(receipt)
    return receipts


def _read_payload() -> dict[str, Any]:
    try:
        payload = json.load(sys.stdin)
    except json.JSONDecodeError:
        return {}
    return payload if isinstance(payload, dict) else {}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Automatic workspace memory indexing")
    parser.add_argument("action", choices=("mark", "flush"))
    parser.add_argument(
        "--scan-if-clean",
        action="store_true",
        help="Run one full incremental scan when no pending marker exists",
    )
    args = parser.parse_args(argv)
    try:
        payload = _read_payload()
        if args.action == "mark":
            mark_pending(payload)
        else:
            flush_pending(scan_if_clean=args.scan_if_clean)
        return 0
    except (AutoIndexError, OSError, RetrieveError, ValueError) as exc:
        print(f"MEMORY_AUTO_INDEX_FAIL_CLOSED: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
