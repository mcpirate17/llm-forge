"""Contracts for the campaign generator.

The generator's job is to remove the hand step that left 761,780 lines of Python
and eight of nine Rust crates unmeasured. Its failure modes are all silent: a
manifest that overwrites a recorded survivor baseline, two subjects sharing one
campaign id, or a venv swept in as a subject would each still report green.
"""

from __future__ import annotations

import json
from pathlib import Path
import subprocess

import pytest

from conductor.mutation_campaign_generate import (
    _branch_scope,
    _package_name,
    _unique_slugs,
    changed_sources,
    existing_subjects,
    plan,
    refresh_rust_campaign,
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

RUST_UNIT_TESTS = """
pub fn keep() -> u8 { 1 }

#[cfg(test)]
mod tests {
    #[test]
    fn t() {}
}
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


def git_repo(root: Path, files: dict[str, str]) -> None:
    """A real git repository, because the scope rule is a real git question."""

    tree(root, files)
    run = lambda *a: subprocess.run(  # noqa: E731
        ("git", "-C", str(root), *a), check=True, capture_output=True
    )
    run("init", "-q", "-b", "main")
    run("config", "user.email", "scope@example.invalid")
    run("config", "user.name", "scope probe")
    run("add", "-A")
    run("commit", "-q", "-m", "base")


def test_the_scope_is_this_branchs_changed_files_and_nothing_else(
    tmp_path: Path,
) -> None:
    """Three changed files must mutate three files, never the whole repository.

    This is the rule the whole campaign generator exists to enforce: agents were
    running repo-wide sweeps for three-file changes. The scope is the committed
    diff against the base, the dirty working tree, and untracked files -- a brand
    new module is precisely the thing whose tests have never been mutated -- and
    nothing else in the tree may appear.
    """

    git_repo(
        tmp_path,
        {
            "pkg/committed.py": "x = 1\n",
            "pkg/dirty.py": "x = 1\n",
            "pkg/untouched.py": "x = 1\n",
        },
    )
    git = lambda *a: subprocess.run(  # noqa: E731
        ("git", "-C", str(tmp_path), *a), check=True, capture_output=True
    )
    git("branch", "-q", "base")
    (tmp_path / "pkg/committed.py").write_text("x = 2\n", encoding="utf-8")
    git("commit", "-qam", "change one file")
    (tmp_path / "pkg/dirty.py").write_text("x = 3\n", encoding="utf-8")
    (tmp_path / "pkg/brand_new.py").write_text("x = 4\n", encoding="utf-8")

    assert changed_sources("base", repo_root=tmp_path) == {
        "pkg/committed.py",
        "pkg/dirty.py",
        "pkg/brand_new.py",
    }

    # A path git still reports but that no longer exists cannot be a subject.
    (tmp_path / "pkg/committed.py").unlink()
    assert changed_sources("base", repo_root=tmp_path) == {
        "pkg/dirty.py",
        "pkg/brand_new.py",
    }

    # Folded in from test_changing_only_a_test_still_plans_the_campaign_for_its
    # _source and test_a_scoped_rust_change_plans_only_the_crate_that_owns_it,
    # both classified MERGE with zero unique kills by the 2026-09-09 generated
    # campaign. What that scope buys is the same claim in both languages, so the
    # assertions are kept here and the redundant nodeids are not.
    #
    # A changed TEST is in scope as much as a changed source: editing only
    # `test_x.py` is exactly the case the campaign for `x.py` has to cover, and
    # scoping on the source path alone planned nothing for it.
    tree(
        tmp_path,
        {
            "conductor/subject.py": "x = 1\n",
            "conductor/test_subject.py": "def test_x(): pass\n",
            "conductor/bystander.py": "x = 1\n",
            "conductor/test_bystander.py": "def test_y(): pass\n",
        },
    )
    python_scoped = plan(
        "python",
        repo_root=tmp_path,
        day="20260909",
        only_sources=["conductor/test_subject.py"],
    )
    assert [m["generator"]["source"] for m in python_scoped["manifests"]] == [
        ["conductor/subject.py"]
    ]

    # And one file inside one crate must not pull in every other crate present.
    tree(
        tmp_path,
        {
            "crates/widget/Cargo.toml": CRATE,
            "crates/widget/src/lib.rs": RUST_UNIT_TESTS,
            "crates/other/Cargo.toml": CRATE.replace("widget-core", "other-core"),
            "crates/other/src/lib.rs": RUST_UNIT_TESTS,
        },
    )
    rust_scoped = plan(
        "rust",
        repo_root=tmp_path,
        day="20260909",
        only_sources=["crates/widget/src/lib.rs"],
    )
    assert [m["generator"]["options"]["package"] for m in rust_scoped["manifests"]] == [
        "widget-core"
    ]
    assert len(plan("rust", repo_root=tmp_path, day="20260909")["manifests"]) == 2


def test_a_base_git_cannot_resolve_yields_no_scope_rather_than_the_tree(
    tmp_path: Path,
) -> None:
    """A failed git call must narrow the scope, never silently widen it."""

    git_repo(tmp_path, {"pkg/a.py": "x = 1\n"})
    assert changed_sources("no-such-ref", repo_root=tmp_path) == set()


def test_an_empty_scope_is_refused_instead_of_becoming_a_whole_tree_sweep(
    tmp_path: Path,
) -> None:
    """Planning nothing is the accident that turns into a 568-campaign sweep."""

    git_repo(tmp_path, {"pkg/a.py": "x = 1\n"})
    with pytest.raises(CampaignError, match="--all-files"):
        _branch_scope("HEAD", repo_root=tmp_path)


def test_refresh_rebinds_a_cargo_campaign_to_the_sources_it_was_asked_for(
    tmp_path: Path,
) -> None:
    """The automatic repair path after a Rust edit, with the ratchet retained.

    A cargo campaign pins crate files by digest, so any edit to one rots it. The
    refresh re-derives every pin from today's tree and carries forward only the
    survivor baseline, which the engine recorded rather than an agent.
    """

    tree(
        tmp_path,
        {
            "crates/widget/Cargo.toml": CRATE,
            "crates/widget/src/lib.rs": RUST_UNIT_TESTS,
            "crates/widget/src/extra.rs": RUST_UNIT_TESTS,
        },
    )
    planned = plan("rust", repo_root=tmp_path, day="20260909")
    written = write(planned["manifests"], repo_root=tmp_path)
    path = tmp_path / written[0]
    payload = json.loads(path.read_text(encoding="utf-8"))
    payload["survivor_baseline"] = ["known"]
    payload["survivor_baseline_recorded"] = True
    path.write_text(json.dumps(payload, indent=2), encoding="utf-8")

    (tmp_path / "crates/widget/src/lib.rs").write_text(
        "#[test] fn t() { assert!(true); }\n", encoding="utf-8"
    )
    refresh_rust_campaign(
        payload["campaign_id"],
        sources=["crates/widget/src/lib.rs"],
        repo_root=tmp_path,
    )
    refreshed = json.loads(path.read_text(encoding="utf-8"))

    assert refreshed["generator"]["source"] == ["src/lib.rs"]
    assert list(refreshed["source_sha256"]) == ["crates/widget/src/lib.rs"]
    assert refreshed["source_sha256"] != payload["source_sha256"]
    assert refreshed["test_sha256"] == refreshed["source_sha256"]
    assert refreshed["survivor_baseline"] == ["known"]

    # Folded in from test_refresh_refuses_a_source_the_crate_does_not_own, which
    # the 2026-09-09 generated campaign classified MERGE with zero unique kills
    # against this test. Binding a file the crate does not own would publish
    # evidence for code the run never mutated.
    (tmp_path / "elsewhere").mkdir()
    (tmp_path / "elsewhere/lib.rs").write_text(RUST_UNIT_TESTS, encoding="utf-8")
    with pytest.raises(CampaignError, match="elsewhere/lib.rs"):
        refresh_rust_campaign(
            payload["campaign_id"], sources=["elsewhere/lib.rs"], repo_root=tmp_path
        )


def test_refresh_refuses_anything_that_is_not_a_generated_cargo_campaign(
    tmp_path: Path,
) -> None:
    """Only the generator's own output may be regenerated by the generator."""

    campaigns = tmp_path / "conductor/mutation_campaigns"
    campaigns.mkdir(parents=True)
    (campaigns / "hand.json").write_text(
        json.dumps({"mutation_engine": "reviewed_unified_diff"}), encoding="utf-8"
    )
    with pytest.raises(CampaignError, match="not a cargo-mutants generated campaign"):
        refresh_rust_campaign("hand", repo_root=tmp_path)

    with pytest.raises(CampaignError, match="cannot load generated campaign"):
        refresh_rust_campaign("absent", repo_root=tmp_path)


def test_a_narrow_second_campaign_may_be_planned_over_a_covered_source(
    tmp_path: Path,
) -> None:
    """The incumbent campaign is often broader than the change that rots it.

    One campaign over five modules costs an hour to answer a question about one
    file, and retiring it to make room would throw away the ratchet it holds.
    `--include-covered` plans the narrow second campaign instead; the newest
    valid receipt wins the evidence row and the incumbent stays registered.
    """

    tree(
        tmp_path,
        {
            "conductor/subject.py": "x = 1\n",
            "conductor/test_subject.py": "def test_x(): pass\n",
            "conductor/mutation_campaigns/wide.json": json.dumps(
                {
                    "campaign_id": "wide",
                    "mutation_engine": "fest",
                    "generator": {"source": ["conductor/subject.py"]},
                }
            ),
            "conductor/mutation_campaigns/registry.json": json.dumps(
                {"campaigns": [{"manifest": "conductor/mutation_campaigns/wide.json"}]}
            ),
        },
    )
    scope = ["conductor/subject.py"]
    skipped = plan("python", repo_root=tmp_path, day="20260909", only_sources=scope)
    assert skipped["manifests"] == []
    assert skipped["already_covered"] == ["conductor/subject.py"]

    narrow = plan(
        "python",
        repo_root=tmp_path,
        day="20260909",
        only_sources=scope,
        include_covered=True,
    )
    assert [m["generator"]["source"] for m in narrow["manifests"]] == [
        ["conductor/subject.py"]
    ]
    assert narrow["already_covered"] == []
