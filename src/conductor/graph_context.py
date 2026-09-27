#!/usr/bin/env python3
"""AST Subgraph & Signature Projection Utility.

Extracts token-minimal Python symbol signatures, docstrings, type hints,
and structural graph relationships (callers/callees) from code-review-graph and ASTs.
"""

from __future__ import annotations

import argparse
import ast
import json
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Final

from conductor._native import graph_context_native
from conductor.candidate_review.git_source import repository_root
from conductor.project_paths import host_root

ROOT: Final[Path] = host_root()


def _graph_native(operation: str, payload: dict[str, Any]) -> Any:
    """Decode a deterministic graph decision from the native core."""
    return json.loads(graph_context_native(operation, json.dumps(payload)))


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

    def _visit_function(
        self, node: ast.FunctionDef | ast.AsyncFunctionDef
    ) -> ast.AST | None:
        self.found_symbols.append(node.name)
        if self.target_symbol and node.name != self.target_symbol:
            return None

        docstring = ast.get_docstring(node)
        new_body = self._stubify_body(docstring)
        return ast.copy_location(
            type(node)(
                name=node.name,
                args=node.args,
                body=new_body,
                decorator_list=node.decorator_list,
                returns=node.returns,
                type_comment=node.type_comment,
            ),
            node,
        )

    def visit_FunctionDef(self, node: ast.FunctionDef) -> ast.AST | None:
        return self._visit_function(node)

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> ast.AST | None:
        return self._visit_function(node)

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
            elif isinstance(item, (ast.AnnAssign, ast.Assign)):
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


def _relations(rows: list[dict[str, str]]) -> list[GraphRelationship]:
    return [GraphRelationship(**row) for row in rows]


def find_syntactic_callers(
    repo: Path, symbol_name: str, target_file_rel: str
) -> list[GraphRelationship]:
    """Find syntax-only callers through the native bounded scanner."""
    rows = _graph_native(
        "syntactic_callers",
        {
            "repo": str(repo),
            "symbol_name": symbol_name,
            "target_file_rel": target_file_rel,
        },
    )
    return _relations(rows)


def query_graph_relationships(
    repo: Path, file_path_str: str, target_symbol: str | None = None
) -> tuple[list[GraphRelationship], list[GraphRelationship], str]:
    """Read graph edges and syntax-only caller fallback through the native core."""
    result = _graph_native(
        "relationships",
        {
            "repo": str(repo),
            "file_path": file_path_str,
            "target_symbol": target_symbol,
        },
    )
    return (
        _relations(result["callers"]),
        _relations(result["callees"]),
        result["status"],
    )


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
    """Classify a graph relationship path using native policy."""
    return bool(_graph_native("is_test_path", {"path": path}))


def format_markdown_context(summary: FileContextSummary) -> str:
    """Format file context with native role classification and output bounds."""
    return str(_graph_native("markdown", summary.to_dict()))


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
