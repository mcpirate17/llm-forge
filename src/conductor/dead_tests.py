#!/usr/bin/env python3
"""Find dead tests and unreferenced sources by first-party import closure.

Signals, all computed from ``git ls-files`` plus an AST pass over every tracked
Python file (no imports are executed):

* ``broken`` — a test whose transitive first-party import closure contains a
  module-level, unguarded import of a module that no longer exists. Such a
  test fails at collection; it guards nothing.
* ``depends_on_untracked`` — a test whose closure imports a module that exists
  on disk but is not tracked by git. It cannot pass on a clean checkout.
* ``stale_imports`` — modules whose guarded or function-level imports name a
  first-party module that no longer exists: dead code paths in sources.
* ``orphan_target`` — a test whose direct first-party targets are reachable
  only from tests: no non-test module imports them and no config surface
  (Makefile, CI, pyproject, hooks, conductor policies) names them. The test
  and its targets are a retire-together candidate; ``notes_only`` marks
  targets that research notes still mention by name.
* ``unreferenced_sources`` — non-test modules nobody imports or names at all
  (entrypoints included), so stale one-off runners surface next to their tests.

Mutation-campaign manifests are deliberately not a reference surface: being
mutated is not evidence that production code uses a module.
"""

from __future__ import annotations

import argparse
import ast
import json
import re
import subprocess
import sys
from collections import defaultdict, deque
from dataclasses import dataclass, field
from pathlib import Path, PurePosixPath
from typing import Sequence

from conductor.audit_root import (
    AuditRootError,
    print_audit_provenance,
    resolve_audit_root,
)

ROOT = Path(__file__).resolve().parents[1]
TEST_NAME = re.compile(r"^(test_.*|.*_test)\.py$")
DOTTED = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_]*)+$")
ISO_DATE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
NATIVE_SUFFIXES = (".pyx", ".rs", ".cpp", ".cc", ".cu", ".c", ".so", ".pyd")
CONFIG_SURFACES = frozenset(
    {
        "Makefile",
        "pyproject.toml",
        "package.json",
        ".pre-commit-config.yaml",
        ".agent_hooks",
        "README.md",
        "AGENTS.md",
        "CLAUDE.md",
    }
)
CONFIG_PREFIXES = (".github/", ".claude/", "conductor/")
CONFIG_SUFFIXES = (".toml", ".json", ".yml", ".yaml", ".sh", ".md", ".txt")
CONFIG_EXCLUDED_PREFIXES = ("conductor/mutation_campaigns/",)
NOTES_PREFIX = "research/notes/"


class DeadTestsError(RuntimeError):
    """Raised when the scan cannot produce a trustworthy result."""


@dataclass
class Module:
    path: str
    has_main: bool = False
    basenames: set[str] = field(default_factory=set)
    deps: set[str] = field(default_factory=set)
    missing: set[str] = field(default_factory=set)
    soft_missing: set[str] = field(default_factory=set)
    untracked: set[str] = field(default_factory=set)


def _git(*args: str, root: Path = ROOT) -> str:
    completed = subprocess.run(
        ["git", *args], cwd=root, capture_output=True, text=True, check=False
    )
    if completed.returncode != 0:
        raise DeadTestsError(f"git {' '.join(args)} failed: {completed.stderr.strip()}")
    return completed.stdout


def tracked_files(*, root: Path = ROOT) -> list[str]:
    return [line for line in _git("ls-files", "-z", root=root).split("\0") if line]


def untracked_tests(*, root: Path = ROOT) -> list[str]:
    lines = _git("ls-files", "--others", "--exclude-standard", "-z", root=root).split(
        "\0"
    )
    return sorted(p for p in lines if p.endswith(".py") and is_test_file(p))


def last_commit_dates(*, root: Path = ROOT) -> dict[str, str]:
    dates: dict[str, str] = {}
    current = ""
    log = _git(
        "log", "--format=%ad", "--date=short", "--name-only", "--no-merges", root=root
    )
    for line in log.splitlines():
        if not line:
            continue
        if ISO_DATE.match(line):
            current = line
        else:
            dates.setdefault(line, current)
    return dates


def is_test_file(path: str) -> bool:
    return bool(TEST_NAME.match(PurePosixPath(path).name))


def is_test_side(path: str) -> bool:
    parts = PurePosixPath(path).parts
    return is_test_file(path) or parts[-1] == "conftest.py" or "tests" in parts[:-1]


class Resolver:
    """Map dotted module names to tracked file paths without importing."""

    def __init__(self, tracked: Sequence[str], *, root: Path = ROOT) -> None:
        self.root = root
        self.files = {p for p in tracked if p.endswith(".py")}
        self.first_party = {PurePosixPath(p).parts[0] for p in self.files if "/" in p}
        self.dirs = {
            str(parent) for p in self.files for parent in PurePosixPath(p).parents
        }
        self.native = {
            p[: -len(PurePosixPath(p).suffix)]
            for p in tracked
            if p.endswith(NATIVE_SUFFIXES)
        }

    def _bases(self, from_file: str) -> list[str]:
        """Repo root first, then each ancestor of the importer.

        The ancestor walk covers ``cd research && pytest`` layouts and the
        ``sys.path[0]`` sibling imports that bare script invocations rely on.
        """
        parent = PurePosixPath(from_file).parent
        bases = [""]
        for ancestor in (parent, *parent.parents):
            bases.append("" if str(ancestor) == "." else f"{ancestor}/")
        return bases

    def resolve(self, dotted: str, from_file: str) -> str | None:
        rel = dotted.replace(".", "/")
        for base in self._bases(from_file):
            for suffix in (".py", "/__init__.py"):
                candidate = f"{base}{rel}{suffix}"
                if candidate in self.files:
                    return candidate
        return None

    def resolve_untracked(self, dotted: str, from_file: str) -> str | None:
        rel = dotted.replace(".", "/")
        for base in self._bases(from_file):
            for suffix in (".py", "/__init__.py"):
                candidate = f"{base}{rel}{suffix}"
                if (self.root / candidate).is_file():
                    return candidate
        return None

    def is_satisfied(self, dotted: str, from_file: str) -> bool:
        """True for namespace packages and native extension modules."""
        rel = dotted.replace(".", "/")
        for base in self._bases(from_file):
            if f"{base}{rel}" in self.dirs or f"{base}{rel}" in self.native:
                return True
        return False

    def is_first_party(self, dotted: str) -> bool:
        return dotted.split(".", 1)[0] in self.first_party


def _relative_base(path: str, level: int) -> str:
    parts = list(PurePosixPath(path).parent.parts)
    kept = parts[: max(0, len(parts) - (level - 1))]
    return ".".join(kept)


def _is_type_checking(test: ast.expr) -> bool:
    if isinstance(test, ast.Name):
        return test.id == "TYPE_CHECKING"
    return isinstance(test, ast.Attribute) and test.attr == "TYPE_CHECKING"


def _is_main_guard(test: ast.expr) -> bool:
    if not isinstance(test, ast.Compare):
        return False
    left = test.left
    return isinstance(left, ast.Name) and left.id == "__name__"


class _ImportCollector(ast.NodeVisitor):
    """Collect imports, classifying module-level unguarded ones as hard."""

    def __init__(self, path: str) -> None:
        self.path = path
        self.hard: set[tuple[str, str | None]] = set()
        self.soft: set[tuple[str, str | None]] = set()
        self.strings: set[str] = set()
        self.basenames: set[str] = set()
        self.has_main = False
        self._guard_depth = 0

    def _add(self, base: str, attr: str | None) -> None:
        (self.soft if self._guard_depth else self.hard).add((base, attr))

    def _guarded(self, node: ast.AST) -> None:
        self._guard_depth += 1
        self.generic_visit(node)
        self._guard_depth -= 1

    def visit_Import(self, node: ast.Import) -> None:
        for alias in node.names:
            self._add(alias.name, None)

    def visit_ImportFrom(self, node: ast.ImportFrom) -> None:
        base = node.module or ""
        if node.level:
            prefix = _relative_base(self.path, node.level)
            base = f"{prefix}.{base}".strip(".") if base else prefix
        for alias in node.names:
            self._add(base, alias.name)

    def visit_Constant(self, node: ast.Constant) -> None:
        if not isinstance(node.value, str):
            return
        if node.value.endswith(".py"):
            if "/" not in node.value:
                self.basenames.add(node.value)
        elif DOTTED.match(node.value):
            self.strings.add(node.value)

    def visit_If(self, node: ast.If) -> None:
        main_guard = _is_main_guard(node.test)
        self.has_main = self.has_main or main_guard
        if main_guard or _is_type_checking(node.test):
            self._guarded(node)
        else:
            self.generic_visit(node)

    visit_Try = _guarded
    visit_TryStar = _guarded
    visit_FunctionDef = _guarded
    visit_AsyncFunctionDef = _guarded


def _record(
    module: Module, resolver: Resolver, base: str, attr: str | None, hard: bool
) -> None:
    if attr and attr != "*":
        sub = resolver.resolve(f"{base}.{attr}" if base else attr, module.path)
        if sub:
            module.deps.add(sub)
            return
    if not base:
        return
    resolved = resolver.resolve(base, module.path)
    if resolved:
        module.deps.add(resolved)
    elif resolver.is_first_party(base) and not resolver.is_satisfied(base, module.path):
        on_disk = resolver.resolve_untracked(base, module.path)
        if on_disk:
            module.untracked.add(on_disk)
        elif hard:
            module.missing.add(base)
        else:
            module.soft_missing.add(base)


def scan_module(path: str, resolver: Resolver, *, root: Path = ROOT) -> Module:
    source = (root / path).read_bytes()
    try:
        tree = ast.parse(source, filename=path)
    except SyntaxError as exc:
        raise DeadTestsError(f"{path} does not parse: {exc}") from exc
    collector = _ImportCollector(path)
    collector.visit(tree)
    module = Module(
        path=path, has_main=collector.has_main, basenames=collector.basenames
    )
    for base, attr in collector.hard:
        _record(module, resolver, base, attr, hard=True)
    for base, attr in collector.soft:
        _record(module, resolver, base, attr, hard=False)
    for dotted in collector.strings:
        resolved = resolver.resolve(dotted, path)
        if resolved:
            module.deps.add(resolved)
    module.deps.discard(path)
    return module


def _read_text(path: str, *, root: Path = ROOT) -> str:
    try:
        return (root / path).read_text(encoding="utf-8", errors="replace")
    except OSError as exc:
        raise DeadTestsError(f"cannot read {path}: {exc}") from exc


def _is_config_surface(path: str) -> bool:
    if path.startswith(CONFIG_EXCLUDED_PREFIXES):
        return False
    if path in CONFIG_SURFACES or path.endswith((".sh", ".yml", ".yaml")):
        return True
    return path.startswith(CONFIG_PREFIXES) and path.endswith(CONFIG_SUFFIXES)


def _on_disk(prefix: str, suffixes: tuple[str, ...], *, root: Path = ROOT) -> list[str]:
    """Files under ``prefix`` present on disk, tracked or not.

    Research notes are mirrored to Obsidian and mostly untracked; hook
    settings under ``.claude`` are untracked. Both still name modules.
    """
    base = root / prefix
    if not base.is_dir():
        return []
    return sorted(
        str(p.relative_to(root))
        for p in base.rglob("*")
        if p.is_file() and p.suffix in suffixes and "__pycache__" not in p.parts
    )


def config_corpus(tracked: Sequence[str], *, root: Path = ROOT) -> tuple[str, str]:
    config_paths = {p for p in tracked if _is_config_surface(p)}
    config_paths.update(_on_disk(".claude", CONFIG_SUFFIXES, root=root))
    notes_paths = _on_disk(NOTES_PREFIX, (".md",), root=root)
    config_text = "\n".join(_read_text(p, root=root) for p in sorted(config_paths))
    notes_text = "\n".join(_read_text(p, root=root) for p in notes_paths)
    return config_text, notes_text


def _dotted_name(path: str) -> str:
    stem = path[:-3] if path.endswith(".py") else path
    for package_marker in ("/__init__", "/__main__"):
        if stem.endswith(package_marker):
            stem = stem[: -len(package_marker)]
    return stem.replace("/", ".")


def _referenced(path: str, corpus: str) -> bool:
    return path in corpus or _dotted_name(path) in corpus


def closure(start: str, modules: dict[str, Module]) -> tuple[set[str], set[str]]:
    """Return (hard-missing names, untracked dep paths) over the import closure."""
    seen: set[str] = set()
    missing: set[str] = set()
    untracked: set[str] = set()
    queue: deque[str] = deque([start])
    while queue:
        current = queue.popleft()
        if current in seen:
            continue
        seen.add(current)
        module = modules[current]
        missing.update(module.missing)
        untracked.update(module.untracked)
        queue.extend(d for d in module.deps if d not in seen)
    return missing, untracked


def _classify_tests(
    tests: Sequence[str],
    modules: dict[str, Module],
    reachable: "callable[[str], bool]",
    notes_text: str,
    dates: dict[str, str],
) -> tuple[list[dict[str, object]], list[dict[str, object]], list[dict[str, object]]]:
    broken: list[dict[str, object]] = []
    on_untracked: list[dict[str, object]] = []
    orphan: list[dict[str, object]] = []
    for test in tests:
        missing, untracked = closure(test, modules)
        if missing:
            broken.append(
                {
                    "test": test,
                    "missing": sorted(missing),
                    "last_commit": dates.get(test),
                }
            )
            continue
        if untracked:
            on_untracked.append(
                {
                    "test": test,
                    "untracked": sorted(untracked),
                    "last_commit": dates.get(test),
                }
            )
        targets = sorted(d for d in modules[test].deps if not is_test_side(d))
        if targets and not any(reachable(t) for t in targets):
            orphan.append(
                {
                    "test": test,
                    "targets": targets,
                    "notes_only": sorted(
                        t for t in targets if _referenced(t, notes_text)
                    ),
                    "last_commit": dates.get(test),
                }
            )
    return broken, on_untracked, orphan


def analyse(tracked: Sequence[str], *, root: Path = ROOT) -> dict[str, object]:
    resolver = Resolver(tracked, root=root)
    py_files = sorted(resolver.files)
    modules = {p: scan_module(p, resolver, root=root) for p in py_files}
    importers: dict[str, set[str]] = defaultdict(set)
    for module in modules.values():
        for dep in module.deps:
            importers[dep].add(module.path)
    config_text, notes_text = config_corpus(tracked, root=root)
    dates = last_commit_dates(root=root)
    dynamic_basenames = {
        name
        for module in modules.values()
        if not is_test_side(module.path)
        for name in module.basenames
    }
    untracked_importers: dict[str, list[str]] = defaultdict(list)
    for module in modules.values():
        for path in module.untracked:
            untracked_importers[path].append(module.path)

    def reachable(path: str) -> bool:
        non_test = any(not is_test_side(i) for i in importers[path])
        dynamic = PurePosixPath(path).name in dynamic_basenames
        return non_test or dynamic or _referenced(path, config_text)

    tests = [p for p in py_files if is_test_file(p)]
    broken, on_untracked, orphan = _classify_tests(
        tests, modules, reachable, notes_text, dates
    )
    stale_imports = [
        {
            "module": p,
            "missing": sorted(modules[p].soft_missing),
            "last_commit": dates.get(p),
        }
        for p in py_files
        if modules[p].soft_missing
    ]
    sources = [
        {
            "path": p,
            "has_main": modules[p].has_main,
            "notes_only": _referenced(p, notes_text),
            "last_commit": dates.get(p),
        }
        for p in py_files
        if not is_test_side(p)
        and not importers[p]
        and not p.endswith("__init__.py")
        and not reachable(p)
    ]
    return {
        "tests_scanned": len(tests),
        "modules_scanned": len(py_files),
        "broken": broken,
        "depends_on_untracked": on_untracked,
        "untracked_importers": {
            k: sorted(v) for k, v in sorted(untracked_importers.items())
        },
        "stale_imports": stale_imports,
        "orphan_target": orphan,
        "untracked_tests": untracked_tests(root=root),
        "unreferenced_sources": sources,
    }


def _rows(report: dict[str, object], key: str) -> list[dict[str, object]]:
    rows = report[key]
    assert isinstance(rows, list)
    return rows


def _print_summary(report: dict[str, object]) -> None:
    counts = " ".join(
        f"{key}={len(_rows(report, key))}"
        for key in (
            "broken",
            "depends_on_untracked",
            "stale_imports",
            "orphan_target",
            "untracked_tests",
            "unreferenced_sources",
        )
    )
    print(f"Dead tests: scanned={report['tests_scanned']} {counts}")
    for row in _rows(report, "broken"):
        print(
            f"BROKEN {row['test']} ({row['last_commit']}): missing {', '.join(row['missing'])}"
        )
    for row in _rows(report, "depends_on_untracked"):
        print(
            f"UNTRACKED-DEP {row['test']} ({row['last_commit']}): {', '.join(row['untracked'])}"
        )
    importers = report["untracked_importers"]
    assert isinstance(importers, dict)
    for path, owners in importers.items():
        print(f"UNTRACKED-SOURCE {path}: imported by {', '.join(owners)}")
    for row in _rows(report, "stale_imports"):
        print(
            f"STALE-IMPORT {row['module']} ({row['last_commit']}): {', '.join(row['missing'])}"
        )
    for row in _rows(report, "orphan_target"):
        print(
            f"ORPHAN {row['test']} ({row['last_commit']}): targets {', '.join(row['targets'])}"
        )
    for path in _rows(report, "untracked_tests"):
        print(f"UNTRACKED-TEST {path}")
    for row in _rows(report, "unreferenced_sources"):
        flags = "main" if row["has_main"] else "lib"
        if row["notes_only"]:
            flags += ",notes-only"
        print(f"UNREFERENCED {row['path']} ({row['last_commit']}) [{flags}]")


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--json-out",
        type=Path,
        default=Path("tasks/audit/dead_tests.json"),
        help="Relative paths resolve against --root (or its default).",
    )
    parser.add_argument(
        "--check", action="store_true", help="exit 1 when any broken test exists"
    )
    parser.add_argument(
        "--root",
        help=(
            "Repository tree to scan. Defaults to the Git worktree containing "
            "the current working directory, never the checkout that supplied "
            "the imported conductor module."
        ),
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        root = resolve_audit_root(args.root)
    except AuditRootError as exc:
        print(f"ERROR: dead-tests: {exc}", file=sys.stderr)
        return 2
    print_audit_provenance("dead-tests", root)
    try:
        report = analyse(tracked_files(root=root), root=root)
    except DeadTestsError as exc:
        print(f"Dead-test scan incomplete: {exc}", file=sys.stderr)
        return 2
    json_out = root / args.json_out
    json_out.parent.mkdir(parents=True, exist_ok=True)
    json_out.write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    _print_summary(report)
    return 1 if args.check and _rows(report, "broken") else 0


if __name__ == "__main__":
    raise SystemExit(main())
