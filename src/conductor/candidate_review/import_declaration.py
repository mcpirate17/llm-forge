"""KB-CI-01: a shipped module may only import what its pyproject declares.

The failure this exists to stop has a specific shape. A module grows an import
of a package nobody declared; the developer's venv already carries it as some
other dependency's dependency, so nothing breaks locally and nothing breaks in
a CI job that installs from the same resolved lockfile. The package is simply
unimportable from a clean install, and the defect surfaces on someone else's
machine, or in a job that installs a narrower set. Three defects in one day
(2026-08-30) were exactly this.

So the rule is deliberately narrow: report an import that **resolves against
installed metadata** to a distribution the file's nearest pyproject does not
declare. That is the whole defect class and nothing else --

* an import that resolves to nothing is out of scope. It is a first-party
  sibling, a `sys.path` injection, or a package this environment simply does
  not have; each of those fails loudly at import time and none of them is a
  silently-undeclared dependency.
* an import of a declared distribution is fine however it got installed.

Being installed is the precondition, not an accident: a transitively-present
package is exactly what makes the omission invisible.
"""

from __future__ import annotations

import ast
import re
import sys
import time
import tomllib
from collections.abc import Iterable, Mapping
from importlib.metadata import packages_distributions
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING

from conductor.candidate_review.model import CheckResult, Finding, Severity

if TYPE_CHECKING:  # `checks` imports this module, so the context type is a cycle.
    from conductor.candidate_review.checks import ReviewContext

_SEPARATORS = re.compile(r"[-_.]+")
_REQUIREMENT_STOP = re.compile(r"[\[<>=!~;@\s]")
MANIFEST = "pyproject.toml"


def canonical_name(name: str) -> str:
    """PEP 503 canonical form: the only shape two spellings can be compared in."""

    return _SEPARATORS.sub("-", name.strip()).lower()


def requirement_name(spec: str) -> str:
    """The distribution a requirement string names, without its version or extras."""

    head = _REQUIREMENT_STOP.split(spec.strip(), 1)[0]
    return canonical_name(head) if head else ""


def declared_distributions(manifest_text: str) -> frozenset[str]:
    """Every distribution a pyproject declares, in canonical form.

    Extras and dependency groups count. An import satisfied only by an extra is
    a packaging decision, not an undeclared dependency -- the name is written
    down and a clean install can ask for it. Build-system requires count for the
    same reason: a setup.py that imports its own build backend is declaring it,
    just in the table that governs when it runs.
    """

    data = tomllib.loads(manifest_text)
    project = data.get("project", {})
    names: set[str] = set()
    if isinstance(project.get("name"), str):
        names.add(canonical_name(project["name"]))
    specs: list[object] = list(_string_list(project.get("dependencies")))
    optional = project.get("optional-dependencies")
    if isinstance(optional, Mapping):
        for group in optional.values():
            specs.extend(_string_list(group))
    groups = data.get("dependency-groups")
    if isinstance(groups, Mapping):
        for group in groups.values():
            specs.extend(_string_list(group))
    build = data.get("build-system")
    if isinstance(build, Mapping):
        specs.extend(_string_list(build.get("requires")))
    for spec in specs:
        name = requirement_name(str(spec))
        if name:
            names.add(name)
    return frozenset(names)


def _string_list(value: object) -> list[str]:
    if not isinstance(value, list):
        return []
    return [item for item in value if isinstance(item, str)]


def nearest_manifest(snapshot: Path, rel_path: str) -> Path | None:
    """The pyproject governing a file: the nearest one at or above its directory.

    A subproject with its own pyproject is judged against that one, not the
    root's -- it is what a clean install of *that* distribution would resolve.
    """

    current = PurePosixPath(rel_path).parent
    while True:
        candidate = snapshot / current / MANIFEST
        if candidate.is_file():
            return candidate
        if current == PurePosixPath("."):
            return None
        current = current.parent


def imported_modules(source: str) -> frozenset[str]:
    """Top-level module names this source imports absolutely.

    Relative imports are first-party by construction and carry no distribution.
    """

    tree = ast.parse(source)
    modules: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                modules.add(alias.name.split(".", 1)[0])
        elif isinstance(node, ast.ImportFrom):
            if node.level == 0 and node.module:
                modules.add(node.module.split(".", 1)[0])
    return frozenset(name for name in modules if name)


def undeclared_imports(
    source: str,
    *,
    declared: Iterable[str],
    distributions: Mapping[str, list[str]],
    stdlib: Iterable[str] = sys.stdlib_module_names,
) -> tuple[tuple[str, tuple[str, ...]], ...]:
    """(module, distributions) pairs this source imports without declaring.

    Sorted so a finding's evidence is stable across runs.
    """

    allowed = {canonical_name(name) for name in declared}
    known = set(stdlib)
    found: list[tuple[str, tuple[str, ...]]] = []
    for module in imported_modules(source):
        if module in known or module.startswith("_"):
            continue
        providers = distributions.get(module)
        if not providers:
            continue
        if any(canonical_name(dist) in allowed for dist in providers):
            continue
        found.append((module, tuple(sorted(providers))))
    return tuple(sorted(found))


def check_import_declaration(ctx: "ReviewContext") -> CheckResult:
    """KB-CI-01: every distribution a changed shipped module imports is declared.

    The resolution map comes from the interpreter running the gate, so the check
    is exactly as strong as that environment is complete. That is the right
    direction to be wrong in: the environment that carries a package
    transitively is the one that can see the omission at all, and an
    environment without it cannot produce a false accusation.
    """

    from conductor.candidate_review.checks import _result

    started = time.perf_counter()
    files = [
        change.path
        for change in ctx.candidate.changes
        if not change.deleted
        and "python" in change.classes
        and "test" not in change.classes
    ]
    findings: list[Finding] = []
    distributions = packages_distributions()
    declared: dict[Path, frozenset[str]] = {}
    unmanifested = 0
    for rel in files:
        manifest = nearest_manifest(ctx.snapshot, rel)
        if manifest is None:
            # Nothing declares this file's dependencies, so nothing here is
            # undeclared. A file outside every distribution is a different
            # finding than an undeclared import, and not this check's.
            unmanifested += 1
            continue
        names = declared.get(manifest)
        if names is None:
            try:
                names = declared_distributions(manifest.read_text(encoding="utf-8"))
            except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError) as exc:
                findings.append(
                    Finding(
                        check_id="import-declaration",
                        rule_id="unreadable-manifest",
                        severity=Severity.CRITICAL,
                        path=rel,
                        message=f"dependency manifest cannot be read: {manifest.name}: {exc}",
                    )
                )
                declared[manifest] = frozenset()
                continue
            declared[manifest] = names
        try:
            source = (ctx.snapshot / rel).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        try:
            missing = undeclared_imports(
                source, declared=names, distributions=distributions
            )
        except SyntaxError:
            # python-ast owns unparseable sources; reporting it twice buys
            # nothing and splits the fix across two findings.
            continue
        for module, providers in missing:
            findings.append(
                Finding(
                    check_id="import-declaration",
                    rule_id="undeclared-dependency",
                    severity=Severity.CRITICAL,
                    path=rel,
                    message=(
                        f"imports {module}, provided by "
                        f"{', '.join(providers)}, which "
                        f"{manifest.parent.name or 'the repository'} does not declare"
                    ),
                    evidence={"module": module, "distributions": list(providers)},
                )
            )
    return _result(
        "import-declaration",
        started,
        findings,
        files=files,
        metrics={
            "python_files": len(files),
            "manifests": len(declared),
            "files_without_manifest": unmanifested,
        },
    )
