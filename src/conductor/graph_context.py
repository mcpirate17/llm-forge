#!/usr/bin/env python3
"""AST Subgraph & Signature Projection Utility.

Extracts token-minimal Python symbol signatures, docstrings, type hints,
and structural graph relationships (callers/callees) from code-review-graph and ASTs.
"""

from __future__ import annotations

import argparse
import ast
import contextlib
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Final

from conductor.candidate_review.git_source import repository_root
from conductor.project_paths import host_root

ROOT: Final[Path] = host_root()


class GraphContextError(RuntimeError):
    """AST or Graph context could not be extracted."""


@dataclass(frozen=True, slots=True)
class GraphRelationship:
    qualified_name: str
    kind: str
    file_path: str


@dataclass(frozen=True, slots=True)
class FileContextSummary:
    file_path: str
    skeleton: str
    symbols: list[str]
    callers: list[GraphRelationship]
    callees: list[GraphRelationship]
    graph_status: str = "ok"

    def to_dict(self) -> dict[str, Any]:
        return {
            "file_path": self.file_path,
            "symbols": self.symbols,
            "skeleton": self.skeleton,
            "graph_status": self.graph_status,
            "callers": [
                {
                    "qualified_name": c.qualified_name,
                    "kind": c.kind,
                    "file_path": c.file_path,
                }
                for c in self.callers
            ],
            "callees": [
                {
                    "qualified_name": c.qualified_name,
                    "kind": c.kind,
                    "file_path": c.file_path,
                }
                for c in self.callees
            ],
        }


class SignatureStubifier(ast.NodeTransformer):
    """Transform AST by replacing function and method implementation bodies with '...'."""

    def __init__(self, target_symbol: str | None = None):
        super().__init__()
        self.target_symbol = target_symbol
        self.found_symbols: list[str] = []

    def _stubify_body(self, docstring: str | None) -> list[ast.stmt]:
        body: list[ast.stmt] = []
        if docstring:
            body.append(ast.Expr(value=ast.Constant(value=docstring)))
        body.append(ast.Expr(value=ast.Constant(value=...)))
        return body

    def visit_FunctionDef(self, node: ast.FunctionDef) -> ast.AST | None:
        self.found_symbols.append(node.name)
        if self.target_symbol and node.name != self.target_symbol:
            return None

        docstring = ast.get_docstring(node)
        new_body = self._stubify_body(docstring)
        return ast.copy_location(
            ast.FunctionDef(
                name=node.name,
                args=node.args,
                body=new_body,
                decorator_list=node.decorator_list,
                returns=node.returns,
                type_comment=node.type_comment,
            ),
            node,
        )

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> ast.AST | None:
        self.found_symbols.append(node.name)
        if self.target_symbol and node.name != self.target_symbol:
            return None

        docstring = ast.get_docstring(node)
        new_body = self._stubify_body(docstring)
        return ast.copy_location(
            ast.AsyncFunctionDef(
                name=node.name,
                args=node.args,
                body=new_body,
                decorator_list=node.decorator_list,
                returns=node.returns,
                type_comment=node.type_comment,
            ),
            node,
        )

    def visit_ClassDef(self, node: ast.ClassDef) -> ast.AST | None:
        self.found_symbols.append(node.name)
        if self.target_symbol and node.name != self.target_symbol:
            has_matching_method = any(
                isinstance(stmt, (ast.FunctionDef, ast.AsyncFunctionDef))
                and stmt.name == self.target_symbol
                for stmt in node.body
            )
            if not has_matching_method:
                return None

        docstring = ast.get_docstring(node)
        new_body: list[ast.stmt] = []
        if docstring:
            new_body.append(ast.Expr(value=ast.Constant(value=docstring)))

        for item in node.body:
            if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef)):
                res = self.visit(item)
                if res is not None:
                    new_body.append(res)  # type: ignore[arg-type]
            elif isinstance(item, ast.AnnAssign):
                new_body.append(item)
            elif isinstance(item, ast.Assign):
                new_body.append(item)

        if not new_body:
            new_body.append(ast.Expr(value=ast.Constant(value=...)))

        return ast.copy_location(
            ast.ClassDef(
                name=node.name,
                bases=node.bases,
                keywords=node.keywords,
                body=new_body,
                decorator_list=node.decorator_list,
            ),
            node,
        )


def _stubify_assign(stmt: ast.Assign) -> ast.Assign:
    """Stubify complex module-level Assign values to '...' while preserving names."""
    # If the value is already a simple constant or literal, keep it; else replace with '...'
    if isinstance(stmt.value, ast.Constant):
        return stmt
    return ast.copy_location(
        ast.Assign(targets=stmt.targets, value=ast.Constant(value=...)), stmt
    )


def extract_ast_skeleton(
    source_code: str, target_symbol: str | None = None, file_path_hint: str = ""
) -> tuple[str, list[str]]:
    """Parse source code and return a stubified representation with bodies replaced by '...'."""
    try:
        tree = ast.parse(source_code)
    except SyntaxError as exc:
        raise GraphContextError(f"syntax error in source: {exc}") from exc

    stubifier = SignatureStubifier(target_symbol=target_symbol)
    new_tree = stubifier.visit(tree)
    ast.fix_missing_locations(new_tree)

    if target_symbol and target_symbol not in stubifier.found_symbols:
        hint = f" in {file_path_hint}" if file_path_hint else ""
        avail = (
            ", ".join(sorted(stubifier.found_symbols))
            if stubifier.found_symbols
            else "<none>"
        )
        raise GraphContextError(
            f"symbol {target_symbol!r} not found{hint}; available: {avail}"
        )

    filtered_body: list[ast.stmt] = []
    for stmt in new_tree.body:
        if isinstance(
            stmt,
            (
                ast.Import,
                ast.ImportFrom,
                ast.ClassDef,
                ast.FunctionDef,
                ast.AsyncFunctionDef,
                ast.AnnAssign,
            ),
        ):
            filtered_body.append(stmt)
        elif isinstance(stmt, ast.Assign) and not target_symbol:
            # Keep top-level constants / specs
            filtered_body.append(_stubify_assign(stmt))

    new_tree.body = filtered_body
    skeleton = ast.unparse(new_tree)
    return skeleton, stubifier.found_symbols


def _rel_path(repo: Path, path_str: str) -> str:
    """Make path relative to repository root if possible."""
    p = Path(path_str)
    if p.is_absolute():
        try:
            return str(p.relative_to(repo.resolve()))
        except ValueError:
            return path_str
    return path_str


def _ripgrep_hits(repo: Path, pattern: str) -> list[tuple[str, int]] | None:
    """``(file, line)`` hits from ripgrep, or ``None`` when it timed out."""
    try:
        proc = subprocess.run(
            ["rg", "-n", "--glob", "*.py", pattern, "."],
            cwd=repo,
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return None
    hits: list[tuple[str, int]] = []
    for line in proc.stdout.splitlines():
        parts = line.split(":", 2)
        if len(parts) >= 2:
            hits.append(
                (PurePosixPath(parts[0].lstrip("./")).as_posix(), int(parts[1]))
            )
    return hits


def _scan_hits(repo: Path, regex: re.Pattern[str]) -> list[tuple[str, int]]:
    """``(file, line)`` hits from a walk of ``*.py`` files under ``repo``.

    Hidden directories (``.git``, ``.venv``, ``.code-review-graph``) are pruned,
    matching ripgrep's default.
    """
    hits: list[tuple[str, int]] = []
    for dirpath, dirnames, filenames in os.walk(repo):
        dirnames[:] = sorted(name for name in dirnames if not name.startswith("."))
        for name in sorted(filenames):
            if not name.endswith(".py"):
                continue
            path = Path(dirpath, name)
            relative = path.relative_to(repo).as_posix()
            lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
            hits.extend(
                (relative, lineno)
                for lineno, line in enumerate(lines, 1)
                if regex.search(line)
            )
    return hits


def find_syntactic_callers(
    repo: Path, symbol_name: str, target_file_rel: str
) -> list[GraphRelationship]:
    """Call sites the graph index missed: ripgrep when installed, else a file scan."""
    norm_target = PurePosixPath(target_file_rel).as_posix()
    pattern = rf"\b{re.escape(symbol_name)}\("
    hits = _ripgrep_hits(repo, pattern) if shutil.which("rg") else None
    if hits is None:
        hits = _scan_hits(repo, re.compile(pattern))
    return [
        GraphRelationship(
            qualified_name=f"{file_path}:{lineno}",
            kind="calls (syntactic)",
            file_path=file_path,
        )
        for file_path, lineno in hits
        if file_path != norm_target and not file_path.startswith(".venv/")
    ]


def query_graph_relationships(
    repo: Path, file_path_str: str, target_symbol: str | None = None
) -> tuple[list[GraphRelationship], list[GraphRelationship], str]:
    """Query .code-review-graph/graph.db for immediate callers and callees."""
    db_path = repo / ".code-review-graph" / "graph.db"
    if not db_path.is_file():
        status = "unavailable (graph.db missing)"
        callers: list[GraphRelationship] = []
        if target_symbol:
            callers = find_syntactic_callers(repo, target_symbol, file_path_str)
        return callers, [], status

    abs_path = str((repo / file_path_str).resolve())
    norm_path = PurePosixPath(file_path_str).as_posix()

    callers: list[GraphRelationship] = []
    callees: list[GraphRelationship] = []
    seen_callers: set[str] = set()

    try:
        # `closing`, not a bare assignment and not `with conn:`. The connection was
        # closed on the last line of this block, so any failure while executing or
        # fetching -- a corrupt page, a schema written by a newer graph build, a
        # query interrupted mid-fetch -- jumped straight to the handler below and
        # leaked the handle. `with conn:` would not have fixed it either: that is a
        # TRANSACTION context, and it commits or rolls back without ever closing.
        with contextlib.closing(
            sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
        ) as conn:
            callers, callees = _read_relationships(
                conn, repo, abs_path, norm_path, target_symbol, seen_callers
            )
        status = "ok"
    except sqlite3.Error as exc:
        status = f"unavailable (sqlite error: {exc})"

    # Supplement with syntactic callers for symbols to close graph resolution gaps
    if target_symbol:
        for syn_rel in find_syntactic_callers(repo, target_symbol, file_path_str):
            if syn_rel.file_path not in seen_callers:
                callers.append(syn_rel)
                seen_callers.add(syn_rel.file_path)

    return callers, callees, status


def _read_relationships(
    conn: sqlite3.Connection,
    repo: Path,
    abs_path: str,
    norm_path: str,
    target_symbol: str | None,
    seen_callers: set[str],
) -> tuple[list[GraphRelationship], list[GraphRelationship]]:
    """Immediate callers and callees for one file, read from an open connection.

    Split out so the caller owns the connection's lifetime and nothing in here can
    return past a close.
    """
    callers: list[GraphRelationship] = []
    callees: list[GraphRelationship] = []
    cursor = conn.cursor()
    target_clause = "WHERE target.file_path IN (?, ?) "
    params: list[Any] = [abs_path, norm_path]
    if target_symbol:
        target_clause += "AND target.name = ? "
        params.append(target_symbol)

    caller_sql = f"""
        SELECT DISTINCT source.qualified_name, edge.kind, source.file_path
        FROM nodes AS target
        JOIN edges AS edge ON edge.target_qualified = target.qualified_name
        JOIN nodes AS source ON source.qualified_name = edge.source_qualified
        {target_clause}
        AND edge.kind != 'contains'
        ORDER BY source.qualified_name
        LIMIT 50
    """
    for qname, kind, fpath in cursor.execute(caller_sql, params).fetchall():
        rel_qname = _rel_path(repo, qname)
        rel_fpath = _rel_path(repo, fpath or "")
        seen_callers.add(rel_fpath)
        callers.append(
            GraphRelationship(qualified_name=rel_qname, kind=kind, file_path=rel_fpath)
        )

    source_clause = "WHERE source.file_path IN (?, ?) "
    callee_params: list[Any] = [abs_path, norm_path]
    if target_symbol:
        source_clause += "AND source.name = ? "
        callee_params.append(target_symbol)

    callee_sql = f"""
        SELECT DISTINCT target.qualified_name, edge.kind, target.file_path
        FROM nodes AS source
        JOIN edges AS edge ON edge.source_qualified = source.qualified_name
        JOIN nodes AS target ON target.qualified_name = edge.target_qualified
        {source_clause}
        AND edge.kind != 'contains'
        ORDER BY target.qualified_name
        LIMIT 50
    """
    for qname, kind, fpath in cursor.execute(callee_sql, callee_params).fetchall():
        rel_qname = _rel_path(repo, qname)
        rel_fpath = _rel_path(repo, fpath or "")
        callees.append(
            GraphRelationship(qualified_name=rel_qname, kind=kind, file_path=rel_fpath)
        )

    return callers, callees


def get_file_context(
    repo: Path,
    file_path: str,
    target_symbol: str | None = None,
    with_graph: bool = True,
) -> FileContextSummary:
    """Extract AST skeleton and graph relationships for a file."""
    src = repo / file_path
    if not src.is_file():
        raise GraphContextError(f"file not found: {file_path}")

    code = src.read_text(encoding="utf-8")
    skeleton, symbols = extract_ast_skeleton(
        code, target_symbol=target_symbol, file_path_hint=file_path
    )

    callers: list[GraphRelationship] = []
    callees: list[GraphRelationship] = []
    graph_status = "disabled"
    if with_graph:
        callers, callees, graph_status = query_graph_relationships(
            repo, file_path, target_symbol=target_symbol
        )

    return FileContextSummary(
        file_path=file_path,
        skeleton=skeleton,
        symbols=symbols,
        callers=callers,
        callees=callees,
        graph_status=graph_status,
    )


def _is_test_path(path: str) -> bool:
    """True only for genuine test files (test_*.py / *_test.py or a tests/ dir).

    A substring check would misbin every relationship of files like
    ``mutation_testing.py`` into Tested By.
    """
    parts = PurePosixPath(path).parts
    name = parts[-1] if parts else ""
    return (
        name.startswith("test_") or name.endswith("_test.py") or "tests" in parts[:-1]
    )


def format_markdown_context(summary: FileContextSummary) -> str:
    """Format file context summary as a compact Markdown block."""
    lines = [
        f"### AST Context: `{summary.file_path}`",
        "```python",
        summary.skeleton.strip(),
        "```",
    ]
    if summary.graph_status.startswith("unavailable"):
        lines.append(f"\n*Notice: code-review-graph {summary.graph_status}*")

    # Group callers and callees cleanly by role
    calls: list[str] = []
    called_by: list[str] = []
    tested_by: list[str] = []

    for c in summary.callers:
        label = f"`{c.qualified_name}`"
        if "test" in c.kind.lower() or _is_test_path(c.file_path):
            tested_by.append(label)
        else:
            called_by.append(f"{label} ({c.kind})")

    for c in summary.callees:
        label = f"`{c.qualified_name}`"
        if "test" in c.kind.lower() or _is_test_path(c.file_path):
            tested_by.append(label)
        else:
            calls.append(f"{label} ({c.kind})")

    if called_by:
        lines.append("\n**Called By (Inbound Call Sites):**")
        for item in called_by[:15]:
            lines.append(f"- {item}")
    if calls:
        lines.append("\n**Calls (Outbound Dependencies):**")
        for item in calls[:15]:
            lines.append(f"- {item}")
    if tested_by:
        lines.append("\n**Tested By (Test Suites / Invariants):**")
        for item in set(tested_by[:15]):
            lines.append(f"- {item}")

    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("file_path", help="Path to Python source file")
    parser.add_argument("--symbol", help="Target specific symbol name")
    parser.add_argument(
        "--no-graph",
        dest="with_graph",
        action="store_false",
        default=True,
        help="Omit graph relationships",
    )
    parser.add_argument(
        "--json", action="store_true", help="Output machine-readable JSON"
    )
    parser.add_argument("--repo", default=str(ROOT), help="Repository root")

    args = parser.parse_args(argv)
    repo = repository_root(Path(args.repo))
    try:
        summary = get_file_context(
            repo,
            file_path=args.file_path,
            target_symbol=args.symbol,
            with_graph=args.with_graph,
        )
        if args.json:
            print(json.dumps(summary.to_dict(), indent=2))
        else:
            print(format_markdown_context(summary))
        return 0
    except GraphContextError as exc:
        print(f"graph-context ERROR: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
