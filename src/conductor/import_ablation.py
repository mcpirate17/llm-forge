"""Module-level import ablation: is this import load-bearing, or just asserted to be?

Static analysis already decides the easy case -- ruff reports unused imports and this
repo has none. What it cannot decide is the import a human has SILENCED: every
`# noqa: F401` is a hand-written claim that an apparently-unused import matters, for
its side effects (registration at import time, library init order) or as a re-export.
Nothing verifies those claims, and one of them killed a GPU run when a lazy import
resolved to a name that had been reset elsewhere.

The ablation is "delete the import"; the oracle is different from the value probe's,
because an import has no return value. Two oracles run instead:

    do the module's driver tests still pass?   -- behaviour
    does anything else in the repo use it?     -- the reference graph

Both are needed. A re-export's consumers live in OTHER modules by definition, so the
test oracle alone reports a live re-export as dead; and the reference graph alone
cannot see an import that exists purely for a side effect.

Nothing is written to the working tree. The ablated source is served through an import
hook, so an interrupted run cannot leave a half-edited file behind -- which is exactly
what an earlier throwaway version of this did.
"""

from __future__ import annotations

import argparse
import ast
import dataclasses
import importlib.abc
import importlib.util
import json
import pathlib
import subprocess
import sys
import tempfile
from typing import Sequence

__all__ = ["ImportSite", "import_sites", "ablate_source", "classify", "main"]

LOAD_BEARING = "LOAD_BEARING"
REEXPORTED = "REEXPORTED_USED_ELSEWHERE"
UNVERIFIED = "NO_DIFFERENCE_OBSERVED"
NOT_EXERCISED = "NOT_EXERCISED"


@dataclasses.dataclass(frozen=True)
class ImportSite:
    module: str
    lineno: int
    statement: str
    names: tuple[str, ...]
    silenced: bool

    @property
    def ident(self) -> str:
        return f"{self.module}:{self.lineno}"


def import_sites(path: pathlib.Path, silenced_only: bool = True) -> list[ImportSite]:
    """Every module-level import, with the names it binds."""
    source = path.read_text()
    lines = source.splitlines()
    tree = ast.parse(source)
    out: list[ImportSite] = []
    for node in tree.body:
        if not isinstance(node, (ast.Import, ast.ImportFrom)):
            continue
        names = tuple(
            (alias.asname or alias.name).split(".")[0] for alias in node.names
        )
        span = lines[node.lineno - 1 : (node.end_lineno or node.lineno)]
        silenced = any("noqa" in ln and "F401" in ln for ln in span)
        if silenced_only and not silenced:
            continue
        out.append(ImportSite(str(path), node.lineno, span[0].strip(), names, silenced))
    return out


def ablate_source(source: str, site: ImportSite) -> str:
    """Return ``source`` with the import statement at ``site`` removed.

    Removal is by AST span, not by line, so a parenthesised multi-line
    ``from x import (a, b, c)`` is taken out whole rather than left half-deleted.
    """
    tree = ast.parse(source)
    node = next(
        (n for n in tree.body
         if isinstance(n, (ast.Import, ast.ImportFrom)) and n.lineno == site.lineno),
        None,
    )
    if node is None:
        raise LookupError(f"no import at line {site.lineno}")
    lines = source.splitlines(keepends=True)
    end = node.end_lineno or node.lineno
    del lines[node.lineno - 1 : end]
    return "".join(lines)


class _AblatedLoader(importlib.abc.Loader):
    def __init__(self, path: pathlib.Path, source: str) -> None:
        self._path = path
        self._source = source

    def create_module(self, spec: importlib.machinery.ModuleSpec) -> None:
        return None

    def exec_module(self, module: object) -> None:
        code = compile(self._source, str(self._path), "exec")
        exec(code, module.__dict__)  # noqa: S102 - the point of the tool


class AblatedFinder(importlib.abc.MetaPathFinder):
    """Serves one module from ablated source, leaving the file on disk untouched."""

    def __init__(self, dotted: str, path: pathlib.Path, source: str) -> None:
        self.dotted = dotted
        self._path = path
        self._source = source

    def find_spec(self, fullname: str, path: object = None, target: object = None):
        if fullname != self.dotted:
            return None
        spec = importlib.util.spec_from_loader(
            fullname, _AblatedLoader(self._path, self._source)
        )
        if spec is not None:
            spec.origin = str(self._path)
        return spec


def consumers(module: str, names: Sequence[str], root: pathlib.Path) -> list[str]:
    """Files that import one of ``names`` from ``module``, or reach it as an attribute."""
    dotted = module[:-3].replace("/", ".")
    tail = dotted.rsplit(".", 1)[-1]
    found: set[str] = set()
    for name in names:
        for pattern in (f"from {dotted} import", f"from .{tail} import", f"{tail}.{name}"):
            out = subprocess.run(
                ["git", "grep", "-l", "-F", pattern, "--", "*.py"],
                cwd=root, capture_output=True, text=True,
            ).stdout.split()
            found.update(f for f in out if f != module)
    return sorted(found)


def classify(
    site: ImportSite, module: str, drivers: Sequence[str], root: pathlib.Path,
    timeout: int = 240,
) -> dict[str, object]:
    """Run both oracles for one import and return a verdict record."""
    if not drivers:
        return {"import": site.statement, "line": site.lineno, "verdict": NOT_EXERCISED,
                "consumers": [], "detail": "no test file imports this module"}
    source = ablate_source((root / module).read_text(), site)
    # The ablated source goes to a temp file, never into the command line: a large
    # module blows past ARG_MAX and the run dies with an OSError that looks like a
    # probe failure rather than what it is -- two modules in the first sweep.
    with tempfile.TemporaryDirectory() as scratch:
        payload = pathlib.Path(scratch) / "ablated.py"
        payload.write_text(source)
        runner = _RUNNER.format(
            dotted=json.dumps(module[:-3].replace("/", ".")),
            path=json.dumps(str(root / module)),
            payload=json.dumps(str(payload)),
            drivers=json.dumps(list(drivers)),
        )
        proc = subprocess.run([sys.executable, "-c", runner], cwd=root,
                              capture_output=True, text=True, timeout=timeout)
    used = consumers(module, site.names, root)
    if proc.returncode != 0:
        verdict = LOAD_BEARING
    elif used:
        verdict = REEXPORTED
    else:
        verdict = UNVERIFIED
    return {"import": site.statement, "line": site.lineno, "verdict": verdict,
            "names": list(site.names), "consumers": used[:6]}


_RUNNER = """
import pathlib, sys, pytest
sys.path.insert(0, ".")
from conductor.import_ablation import AblatedFinder
source = pathlib.Path({payload}).read_text()
sys.meta_path.insert(0, AblatedFinder({dotted}, {path}, source))
raise SystemExit(pytest.main(["-q", "-p", "no:cacheprovider", "-o", "addopts="] + {drivers}))
"""


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="conductor.import_ablation")
    parser.add_argument("module")
    parser.add_argument("--root", type=pathlib.Path, default=pathlib.Path.cwd())
    parser.add_argument("--all-imports", action="store_true",
                        help="also ablate imports nobody silenced (ruff already covers those)")
    parser.add_argument("--json", type=pathlib.Path)
    args = parser.parse_args(argv)

    from conductor import slop_gate

    drivers = slop_gate.drivers_for(args.module, args.root)
    sites = import_sites(args.root / args.module, silenced_only=not args.all_imports)
    records = [classify(s, args.module, drivers, args.root) for s in sites]
    if args.json:
        args.json.write_text(json.dumps(records, indent=2) + "\n")
    for r in records:
        print(f"{r['verdict']:26s} {args.module}:{r['line']}  {r['import'][:80]}")
        if r["verdict"] == REEXPORTED:
            print(f"{'':26s}   used by {', '.join(r['consumers'][:3])}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
