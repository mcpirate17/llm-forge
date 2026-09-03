"""Tests for the compact / path-filtered ownership claims view."""

from __future__ import annotations

import json
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

from conductor.candidate_review import cli as review_cli
from conductor.candidate_review.ownership import OwnershipClaim, create_claim
from conductor.test_candidate_review import _init_repo


@pytest.fixture
def repo_with_claims(tmp_path: Path) -> Path:
    repo = _init_repo(tmp_path / "repo")
    (repo / "conductor").mkdir()
    (repo / "conductor" / "a.py").write_text("A = 1\n", encoding="utf-8")
    (repo / "conductor" / "b.py").write_text("B = 1\n", encoding="utf-8")
    (repo / "docs.md").write_text("# docs\n", encoding="utf-8")
    create_claim(
        repo,
        owner="alpha",
        paths=["conductor/a.py", "conductor/b.py"],
        justification="alpha " + "x" * 100,
        hours=1,
    )
    create_claim(
        repo, owner="beta", paths=["docs.md"], justification="beta docs", hours=1
    )
    return repo


def test_compact_view_is_one_line_per_claim_plus_paths(
    repo_with_claims: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    assert (
        review_cli.main(["claims", "--repo", str(repo_with_claims), "--compact"]) == 0
    )
    out = capsys.readouterr().out
    lines = out.splitlines()
    assert lines[0].startswith("claims: 2 active, 0 expired, sha256 ")
    assert len(lines) == 3
    alpha = next(line for line in lines if " alpha " in line)
    assert " 2 paths  conductor/(2)  " in alpha and alpha.endswith("…")
    assert len(alpha.split("  ")[-1]) == review_cli.COMPACT_JUSTIFICATION_CHARS
    beta = next(line for line in lines if " beta " in line)
    assert " 1 paths  .(1)  beta docs" in beta
    assert "conductor/a.py" not in out
    assert len(out) < 400
    assert (
        review_cli.main(
            ["claims", "--repo", str(repo_with_claims), "--compact", "--paths"]
        )
        == 0
    )
    lines = capsys.readouterr().out.splitlines()
    assert len(lines) == 5
    assert "    conductor/a.py conductor/b.py" in lines
    assert "    docs.md" in lines


def test_path_filter_applies_to_both_views(
    repo_with_claims: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    args = ["claims", "--repo", str(repo_with_claims), "--path", "conductor/b.py"]
    assert review_cli.main(args) == 0
    payload = json.loads(capsys.readouterr().out)
    assert [claim["owner"] for claim in payload["claims"]] == ["alpha"]
    assert review_cli.main([*args, "--compact"]) == 0
    out = capsys.readouterr().out
    assert "claims: 1 active" in out and "beta" not in out
    assert "    conductor/a.py conductor/b.py" in out.splitlines()
    assert review_cli.main([*args, "--path", "docs.md", "--compact"]) == 0
    assert "claims: 2 active" in capsys.readouterr().out
    assert review_cli.main([*args[:-1], "nothing/here.py", "--compact"]) == 0
    assert capsys.readouterr().out.startswith("claims: 0 active")


def test_default_json_view_is_unchanged(
    repo_with_claims: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    assert review_cli.main(["claims", "--repo", str(repo_with_claims)]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert set(payload) == {"sha256", "claims"}
    assert len(payload["claims"]) == 2
    assert set(payload["claims"][0]) == {
        "claim_id",
        "owner",
        "paths",
        "justification",
        "created_at",
        "expires_at",
    }


def test_compact_text_counts_expired_and_hides_them() -> None:
    now = datetime(2026, 8, 27, 12, 0, tzinfo=timezone.utc)

    def claim(name: str, delta: timedelta) -> OwnershipClaim:
        return OwnershipClaim(
            claim_id=f"claim-{name}",
            owner=name,
            paths=("p.py",),
            justification="j",
            created_at=now.isoformat(),
            expires_at=(now + delta).isoformat(),
        )

    text = review_cli.compact_claims_text(
        [claim("live", timedelta(hours=2)), claim("dead", -timedelta(hours=2))],
        "abcdef0123456789",
        now=now,
    )
    assert text.splitlines()[0] == "claims: 1 active, 1 expired, sha256 abcdef012345"
    assert "claim-live" in text and "claim-dead" not in text
    # 14:00 is what it asked for; 13:30 is the idle lapse, and the earlier one is
    # what actually releases the path. Printing the later one would be a lie.
    assert "exp 08-27 13:30Z" in text
    assert "idle   0m" in text
