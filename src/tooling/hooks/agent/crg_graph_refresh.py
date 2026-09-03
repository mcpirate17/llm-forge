#!/usr/bin/env python3
"""PostToolUse graph refresh for Edit/Write: queued, detached, freshness-guarded.

``code-review-graph update`` derives its file list from ``git diff HEAD~1`` and
only falls back to ``git status`` when that diff is empty — never true on the
shared dirty tree — so a file that is not yet tracked never enters the graph
(2026-08-27: 14 untracked ``conductor/`` substrate modules had zero nodes).

Two wirings, one body:

* Dispatcher (``tooling/hooks/dispatch/registry.py``): ``hook_output`` only
  queues the edited file(s) in the graph store's ``refresh.pending`` marker and
  spawns one detached worker when none is running (``crg_refresh_state``); it
  returns in milliseconds. Graph MCP queries wait on the marker
  (``wait_output``) so they never read a graph an edit has outrun; a failed or
  warning refresh is surfaced by the next hook event (``failure_output``).
* Legacy ``.claude/settings.json`` wiring (``.agent_hooks/crg_graph_refresh.py``
  exec'ing this file with no arguments): ``sync_output`` refreshes inline and
  reports in ``additionalContext`` exactly as before, because legacy wiring has
  no wait/report hooks to make an asynchronous refresh safe.

The worker (``--worker``) debounces, coalesces, then runs each batch in a
bounded child (``--batch``, ``BATCH_TIMEOUT_SECONDS``): re-parse exactly the
queued files plus their importers (``incremental_update``), refresh signatures +
FTS, embed those files' nodes through the workspace embedding bridge and prune
vectors whose node no longer exists; a queued ``*`` (a git working-tree change)
runs ``code-review-graph update`` instead. The child re-executes itself under
the code-review-graph pipx interpreter; a child that fails, warns or hangs is
killed/recorded for the next hook event, never silently dropped.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sqlite3
import subprocess
import sys
from pathlib import Path
from typing import Any, Final

HOOK_DIR: Final[Path] = Path(__file__).resolve().parent
sys.path.insert(0, str(HOOK_DIR))
from crg_gate import REPO_ROOT, _read_payload, _repo_relative_targets  # noqa: E402
from crg_refresh_state import (  # noqa: E402
    FULL_UPDATE,
    WAIT_SECONDS,
    drain,
    record_warning,
    request,
    store_for,
    take_notices,
    wait_for_fresh,
    worker_command,
)

GRAPH_SUFFIXES: Final[frozenset[str]] = frozenset(
    {".py", ".rs", ".c", ".cc", ".cpp", ".cxx", ".h", ".hpp", ".sh", ".bash"}
)
SKIP_EMBED_ENV: Final[str] = "CRG_SKIP_EMBED"
BATCH_TIMEOUT_SECONDS: Final[float] = 300.0


def _crg_python() -> Path | None:
    binary = shutil.which("code-review-graph")
    if binary is None:
        return None
    python = Path(binary).resolve().parent / "python"
    return python if python.is_file() else None


def _reexec_under_crg() -> None:
    """Restart this batch child under the interpreter that can import code_review_graph."""
    python = _crg_python()
    if python is None:
        raise RuntimeError("code-review-graph is not installed; graph NOT refreshed")
    os.execve(str(python), [str(python), __file__, *sys.argv[1:]], _batch_env())


def _batch_env() -> dict[str, str]:
    """The child's env: the repo on PYTHONPATH so ``conductor`` imports under crg's python."""
    env = dict(os.environ)
    env["PYTHONPATH"] = str(REPO_ROOT) + (
        os.pathsep + env["PYTHONPATH"] if env.get("PYTHONPATH") else ""
    )
    return env


def _prune_orphans(db_path: Path, abs_paths: list[str], emb_store: Any) -> int:
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        pruned = 0
        for abs_path in abs_paths:
            rows = conn.execute(
                """
                SELECT e.qualified_name FROM embeddings AS e
                WHERE e.qualified_name LIKE ?
                  AND NOT EXISTS (
                    SELECT 1 FROM nodes AS n WHERE n.qualified_name = e.qualified_name
                  )
                """,
                (abs_path + "::%",),
            ).fetchall()
            for (qualified_name,) in rows:
                emb_store.remove_node(qualified_name)
                pruned += 1
        return pruned
    finally:
        conn.close()


def refresh(files: list[str]) -> str:
    """Re-parse *files*, refresh FTS/signatures, re-embed; return a warning or ''."""
    from code_review_graph.embeddings import EmbeddingStore
    from code_review_graph.graph import GraphStore
    from code_review_graph.incremental import get_db_path, incremental_update
    from code_review_graph.tools.build import _run_postprocess

    repo = REPO_ROOT
    db_path = get_db_path(repo)
    store = GraphStore(db_path)
    try:
        result = incremental_update(repo, store, changed_files=files)
        warnings = _run_postprocess(
            store,
            {},
            "minimal",
            full_rebuild=False,
            changed_files=result.get("changed_files"),
        )
        if os.environ.get(SKIP_EMBED_ENV, "").strip() == "1":
            return "; ".join(warnings)
        abs_paths = [str((repo / f).resolve()) for f in files]
        nodes = [node for path in abs_paths for node in store.get_nodes_by_file(path)]
    finally:
        store.close()

    from conductor.crg_embedding_bridge import CrgBridgeError, install_bridge
    from conductor.crg_embedding_text import install_node_text

    try:
        install_bridge()
        install_node_text(repo)
        emb_store = EmbeddingStore(db_path)
    except CrgBridgeError as exc:
        warnings.append(
            f"embedding skipped ({exc}); semantic search is stale for {files}"
        )
        return "; ".join(warnings)
    try:
        emb_store.embed_nodes(nodes)
        _prune_orphans(db_path, abs_paths, emb_store)
    finally:
        emb_store.close()
    return "; ".join(warnings)


def _batch_command(paths: list[str]) -> list[str]:
    """The bounded child that refreshes one batch: whole-tree update or ``--batch``."""
    if paths == [FULL_UPDATE]:
        return ["code-review-graph", "update", "--skip-flows"]
    python = _crg_python() or Path(sys.executable)
    return [str(python), __file__, "--batch", *paths]


def _run_batch_child(paths: list[str]) -> None:
    """Run one batch in a child capped at ``BATCH_TIMEOUT_SECONDS``.

    A hung child is killed and reported; a failing child's stderr and a
    warning child's stdout are both recorded for the next hook event.
    """
    try:
        proc = subprocess.run(
            _batch_command(paths),
            capture_output=True,
            text=True,
            cwd=REPO_ROOT,
            env=_batch_env(),
            timeout=BATCH_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(
            f"refresh exceeded {exc.timeout:.1f}s and was killed"
        ) from None
    if proc.returncode != 0:
        detail = (proc.stderr or proc.stdout).strip().splitlines()[-1:]
        raise RuntimeError(
            f"refresh child exited {proc.returncode}: {' '.join(detail) or 'no output'}"
        )
    warning = proc.stdout.strip() if paths != [FULL_UPDATE] else ""
    if warning:
        record_warning(store_for(REPO_ROOT), paths, warning)


def _refresh_batch(paths: list[str]) -> None:
    """The worker's unit of work: one coalesced batch, whole-tree when ``*`` is queued."""
    if FULL_UPDATE in paths:
        _run_batch_child([FULL_UPDATE])
        paths = [path for path in paths if path != FULL_UPDATE]
    if paths:
        _run_batch_child(paths)


def _batch(paths: list[str]) -> int:
    """``--batch``: refresh *paths* in this process; warnings on stdout, failure exit 1."""
    try:
        import code_review_graph  # noqa: F401
    except ImportError:
        try:
            _reexec_under_crg()
        except RuntimeError as exc:
            print(str(exc), file=sys.stderr)
            return 1
    print(refresh(paths))
    return 0


def _worker() -> int:
    drain(store_for(REPO_ROOT), _refresh_batch)
    return 0


def _queue(paths: list[str]) -> str:
    """Queue *paths* for the detached worker; the caller returns at once."""
    if shutil.which("code-review-graph") is None:
        return (
            "WARNING: code-review-graph is not installed; graph NOT refreshed for "
            + ", ".join(paths)
        )
    request(
        store_for(REPO_ROOT),
        paths,
        worker_argv=worker_command(Path(__file__).resolve(), REPO_ROOT),
        cwd=REPO_ROOT,
    )
    return ""


def _post(context: str = "") -> dict[str, Any]:
    output: dict[str, Any] = {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}
    if context:
        output["hookSpecificOutput"]["additionalContext"] = context
    return output


def _graph_files(payload: dict[str, Any]) -> list[str]:
    return [
        target
        for target in _repo_relative_targets(payload)
        if Path(target).suffix in GRAPH_SUFFIXES and (REPO_ROOT / target).is_file()
    ]


def hook_output(payload: dict[str, Any]) -> dict[str, Any]:
    """PostToolUse Edit/Write under the dispatcher: queue the targets, never wait."""
    files = _graph_files(payload)
    if not files:
        return _post()
    return _post(_queue(files))


def sync_output(payload: dict[str, Any]) -> dict[str, Any]:
    """PostToolUse Edit/Write under legacy wiring: refresh inline, report inline."""
    files = _graph_files(payload)
    if not files:
        return _post()
    try:
        _run_batch_child(files)
    except Exception as exc:  # noqa: BLE001 - hook must report, never raise
        return _post(
            f"WARNING: graph refresh FAILED for {files}: {type(exc).__name__}: {exc}. "
            "Graph reads are STALE until `code-review-graph update` succeeds."
        )
    notices = take_notices(store_for(REPO_ROOT))
    return _post("; ".join(f"graph refresh warning: {n['text']}" for n in notices))


def full_update_output() -> dict[str, Any]:
    """PostToolUse Bash after a git working-tree change: queue a whole-tree update."""
    warning = _queue([FULL_UPDATE])
    return _post(
        warning or "code-review-graph refresh queued after a git working-tree change."
    )


def wait_output(timeout: float = WAIT_SECONDS) -> dict[str, Any] | None:
    """PreToolUse for graph MCP tools: block, bounded, until no refresh is pending."""
    store = store_for(REPO_ROOT)
    outcome = wait_for_fresh(
        store,
        timeout=timeout,
        respawn=lambda: _queue([]),
    )
    if outcome == "fresh":
        return None
    message = (
        f"WARNING: a code-review-graph refresh was still running after {timeout:.0f}s; "
        "this query may read a STALE graph."
    )
    return {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "additionalContext": message,
        },
        "systemMessage": message,
    }


def failure_output(event: str) -> dict[str, Any] | None:
    """Any event: surface failed and warning background refreshes, once, visibly."""
    notices = take_notices(store_for(REPO_ROOT))
    if not notices:
        return None
    failures = [n["text"] for n in notices if n["kind"] == "failure"]
    warnings = [n["text"] for n in notices if n["kind"] != "failure"]
    parts = []
    if failures:
        parts.append(
            f"WARNING: background graph refresh FAILED: {'; '.join(failures)}. Graph "
            "reads are STALE until `code-review-graph update` succeeds."
        )
    if warnings:
        parts.append(f"WARNING: background graph refresh: {'; '.join(warnings)}")
    message = " ".join(parts)
    return {
        "hookSpecificOutput": {"hookEventName": event, "additionalContext": message},
        "systemMessage": message,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--worker", action="store_true", help="drain the queue (detached)"
    )
    parser.add_argument(
        "--repo",
        default=None,
        help="checkout served (worker; shown in ps, PROJECT_DIR rules)",
    )
    parser.add_argument(
        "--batch", nargs="+", default=None, help="refresh these paths now"
    )
    args = parser.parse_args()
    if args.worker:
        return _worker()
    if args.batch:
        return _batch(args.batch)
    # No arguments: legacy settings.json wiring, which has no wait/report hooks,
    # so the refresh stays synchronous there. Async is dispatcher-only.
    print(json.dumps(sync_output(_read_payload())))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
