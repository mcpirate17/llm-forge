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
from datetime import UTC, datetime

import pytest

from conductor import mutation_campaign_generate as campaign_generate
from conductor.mutation_campaign_generate import (
    _branch_scope,
    _explicit_scope,
    _is_test,
    _package_name,
    _unique_slugs,
    cargo_manifest,
    changed_sources,
    fest_manifest,
    existing_subjects,
    plan,
    refresh_python_campaign,
    refresh_rust_campaign,
    python_subjects,
    rust_subjects,
    rust_test_files,
    write,
)
from conductor.candidate_review.ownership import create_claim
from conductor.mutation_engine_generated import load_generated_campaign
from conductor.mutation_scope import CampaignError
from conductor.project_paths import host_root

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
    path.write_text('[dependencies]\nname = "not-a-package"\n', encoding="utf-8")
    assert _package_name(path) is None
    path.write_text("[package]\nname = 'quoted-package'\n", encoding="utf-8")
    assert _package_name(path) == "quoted-package"
    # A bare key before the first TOML section is not a package declaration.
    # This also distinguishes the initial state from a package section that has
    # already been seen.
    path.write_text('name = "headerless"\n[dependencies]\n', encoding="utf-8")
    assert _package_name(path) is None
    # `version` is valid in `[package]`, but must not be mistaken for its name.
    path.write_text(
        '[package]\nversion = "0.1.0"\nname = "after-version"\n', encoding="utf-8"
    )
    assert _package_name(path) == "after-version"


def test_test_file_recognition_covers_all_supported_layouts_only() -> None:
    """A test outside the two layouts cannot silently become a mutable source."""

    assert _is_test("conductor/conftest.py", "conftest.py")
    assert _is_test("conductor/test_campaign.py", "test_campaign.py")
    assert _is_test("research/tests/check_campaign.py", "check_campaign.py")
    assert not _is_test("research/tools/check_campaign.py", "check_campaign.py")


def test_fest_manifest_binds_its_generated_engine_contract(tmp_path: Path) -> None:
    """Generated Python campaigns must retain the exact engine and timeout contract."""

    tree(
        tmp_path,
        {"pkg/subject.py": "x = 1\n", "pkg/test_subject.py": "def test_x(): pass\n"},
    )
    manifest = fest_manifest(
        {"source": "pkg/subject.py", "tests": ["pkg/test_subject.py"]},
        campaign_id="owner_subject_fest_20260910",
        repo_root=tmp_path,
        run_timeout_seconds=91,
    )
    assert manifest["schema_version"] == 1
    assert (
        manifest["title"]
        == "Generated mutants for pkg/subject.py, scored on the survivor set"
    )
    assert manifest["language"] == "python"
    assert manifest["mutation_engine"] == "fest"
    assert manifest["generator"]["exclude"] == ["**/test_*.py", "**/conftest.py"]
    assert manifest["generator"]["operators"] == []
    assert manifest["generator"]["seed"] == 0
    # No per-mutant bound is written: the run derives it from the baseline
    # suite's wall time (3x, floored at 60 s) and records the value it used.
    # The old pinned 30 timed out six honest kills of native/forge on every
    # run, each of them then read as a campaign ERROR.
    assert "mutant_timeout_seconds" not in manifest["generator"]
    assert manifest["generator"]["run_timeout_seconds"] == 91
    assert manifest["environment"] == {}
    assert manifest["survivor_baseline"] == []
    assert manifest["survivor_baseline_recorded"] is False
    assert "FIRST engine run" in manifest["survivor_baseline_note"]


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


def test_rust_test_discovery_includes_integration_and_inline_tests_but_skips_venvs(
    tmp_path: Path,
) -> None:
    """The manifest must hash every test cargo executes, not build artefacts."""

    tree(
        tmp_path,
        {
            "crate/src/lib.rs": "#[cfg(test)]\nmod unit {}\n",
            "crate/tests/integration.rs": "#[test]\nfn it_works() {}\n",
            "crate/tests/.venv/generated.rs": "#[test]\nfn stale() {}\n",
        },
    )
    subject = {
        "package": "crate",
        "root": "crate",
        "manifest": "crate/Cargo.toml",
        "files": ["crate/src/lib.rs"],
    }
    assert rust_test_files(subject, repo_root=tmp_path) == [
        "crate/src/lib.rs",
        "crate/tests/integration.rs",
    ]


def test_cargo_manifest_preserves_the_exact_scoped_engine_contract(
    tmp_path: Path,
) -> None:
    """A selected Rust file must retain its crate root, pinning and limits."""

    tree(
        tmp_path,
        {
            "crate/src/lib.rs": "#[cfg(test)]\nmod unit {}\n",
            "crate/tests/integration.rs": "#[test]\nfn it_works() {}\n",
        },
    )
    subject = {
        "package": "crate",
        "root": "crate",
        "manifest": "crate/Cargo.toml",
        "files": ["crate/src/lib.rs"],
    }
    manifest = cargo_manifest(
        subject,
        campaign_id="owner_crate_cargo_20260910",
        repo_root=tmp_path,
        jobs=2,
        run_timeout_seconds=91,
        sources=["crate/src/lib.rs"],
    )
    assert manifest["schema_version"] == 1
    assert manifest["generator"]["source"] == ["src/lib.rs"]
    assert manifest["generator"]["exclude"] == []
    assert manifest["generator"]["operators"] == []
    assert manifest["generator"]["options"] == {
        "manifest_path": "crate/Cargo.toml",
        "package": "crate",
        "package_root": "crate",
    }
    assert manifest["generator"]["seed"] == 0
    # Derived per run from the baseline wall time; see the fest builder test.
    assert "mutant_timeout_seconds" not in manifest["generator"]
    assert manifest["generator"]["run_timeout_seconds"] == 91
    assert manifest["test_argv"] == [
        "cargo",
        "test",
        "--manifest-path",
        "crate/Cargo.toml",
        "--package",
        "crate",
    ]
    assert manifest["environment"] == {}
    assert manifest["survivor_baseline"] == []
    assert manifest["survivor_baseline_recorded"] is False
    assert "FIRST engine run" in manifest["survivor_baseline_note"]


def test_rust_planning_intersects_scope_instead_of_expanding_to_the_crate(
    tmp_path: Path,
) -> None:
    """Changing one Rust file may not cause cargo-mutants to target its sibling."""

    tree(
        tmp_path,
        {
            "crate/Cargo.toml": '[package]\nname = "crate"\n',
            "crate/src/lib.rs": "#[cfg(test)]\nmod unit {}\n",
            "crate/src/sibling.rs": "pub fn sibling() {}\n",
        },
    )
    planned = plan(
        "rust",
        repo_root=tmp_path,
        day="20260910",
        only_sources=["crate/src/lib.rs"],
    )
    assert planned["language"] == "rust"
    assert planned["unpaired"] == []
    assert planned["unpaired_lines"] == 0
    assert planned["manifests"][0]["generator"]["source"] == ["src/lib.rs"]


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


def test_same_basename_tests_in_two_packages_pair_with_their_own_modules(
    tmp_path: Path,
) -> None:
    """Basename-only pairing once crossed packages and every mutant went unreached."""

    tree(
        tmp_path,
        {
            "src/alpha/__main__.py": "x = 1\n",
            "src/alpha/test___main__.py": "def test_a(): pass\n",
            "src/beta/__main__.py": "x = 2\n",
            "src/beta/test___main__.py": "def test_b(): pass\n",
        },
    )
    paired, unpaired = python_subjects(tmp_path)
    by_source = {subject["source"]: subject["tests"] for subject in paired}
    assert by_source["src/alpha/__main__.py"] == ["src/alpha/test___main__.py"]
    assert by_source["src/beta/__main__.py"] == ["src/beta/test___main__.py"]
    assert unpaired == []


def test_an_unmirrored_basename_collision_refuses_naming_both_candidates(
    tmp_path: Path,
) -> None:
    """Two unrelated same-basename tests with no mirror is a refusal, not a guess."""

    tree(
        tmp_path,
        {
            "pkg/subject.py": "x = 1\n",
            "elsewhere/test_subject.py": "def test_x(): pass\n",
            "further/test_subject.py": "def test_y(): pass\n",
        },
    )
    with pytest.raises(CampaignError) as exc:
        python_subjects(tmp_path)
    assert "elsewhere/test_subject.py" in str(exc.value)
    assert "further/test_subject.py" in str(exc.value)


def test_a_sole_unmirrored_same_basename_test_still_pairs(tmp_path: Path) -> None:
    """The legacy layout -- one test elsewhere -- keeps its pairing."""

    tree(
        tmp_path,
        {
            "pkg/candidate_review/policy.py": "x = 1\n",
            "pkg/test_policy.py": "def test_x(): pass\n",
        },
    )
    paired, _unpaired = python_subjects(tmp_path)
    by_source = {subject["source"]: subject["tests"] for subject in paired}
    assert by_source["pkg/candidate_review/policy.py"] == ["pkg/test_policy.py"]


def test_a_tests_mirror_beats_a_same_basename_stranger(tmp_path: Path) -> None:
    """With a mirror present, the fallback must not get a vote.

    A module with one test under its own ``tests/`` mirror and a stranger
    elsewhere sharing the basename is exactly the case the mirror rule
    exists for: the mirror says which test counts, where basename-counting
    alone would see two candidates and refuse (or, before the rule, guess).
    The fallback is for modules with NO mirror -- letting it fire here would
    make the mirror branch unobservable.
    """

    tree(
        tmp_path,
        {
            "pkg/deep/subject.py": "x = 1\n",
            "pkg/tests/deep/test_subject.py": "def test_x(): pass\n",
            "elsewhere/test_subject.py": "def test_y(): pass\n",
        },
    )
    paired, unpaired = python_subjects(tmp_path)
    by_source = {subject["source"]: subject["tests"] for subject in paired}
    assert by_source["pkg/deep/subject.py"] == ["pkg/tests/deep/test_subject.py"]
    assert unpaired == []


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
    assert (
        (tmp_path / "conductor/mutation_campaigns/x.json")
        .read_text(encoding="utf-8")
        .endswith("\n")
    )


def test_a_generated_rust_manifest_loads_as_a_generated_campaign(
    tmp_path: Path,
) -> None:
    """The generator's output must satisfy the runner's own model, not resemble it."""

    result = plan("rust", day="20260907", repo_root=host_root(Path(__file__)))
    manifest = next(
        m for m in result["manifests"] if m["generator"]["options"]["package"]
    )
    path = tmp_path / "_generated_probe.json"
    path.write_text(json.dumps(manifest), encoding="utf-8")
    loaded = load_generated_campaign(path)
    assert loaded.mutation_engine == "cargo-mutants"
    assert loaded.language == "rust"
    assert loaded.source_sha256, "every mutated file must be pinned"
    assert "CARGO_TARGET_DIR" not in loaded.environment, (
        "a shared build directory makes whether a mutant compiles jitter"
    )


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


def _assert_branch_scope_is_limited_to_changed_files(
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
    create_claim(
        tmp_path,
        owner="main",
        paths=["pkg/committed.py", "pkg/dirty.py", "pkg/brand_new.py"],
        justification="scope fixture owns its deliberate dirty files",
    )

    assert changed_sources("base", repo_root=tmp_path, owner="main") == {
        "pkg/committed.py",
        "pkg/dirty.py",
        "pkg/brand_new.py",
    }

    # A path git still reports but that no longer exists cannot be a subject.
    (tmp_path / "pkg/committed.py").unlink()
    assert changed_sources("base", repo_root=tmp_path, owner="main") == {
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
    manifest = rust_scoped["manifests"][0]
    assert manifest["generator"]["source"] == ["src/lib.rs"]
    assert list(manifest["source_sha256"]) == ["crates/widget/src/lib.rs"]
    assert "crates/other/src/lib.rs" not in manifest["source_sha256"]
    assert len(plan("rust", repo_root=tmp_path, day="20260909")["manifests"]) == 2


def test_shared_dirty_scope_never_admits_another_lanes_files(tmp_path: Path) -> None:
    """A shared checkout has unrelated dirt, so claims are the admission boundary."""

    git_repo(tmp_path, {"pkg/mine.py": "x = 1\n", "pkg/theirs.py": "x = 1\n"})
    (tmp_path / "pkg/mine.py").write_text("x = 2\n", encoding="utf-8")
    (tmp_path / "pkg/theirs.py").write_text("x = 2\n", encoding="utf-8")
    create_claim(
        tmp_path,
        owner="mine",
        paths=["pkg/mine.py"],
        justification="scope fixture lane one",
    )
    create_claim(
        tmp_path,
        owner="theirs",
        paths=["pkg/theirs.py"],
        justification="scope fixture lane two",
    )

    assert changed_sources("HEAD", repo_root=tmp_path, owner="mine") == {"pkg/mine.py"}
    with pytest.raises(CampaignError, match="no active ownership claim"):
        changed_sources("HEAD", repo_root=tmp_path, owner="missing")
    _assert_branch_scope_is_limited_to_changed_files(tmp_path / "branch-scope")


def test_explicit_scope_is_exact_and_does_not_read_shared_dirty_files(
    tmp_path: Path,
) -> None:
    """`--only` accepts exact existing paths and rejects traversal or absent inputs."""

    tree(tmp_path, {"pkg/subject.py": "x = 1\n"})
    assert _explicit_scope(
        ["pkg/subject.py", "pkg/subject.py"], repo_root=tmp_path
    ) == ["pkg/subject.py"]
    assert _explicit_scope(["pkg\\subject.py"], repo_root=tmp_path) == [
        "pkg/subject.py"
    ]
    with pytest.raises(CampaignError, match="repository-relative"):
        _explicit_scope(["./pkg/subject.py"], repo_root=tmp_path)
    with pytest.raises(CampaignError, match="repository-relative"):
        _explicit_scope(["../pkg/subject.py"], repo_root=tmp_path)
    with pytest.raises(CampaignError, match="repository-relative"):
        _explicit_scope(["/pkg/subject.py"], repo_root=tmp_path)
    with pytest.raises(CampaignError, match="does not exist"):
        _explicit_scope(["pkg/absent.py"], repo_root=tmp_path)


def test_default_campaign_day_is_utc_and_an_explicit_day_wins(
    monkeypatch, tmp_path: Path
) -> None:
    """Campaign ids are reproducible on request and calendar-correct by default."""

    tree(
        tmp_path,
        {"pkg/subject.py": "x = 1\n", "pkg/test_subject.py": "def test_x(): pass\n"},
    )

    class FrozenDatetime:
        @classmethod
        def now(cls, tz):  # noqa: ANN001 - mirrors datetime.now
            assert tz is not None
            return datetime(2026, 1, 2, tzinfo=UTC)

    monkeypatch.setattr(campaign_generate, "datetime", FrozenDatetime)
    assert plan("python", repo_root=tmp_path)["manifests"][0]["campaign_id"].endswith(
        "_20260102"
    )
    assert plan("python", repo_root=tmp_path, day="20261231")["manifests"][0][
        "campaign_id"
    ].endswith("_20261231")


def _assert_unresolvable_base_refuses_scope(tmp_path: Path) -> None:
    """A failed git call must narrow the scope, never silently widen it."""

    git_repo(tmp_path, {"pkg/a.py": "x = 1\n"})
    with pytest.raises(CampaignError, match="cannot determine mutation scope"):
        changed_sources("no-such-ref", repo_root=tmp_path)


def test_a_git_scope_failure_without_output_is_actionable(
    monkeypatch, tmp_path: Path
) -> None:
    """An empty Git diagnostic must still refuse scope discovery with context."""

    class FailedGit:
        returncode = 1
        stderr = ""
        stdout = ""

    monkeypatch.setattr(
        campaign_generate.subprocess, "run", lambda *args, **kwargs: FailedGit()
    )
    with pytest.raises(CampaignError, match="no output"):
        changed_sources("HEAD", repo_root=tmp_path)


def test_a_git_scope_failure_prefers_stderr_over_an_empty_stdout(
    monkeypatch, tmp_path: Path
) -> None:
    """The diagnostic must retain stderr when Git leaves stdout empty."""

    _assert_unresolvable_base_refuses_scope(tmp_path / "unresolvable-base")

    class FailedGit:
        returncode = 1
        stderr = "unknown base"
        stdout = ""

    monkeypatch.setattr(
        campaign_generate.subprocess, "run", lambda *args, **kwargs: FailedGit()
    )
    with pytest.raises(CampaignError, match="unknown base"):
        changed_sources("HEAD", repo_root=tmp_path)


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
    payload["generator"]["jobs"] = 2
    payload["generator"]["run_timeout_seconds"] = 91
    path.write_text(json.dumps(payload, indent=2), encoding="utf-8")

    (tmp_path / "crates/widget/src/lib.rs").write_text(
        "#[test] fn t() { assert!(true); }\n", encoding="utf-8"
    )
    assert (
        refresh_rust_campaign(
            payload["campaign_id"],
            sources=["crates/widget/src/lib.rs"],
            repo_root=tmp_path,
        )
        == path.relative_to(tmp_path).as_posix()
    )
    refreshed = json.loads(path.read_text(encoding="utf-8"))

    assert refreshed["generator"]["source"] == ["src/lib.rs"]
    assert list(refreshed["source_sha256"]) == ["crates/widget/src/lib.rs"]
    assert refreshed["source_sha256"] != payload["source_sha256"]
    assert refreshed["test_sha256"] == refreshed["source_sha256"]
    assert refreshed["survivor_baseline"] == ["known"]
    assert refreshed["generator"]["jobs"] == 2
    assert refreshed["generator"]["run_timeout_seconds"] == 91
    assert refreshed["survivor_baseline_note"] == payload["survivor_baseline_note"]
    assert path.read_text(encoding="utf-8").endswith("\n")

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


def _assert_rust_refresh_preserves_declared_scope_and_ratchet_metadata(
    tmp_path: Path,
) -> None:
    tree(
        tmp_path,
        {
            "crates/widget/Cargo.toml": CRATE,
            "crates/widget/src/lib.rs": RUST_UNIT_TESTS,
            "crates/widget/src/extra.rs": RUST_UNIT_TESTS,
        },
    )
    manifest = plan("rust", repo_root=tmp_path, day="20260909")["manifests"][0]
    manifest["survivor_baseline"] = ["known"]
    manifest["survivor_baseline_recorded"] = True
    manifest["survivor_baseline_note"] = "engine note"
    manifest["survivor_baseline_recorded_at"] = "2026-09-09T00:00:00Z"
    path = tmp_path / write([manifest], repo_root=tmp_path)[0]
    before_scope = manifest["generator"]["source"]
    refresh_rust_campaign(manifest["campaign_id"], repo_root=tmp_path)
    refreshed = json.loads(path.read_text(encoding="utf-8"))
    assert refreshed["generator"]["source"] == before_scope
    assert refreshed["survivor_baseline"] == ["known"]
    assert refreshed["survivor_baseline_recorded"] is True
    assert refreshed["survivor_baseline_note"] == "engine note"
    assert refreshed["survivor_baseline_recorded_at"] == "2026-09-09T00:00:00Z"


def _assert_rust_refresh_rejects_malformed_implicit_source_scope(
    tmp_path: Path,
) -> None:
    tree(
        tmp_path,
        {
            "crates/widget/Cargo.toml": CRATE,
            "crates/widget/src/lib.rs": RUST_UNIT_TESTS,
        },
    )
    manifest = plan("rust", repo_root=tmp_path, day="20260909")["manifests"][0]
    manifest["generator"]["source"] = ["../escape.rs"]
    path = tmp_path / write([manifest], repo_root=tmp_path)[0]
    with pytest.raises(CampaignError, match="malformed generated Rust source scope"):
        refresh_rust_campaign(manifest["campaign_id"], repo_root=tmp_path)
    assert json.loads(path.read_text(encoding="utf-8"))["generator"]["source"] == [
        "../escape.rs"
    ]


def test_declared_rust_scope_requires_a_nonempty_list_and_returns_exact_paths(
    tmp_path: Path,
) -> None:
    """A non-list scope must not reach path resolution as an iterable of strings."""

    assert campaign_generate._safe_declared_rust_scope("src/lib.rs") is None
    assert campaign_generate._safe_declared_rust_scope([]) is None
    assert campaign_generate._safe_declared_rust_scope(["src/lib.rs"]) == ["src/lib.rs"]

    tree(
        tmp_path,
        {
            "crates/widget/Cargo.toml": CRATE,
            "crates/widget/src/lib.rs": RUST_UNIT_TESTS,
        },
    )
    subject = rust_subjects(tmp_path)[0]
    manifest = tmp_path / "conductor/mutation_campaigns/widget.json"
    assert campaign_generate._declared_rust_sources(
        {"source": ["src/lib.rs"]}, subject, manifest, tmp_path
    ) == ["crates/widget/src/lib.rs"]


@pytest.mark.parametrize("declared", [[], ["src/not-generated.rs"]])
def test_rust_refresh_rejects_empty_or_unbound_implicit_scope(
    tmp_path: Path, declared: list[str]
) -> None:
    tree(
        tmp_path,
        {
            "crates/widget/Cargo.toml": CRATE,
            "crates/widget/src/lib.rs": RUST_UNIT_TESTS,
        },
    )
    manifest = plan("rust", repo_root=tmp_path, day="20260909")["manifests"][0]
    manifest["generator"]["source"] = declared
    write([manifest], repo_root=tmp_path)
    message = "no non-empty" if not declared else "outside its current crate"
    with pytest.raises(CampaignError, match=message):
        refresh_rust_campaign(manifest["campaign_id"], repo_root=tmp_path)
    _assert_rust_refresh_rejects_malformed_implicit_source_scope(tmp_path / "malformed")


def test_rust_plan_reports_an_untested_scoped_crate_without_hiding_its_identity(
    tmp_path: Path,
) -> None:
    """An untestable changed crate is a finding, with package, root and size retained."""

    tree(
        tmp_path,
        {
            "crates/untested/Cargo.toml": CRATE.replace("widget-core", "untested-core"),
            "crates/untested/src/lib.rs": "pub fn f() {}\n",
        },
    )
    result = plan(
        "rust",
        repo_root=tmp_path,
        day="20260910",
        only_sources=["crates/untested/src/lib.rs"],
    )
    assert result["manifests"] == []
    assert result["untested"] == [
        {
            "package": "untested-core",
            "root": "crates/untested",
            "lines": 1,
            "reason": "crate untested-core has no tests: no tests/*.rs and no #[cfg(test)] module. A mutation campaign over untested code would report every mutant as survived and prove nothing that reading the crate does not already say.",
        }
    ]


def test_rust_plan_uses_zero_lines_only_for_partial_subject_metadata(
    monkeypatch, tmp_path: Path
) -> None:
    """A defensive untested finding must not invent a line count when metadata is partial.

    Forces `CONDUCTOR_PLAN_IMPL=python`: the monkeypatches below only reach it.
    """

    monkeypatch.setenv("CONDUCTOR_PLAN_IMPL", "python")
    subject = {
        "package": "partial",
        "root": "partial",
        "manifest": "partial/Cargo.toml",
        "files": ["partial/src/lib.rs"],
    }
    monkeypatch.setattr(campaign_generate, "rust_subjects", lambda _root: [subject])

    def unavailable(*_args, **_kwargs):
        raise CampaignError("partial has no tests")

    monkeypatch.setattr(campaign_generate, "cargo_manifest", unavailable)
    result = plan(
        "rust",
        repo_root=tmp_path,
        day="20260910",
        only_sources=["partial/src/lib.rs"],
    )
    assert result["untested"] == [
        {
            "package": "partial",
            "root": "partial",
            "lines": 0,
            "reason": "partial has no tests",
        }
    ]


def test_refresh_rebinds_a_fest_campaign_without_erasing_its_baseline(
    tmp_path: Path,
) -> None:
    """Automatic Python refresh updates both hashes while preserving engine evidence."""

    tree(
        tmp_path,
        {
            "conductor/subject.py": "x = 1\n",
            "conductor/test_subject.py": "def test_subject(): assert True\n",
        },
    )
    manifest = plan("python", repo_root=tmp_path, day="20260910")["manifests"][0]
    path = tmp_path / write([manifest], repo_root=tmp_path)[0]
    payload = json.loads(path.read_text(encoding="utf-8"))
    payload["survivor_baseline"] = ["engine-recorded"]
    payload["survivor_baseline_recorded"] = True
    path.write_text(json.dumps(payload), encoding="utf-8")
    (tmp_path / "conductor/subject.py").write_text("x = 2\n", encoding="utf-8")
    (tmp_path / "conductor/test_subject.py").write_text(
        "def test_subject(): assert 2 == 2\n", encoding="utf-8"
    )

    assert refresh_python_campaign(payload["campaign_id"], repo_root=tmp_path) == (
        path.relative_to(tmp_path).as_posix()
    )
    refreshed = json.loads(path.read_text(encoding="utf-8"))
    assert path.read_text(encoding="utf-8").endswith("\n")
    assert refreshed["survivor_baseline"] == ["engine-recorded"]
    assert refreshed["survivor_baseline_recorded"] is True
    assert refreshed["source_sha256"] != payload["source_sha256"]
    assert refreshed["test_sha256"] != payload["test_sha256"]

    refreshed["generator"]["source"] = []
    path.write_text(json.dumps(refreshed), encoding="utf-8")
    with pytest.raises(CampaignError, match="exactly one Python source"):
        refresh_python_campaign(payload["campaign_id"], repo_root=tmp_path)

    refreshed["generator"]["source"] = ["conductor/absent.py"]
    path.write_text(json.dumps(refreshed), encoding="utf-8")
    with pytest.raises(CampaignError, match="no longer exists in the tree"):
        refresh_python_campaign(payload["campaign_id"], repo_root=tmp_path)


def test_refresh_carries_an_extra_test_campaign_forward_without_erasing_its_baseline(
    tmp_path: Path,
) -> None:
    """An extra-test admission refreshes like any other; `--force` is not needed.

    Refusing these campaigns left `write --include-covered --force` as the only
    path, and force resets the survivor baseline the engine is holding.
    """

    tree(
        tmp_path,
        {
            "conductor/_bash_quiet.py": "LIMIT = 8000\n",
            "conductor/test_bash_quiet.py": "def test_limit(): assert True\n",
        },
    )
    manifest = plan(
        "python",
        repo_root=tmp_path,
        day="20260910",
        extra_tests={"conductor/_bash_quiet.py": ["conductor/test_bash_quiet.py"]},
    )["manifests"][0]
    path = tmp_path / write([manifest], repo_root=tmp_path)[0]
    payload = json.loads(path.read_text(encoding="utf-8"))
    payload["survivor_baseline"] = ["engine-recorded-survivor"]
    payload["survivor_baseline_recorded"] = True
    path.write_text(json.dumps(payload), encoding="utf-8")
    (tmp_path / "conductor/_bash_quiet.py").write_text(
        "LIMIT = 4000\n", encoding="utf-8"
    )

    assert refresh_python_campaign(payload["campaign_id"], repo_root=tmp_path) == (
        path.relative_to(tmp_path).as_posix()
    )
    refreshed = json.loads(path.read_text(encoding="utf-8"))
    assert refreshed["survivor_baseline"] == ["engine-recorded-survivor"]
    assert refreshed["survivor_baseline_recorded"] is True
    assert refreshed["test_argv"] == [
        "python",
        "-m",
        "pytest",
        "-q",
        "--rootdir=.",
        "conductor/test_bash_quiet.py",
    ]
    assert list(refreshed["test_sha256"]) == ["conductor/test_bash_quiet.py"]
    assert refreshed["source_sha256"] != payload["source_sha256"]

    # A manifest that recorded no test list cannot invent one to carry forward.
    payload["test_sha256"] = {}
    path.write_text(json.dumps(payload), encoding="utf-8")
    with pytest.raises(CampaignError, match="no recorded test list"):
        refresh_python_campaign(payload["campaign_id"], repo_root=tmp_path)


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

    (campaigns / "bad-cargo.json").write_text(
        json.dumps(
            {
                "mutation_engine": "cargo-mutants",
                "generator": {"options": {"package": "x"}},
            }
        ),
        encoding="utf-8",
    )
    with pytest.raises(CampaignError, match="no generated cargo package"):
        refresh_rust_campaign("bad-cargo", repo_root=tmp_path)


def test_rust_refresh_repairs_one_legacy_manifest_path_and_retains_its_note(
    tmp_path: Path,
) -> None:
    """A sole package may repair an old snapshot manifest, but only unambiguously."""

    tree(
        tmp_path,
        {
            "crates/widget/Cargo.toml": CRATE,
            "crates/widget/src/lib.rs": RUST_UNIT_TESTS,
        },
    )
    manifest = plan("rust", repo_root=tmp_path, day="20260910")["manifests"][0]
    manifest["generator"]["options"]["manifest_path"] = "gone/Cargo.toml"
    manifest["survivor_baseline_note"] = "engine-recorded note"
    path = tmp_path / write([manifest], repo_root=tmp_path)[0]
    assert refresh_rust_campaign(manifest["campaign_id"], repo_root=tmp_path) == (
        path.relative_to(tmp_path).as_posix()
    )
    refreshed = json.loads(path.read_text(encoding="utf-8"))
    assert (
        refreshed["generator"]["options"]["manifest_path"] == "crates/widget/Cargo.toml"
    )
    assert refreshed["survivor_baseline_note"] == "engine-recorded note"
    _assert_rust_refresh_preserves_declared_scope_and_ratchet_metadata(
        tmp_path / "declared-scope"
    )


def test_rust_refresh_refuses_an_ambiguous_legacy_package_path(tmp_path: Path) -> None:
    """A stale path may be repaired only when one real package matches it."""

    tree(
        tmp_path,
        {
            "a/Cargo.toml": CRATE,
            "a/src/lib.rs": RUST_UNIT_TESTS,
            "b/Cargo.toml": CRATE,
            "b/src/lib.rs": RUST_UNIT_TESTS,
        },
    )
    manifest = plan("rust", repo_root=tmp_path, day="20260910")["manifests"][0]
    manifest["generator"]["options"]["manifest_path"] = "gone/Cargo.toml"
    write([manifest], repo_root=tmp_path)
    with pytest.raises(CampaignError, match="no longer exists"):
        refresh_rust_campaign(manifest["campaign_id"], repo_root=tmp_path)


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


def test_an_extra_test_pairs_a_subject_no_test_is_named_after(tmp_path: Path) -> None:
    """`--extra-test` is the only way a module tested under another name gets a campaign."""

    tree(
        tmp_path,
        {
            "research/tools/overrides.py": "x = 1\n",
            "research/tools/report.py": "x = 1\n",
            "research/tests/test_report.py": "def test_x(): pass\n",
        },
    )
    extra = {"research/tools/overrides.py": ["research/tests/test_report.py"]}
    without = plan("python", repo_root=tmp_path, day="20260912")
    assert [u["source"] for u in without["unpaired"]] == ["research/tools/overrides.py"]
    result = plan("python", repo_root=tmp_path, day="20260912", extra_tests=extra)
    assert result["unpaired"] == []
    by_source = {m["generator"]["source"][0]: m for m in result["manifests"]}
    manifest = by_source["research/tools/overrides.py"]
    assert manifest["test_argv"][5:] == ["research/tests/test_report.py"]
    assert set(manifest["test_sha256"]) == {"research/tests/test_report.py"}
    assert by_source["research/tools/report.py"]["test_argv"][5:] == [
        "research/tests/test_report.py"
    ]
    with pytest.raises(CampaignError, match="names no python subject"):
        plan("python", repo_root=tmp_path, extra_tests={"research/tools/gone.py": []})
    with pytest.raises(CampaignError, match="python subjects only"):
        plan("rust", repo_root=tmp_path, extra_tests=extra)
    parsed = campaign_generate._extra_tests(
        ["research/tools/overrides.py=research/tests/test_report.py"],
        repo_root=tmp_path,
    )
    assert parsed == extra
    with pytest.raises(CampaignError, match="not a test file"):
        campaign_generate._extra_tests(
            ["research/tools/overrides.py=research/tools/report.py"], repo_root=tmp_path
        )
    with pytest.raises(CampaignError, match="SOURCE=TEST"):
        campaign_generate._extra_tests(
            ["research/tools/overrides.py"], repo_root=tmp_path
        )
