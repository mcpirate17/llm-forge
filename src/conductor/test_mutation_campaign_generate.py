"""Contracts for the campaign generator.

The generator's job is to remove the hand step that left 761,780 lines of Python
and eight of nine Rust crates unmeasured. Its failure modes are all silent: a
manifest that overwrites a recorded survivor baseline, two subjects sharing one
campaign id, or a venv swept in as a subject would each still report green.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from conductor.mutation_campaign_generate import (
    _package_name,
    _unique_slugs,
    existing_subjects,
    plan,
    python_subjects,
    rust_subjects,
    write,
)
from conductor.mutation_engine_generated import load_generated_campaign
from conductor.mutation_scope import CampaignError

REPO_ROOT = Path(__file__).resolve().parents[1]


def tree(root: Path, files: dict[str, str]) -> Path:
    """Materialise a fake repository."""

    for relative, body in files.items():
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body, encoding="utf-8")
    return root


CRATE = """
[package]
name = "widget-core"
version = "0.1.0"

[[bin]]
name = "not-the-package"

[dependencies]
name = "also-not-the-package"
"""


def test_the_package_name_comes_from_package_not_bin_or_dependencies(
    tmp_path: Path,
) -> None:
    """`--package` names what cargo-mutants mutates.

    `[[bin]]` and `[dependencies]` both carry a `name` key. Taking the first one
    in the file would point a whole campaign at a different crate, and it would
    still run and still publish a score.
    """

    path = tmp_path / "Cargo.toml"
    path.write_text(CRATE, encoding="utf-8")
    assert _package_name(path) == "widget-core"


def test_a_workspace_root_declaring_no_package_is_not_a_subject(tmp_path: Path) -> None:
    """Mutating a virtual workspace would target whatever it happens to contain."""

    tree(
        tmp_path,
        {
            "Cargo.toml": '[workspace]\nmembers = ["w"]\n',
            "w/Cargo.toml": '[package]\nname = "w"\n',
            "w/src/lib.rs": "pub fn f() -> i32 { 1 }\n#[cfg(test)]\nmod t {}\n",
        },
    )
    assert [c["package"] for c in rust_subjects(tmp_path)] == ["w"]


def test_a_crate_with_a_package_but_no_sources_is_refused(tmp_path: Path) -> None:
    """Silently emitting a campaign over nothing would publish a green empty run."""

    tree(tmp_path, {"w/Cargo.toml": '[package]\nname = "w"\n'})
    with pytest.raises(CampaignError, match="no src"):
        rust_subjects(tmp_path)


def test_a_vendored_or_virtualenv_tree_is_never_a_subject(tmp_path: Path) -> None:
    """A venv in the worktree put 262 pygments lexers into a first count of this."""

    tree(
        tmp_path,
        {
            "pkg/real.py": "x = 1\n",
            "pkg/test_real.py": "def test_x(): pass\n",
            "venv-mut/lib/python3.12/site-packages/rich/console.py": "x = 1\n",
            ".venv/lib/python3.12/site-packages/rich/test_console.py": "x = 1\n",
            "target/debug/build.py": "x = 1\n",
            "node_modules/a/b.py": "x = 1\n",
        },
    )
    paired, unpaired = python_subjects(tmp_path)
    assert [s["source"] for s in paired] == ["pkg/real.py"]
    assert unpaired == []


def test_both_test_layouts_in_this_repo_are_paired(tmp_path: Path) -> None:
    """`conductor/` keeps tests beside sources; `research/` keeps them in tests/."""

    tree(
        tmp_path,
        {
            "conductor/gate.py": "x = 1\n",
            "conductor/test_gate.py": "def test_x(): pass\n",
            "research/tools/report.py": "x = 1\n",
            "research/tests/test_report.py": "def test_x(): pass\n",
            "research/tools/orphan.py": "x = 1\n" * 7,
        },
    )
    paired, unpaired = python_subjects(tmp_path)
    assert {s["source"] for s in paired} == {
        "conductor/gate.py",
        "research/tools/report.py",
    }
    assert [(u["source"], u["lines"]) for u in unpaired] == [
        ("research/tools/orphan.py", 7)
    ]


def test_a_subject_a_committed_campaign_already_covers_is_skipped(
    tmp_path: Path,
) -> None:
    """A second campaign for one subject is born with an empty survivor baseline.

    It would then report green over exactly the survivors the first campaign is
    holding a ratchet against -- a regression that arrives looking like a pass.
    """

    tree(
        tmp_path,
        {
            "a/Cargo.toml": '[package]\nname = "a"\n',
            "a/src/lib.rs": "pub fn f() -> i32 { 1 }\n#[cfg(test)]\nmod t {}\n",
            "b/Cargo.toml": '[package]\nname = "b"\n',
            "b/src/lib.rs": "pub fn g() -> i32 { 2 }\n#[cfg(test)]\nmod t {}\n",
            "conductor/mutation_campaigns/old.json": json.dumps(
                {
                    "mutation_engine": "cargo-mutants",
                    "generator": {
                        "source": ["src/**/*.rs"],
                        "options": {"package": "a"},
                    },
                }
            ),
        },
    )
    assert "a" in existing_subjects(tmp_path)
    result = plan("rust", repo_root=tmp_path, day="20260907")
    assert result["already_covered"] == ["a"]
    assert [m["generator"]["options"]["package"] for m in result["manifests"]] == ["b"]


def test_two_subjects_never_share_one_campaign_id() -> None:
    """A collision silently overwrites one manifest with the other."""

    slugs = _unique_slugs(
        ["conductor/gate.py", "research/tools/gate.py", "conductor/unique.py"]
    )
    assert slugs["conductor/unique.py"] == "unique"
    assert len(set(slugs.values())) == 3
    assert slugs["conductor/gate.py"] == "conductor_gate"


def test_write_refuses_to_replace_a_recorded_baseline(tmp_path: Path) -> None:
    """Overwriting a run campaign with an empty baseline erases its ratchet."""

    (tmp_path / "conductor/mutation_campaigns").mkdir(parents=True)
    manifest = {"campaign_id": "x", "survivor_baseline": []}
    assert write([manifest], repo_root=tmp_path) == [
        "conductor/mutation_campaigns/x.json"
    ]
    with pytest.raises(CampaignError, match="refusing to replace"):
        write([manifest], repo_root=tmp_path)
    assert write([manifest], repo_root=tmp_path, force=True)


def test_a_generated_rust_manifest_loads_as_a_generated_campaign() -> None:
    """The generator's output must satisfy the runner's own model, not resemble it."""

    result = plan("rust", day="20260907")
    manifest = next(
        m for m in result["manifests"] if m["generator"]["options"]["package"]
    )
    path = Path(REPO_ROOT / "conductor/mutation_campaigns") / "_generated_probe.json"
    try:
        path.write_text(json.dumps(manifest), encoding="utf-8")
        loaded = load_generated_campaign(path)
        assert loaded.mutation_engine == "cargo-mutants"
        assert loaded.language == "rust"
        assert loaded.source_sha256, "every mutated file must be pinned"
        assert "CARGO_TARGET_DIR" not in loaded.environment, (
            "a shared build directory makes whether a mutant compiles jitter"
        )
    finally:
        path.unlink(missing_ok=True)


def _assert_names_its_own_tests(manifest: dict) -> None:
    """Every invariant that stops a python campaign scoring a subject it never ran."""

    source = manifest["generator"]["source"][0]
    stem = source.rsplit("/", 1)[-1].removesuffix(".py")
    # --rootdir=. is load-bearing, not cosmetic: a nested pytest.ini under
    # research/ or component_fab/ otherwise wins rootdir discovery and reports
    # nodeids relative to that subtree, so nothing in the run matches a nodeid
    # in test_argv and the campaign scores with test_value null.
    assert manifest["test_argv"][:5] == [
        "python",
        "-m",
        "pytest",
        "-q",
        "--rootdir=.",
    ]
    tests = manifest["test_argv"][5:]
    assert tests, f"{source} was planned with no test to run"
    assert any(stem in test for test in tests), (
        f"{source} runs {tests}, none of which names it"
    )
    # The digests are what stop a receipt outliving the tests it describes,
    # and they have to agree with the command that produced it.
    assert set(manifest["test_sha256"]) == set(tests)
    assert manifest["generator"].get("jobs", 1) == 1, (
        "fest at host width was measured giving 4/5/5/6/5 survivors over five runs"
    )


def test_a_generated_python_manifest_runs_the_tests_that_name_its_subject(
    tmp_path: Path,
) -> None:
    """A campaign whose test_argv misses its own tests reports every mutant survived.

    Planned against a fixture rather than this repo: once the repo-wide sweep has
    a campaign for every paired module, `plan` here returns nothing, and a check
    over an empty list passes without ever having checked anything.
    """

    tree(
        tmp_path,
        {
            "conductor/gate.py": "x = 1\n",
            "conductor/test_gate.py": "def test_x(): pass\n",
            "research/tools/report.py": "x = 1\n",
            "research/tests/test_report.py": "def test_x(): pass\n",
        },
    )
    planned = plan("python", repo_root=tmp_path, day="20260907")["manifests"]
    assert [m["generator"]["source"][0] for m in planned] == [
        "conductor/gate.py",
        "research/tools/report.py",
    ]
    for manifest in planned:
        _assert_names_its_own_tests(manifest)

    # Whatever this repo still has unplanned has to satisfy the same rule.
    for manifest in plan("python", day="20260907")["manifests"]:
        _assert_names_its_own_tests(manifest)
