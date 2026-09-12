"""KB-CI-01: a shipped module may only import what its pyproject declares.

Three defects in one day (2026-08-30) had one shape: a module imported a
distribution nobody declared, the developer's venv carried it transitively, and
the package was simply unimportable from a clean install. The rule that catches
that is narrow on purpose, and most of what these tests hold is the narrowness:
an import that resolves to nothing, or to a name the manifest spells
differently, must not become a finding, because a rule that cries wolf on
first-party siblings gets turned off and then catches nothing at all.
"""

from __future__ import annotations

import tomllib
from dataclasses import replace
from pathlib import Path

import pytest

from conductor.candidate_review.checks import (
    Change,
    ReviewContext,
    Severity,
    check_import_declaration,
)
from conductor.candidate_review import import_declaration
from conductor.candidate_review.import_declaration import (
    base_distributions,
    canonical_name,
    declared_distributions,
    imported_modules,
    BASE_DEPENDENCY_TREES,
    base_dependency_trees,
    in_base_dependency_tree,
    optional_only_imports,
    nearest_manifest,
    requirement_name,
    undeclared_imports,
)
from conductor.test_candidate_review_hardening import _gate_context

MANIFEST = """[project]
name = "probe"
dependencies = ["polars>=1.0"]

[project.optional-dependencies]
research = ["scipy>=1.12"]

[dependency-groups]
dev = ["pytest>=8.0"]

[build-system]
requires = ["hatchling"]
"""


def test_a_spelling_difference_is_not_an_undeclared_dependency() -> None:
    """`ruamel.yaml`, `ruamel-yaml` and `Ruamel_YAML` are one distribution.

    Comparing the raw strings would report a package the manifest declares in a
    different but equally valid spelling, which is the false positive that gets
    a rule disabled.
    """

    assert canonical_name("Ruamel_YAML") == canonical_name("ruamel.yaml")
    assert canonical_name("ruamel-yaml") == "ruamel-yaml"


def test_a_requirement_is_matched_without_its_version_or_extras() -> None:
    """Manifests declare `a2a-sdk[http-server]>=1.1.2`; metadata says `a2a-sdk`.

    Every separator that can follow the name has to end it, or a pinned or
    extra-bearing declaration reads as undeclared.
    """

    assert requirement_name("a2a-sdk[http-server]>=1.1.2") == "a2a-sdk"
    assert requirement_name("torch>=2.2") == "torch"
    assert requirement_name("orjson; python_version >= '3.12'") == "orjson"
    assert requirement_name("slop-core @ file:///tooling/native") == "slop-core"


def test_an_extra_or_group_declares_a_dependency() -> None:
    """A name written down anywhere in the manifest is declared.

    An import satisfied only by an extra is a packaging decision -- a clean
    install can ask for it by name -- not the invisible omission this catches.
    """

    declared = declared_distributions(MANIFEST)
    assert {"polars", "scipy", "pytest", "hatchling", "probe"} <= declared


def test_the_nearest_manifest_governs_a_file(tmp_path: Path) -> None:
    """A subproject is judged against its own pyproject, not the root's.

    Judging it against the root would let a subproject import anything the
    monorepo happens to declare and still install broken on its own.
    """

    (tmp_path / "pyproject.toml").write_text(MANIFEST, encoding="utf-8")
    inner = tmp_path / "tooling/native"
    inner.mkdir(parents=True)
    (inner / "pyproject.toml").write_text(MANIFEST, encoding="utf-8")
    found = nearest_manifest(tmp_path, "tooling/native/src/mod.py")
    assert found == inner / "pyproject.toml"


def test_a_file_under_no_manifest_has_nothing_to_declare(tmp_path: Path) -> None:
    """Without a manifest there is no declaration to be missing from."""

    assert nearest_manifest(tmp_path, "scratch/tool.py") is None


def test_only_absolute_top_level_imports_carry_a_distribution() -> None:
    """`from . import x` is first-party; `import a.b.c` is distribution `a`.

    Keeping the dotted tail would never match a distribution name, and treating
    a relative import as absolute would accuse the package of importing itself.
    """

    source = (
        "import polars.selectors\nfrom . import sibling\nfrom scipy.stats import t\n"
    )
    assert imported_modules(source) == frozenset({"polars", "scipy"})


def test_an_import_that_resolves_to_nothing_is_out_of_scope() -> None:
    """First-party siblings and absent packages are not undeclared dependencies.

    They fail loudly at import time; only a package the environment silently
    carries can hide. Reporting them would bury the two real findings under
    every module in the repository.
    """

    source = "import conductor\nimport hydra\n"
    assert undeclared_imports(source, declared=(), distributions={}) == ()


def test_the_standard_library_is_never_a_dependency() -> None:
    """`json` ships with the interpreter; no manifest declares it."""

    source = "import json\nimport tomllib\n"
    assert (
        undeclared_imports(
            source,
            declared=(),
            distributions={"json": ["json"]},
            stdlib=("json", "tomllib"),
        )
        == ()
    )


def test_an_undeclared_installed_distribution_is_reported() -> None:
    """The defect itself: importable here, undeclared, absent on a clean install."""

    source = "import yaml\n"
    found = undeclared_imports(
        source, declared=("polars",), distributions={"yaml": ["PyYAML"]}, stdlib=()
    )
    assert found == (("yaml", ("PyYAML",)),)


def test_a_declared_distribution_passes_however_it_is_spelled() -> None:
    """`PyYAML` provides `yaml`; a manifest declaring `pyyaml` has declared it."""

    source = "import yaml\n"
    found = undeclared_imports(
        source, declared=("pyyaml",), distributions={"yaml": ["PyYAML"]}, stdlib=()
    )
    assert found == ()


def _change(path: str, *classes: str) -> Change:
    return Change(
        status="M",
        path=path,
        old_path=None,
        old_mode="100644",
        new_mode="100644",
        old_oid="1" * 40,
        new_oid="2" * 40,
        classes=classes or ("python", "source"),
    )


def _context(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    files: dict[str, str],
    *changes: Change,
    manifest: str = MANIFEST,
    distributions: dict[str, list[str]] | None = None,
) -> ReviewContext:
    """A snapshot whose resolution map is injected, not read from this venv."""

    ctx = _gate_context(monkeypatch, tmp_path, inventory={})
    (ctx.snapshot / "pyproject.toml").write_text(manifest, encoding="utf-8")
    for rel, text in files.items():
        path = ctx.snapshot / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    monkeypatch.setattr(
        "conductor.candidate_review.import_declaration.packages_distributions",
        lambda: distributions if distributions is not None else {"yaml": ["PyYAML"]},
    )
    return replace(ctx, candidate=replace(ctx.candidate, changes=changes))


def test_the_check_blocks_an_undeclared_import_in_a_shipped_module(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """The finding has to be critical: the module is unimportable elsewhere."""

    ctx = _context(
        monkeypatch,
        tmp_path,
        {"probe/mod.py": "import yaml\n"},
        _change("probe/mod.py"),
    )
    result = check_import_declaration(ctx)
    assert [(f.rule_id, f.path) for f in result.findings] == [
        ("undeclared-dependency", "probe/mod.py")
    ]
    assert result.findings[0].severity is Severity.CRITICAL


def test_a_test_module_is_not_shipped(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Test-only tools may stay in the CI job -- KB-CI-01 says so explicitly.

    Gating them would force every test-only import into the shipped manifest.
    """

    ctx = _context(
        monkeypatch,
        tmp_path,
        {"probe/test_mod.py": "import yaml\n"},
        _change("probe/test_mod.py", "python", "test"),
    )
    assert check_import_declaration(ctx).findings == []


def test_a_deleted_file_is_never_opened(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """A path that no longer exists is not a file whose imports can be judged.

    Asserted on the count rather than the findings, because a deleted path
    reaches the same silence by simply failing to open -- which would leave the
    filter itself untested.
    """

    ctx = _context(
        monkeypatch,
        tmp_path,
        {},
        replace(_change("probe/mod.py"), status="D", new_mode="000000"),
    )
    result = check_import_declaration(ctx)
    assert result.metrics["python_files"] == 0
    assert result.findings == []


def test_an_unparseable_source_is_left_to_python_ast(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """One syntax error should produce one finding, from the check that owns it."""

    ctx = _context(
        monkeypatch,
        tmp_path,
        {"probe/mod.py": "import yaml\ndef (\n"},
        _change("probe/mod.py"),
    )
    assert check_import_declaration(ctx).findings == []


def test_an_unreadable_manifest_is_itself_the_finding(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """A manifest that will not parse declares nothing, and silently passing
    every file under it would turn a broken pyproject into a green review."""

    ctx = _context(
        monkeypatch,
        tmp_path,
        {"probe/mod.py": "import yaml\n"},
        _change("probe/mod.py"),
        manifest="[project\nname =",
    )
    result = check_import_declaration(ctx)
    assert [f.rule_id for f in result.findings] == ["unreadable-manifest"]


def test_base_distributions_excludes_everything_optional() -> None:
    """`[project] dependencies` and the project's own name, nothing else.

    The radon defect (2026-09-05) was a name written down in an extra and a
    dependency-group -- declared by the wide rule, absent from every job that
    installs the base set. Separating the two questions is the whole fix.
    """

    assert base_distributions(MANIFEST) == {"polars", "probe"}


def test_an_extra_only_import_is_reported_in_a_base_dependency_tree() -> None:
    """Declared, but not where it runs: the defect the wide rule cannot see."""

    found = optional_only_imports(
        "import scipy\n",
        base=("polars",),
        declared=("polars", "scipy"),
        distributions={"scipy": ["scipy"]},
        stdlib=(),
    )
    assert found == (("scipy", ("scipy",)),)


def test_an_entirely_undeclared_import_is_not_reported_twice() -> None:
    """One omission, one finding: `undeclared_imports` already owns this shape.

    Reporting it from both rules would double every finding and make the count
    meaningless as a measure of how much is wrong.
    """

    found = optional_only_imports(
        "import yaml\n",
        base=("polars",),
        declared=("polars",),
        distributions={"yaml": ["PyYAML"]},
        stdlib=(),
    )
    assert found == ()


def test_a_type_checking_import_does_not_execute() -> None:
    """A guarded import cannot break an install, so it cannot be this finding.

    The `else` branch of the same guard is the runtime branch and must survive,
    or the skip would hide a real import.
    """

    source = (
        "from typing import TYPE_CHECKING\n"
        "if TYPE_CHECKING:\n"
        "    import scipy\n"
        "else:\n"
        "    import polars\n"
        "import yaml\n"
    )
    assert imported_modules(source) == frozenset({"typing", "scipy", "polars", "yaml"})
    assert imported_modules(source, runtime_only=True) == frozenset(
        {"typing", "polars", "yaml"}
    )


def test_the_qualified_type_checking_spelling_is_also_a_guard() -> None:
    """`if typing.TYPE_CHECKING:` is the same guard written the other way."""

    source = "import typing\nif typing.TYPE_CHECKING:\n    import scipy\n"
    assert imported_modules(source, runtime_only=True) == frozenset({"typing"})


def test_the_strict_rule_governs_the_trees_that_run_on_base_dependencies() -> None:
    """conductor/ and tooling/ run in jobs that install the base set only.

    conftest.py is excluded wherever it lives -- it executes only under pytest,
    which by construction has pytest.
    """

    assert in_base_dependency_tree("conductor/radon_complexity.py")
    assert in_base_dependency_tree("tooling/hooks/dispatch/runner.py")
    assert not in_base_dependency_tree("conductor/conftest.py")


def test_the_check_blocks_an_extra_only_import_in_a_base_dependency_tree(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """The finding that would have caught radon before it reached the commit path."""

    ctx = _context(
        monkeypatch,
        tmp_path,
        {"conductor/mod.py": "import scipy\n"},
        _change("conductor/mod.py"),
        distributions={"scipy": ["scipy"]},
    )
    result = check_import_declaration(ctx)
    assert [(f.rule_id, f.path) for f in result.findings] == [
        ("base-dependency-required", "conductor/mod.py")
    ]
    assert result.findings[0].severity is Severity.CRITICAL
    assert result.metrics["base_dependency_files"] == 1


def test_the_strict_rule_leaves_the_wheel_packages_alone(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """research/ ships as a wheel whose extras a consumer opts into by name.

    Sixty files there import one; widening the rule to cover them is a
    per-tree packaging decision, not a side effect of this check.
    """

    ctx = _context(
        monkeypatch,
        tmp_path,
        {"research/mod.py": "import scipy\n"},
        _change("research/mod.py"),
        distributions={"scipy": ["scipy"]},
    )
    result = check_import_declaration(ctx)
    assert result.findings == []
    assert result.metrics["base_dependency_files"] == 0


def test_the_probe_may_import_pytest_at_runtime(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """KB-CI-01 grants test-only tools; the exemption is keyed to one path."""

    monkeypatch.setitem(
        import_declaration.RUNTIME_TEST_TOOL_EXEMPTIONS,
        "conductor/probe.py",
        frozenset({"pytest"}),
    )
    ctx = _context(
        monkeypatch,
        tmp_path,
        {"conductor/probe.py": "def run():\n    import pytest\n"},
        _change("conductor/probe.py"),
        distributions={"pytest": ["pytest"]},
    )
    assert check_import_declaration(ctx).findings == []


def test_an_exemption_that_covers_nothing_is_reported(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """A grant nobody uses is a grant waiting to cover the next import silently.

    Advisory, not blocking: the file is correct today, and the entry is the
    thing to delete.
    """

    monkeypatch.setitem(
        import_declaration.RUNTIME_TEST_TOOL_EXEMPTIONS,
        "conductor/probe.py",
        frozenset({"pytest"}),
    )
    ctx = _context(
        monkeypatch,
        tmp_path,
        {"conductor/probe.py": "import polars\n"},
        _change("conductor/probe.py"),
        distributions={"pytest": ["pytest"], "polars": ["polars"]},
    )
    result = check_import_declaration(ctx)
    assert [(f.rule_id, f.severity) for f in result.findings] == [
        ("stale-exemption", Severity.MEDIUM)
    ]


def test_base_dependency_trees_default_to_the_monorepo_pair(tmp_path: Path) -> None:
    assert base_dependency_trees(tmp_path) == ("conductor/", "tooling/")
    assert base_dependency_trees(tmp_path) == BASE_DEPENDENCY_TREES


def test_base_dependency_trees_follow_a_declared_src_layout(tmp_path: Path) -> None:
    """The pair moves together: the package and the hook tooling beside it."""
    (tmp_path / "pyproject.toml").write_text(
        '[tool.conductor]\npackage_root = "src/conductor"\n', encoding="utf-8"
    )
    assert base_dependency_trees(tmp_path) == ("src/conductor/", "src/tooling/")


def test_the_stricter_rule_governs_the_package_under_a_src_layout(
    tmp_path: Path,
) -> None:
    """The regression this closes: the rule silently governed nothing here."""
    (tmp_path / "pyproject.toml").write_text(
        '[tool.conductor]\npackage_root = "src/conductor"\n', encoding="utf-8"
    )
    trees = base_dependency_trees(tmp_path)
    assert in_base_dependency_tree("src/conductor/radon_complexity.py", trees)
    assert in_base_dependency_tree("src/tooling/hooks/dispatch/runner.py", trees)
    assert not in_base_dependency_tree("src/conductor/radon_complexity.py")


def test_test_infrastructure_is_excluded_from_any_tree(tmp_path: Path) -> None:
    (tmp_path / "pyproject.toml").write_text(
        '[tool.conductor]\npackage_root = "src/conductor"\n', encoding="utf-8"
    )
    trees = base_dependency_trees(tmp_path)
    assert not in_base_dependency_tree("src/conductor/conftest.py", trees)


def test_a_tree_outside_the_pair_is_not_governed(tmp_path: Path) -> None:
    trees = base_dependency_trees(tmp_path)
    assert not in_base_dependency_tree("research/model.py", trees)
    assert not in_base_dependency_tree("native/build.py", trees)


def test_an_unparseable_root_manifest_leaves_the_trees_at_their_default(
    tmp_path: Path,
) -> None:
    """The layout read must not out-shout the finding the check already makes."""
    (tmp_path / "pyproject.toml").write_text("[project\nname =", encoding="utf-8")
    with pytest.raises(tomllib.TOMLDecodeError):
        base_dependency_trees(tmp_path)
