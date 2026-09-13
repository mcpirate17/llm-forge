#!/usr/bin/env python3
"""Path-and-docstring node text for code-review-graph embeddings.

The stock ``embeddings._node_to_text`` embeds ``name kind params returns
language`` — no module path, no docstring — so recall on "session preamble"
surfaced ``_print_header``. This override embeds
``<repo-relative path>::<qualified suffix> <kind> <params> returns <type>
— <first docstring line>``. Docstrings are read on demand from the source
file through a small in-process LRU; nothing is persisted. Installed by
``conductor.crg_server`` and ``.agent_hooks/crg_graph_refresh.py`` so every
writer of the embeddings table uses the same text (a mismatch would make the
two writers re-embed each other's nodes forever).
"""

from __future__ import annotations

import ast
from functools import lru_cache
from pathlib import Path
from typing import Any, Final

from conductor.project_paths import host_root

ROOT: Final[Path] = host_root()
DOC_CHARS: Final[int] = 160
TEXT_CHARS: Final[int] = 600
LINE_TOLERANCE: Final[int] = 3


def _first_line(doc: str | None) -> str:
    if not doc:
        return ""
    for line in doc.strip().splitlines():
        line = line.strip()
        if line:
            return line if len(line) <= DOC_CHARS else line[: DOC_CHARS - 1] + "…"
    return ""


@lru_cache(maxsize=64)
def _python_docstrings(
    path: str, mtime_ns: int, size: int
) -> dict[str, list[tuple[int, str]]]:
    """Map ``Parent.name`` / ``name`` -> [(lineno, first docstring line)] for one file."""
    del mtime_ns, size  # cache-key only
    try:
        tree = ast.parse(Path(path).read_text(encoding="utf-8"))
    except (OSError, SyntaxError, UnicodeDecodeError):
        return {}
    found: dict[str, list[tuple[int, str]]] = {}

    def visit(node: ast.AST, parent: str) -> None:
        for child in ast.iter_child_nodes(node):
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                key = f"{parent}.{child.name}" if parent else child.name
                doc = _first_line(ast.get_docstring(child))
                if doc:
                    found.setdefault(key, []).append((child.lineno, doc))
                visit(child, key if isinstance(child, ast.ClassDef) else parent)

    visit(tree, "")
    return found


def docstring_for(node: Any) -> str:
    """First docstring line for a Python graph node, or ''."""
    path = Path(node.file_path)
    if path.suffix != ".py":
        return ""
    try:
        stat = path.stat()
    except OSError:
        return ""
    table = _python_docstrings(str(path), stat.st_mtime_ns, stat.st_size)
    keys = [node.name]
    if node.parent_name:
        keys.insert(0, f"{node.parent_name}.{node.name}")
    for key in keys:
        candidates = table.get(key, [])
        near = [
            doc
            for lineno, doc in candidates
            if abs(lineno - int(node.line_start or 0)) <= LINE_TOLERANCE
        ]
        if near:
            return near[0]
        if len(candidates) == 1:
            return candidates[0][1]
    return ""


def node_text(node: Any, repo_root: Path = ROOT) -> str:
    """Embedding text for one graph node (File nodes are never embedded)."""
    prefix = str(repo_root) + "/"
    qualified = node.qualified_name
    if qualified.startswith(prefix):
        qualified = qualified[len(prefix) :]
    parts = [qualified, node.kind.lower()]
    if node.params:
        parts.append(node.params)
    if node.return_type:
        parts.append(f"returns {node.return_type}")
    doc = docstring_for(node)
    if doc:
        parts.append(f"— {doc}")
    text = " ".join(" ".join(parts).split())
    return text if len(text) <= TEXT_CHARS else text[: TEXT_CHARS - 1] + "…"


def install_node_text(repo_root: Path = ROOT) -> None:
    """Replace ``code_review_graph.embeddings._node_to_text`` on the pinned release."""
    from conductor.crg_embedding_bridge import assert_supported_crg

    assert_supported_crg()
    import code_review_graph.embeddings as embeddings

    def workspace_node_text(node: Any) -> str:
        return node_text(node, repo_root)

    embeddings._node_to_text = workspace_node_text
