#!/usr/bin/env python3
"""PostToolUse graph refresh for Edit/Write that is safe for untracked files.

``code-review-graph update`` derives its file list from ``git diff HEAD~1`` and
only falls back to ``git status`` when that diff is empty — never true on the
shared dirty tree — so a file that is not yet tracked never enters the graph
(2026-08-27: 14 untracked ``conductor/`` substrate modules had zero nodes).

This hook re-parses exactly the edited file(s) plus their importers (what
``incremental_update`` does for any explicit list), refreshes signatures + FTS,
then embeds those files' nodes through the workspace embedding bridge and
prunes vectors whose node no longer exists. Harness-agnostic: payload parsing
is shared with ``crg_gate.py``. It re-executes itself under the code-review-graph
pipx interpreter when started by the system python.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sqlite3
import sys
from pathlib import Path
from typing import Any, Final

HOOK_DIR: Final[Path] = Path(__file__).resolve().parent
sys.path.insert(0, str(HOOK_DIR))
from crg_gate import REPO_ROOT, _read_payload, _repo_relative_targets  # noqa: E402

GRAPH_SUFFIXES: Final[frozenset[str]] = frozenset(
    {".py", ".rs", ".c", ".cc", ".cpp", ".cxx", ".h", ".hpp", ".sh", ".bash"}
)
SKIP_EMBED_ENV: Final[str] = "CRG_SKIP_EMBED"


def _emit(context: str = "") -> None:
    output: dict[str, Any] = {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}
    if context:
        output["hookSpecificOutput"]["additionalContext"] = context
    print(json.dumps(output))


def _crg_python() -> Path | None:
    binary = shutil.which("code-review-graph")
    if binary is None:
        return None
    python = Path(binary).resolve().parent / "python"
    return python if python.is_file() else None


def _reexec_under_crg(files: list[str]) -> None:
    python = _crg_python()
    if python is None:
        _emit(
            "WARNING: code-review-graph is not installed; graph NOT refreshed for "
            + ", ".join(files)
        )
        sys.exit(0)
    env = dict(os.environ)
    env["PYTHONPATH"] = str(REPO_ROOT) + (
        os.pathsep + env["PYTHONPATH"] if env.get("PYTHONPATH") else ""
    )
    os.execve(str(python), [str(python), __file__, "--files", *files], env)


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


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--files", nargs="*", default=None)
    args = parser.parse_args()
    if args.files is None:
        payload = _read_payload()
        files = [
            target
            for target in _repo_relative_targets(payload)
            if Path(target).suffix in GRAPH_SUFFIXES and (REPO_ROOT / target).is_file()
        ]
        if not files:
            _emit()
            return 0
        try:
            import code_review_graph  # noqa: F401
        except ImportError:
            _reexec_under_crg(files)
        args.files = files
    try:
        warning = refresh(args.files)
    except Exception as exc:  # noqa: BLE001 - hook must report, never raise
        _emit(
            f"WARNING: graph refresh FAILED for {args.files}: {type(exc).__name__}: {exc}. "
            "Graph reads are STALE until `code-review-graph update` succeeds."
        )
        return 0
    _emit(f"graph refresh warning: {warning}" if warning else "")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
