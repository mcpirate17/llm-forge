"""One name for claiming and for writing.

The claim store separated nothing while the two ends disagreed: codex ran its gate
as the literal ``codex`` and claimed as ``codex-<lane>``, so its gate did not
recognise its own claims; every Claude session resolved to ``claude``, so any one of
them could write over another's. These tests pin the single resolver both ends now
call, and the two rules that make it an identity rather than a label: a vendor name
is never one, and a claim may not be filed under a name its own writer will not use.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

from conductor.candidate_review.identity import (
    OwnerIdentityError,
    is_vendor,
    lane_of,
    normalize,
    require_lane_owner,
    resolve_owner,
    vendor_for,
)


@pytest.fixture
def main_checkout(tmp_path: Path) -> Path:
    repo = tmp_path / "LLM"
    repo.mkdir()
    subprocess.run(
        ["git", "init", "-q", "-b", "codex/audit-rust-20260903"], cwd=repo, check=True
    )
    return repo


def _worktree(main: Path, name: str, branch: str) -> Path:
    (main / "seed.txt").write_text("seed\n", encoding="utf-8")
    subprocess.run(["git", "add", "seed.txt"], cwd=main, check=True)
    subprocess.run(
        ["git", "-c", "user.email=t@e", "-c", "user.name=t", "commit", "-qm", "seed"],
        cwd=main,
        check=True,
    )
    tree = main.parent / name
    subprocess.run(
        ["git", "worktree", "add", "-q", "-b", branch, str(tree)], cwd=main, check=True
    )
    return tree


def test_a_worktree_is_named_for_itself_not_its_branch(main_checkout: Path) -> None:
    """A lane keeps its name across a branch switch, so the directory is the key."""
    tree = _worktree(
        main_checkout, "codex-rust-hotpath-next-20260903", "codex/other-20260903"
    )
    assert lane_of(tree) == "codex-rust-hotpath-next-20260903"


def test_the_main_checkout_falls_back_to_its_branch(main_checkout: Path) -> None:
    """Its directory name is shared by every session in it, so it names nothing."""
    assert lane_of(main_checkout) == "codex-audit-rust-20260903"


def test_a_detached_checkout_has_no_lane(main_checkout: Path) -> None:
    (main_checkout / ".git" / "HEAD").write_text("a" * 40 + "\n", encoding="utf-8")
    assert lane_of(main_checkout) == ""


def test_the_vendor_pin_that_broke_codex_is_ignored(main_checkout: Path) -> None:
    """``.codex/hooks.json`` pins ``GOVERNANCE_OWNER=codex``; the lane still wins.

    Honouring that pin is precisely the defect: it is what made codex's gate check
    a name codex's own claims were never written under.
    """
    tree = _worktree(
        main_checkout, "codex-rust-hotpath-next-20260903", "codex/hot-20260903"
    )
    env = {"GOVERNANCE_OWNER": "codex", "CODEX_HOME": "/home/tim/.codex"}
    assert resolve_owner(tree, env) == "codex-rust-hotpath-next-20260903"


def test_a_real_declaration_still_wins(main_checkout: Path) -> None:
    env = {"GOVERNANCE_OWNER": "glm-adaptation-rust-20260902"}
    assert resolve_owner(main_checkout, env) == "glm-adaptation-rust-20260902"


def test_the_vendor_only_stands_in_when_no_lane_can_be_derived(
    main_checkout: Path,
) -> None:
    (main_checkout / ".git" / "HEAD").write_text("a" * 40 + "\n", encoding="utf-8")
    assert resolve_owner(main_checkout, {"CODEX_HOME": "/x"}) == "codex"


def test_an_unnameable_lane_raises_rather_than_guessing(main_checkout: Path) -> None:
    """The old ladder ended in ``return "codex"``: an unknown lane wrote as codex."""
    (main_checkout / ".git" / "HEAD").write_text("a" * 40 + "\n", encoding="utf-8")
    with pytest.raises(OwnerIdentityError, match="no governance identity"):
        resolve_owner(main_checkout, {})


@pytest.mark.parametrize(
    "vendor", ["claude", "codex", "qwen", "grok", "CoDeX", " codex "]
)
def test_a_vendor_name_is_refused_as_a_claim_owner(vendor: str) -> None:
    assert is_vendor(vendor)
    with pytest.raises(OwnerIdentityError, match="names a vendor"):
        require_lane_owner(vendor)


def test_a_lane_name_is_accepted_and_folded() -> None:
    assert (
        require_lane_owner("Codex/Rust Hotpath 20260903")
        == "codex-rust-hotpath-20260903"
    )


def test_normalize_drops_what_the_owner_charset_cannot_hold() -> None:
    assert normalize("  claude/claim@expected  ") == "claude-claim-expected"
    assert normalize("---") == ""
    assert len(normalize("x" * 200)) == 64


def test_the_legacy_vendor_of_a_lane_survives_a_bare_shell() -> None:
    """The launcher marker is absent outside a hook, so the prefix has to answer.

    Without it the fallback that keeps pre-existing ``owner=claude`` claims alive
    would go dark in exactly the sessions still relying on it.
    """
    assert vendor_for("codex-rust-hotpath-next-20260903", {}) == "codex"
    assert vendor_for("branch-policy", {"CLAUDE_PROJECT_DIR": "/x"}) == "claude"
    assert vendor_for("branch-policy", {}) == ""


def _claim(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            sys.executable,
            "-m",
            "conductor.candidate_review.cli",
            "claim",
            "--repo",
            str(repo),
            "--justification",
            "identity contract",
            "--expected-minutes",
            "5",
            "--max-minutes",
            "10",
            *args,
            "pkg/mod.py",
        ],
        cwd=Path(__file__).resolve().parents[1],
        capture_output=True,
        text=True,
        env=os.environ | {"GOVERNANCE_OWNER": ""},
    )


def test_a_claim_defaults_to_the_lane_that_will_write_it(main_checkout: Path) -> None:
    done = _claim(main_checkout)
    assert done.returncode == 0, done.stderr
    assert "owner=codex-audit-rust-20260903" in done.stdout


def test_claiming_as_another_lane_is_refused(main_checkout: Path) -> None:
    """The two ends must name the same lane or the claim gates nothing."""
    done = _claim(main_checkout, "--owner", "codex-rust-hotpath-next-20260903")
    assert done.returncode == 1
    assert "is not this lane" in done.stderr


def test_claiming_as_a_bare_vendor_is_refused(main_checkout: Path) -> None:
    done = _claim(main_checkout, "--owner", "codex")
    assert done.returncode == 1
    assert "names a vendor" in done.stderr
