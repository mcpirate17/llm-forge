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
from collections.abc import Iterable, Iterator, Mapping
from importlib.metadata import packages_distributions
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING

from conductor.candidate_review.model import CheckResult, Finding, Severity
from conductor.project_paths import package_relative

if TYPE_CHECKING:  # `checks` imports this module, so the context type is a cycle.
    from conductor.candidate_review.checks import ReviewContext

_SEPARATORS = re.compile(r"[-_.]+")
_REQUIREMENT_STOP = re.compile(r"[\[<>=!~;@\s]")
MANIFEST = "pyproject.toml"

# Trees whose modules run in jobs that install `[project] dependencies` and
# nothing else: the contract jobs, the pre-commit profile and `make gate`. For
# these an extra is not "a clean install can ask for it" -- nothing asks, so an
# import declared only in an extra or a dependency-group is unimportable exactly
# where it runs. That is the radon defect (2026-09-05): `radon` sat in the
# component-fab-dev extra and the dev group, present in every developer venv and
# absent from the job that runs the complexity ratchet on the commit path.
#
# research/, component_fab/ and aria_designer/ are deliberately outside this.
# They are wheel packages whose optional dependencies (triton, scipy, requests,
# psutil, jsonschema) really are extras a consumer opts into, and 60 files there
# import one; widening the rule to cover them is a packaging decision that has to
# be made per tree, not a side effect of this check.
BASE_DEPENDENCY_TREES = ("conductor/", "tooling/")


def base_dependency_trees(root: Path) -> tuple[str, ...]:
    """`BASE_DEPENDENCY_TREES` as the tree at ``root`` spells them.

    The two trees are the conductor package and the hook tooling beside it. Where
    that pair sits is the host's to declare (the repo root in the monorepo,
    ``src/`` here); resolving it keeps the stricter rule pointed at the package
    instead of silently governing nothing under a src layout.
    """

    package = package_relative(root)
    return (f"{package}/", f"{(package.parent / 'tooling').as_posix()}/")

# The one shipped module that imports a test tool at runtime and means it.
# KB-CI-01 grants this explicitly ("Test-only tools (pytest, vulture) may stay in
# the job"); the probe imports pytest inside a function to run the tests it is
# measuring. Keyed by path so the grant cannot silently spread: move the file and
# the exemption stops applying, which is the direction that fails loud.
RUNTIME_TEST_TOOL_EXEMPTIONS: Mapping[str, frozenset[str]] = {
    "conductor/equivalence_probe.py": frozenset({"pytest"}),
}

# conftest.py executes only under pytest, which by definition has pytest. It is
# test infrastructure that the test-path rule does not classify as a test file.
TEST_INFRASTRUCTURE = frozenset({"conftest.py"})


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


def base_distributions(manifest_text: str) -> frozenset[str]:
    """Only what `[project] dependencies` declares, plus the project's own name.

    This is what a bare install of the distribution carries -- no extras, no
    dependency-groups, no build-system requires. `declared_distributions` is the
    wider question ("is the name written down anywhere?"); this is the narrower
    one ("is it there when nobody asked for an extra?") and it is the one the
    repository's own jobs answer.
    """

    data = tomllib.loads(manifest_text)
    project = data.get("project", {})
    names: set[str] = set()
    if isinstance(project.get("name"), str):
        names.add(canonical_name(project["name"]))
    for spec in _string_list(project.get("dependencies")):
        name = requirement_name(spec)
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


def _is_type_checking_guard(test: ast.expr) -> bool:
    """`if TYPE_CHECKING:` or `if typing.TYPE_CHECKING:` -- either spelling."""

    if isinstance(test, ast.Name):
        return test.id == "TYPE_CHECKING"
    if isinstance(test, ast.Attribute):
        return test.attr == "TYPE_CHECKING"
    return False


def _runtime_nodes(node: ast.AST) -> Iterator[ast.AST]:
    """Every node that executes, with `if TYPE_CHECKING:` bodies left out.

    The `else` branch of such a guard is exactly the runtime branch, so it is
    walked; only the body the type checker alone sees is dropped.
    """

    for child in ast.iter_child_nodes(node):
        if isinstance(child, ast.If) and _is_type_checking_guard(child.test):
            for fallback in child.orelse:
                yield fallback
                yield from _runtime_nodes(fallback)
            continue
        yield child
        yield from _runtime_nodes(child)


def imported_modules(source: str, *, runtime_only: bool = False) -> frozenset[str]:
    """Top-level module names this source imports absolutely.

    Relative imports are first-party by construction and carry no distribution.
    With `runtime_only`, imports that only a type checker executes are excluded:
    a `TYPE_CHECKING`-guarded import cannot break an install, so holding it to
    the stricter base-dependency rule would be a false accusation.
    """

    tree = ast.parse(source)
    nodes = _runtime_nodes(tree) if runtime_only else ast.walk(tree)
    modules: set[str] = set()
    for node in nodes:
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


def optional_only_imports(
    source: str,
    *,
    base: Iterable[str],
    declared: Iterable[str],
    distributions: Mapping[str, list[str]],
    stdlib: Iterable[str] = sys.stdlib_module_names,
) -> tuple[tuple[str, tuple[str, ...]], ...]:
    """(module, distributions) pairs declared, but only in an extra or a group.

    A module outside `declared` entirely is `undeclared_imports`' finding and is
    deliberately not repeated here -- one omission, one finding. Only runtime
    imports count: a `TYPE_CHECKING` guard never executes and never breaks an
    install.
    """

    installed = {canonical_name(name) for name in base}
    written_down = {canonical_name(name) for name in declared}
    known = set(stdlib)
    found: list[tuple[str, tuple[str, ...]]] = []
    for module in imported_modules(source, runtime_only=True):
        if module in known or module.startswith("_"):
            continue
        providers = distributions.get(module)
        if not providers:
            continue
        canonical = {canonical_name(dist) for dist in providers}
        if canonical & installed:
            continue
        if not canonical & written_down:
            continue
        found.append((module, tuple(sorted(providers))))
    return tuple(sorted(found))


def in_base_dependency_tree(
    rel_path: str, trees: tuple[str, ...] = BASE_DEPENDENCY_TREES
) -> bool:
    """Whether the stricter base-dependency rule governs this file.

    Test infrastructure is excluded wherever it lives: `conftest.py` runs only
    under pytest, so pytest is present by construction.
    """

    if PurePosixPath(rel_path).name in TEST_INFRASTRUCTURE:
        return False
    return rel_path.startswith(trees)


def _undeclared_finding(
    rel: str, module: str, providers: tuple[str, ...], manifest: str
) -> Finding:
    return Finding(
        check_id="import-declaration",
        rule_id="undeclared-dependency",
        severity=Severity.CRITICAL,
        path=rel,
        message=(
            f"imports {module}, provided by {', '.join(providers)}, "
            f"which {manifest} does not declare"
        ),
        evidence={
            "module": module,
            "distributions": list(providers),
            "manifest": manifest,
        },
    )


def _optional_only_finding(
    rel: str, module: str, providers: tuple[str, ...], manifest: str
) -> Finding:
    return Finding(
        check_id="import-declaration",
        rule_id="base-dependency-required",
        severity=Severity.CRITICAL,
        path=rel,
        message=(
            f"imports {module}, provided by {', '.join(providers)}, which "
            f"{manifest} declares only in an extra or a dependency-group; this "
            "tree runs in jobs that install [project] dependencies only, so "
            "the import fails there"
        ),
        evidence={
            "module": module,
            "distributions": list(providers),
            "manifest": manifest,
        },
    )


def _stale_exemption_finding(rel: str, module: str) -> Finding:
    return Finding(
        check_id="import-declaration",
        rule_id="stale-exemption",
        severity=Severity.MEDIUM,
        path=rel,
        message=(
            f"RUNTIME_TEST_TOOL_EXEMPTIONS grants {rel} a runtime import of "
            f"{module}, but the file no longer imports it; delete the entry so "
            "the grant cannot quietly cover a future import"
        ),
        evidence={"module": module},
    )


def _manifest_names(
    manifest: Path,
) -> tuple[frozenset[str], frozenset[str]]:
    """(base, declared) for a manifest. Raises on an unreadable one."""

    text = manifest.read_text(encoding="utf-8")
    return base_distributions(text), declared_distributions(text)


def _findings_for_file(
    rel: str,
    source: str,
    *,
    base: frozenset[str],
    declared: frozenset[str],
    distributions: Mapping[str, list[str]],
    manifest: str,
    trees: tuple[str, ...],
) -> list[Finding]:
    """Every import finding for one file, both rules, exemptions applied."""

    findings = [
        _undeclared_finding(rel, module, providers, manifest)
        for module, providers in undeclared_imports(
            source, declared=declared, distributions=distributions
        )
    ]
    if not in_base_dependency_tree(rel, trees):
        return findings
    exempt = RUNTIME_TEST_TOOL_EXEMPTIONS.get(rel, frozenset())
    optional = optional_only_imports(
        source, base=base, declared=declared, distributions=distributions
    )
    used = {module for module, _ in optional} & exempt
    findings.extend(
        _optional_only_finding(rel, module, providers, manifest)
        for module, providers in optional
        if module not in exempt
    )
    findings.extend(
        _stale_exemption_finding(rel, module) for module in sorted(exempt - used)
    )
    return findings


def check_import_declaration(ctx: "ReviewContext") -> CheckResult:
    """KB-CI-01: every distribution a changed shipped module imports is declared.

    The resolution map comes from the interpreter running the gate, so the check
    is exactly as strong as that environment is complete. That is the right
    direction to be wrong in: the environment that carries a package
    transitively is the one that can see the omission at all, and an
    environment without it cannot produce a false accusation.

    Two rules, narrowest first. Everywhere: an import that resolves to a
    distribution the manifest does not name at all is critical. Under
    `BASE_DEPENDENCY_TREES`: an import the manifest names only in an extra or a
    dependency-group is critical too, because the jobs those trees run in
    install neither.
    """

    from conductor.candidate_review.checks import _result

    started = time.perf_counter()
    trees = base_dependency_trees(ctx.snapshot)
    files = [
        change.path
        for change in ctx.candidate.changes
        if not change.deleted
        and "python" in change.classes
        and "test" not in change.classes
    ]
    findings: list[Finding] = []
    distributions = packages_distributions()
    names: dict[Path, tuple[frozenset[str], frozenset[str]]] = {}
    unmanifested = 0
    strict = 0
    for rel in files:
        manifest = nearest_manifest(ctx.snapshot, rel)
        if manifest is None:
            # Nothing declares this file's dependencies, so nothing here is
            # undeclared. A file outside every distribution is a different
            # finding than an undeclared import, and not this check's.
            unmanifested += 1
            continue
        if manifest not in names:
            try:
                names[manifest] = _manifest_names(manifest)
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
                names[manifest] = (frozenset(), frozenset())
                continue
        base, declared = names[manifest]
        owner = manifest.relative_to(ctx.snapshot).as_posix()
        try:
            source = (ctx.snapshot / rel).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        try:
            findings.extend(
                _findings_for_file(
                    rel,
                    source,
                    base=base,
                    declared=declared,
                    distributions=distributions,
                    manifest=owner,
                    trees=trees,
                )
            )
        except SyntaxError:
            # python-ast owns unparseable sources; reporting it twice buys
            # nothing and splits the fix across two findings.
            continue
        strict += in_base_dependency_tree(rel, trees)
    return _result(
        "import-declaration",
        started,
        findings,
        files=files,
        metrics={
            "python_files": len(files),
            "manifests": len(names),
            "files_without_manifest": unmanifested,
            "base_dependency_files": strict,
        },
    )
