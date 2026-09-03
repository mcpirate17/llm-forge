"""Boundary contract for the extractable tooling (``conductor/`` + agent hooks).

The tooling is being split into a standalone package. These rules are what "the
boundary holds" means, checked mechanically from the ASTs and the raw hook sources:

a. no module under ``conductor/`` (tests included) reaches a project package
   (``research``, ``component_fab``, ``aria_*``, ``research_runtime_native``) at any
   scope -- import statements, function-scope imports, ``importlib`` string literals;
b. no generic hook under ``.claude/hooks`` (``project/`` excluded) or ``.agent_hooks``
   contains a project path literal;
c. no non-test conductor module contains a host path literal (``Path.home()`` / env);
d. ``conductor/_native.py`` is the only module naming the native crate, and every
   symbol it re-exports exists in the installed crate.

Every finding names ``file:line``. ``python -m conductor.tooling_boundary`` exits 1 on
any finding and is wired into the gate as an always-on command check, so a PR that
reintroduces a coupling fails before review.
"""

from __future__ import annotations

import argparse
import ast
import importlib
import json
import sys
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path

from conductor._project_hooks import DEFAULT_TEST_PLUGIN
from conductor._native import tooling_boundary_facts_native

PROJECT_PACKAGES: tuple[str, ...] = (
    "research",
    "audit",
    "component_fab",
    "aria_designer",
    "aria_core",
    "research_runtime_native",
)
NATIVE_CRATE = "conductor_native"
NATIVE_SEAM = "_native.py"
PATH_LITERALS: tuple[str, ...] = ("/home/tim", "/mnt/data")
HOOK_LITERALS: tuple[str, ...] = ("research/", "/home/tim", "/mnt/data")
HOOK_EXCLUDED_SUBDIR = "project"

# (conductor-relative path, kind, value, reason). ``kind`` is "import" for an import
# statement, "string" for a dotted-name string constant, "literal" for a raw path
# literal; the value must match exactly. Widening this list is a boundary decision,
# so every entry carries why.
ALLOWLIST: tuple[tuple[str, str, str, str], ...] = (
    (
        "_project_hooks.py",
        "string",
        DEFAULT_TEST_PLUGIN.split(":")[0],
        "default CONDUCTOR_PROJECT_TEST_PLUGIN spec, resolved via importlib only",
    ),
    (
        "tooling_boundary.py",
        "string",
        NATIVE_CRATE,
        "the checker must spell the crate whose seam it guards",
    ),
    *(
        (
            "tooling_boundary.py",
            "literal",
            literal,
            "the checker must spell what it hunts",
        )
        for literal in PATH_LITERALS
    ),
)


@dataclass(frozen=True, slots=True)
class Violation:
    rule: str
    path: str
    line: int
    message: str

    def __str__(self) -> str:
        return f"{self.path}:{self.line}: [{self.rule}] {self.message}"


def _rel(path: Path, package_dir: Path) -> str:
    root = package_dir.parent
    try:
        return path.resolve().relative_to(root.resolve()).as_posix()
    except ValueError:
        return path.as_posix()


def _imported_modules(node: ast.AST) -> list[str]:
    if isinstance(node, ast.Import):
        return [alias.name for alias in node.names]
    if isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
        return [node.module]
    return []


def _parse(path: Path) -> ast.Module:
    return ast.parse(path.read_text(encoding="utf-8"), filename=str(path))


def _facts(
    package_dir: Path, hook_dirs: Sequence[Path] = ()
) -> dict[str, list[dict[str, object]]]:
    return json.loads(
        tooling_boundary_facts_native(
            str(package_dir),
            [str(path) for path in hook_dirs],
            list(PROJECT_PACKAGES),
            NATIVE_CRATE,
            NATIVE_SEAM,
            list(PATH_LITERALS),
            list(HOOK_LITERALS),
            HOOK_EXCLUDED_SUBDIR,
            [(path, kind, value) for path, kind, value, _ in ALLOWLIST],
        )
    )


def _violations(rule: str, facts: Sequence[dict[str, object]]) -> list[Violation]:
    templates = {
        "project_import": "imports project module {}",
        "project_string": "names project module {!r} as a string",
        "hook_literal": "hook contains {!r}",
        "module_literal": "module contains {!r}",
        "native_import": "imports {} outside the seam",
        "native_string": "names {} outside the seam",
    }
    return [
        Violation(
            rule,
            str(fact["path"]),
            int(fact["line"]),
            templates[str(fact["kind"])].format(fact["value"]),
        )
        for fact in facts
    ]


def check_project_imports(package_dir: Path) -> list[Violation]:
    """Rule a: no project package is reachable from any conductor module."""
    return _violations("a", _facts(package_dir)["a"])


def default_hook_dirs(package_dir: Path) -> list[Path]:
    """Hook trees next to the package: launchers, the tooling bodies, the standalone one."""
    root = package_dir.parent
    candidates = (
        root / ".claude" / "hooks",
        root / ".agent_hooks",
        root / "tooling" / "hooks",
        root.parent / "hooks",
    )
    return [d for d in candidates if d.is_dir()]


def check_hook_literals(
    hook_dirs: Sequence[Path], package_dir: Path
) -> list[Violation]:
    """Rule b: generic hooks carry no project path literal."""
    return _violations("b", _facts(package_dir, hook_dirs)["b"])


def check_path_literals(package_dir: Path) -> list[Violation]:
    """Rule c: non-test modules carry no host path literal."""
    return _violations("c", _facts(package_dir)["c"])


def _check_live_native_exports(package_dir: Path) -> list[Violation]:
    found: list[Violation] = []
    seam = package_dir / NATIVE_SEAM
    shown = _rel(seam, package_dir)
    crate = importlib.import_module(NATIVE_CRATE)
    exported: list[str] = []
    for node in _parse(seam).body:
        modules = _imported_modules(node)
        if not modules:
            continue
        if modules != [NATIVE_CRATE] and modules != ["__future__"]:
            found.append(
                Violation(
                    "d", shown, node.lineno, f"seam imports {modules[0]}, not the crate"
                )
            )
            continue
        if isinstance(node, ast.ImportFrom) and node.module == NATIVE_CRATE:
            for alias in node.names:
                exported.append(alias.name)
                if not hasattr(crate, alias.name):
                    found.append(
                        Violation(
                            "d",
                            shown,
                            node.lineno,
                            f"{alias.name} is not exported by {NATIVE_CRATE}",
                        )
                    )
    return found


def check_native_seam(package_dir: Path) -> list[Violation]:
    """Rule d: ``_native.py`` is the only seam and re-exports only real symbols."""
    return _check_live_native_exports(package_dir) + _violations(
        "d", _facts(package_dir)["d"]
    )


def check_all(
    package_dir: Path, hook_dirs: Sequence[Path] | None = None
) -> dict[str, list[Violation]]:
    dirs = default_hook_dirs(package_dir) if hook_dirs is None else list(hook_dirs)
    if not dirs:
        raise FileNotFoundError(f"no hook tree found next to {package_dir}")
    facts = _facts(package_dir, dirs)
    return {
        "a": _violations("a", facts["a"]),
        "b": _violations("b", facts["b"]),
        "c": _violations("c", facts["c"]),
        "d": _check_live_native_exports(package_dir) + _violations("d", facts["d"]),
    }


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m conductor.tooling_boundary")
    parser.add_argument(
        "--root",
        default=".",
        help="tree root whose conductor/ package is checked (default: cwd)",
    )
    args = parser.parse_args(argv)
    root = Path(args.root).resolve()
    package_dir = root / "conductor"
    if not package_dir.is_dir():
        print(f"tooling-boundary: no conductor/ under {root}", file=sys.stderr)
        return 2
    results = check_all(package_dir)
    total = sum(len(v) for v in results.values())
    print(f"tooling-boundary: root={root} findings={total}")
    for rule, violations in results.items():
        for violation in violations:
            print(f"  {violation}")
        if not violations:
            print(f"  rule {rule}: clean")
    return 1 if total else 0


if __name__ == "__main__":
    raise SystemExit(main())
