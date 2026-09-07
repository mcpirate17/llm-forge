"""Crate selection for the gate's cargo-fmt and cargo-clippy checks.

The roster in tooling/native/crates.toml is the only thing standing between
"this crate is linted in CI and not locally" and the reverse, so the reads of
it are what these tests pin.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from conductor.candidate_review import cargo_lint_files as mod

ROSTER = """
[crates]
tested = ["a/keep", "a/skip", "a/held"]
linted = ["a/keep", "a/held"]
unstyled = ["a/skip"]
excluded = ["a/old"]

[globs]
manifests = ["a/*/Cargo.toml"]

[prerequisites."a/keep"]
artifact = "build/lib.a"
command = "make kernels"
reason = "needs the C archive"

# No artifact: nothing on disk can clear this one, so it is always blocked.
[prerequisites."a/held"]
command = "ask an operator"
reason = "the vendor SDK is not redistributable"
"""

BARE_ROSTER = """
[crates]
tested = ["a/only"]

[globs]
manifests = ["a/*/Cargo.toml"]
"""


def _crate(root: Path, name: str) -> Path:
    crate = root / name
    (crate / "src").mkdir(parents=True)
    (crate / "Cargo.toml").write_text("[package]\n", encoding="utf-8")
    (crate / "src" / "lib.rs").write_text("", encoding="utf-8")
    return crate


@pytest.fixture()
def root(tmp_path: Path) -> Path:
    (tmp_path / "tooling" / "native").mkdir(parents=True)
    (tmp_path / mod.ROSTER).write_text(ROSTER, encoding="utf-8")
    for name in ("keep", "skip", "old", "held"):
        _crate(tmp_path, f"a/{name}")
    return tmp_path


@pytest.fixture()
def bare_root(tmp_path: Path) -> Path:
    """A tree whose roster accounts for every crate on it and excludes none."""
    (tmp_path / "tooling" / "native").mkdir(parents=True)
    (tmp_path / mod.ROSTER).write_text(BARE_ROSTER, encoding="utf-8")
    _crate(tmp_path, "a/only")
    return tmp_path


def test_absent_roster_refuses_rather_than_linting_nothing(tmp_path: Path) -> None:
    with pytest.raises(mod.RosterError):
        mod.Roster.load(tmp_path)


def test_malformed_roster_refuses(tmp_path: Path) -> None:
    (tmp_path / "tooling" / "native").mkdir(parents=True)
    (tmp_path / mod.ROSTER).write_text("[crates\n", encoding="utf-8")
    with pytest.raises(mod.RosterError):
        mod.Roster.load(tmp_path)


def test_roster_without_manifest_globs_refuses(tmp_path: Path) -> None:
    (tmp_path / "tooling" / "native").mkdir(parents=True)
    (tmp_path / mod.ROSTER).write_text("[crates]\ntested = []\n", encoding="utf-8")
    with pytest.raises(mod.RosterError):
        mod.Roster.load(tmp_path)


def test_crate_in_neither_tested_nor_excluded_is_reported(root: Path) -> None:
    _crate(root, "a/new")
    assert "a/new" in mod.Roster.load(root).unclassified()


def test_fully_classified_tree_reports_nothing(root: Path) -> None:
    assert mod.Roster.load(root).unclassified() == []


def test_prerequisite_blocks_only_while_the_artifact_is_absent(root: Path) -> None:
    roster = mod.Roster.load(root)
    blocked = roster.blocked_by_prerequisite("a/keep")
    assert blocked is not None
    assert "make kernels" in blocked
    (root / "build").mkdir()
    (root / "build" / "lib.a").write_text("", encoding="utf-8")
    assert mod.Roster.load(root).blocked_by_prerequisite("a/keep") is None


def test_crate_without_a_prerequisite_is_never_blocked(root: Path) -> None:
    assert mod.Roster.load(root).blocked_by_prerequisite("a/skip") is None


def test_owning_crate_walks_up_to_the_manifest(root: Path) -> None:
    assert mod.owning_crate(root / "a/keep/src/lib.rs", root=root) == "a/keep"


def test_changed_crates_separates_orphans_from_owned(root: Path) -> None:
    # Repo-relative paths, in the order the gate happens to hand them over.
    (root / "loose.rs").write_text("", encoding="utf-8")
    crates, orphans = mod.changed_crates(
        ["a/skip/Cargo.toml", "a/keep/Cargo.toml", "loose.rs"], root=root
    )
    assert crates == ["a/keep", "a/skip"]
    assert orphans == ["loose.rs"]


def test_fmt_covers_every_crate_except_the_unstyled(root: Path) -> None:
    selected, _ = mod._selected(
        "fmt", ["a/keep", "a/skip", "a/old"], mod.Roster.load(root)
    )
    assert selected == ["a/keep", "a/old"]


def test_clippy_reaches_only_the_linted_roster(root: Path) -> None:
    # a/keep's prerequisite is met, so roster membership is the only thing
    # left between a/skip and a full compile.
    (root / "build").mkdir()
    (root / "build" / "lib.a").write_text("", encoding="utf-8")
    roster = mod.Roster.load(root)
    assert mod._selected("clippy", ["a/keep", "a/skip"], roster)[0] == ["a/keep"]


def test_clippy_skips_a_linted_crate_it_cannot_build(root: Path) -> None:
    # a/held is on the clippy roster and permanently blocked, so selecting it
    # would hand clippy a build nothing in this tree can satisfy.
    assert mod._selected("clippy", ["a/held"], mod.Roster.load(root))[0] == []


def test_main_refuses_when_a_crate_is_unclassified(root: Path, monkeypatch) -> None:
    # Whatever the roster read reports, main's job is to turn it into a refusal
    # rather than lint a tree it cannot account for.
    monkeypatch.setattr(mod.Roster, "unclassified", lambda self: ["a/new"])
    monkeypatch.chdir(root)
    assert mod.main(["--mode", "fmt", "a/keep/src/lib.rs"]) == 1


def test_main_refuses_a_rust_file_owned_by_no_crate(root: Path, monkeypatch) -> None:
    (root / "loose.rs").write_text("", encoding="utf-8")
    monkeypatch.chdir(root)
    assert mod.main(["--mode", "fmt", "loose.rs"]) == 1


def test_main_passes_when_nothing_changed_maps_to_a_crate(
    bare_root: Path, monkeypatch
) -> None:
    monkeypatch.chdir(bare_root)
    assert mod.main(["--mode", "fmt"]) == 0
