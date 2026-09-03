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
import re
import sys
from collections.abc import Iterable, Sequence
from dataclasses import dataclass
from pathlib import Path

from conductor._project_hooks import DEFAULT_TEST_PLUGIN

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

# A dotted module path (``pkg.sub``) or a plugin spec (``pkg:fn``). A bare package
# name is a path segment, not an import edge: that is a path coupling, which the
# rehearsal (``tooling_standalone_smoke``) surfaces rather than this rule.
_MODULE_RE = re.compile(
    r"^(?:"
    + "|".join(PROJECT_PACKAGES)
    + r")(?:(?:\.[A-Za-z_]\w*)+(?::[A-Za-z_]\w*)?|:[A-Za-z_]\w*)$"
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


def _is_test_path(rel: Path) -> bool:
    return rel.name.startswith("test_") or "tests" in rel.parts[:-1]


def package_modules(package_dir: Path) -> list[Path]:
    """Every ``.py`` under the conductor package, sorted, caches excluded."""
    return sorted(
        p
        for p in package_dir.rglob("*.py")
        if "__pycache__" not in p.relative_to(package_dir).parts
    )


def _allowed(rel: str, kind: str, module: str) -> bool:
    return any(rel == path and kind == k and module == m for path, k, m, _ in ALLOWLIST)


def _imported_modules(node: ast.AST) -> list[str]:
    if isinstance(node, ast.Import):
        return [alias.name for alias in node.names]
    if isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
        return [node.module]
    return []


def _parse(path: Path) -> ast.Module:
    return ast.parse(path.read_text(encoding="utf-8"), filename=str(path))


def check_project_imports(package_dir: Path) -> list[Violation]:
    """Rule a: no project package is reachable from any conductor module."""
    found: list[Violation] = []
    for path in package_modules(package_dir):
        rel = path.relative_to(package_dir).as_posix()
        shown = _rel(path, package_dir)
        for node in ast.walk(_parse(path)):
            for module in _imported_modules(node):
                if module.split(".")[0] in PROJECT_PACKAGES and not _allowed(
                    rel, "import", module
                ):
                    found.append(
                        Violation(
                            "a", shown, node.lineno, f"imports project module {module}"
                        )
                    )
            if isinstance(node, ast.Constant) and isinstance(node.value, str):
                if _MODULE_RE.match(node.value) and not _allowed(
                    rel, "string", node.value.split(":")[0]
                ):
                    found.append(
                        Violation(
                            "a",
                            shown,
                            node.lineno,
                            f"names project module {node.value!r} as a string",
                        )
                    )
    return found


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


def _hook_files(hook_dir: Path) -> Iterable[Path]:
    for path in sorted(hook_dir.rglob("*")):
        if not path.is_file():
            continue
        rel = path.relative_to(hook_dir)
        if rel.parts[0] == HOOK_EXCLUDED_SUBDIR or rel.name.startswith("test_"):
            continue
        if "__pycache__" in rel.parts:  # bytecode embeds the compiling checkout's path
            continue
        yield path


def _literal_hits(path: Path, literals: Sequence[str]) -> Iterable[tuple[int, str]]:
    text = path.read_text(encoding="utf-8", errors="replace")
    for lineno, line in enumerate(text.splitlines(), start=1):
        for literal in literals:
            if literal in line:
                yield lineno, literal


def check_hook_literals(
    hook_dirs: Sequence[Path], package_dir: Path
) -> list[Violation]:
    """Rule b: generic hooks carry no project path literal."""
    return [
        Violation("b", _rel(path, package_dir), lineno, f"hook contains {literal!r}")
        for hook_dir in hook_dirs
        for path in _hook_files(hook_dir)
        for lineno, literal in _literal_hits(path, HOOK_LITERALS)
    ]


def check_path_literals(package_dir: Path) -> list[Violation]:
    """Rule c: non-test modules carry no host path literal."""
    return [
        Violation("c", _rel(path, package_dir), lineno, f"module contains {literal!r}")
        for path in package_modules(package_dir)
        if not _is_test_path(path.relative_to(package_dir))
        for lineno, literal in _literal_hits(path, PATH_LITERALS)
        if not _allowed(path.relative_to(package_dir).as_posix(), "literal", literal)
    ]


def check_native_seam(package_dir: Path) -> list[Violation]:
    """Rule d: ``_native.py`` is the only seam and re-exports only real symbols."""
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
    for path in package_modules(package_dir):
        if path == seam:
            continue
        rel = path.relative_to(package_dir).as_posix()
        for node in ast.walk(_parse(path)):
            names = [
                m for m in _imported_modules(node) if m.split(".")[0] == NATIVE_CRATE
            ]
            if names and not _allowed(rel, "import", NATIVE_CRATE):
                found.append(
                    Violation(
                        "d",
                        _rel(path, package_dir),
                        node.lineno,
                        f"imports {NATIVE_CRATE} outside the seam",
                    )
                )
            if (
                isinstance(node, ast.Constant)
                and node.value == NATIVE_CRATE
                and not _allowed(rel, "string", NATIVE_CRATE)
            ):
                found.append(
                    Violation(
                        "d",
                        _rel(path, package_dir),
                        node.lineno,
                        f"names {NATIVE_CRATE} outside the seam",
                    )
                )
    return found


def check_all(
    package_dir: Path, hook_dirs: Sequence[Path] | None = None
) -> dict[str, list[Violation]]:
    dirs = default_hook_dirs(package_dir) if hook_dirs is None else list(hook_dirs)
    if not dirs:
        raise FileNotFoundError(f"no hook tree found next to {package_dir}")
    return {
        "a": check_project_imports(package_dir),
        "b": check_hook_literals(dirs, package_dir),
        "c": check_path_literals(package_dir),
        "d": check_native_seam(package_dir),
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
