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
from collections.abc import Sequence
from dataclasses import dataclass, field
from pathlib import Path, PurePosixPath
from typing import NoReturn

from conductor._native import (
    DeadTestsAnalysisNative,
    DeadTestsResolverNative,
    dead_tests_closure_native,
)

from conductor.audit_root import (
    AuditRootError,
    print_audit_provenance,
    resolve_audit_root,
)
from conductor.project_paths import DEFAULT_MUTATION_REGISTRY, notes_relative

from conductor.project_paths import host_root
ROOT = host_root()
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
CONFIG_EXCLUDED_PREFIXES = (f"{DEFAULT_MUTATION_REGISTRY.parent}/",)


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
        self._native = DeadTestsResolverNative(str(root), list(tracked))
        # Preserve the longstanding public inspection attributes while the
        # resolver operations themselves are owned by Rust.
        self.files = set(self._native.files())
        self.first_party = set(self._native.first_party())
        self.dirs = set(self._native.directories())
        self.native = set(self._native.native_modules())

    def resolve(self, dotted: str, from_file: str) -> str | None:
        return self._native.resolve(dotted, from_file)

    def resolve_untracked(self, dotted: str, from_file: str) -> str | None:
        return self._native.resolve_untracked(dotted, from_file)

    def is_satisfied(self, dotted: str, from_file: str) -> bool:
        """True for namespace packages and native extension modules."""
        return self._native.is_satisfied(dotted, from_file)

    def is_first_party(self, dotted: str) -> bool:
        return self._native.is_first_party(dotted)


def _raise_parse_error(exc: ValueError, *, root: Path) -> NoReturn:
    message = str(exc)
    path, marker, _detail = message.partition(" does not parse: ")
    if marker:
        try:
            ast.parse((root / path).read_bytes(), filename=path)
        except SyntaxError as syntax:
            raise DeadTestsError(f"{path} does not parse: {syntax}") from syntax
    raise DeadTestsError(message) from exc


def scan_module(path: str, resolver: Resolver, *, root: Path = ROOT) -> Module:
    try:
        payload = json.loads(resolver._native.scan_module(path, str(root)))
    except ValueError as exc:
        _raise_parse_error(exc, root=root)
    return Module(
        path=payload["path"],
        has_main=payload["has_main"],
        basenames=set(payload["basenames"]),
        deps=set(payload["deps"]),
        missing=set(payload["missing"]),
        soft_missing=set(payload["soft_missing"]),
        untracked=set(payload["untracked"]),
    )


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
    # The knowledge tree per this root's own configuration, not a monorepo literal.
    notes_paths = _on_disk(f"{notes_relative(root)}/", (".md",), root=root)
    config_text = "\n".join(_read_text(p, root=root) for p in sorted(config_paths))
    notes_text = "\n".join(_read_text(p, root=root) for p in notes_paths)
    return config_text, notes_text


def closure(start: str, modules: dict[str, Module]) -> tuple[set[str], set[str]]:
    """Return (hard-missing names, untracked dep paths) over the import closure."""
    payload = {
        path: {
            "path": module.path,
            "has_main": module.has_main,
            "basenames": sorted(module.basenames),
            "deps": sorted(module.deps),
            "missing": sorted(module.missing),
            "soft_missing": sorted(module.soft_missing),
            "untracked": sorted(module.untracked),
        }
        for path, module in modules.items()
    }
    missing, untracked = json.loads(
        dead_tests_closure_native(start, json.dumps(payload, sort_keys=True))
    )
    return set(missing), set(untracked)


def analyse(tracked: Sequence[str], *, root: Path = ROOT) -> dict[str, object]:
    try:
        analysis = DeadTestsAnalysisNative(str(root), list(tracked))
    except ValueError as exc:
        _raise_parse_error(exc, root=root)
    config_text, notes_text = config_corpus(tracked, root=root)
    dates = last_commit_dates(root=root)
    report = json.loads(analysis.report(config_text, notes_text, dates))
    report["untracked_tests"] = untracked_tests(root=root)
    return report


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
