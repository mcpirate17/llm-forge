"""Test selection for a candidate: graph-selected and convention-matched pytest files.

Split out of ``verification.py`` (which owns the evidence checks) so each stays
under the module size bar. The graph query reads the code-review-graph SQLite
store read-only and fails closed when its head does not match the candidate.
"""

from __future__ import annotations

import ast
import sqlite3
from pathlib import Path, PurePosixPath
from typing import Sequence

from conductor.candidate_review.checks import ReviewContext
from conductor.candidate_review.model import sha256_json
from conductor.project_paths import package_relative


def _graph_database(repo: Path) -> Path:
    return repo / ".code-review-graph" / "graph.db"


def _graph_test_paths(
    ctx: ReviewContext, source_paths: Sequence[str]
) -> tuple[set[str], dict[str, object]]:
    database = _graph_database(ctx.repo)
    if not database.is_file():
        raise RuntimeError("code-review graph database is missing")
    uri = f"file:{database.as_posix()}?mode=ro&immutable=1"
    connection = sqlite3.connect(uri, uri=True, timeout=2.0)
    try:
        metadata = dict(connection.execute("SELECT key, value FROM metadata"))
        expected = (
            ctx.candidate.base_commit_oid
            if ctx.candidate.kind == "index"
            else ctx.candidate.commit_oid
        )
        if not expected or metadata.get("git_head_sha") != expected:
            raise RuntimeError(
                "stale code-review graph: "
                f"expected {expected}, found {metadata.get('git_head_sha')}"
            )
        absolute = [str((ctx.repo / path).resolve()) for path in source_paths]
        if not absolute:
            return set(), {
                "head_sha": expected,
                "schema_version": metadata.get("schema_version"),
            }
        placeholders = ",".join("?" for _ in absolute)
        rows = connection.execute(
            f"""
            SELECT DISTINCT source.file_path, edge.kind, target.qualified_name
            FROM nodes AS target
            JOIN edges AS edge ON edge.target_qualified = target.qualified_name
            JOIN nodes AS source ON source.qualified_name = edge.source_qualified
            WHERE target.file_path IN ({placeholders}) AND source.is_test = 1
            ORDER BY source.file_path, edge.kind, target.qualified_name
            """,
            absolute,
        ).fetchall()
        tests: set[str] = set()
        evidence_rows: list[tuple[str, str, str]] = []
        for file_path, edge_kind, target in rows:
            try:
                relative = Path(file_path).resolve().relative_to(ctx.repo).as_posix()
            except ValueError:
                continue
            # Rust `#[test]` nodes are is_test too; they run under cargo test, not pytest.
            if relative.endswith(".py") and (ctx.snapshot / relative).is_file():
                tests.add(relative)
                evidence_rows.append((relative, edge_kind, target))
        graph = {
            "head_sha": expected,
            "schema_version": metadata.get("schema_version"),
            "last_updated": metadata.get("last_updated"),
            "selected_edges": len(evidence_rows),
            "evidence_sha256": sha256_json(evidence_rows),
        }
        return tests, graph
    finally:
        connection.close()


def _public_names(module: Path) -> set[str]:
    """Public top-level names of ``module``: its ``__all__`` when present, else
    every top-level binding not prefixed with an underscore."""
    try:
        tree = ast.parse(module.read_text(encoding="utf-8"))
    except (OSError, SyntaxError, UnicodeDecodeError):
        return set()
    for node in tree.body:
        if not isinstance(node, ast.Assign):
            continue
        if not any(isinstance(t, ast.Name) and t.id == "__all__" for t in node.targets):
            continue
        if isinstance(node.value, (ast.List, ast.Tuple, ast.Set)):
            return {
                element.value
                for element in node.value.elts
                if isinstance(element, ast.Constant) and isinstance(element.value, str)
            }
    names: set[str] = set()
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            names.add(node.name)
        elif isinstance(node, ast.Assign):
            names.update(t.id for t in node.targets if isinstance(t, ast.Name))
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            names.add(node.target.id)
    return {name for name in names if not name.startswith("_")}


def _reexport_surfaces(
    ctx: ReviewContext, source_paths: Sequence[str]
) -> dict[str, set[str]]:
    """Map package module string -> names its ``__init__`` re-exports from a changed file.

    The code-review graph does not resolve ``from .module import name`` inside a
    package ``__init__`` (those files carry no outbound edge), so a test importing
    through the re-export yields no edge into the changed file and the
    ``test_<stem>.py`` convention cannot match either. Without this the adapters
    under ``component_fab/*/adaptation.py`` select zero tests despite being
    covered by five existing ones.
    """
    surfaces: dict[str, set[str]] = {}
    for source in source_paths:
        path = PurePosixPath(source)
        if path.suffix != ".py" or path.name == "__init__.py":
            continue
        init = ctx.snapshot / path.parent / "__init__.py"
        if not init.is_file():
            continue
        try:
            tree = ast.parse(init.read_text(encoding="utf-8"))
        except (OSError, SyntaxError, UnicodeDecodeError):
            continue
        names: set[str] = set()
        for node in ast.walk(tree):
            # ast.walk descends into Try/If bodies, so conditional and
            # try/except re-exports are covered.
            if (
                not isinstance(node, ast.ImportFrom)
                or node.level != 1
                or node.module != path.stem
            ):
                continue
            for alias in node.names:
                if alias.name == "*":
                    # `from .module import *` names nothing here; the re-exported
                    # surface is the changed module's own public API. Missing it
                    # would under-select, which reads as a pass.
                    names |= _public_names(ctx.snapshot / source)
                else:
                    names.add(alias.asname or alias.name)
        if names:
            surfaces.setdefault(path.parent.as_posix().replace("/", "."), set()).update(
                names
            )
    return surfaces


def _imports_reexport(text: str, surfaces: dict[str, set[str]]) -> bool:
    """True when ``text`` imports a re-exported name from its owning package."""
    if not surfaces:
        return False
    try:
        tree = ast.parse(text)
    except (SyntaxError, ValueError):
        return False
    for node in ast.walk(tree):
        if not isinstance(node, ast.ImportFrom) or node.level or not node.module:
            continue
        names = surfaces.get(node.module)
        if names and any((alias.asname or alias.name) in names for alias in node.names):
            return True
    return False


def _rust_crate_tests(
    ctx: ReviewContext, source_paths: Sequence[str]
) -> dict[str, tuple[str, ...]]:
    """Each changed Rust source mapped to the test files of its own crate.

    Every other selector here is Python-only: it matches ``test_<stem>.py``
    names and walks Python import edges. A crate whose tests are ``#[cfg(test)]``
    modules or files under ``tests/`` therefore selected nothing, so a change
    confined to Rust reported "no targeted tests" while its own suite sat beside
    it -- structural for a project whose compute is deliberately native.

    Returned per source rather than as one set, so a change that touches Python
    as well is still answerable for the Python half. These paths are evidence
    that the changed code is tested, not work for the pytest runner, so the
    caller keeps them out of the executed set.
    """

    crates: dict[str, PurePosixPath] = {}
    for source in source_paths:
        path = PurePosixPath(source)
        if path.suffix != ".rs":
            continue
        for parent in path.parents:
            if (ctx.snapshot / parent / "Cargo.toml").is_file():
                crates[source] = parent
                break
    cache: dict[PurePosixPath, tuple[str, ...]] = {}
    covered: dict[str, tuple[str, ...]] = {}
    for source, crate in crates.items():
        files = cache.get(crate)
        if files is None:
            files = _crate_test_files(ctx, crate)
            cache[crate] = files
        if files:
            covered[source] = files
    return covered


def _crate_test_files(ctx: ReviewContext, crate: PurePosixPath) -> tuple[str, ...]:
    """The Rust files in ``crate`` that carry tests, in path order."""

    tests: list[str] = []
    for candidate in sorted((ctx.snapshot / crate).rglob("*.rs")):
        rel = candidate.relative_to(ctx.snapshot).as_posix()
        # target/ is build output: a vendored dependency's test modules there
        # would report coverage this crate does not have.
        if "/target/" in f"/{rel}":
            continue
        if candidate.parent == ctx.snapshot / crate / "tests":
            tests.append(rel)
            continue
        try:
            text = candidate.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        if "#[cfg(test)]" in text:
            tests.append(rel)
    return tuple(tests)


def _module_names(root: Path, source_paths: Sequence[str]) -> list[str]:
    """Dotted import names for changed sources, as a test file would spell them.

    A repo-relative path is the import path only when the package sits at the repo
    root. Under a src layout ``src/conductor/widget.py`` imports as
    ``conductor.widget``, and dotting the raw path yields ``src.conductor.widget``,
    which no test file contains -- so every convention match by module name is lost.
    """

    prefix = package_relative(root).parent.as_posix()
    names = []
    for path in source_paths:
        rel = path
        if prefix != "." and path.startswith(f"{prefix}/"):
            rel = path[len(prefix) + 1 :]
        names.append(rel.removesuffix(".py").replace("/", "."))
    return names


def _convention_tests(ctx: ReviewContext, source_paths: Sequence[str]) -> set[str]:
    names = {f"test_{PurePosixPath(path).stem}.py" for path in source_paths}
    modules = _module_names(ctx.snapshot, source_paths)
    surfaces = _reexport_surfaces(ctx, source_paths)
    tests: set[str] = set()
    # The package's own tests sit beside it, wherever the candidate declares it --
    # the repo root in the monorepo, src/conductor here. A hardcoded "conductor"
    # would silently find no test file at all under any other layout, which reads
    # as "no convention test exists" rather than as a layout mismatch.
    for base in (
        package_relative(ctx.snapshot).as_posix(),
        "research/tests",
        "component_fab/tests",
        "aria_core/tests",
        "aria_designer/tests",
    ):
        root = ctx.snapshot / base
        if not root.is_dir():
            continue
        for path in root.rglob("test*.py"):
            rel = path.relative_to(ctx.snapshot).as_posix()
            if path.name in names:
                tests.add(rel)
                continue
            try:
                text = path.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                continue
            if any(module in text for module in modules) or _imports_reexport(
                text, surfaces
            ):
                tests.add(rel)
    return tests
