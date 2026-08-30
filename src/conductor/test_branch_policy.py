"""Tests for conductor.branch_policy.

Every repo is a throwaway ``git init`` under ``tmp_path`` -- never the real checkout.
Boundary cases are paired (just-under / at / just-over; True-branch / False-branch) so
that flipping a comparison or boolean operator in the module under test fails a test.
"""

from __future__ import annotations

import json
import subprocess
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from conductor import branch_policy as bp
from conductor.candidate_review import ownership as ownership_mod
from conductor.candidate_review.model import sha256_json
from conductor.candidate_review.ownership import create_claim

# --------------------------------------------------------------------------- fixtures


def _git(repo: Path, *args: str) -> str:
    completed = subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=False
    )
    if completed.returncode != 0:
        pytest.fail(
            f"git {' '.join(args)} failed ({completed.returncode}): {completed.stderr.strip()}"
        )
    return completed.stdout


def _init_repo(path: Path, *, initial_branch: str = "w7-trident-program") -> Path:
    path.mkdir(exist_ok=True)
    _git(path, "init", "--quiet", f"--initial-branch={initial_branch}")
    _git(path, "config", "user.name", "Branch Policy Test")
    _git(path, "config", "user.email", "branch-policy@example.invalid")
    return path


def _commit(repo: Path, name: str, *, message: str | None = None) -> str:
    (repo / name).write_text(f"{name}\n", encoding="utf-8")
    _git(repo, "add", "--all")
    _git(repo, "commit", "--quiet", "-m", message or f"add {name}")
    return _git(repo, "rev-parse", "HEAD").strip()


def _checkout_new(repo: Path, branch: str, *, start: str | None = None) -> None:
    args = ["checkout", "--quiet", "-b", branch]
    if start:
        args.append(start)
    _git(repo, *args)


def _rev_parse(repo: Path, ref: str) -> str:
    return _git(repo, "rev-parse", ref).strip()


def _write_claim(
    repo: Path,
    *,
    owner: str,
    paths: list[str],
    justification: str = "test claim",
    created_at: datetime | None = None,
    expires_at: datetime | None = None,
) -> ownership_mod.OwnershipClaim:
    """Write a claim directly so expiry can be placed in the past deterministically."""
    now = datetime.now(UTC)
    created_at = created_at or now
    expires_at = expires_at or (now + timedelta(hours=1))
    normalized = tuple(sorted({ownership_mod.normalize_claim_path(p) for p in paths}))
    created_iso = created_at.isoformat()
    expires_iso = expires_at.isoformat()
    identity = sha256_json(
        {
            "owner": owner,
            "paths": normalized,
            "justification": justification,
            "created_at": created_iso,
            "expires_at": expires_iso,
        }
    )
    claim = ownership_mod.OwnershipClaim(
        claim_id=f"claim-{identity[:20]}",
        owner=owner,
        paths=normalized,
        justification=justification,
        created_at=created_iso,
        expires_at=expires_iso,
    )
    ownership_mod._write_claims(repo, [claim])
    return claim


def _write_binding_row(repo: Path, *, branch: str, claim_id: str, owner: str) -> None:
    """Append a binding row straight to the store, bypassing bind_branch's own guard.

    Used to simulate a fan-out that already happened (e.g. a hand-edited store, or a
    binding written before the guard existed) so evaluate_push's independent, push-time
    re-check of the same rule can be exercised on its own.
    """
    path = bp.binding_store_path(repo)
    existing = (
        json.loads(path.read_text())
        if path.is_file()
        else {"schema_version": 1, "bindings": []}
    )
    existing["bindings"].append(
        {
            "branch": branch,
            "claim_id": claim_id,
            "owner": owner,
            "created_at": datetime.now(UTC).isoformat(),
            "last_push_at": None,
            "pr_number": None,
        }
    )
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(existing), encoding="utf-8")


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    return _init_repo(tmp_path / "repo")


# --------------------------------------------------------------------------- naming


class TestValidateBranchName:
    def test_valid_shape(self) -> None:
        parsed = bp.validate_branch_name("claude/branch-policy-20260829")
        assert parsed == bp.ParsedBranch(
            raw="claude/branch-policy-20260829",
            agent="claude",
            topic="branch-policy",
            date="20260829",
        )

    def test_valid_agent_with_digits_and_hyphens(self) -> None:
        parsed = bp.validate_branch_name("glm-flash-04/topic-name-20260101")
        assert parsed.agent == "glm-flash-04"
        assert parsed.topic == "topic-name"
        assert parsed.date == "20260101"

    def test_missing_slash_refused_with_that_specific_reason(self) -> None:
        with pytest.raises(bp.BranchPolicyError, match="no '<agent>/' segment"):
            bp.validate_branch_name("no-slash-here-20260829")

    def test_invalid_agent_uppercase_refused_with_that_specific_reason(self) -> None:
        with pytest.raises(bp.BranchPolicyError, match="invalid agent slug"):
            bp.validate_branch_name("Claude/topic-20260829")

    def test_invalid_agent_leading_digit_refused(self) -> None:
        with pytest.raises(bp.BranchPolicyError, match="invalid agent slug"):
            bp.validate_branch_name("9agent/topic-20260829")

    def test_missing_topic_date_tail_refused_with_that_specific_reason(self) -> None:
        with pytest.raises(
            bp.BranchPolicyError, match="no '<topic>-<yyyymmdd>' segment"
        ):
            bp.validate_branch_name("claude/nodatehere")

    def test_topic_leading_hyphen_refused(self) -> None:
        with pytest.raises(
            bp.BranchPolicyError, match="no '<topic>-<yyyymmdd>' segment"
        ):
            bp.validate_branch_name("claude/-topic-20260829")

    def test_date_wrong_digit_count_refused(self) -> None:
        with pytest.raises(
            bp.BranchPolicyError, match="no '<topic>-<yyyymmdd>' segment"
        ):
            bp.validate_branch_name("claude/topic-2026082")

    def test_date_boundary_valid_feb28_non_leap_year(self) -> None:
        parsed = bp.validate_branch_name("claude/topic-20260228")
        assert parsed.date == "20260228"

    def test_date_boundary_invalid_feb29_non_leap_year_refused_with_that_specific_reason(
        self,
    ) -> None:
        with pytest.raises(bp.BranchPolicyError, match="invalid calendar date"):
            bp.validate_branch_name("claude/topic-20260229")

    def test_every_refusal_carries_a_suggestion(self) -> None:
        for bad in (
            "nodate",
            "claude/nodate",
            "Claude/topic-20260829",
            "claude/topic-20260229",
        ):
            with pytest.raises(bp.BranchPolicyError, match=r"try '"):
                bp.validate_branch_name(bad)

    def test_suggest_branch_name_is_itself_valid(self) -> None:
        for bad in (
            "nodate",
            "Claude/topic-20260829",
            "claude/topic-20260229",
            "bad name here",
        ):
            suggestion = bp.suggest_branch_name(bad)
            bp.validate_branch_name(suggestion)  # must not raise


class TestIsIntegrationBranch:
    def test_true_for_primary(self) -> None:
        assert bp.is_integration_branch("w7-trident-program") is True

    def test_true_for_mirror(self) -> None:
        assert bp.is_integration_branch("master") is True

    def test_false_for_feature_branch(self) -> None:
        assert bp.is_integration_branch("claude/topic-20260829") is False

    def test_false_for_near_miss_substring(self) -> None:
        assert bp.is_integration_branch("mastered") is False
        assert bp.is_integration_branch("w7-trident-program-old") is False


# --------------------------------------------------------------------------- fast-forward


class TestIsFastForward:
    def test_equal_shas_is_ff(self, repo: Path) -> None:
        sha = _commit(repo, "a.txt")
        assert bp.is_fast_forward(repo, old=sha, new=sha) is True

    def test_empty_old_is_ff(self, repo: Path) -> None:
        sha = _commit(repo, "a.txt")
        assert bp.is_fast_forward(repo, old="", new=sha) is True

    def test_zero_sha_old_is_ff(self, repo: Path) -> None:
        sha = _commit(repo, "a.txt")
        assert bp.is_fast_forward(repo, old="0" * 40, new=sha) is True

    def test_true_when_old_is_a_true_ancestor(self, repo: Path) -> None:
        old = _commit(repo, "a.txt")
        new = _commit(repo, "b.txt")
        assert bp.is_fast_forward(repo, old=old, new=new) is True

    def test_false_when_roles_reversed(self, repo: Path) -> None:
        old = _commit(repo, "a.txt")
        new = _commit(repo, "b.txt")
        assert bp.is_fast_forward(repo, old=new, new=old) is False

    def test_false_on_diverged_history(self, repo: Path) -> None:
        base = _commit(repo, "a.txt")
        _checkout_new(repo, "side", start=base)
        side_tip = _commit(repo, "c.txt")
        _git(repo, "checkout", "--quiet", "w7-trident-program")
        main_tip = _commit(repo, "b.txt")
        assert bp.is_fast_forward(repo, old=side_tip, new=main_tip) is False


# --------------------------------------------------------------------------- local-only commits


class TestLocalOnlyCommits:
    def test_reports_everything_when_nothing_is_excluded(self, repo: Path) -> None:
        sha = _commit(repo, "a.txt")
        rows = bp.local_only_commits(repo, "w7-trident-program")
        assert [row["sha"] for row in rows] == [sha]

    def test_excluded_by_remote_ref(self, repo: Path) -> None:
        sha = _commit(repo, "a.txt")
        _git(repo, "update-ref", "refs/remotes/origin/w7-trident-program", sha)
        assert bp.local_only_commits(repo, "w7-trident-program") == ()

    def test_excluded_by_snapshot_ref(self, repo: Path) -> None:
        sha = _commit(repo, "a.txt")
        _git(repo, "update-ref", "refs/snapshots/backup-1", sha)
        assert bp.local_only_commits(repo, "w7-trident-program") == ()

    def test_only_the_unpushed_tail_is_reported(self, repo: Path) -> None:
        base = _commit(repo, "a.txt")
        _git(repo, "update-ref", "refs/remotes/origin/w7-trident-program", base)
        newer = _commit(repo, "b.txt")
        rows = bp.local_only_commits(repo, "w7-trident-program")
        assert [row["sha"] for row in rows] == [newer]


# --------------------------------------------------------------------------- merged branches


class TestMergedBranches:
    def test_includes_branch_whose_tip_is_an_ancestor(self, repo: Path) -> None:
        base = _commit(repo, "a.txt")
        _checkout_new(repo, "claude/x-20260829", start=base)
        _git(repo, "checkout", "--quiet", "w7-trident-program")
        _commit(repo, "b.txt")
        assert "claude/x-20260829" in bp.merged_branches(repo)

    def test_excludes_branch_with_a_unique_commit(self, repo: Path) -> None:
        base = _commit(repo, "a.txt")
        _checkout_new(repo, "claude/y-20260829", start=base)
        _commit(repo, "unique.txt")
        _git(repo, "checkout", "--quiet", "w7-trident-program")
        assert "claude/y-20260829" not in bp.merged_branches(repo)

    def test_excludes_integration_branches_themselves(self, repo: Path) -> None:
        _commit(repo, "a.txt")
        result = bp.merged_branches(repo)
        assert "w7-trident-program" not in result
        assert "master" not in result


# --------------------------------------------------------------------------- claim binding resolution


class TestBranchClaimBinding:
    def _feature_branch_with_changed_file(
        self, repo: Path, branch: str, filename: str
    ) -> None:
        _commit(repo, "base.txt")
        _checkout_new(repo, branch)
        (repo / filename).write_text("x\n", encoding="utf-8")
        _git(repo, "add", "--all")
        _git(repo, "commit", "--quiet", "-m", f"add {filename}")

    def test_matches_owner_and_overlapping_path(self, repo: Path) -> None:
        self._feature_branch_with_changed_file(repo, "claude/topic-20260829", "src.py")
        claim = create_claim(
            repo, owner="claude", paths=["src.py"], justification="t", hours=1
        )
        matches = bp.branch_claim_binding(repo, "claude/topic-20260829")
        assert claim in matches

    def test_excludes_non_overlapping_path(self, repo: Path) -> None:
        self._feature_branch_with_changed_file(repo, "claude/topic-20260829", "src.py")
        create_claim(
            repo, owner="claude", paths=["other.py"], justification="t", hours=1
        )
        assert bp.branch_claim_binding(repo, "claude/topic-20260829") == ()

    def test_excludes_different_owner(self, repo: Path) -> None:
        self._feature_branch_with_changed_file(repo, "claude/topic-20260829", "src.py")
        create_claim(
            repo, owner="fable-5", paths=["src.py"], justification="t", hours=1
        )
        assert bp.branch_claim_binding(repo, "claude/topic-20260829") == ()

    def test_excludes_expired_claim(self, repo: Path) -> None:
        self._feature_branch_with_changed_file(repo, "claude/topic-20260829", "src.py")
        now = datetime.now(UTC)
        _write_claim(
            repo,
            owner="claude",
            paths=["src.py"],
            created_at=now - timedelta(hours=2),
            expires_at=now - timedelta(hours=1),
        )
        assert bp.branch_claim_binding(repo, "claude/topic-20260829") == ()

    def test_includes_active_claim(self, repo: Path) -> None:
        self._feature_branch_with_changed_file(repo, "claude/topic-20260829", "src.py")
        now = datetime.now(UTC)
        claim = _write_claim(
            repo,
            owner="claude",
            paths=["src.py"],
            created_at=now,
            expires_at=now + timedelta(hours=1),
        )
        assert claim in bp.branch_claim_binding(repo, "claude/topic-20260829")


# --------------------------------------------------------------------------- binding store


class TestBindingStore:
    def test_load_bindings_missing_file_is_empty(self, repo: Path) -> None:
        assert bp.load_bindings(repo) == ()

    def test_load_bindings_rejects_missing_schema_version(self, repo: Path) -> None:
        path = bp.binding_store_path(repo)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"bindings": []}), encoding="utf-8")
        with pytest.raises(bp.BranchPolicyError, match="invalid top-level schema"):
            bp.load_bindings(repo)

    def test_load_bindings_rejects_wrong_schema_version(self, repo: Path) -> None:
        path = bp.binding_store_path(repo)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            json.dumps({"schema_version": 2, "bindings": []}), encoding="utf-8"
        )
        with pytest.raises(bp.BranchPolicyError, match="schema version"):
            bp.load_bindings(repo)

    def test_load_bindings_rejects_duplicate_branches(self, repo: Path) -> None:
        row = {
            "branch": "claude/topic-20260829",
            "claim_id": "c1",
            "owner": "claude",
            "created_at": "2026-01-01T00:00:00+00:00",
            "last_push_at": None,
            "pr_number": None,
        }
        path = bp.binding_store_path(repo)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            json.dumps({"schema_version": 1, "bindings": [row, row]}), encoding="utf-8"
        )
        with pytest.raises(bp.BranchPolicyError, match="duplicate branches"):
            bp.load_bindings(repo)

    def test_bind_then_load_roundtrips(self, repo: Path) -> None:
        _commit(repo, "a.txt")
        _checkout_new(repo, "claude/topic-20260829")
        binding = bp.bind_branch(
            repo, branch="claude/topic-20260829", claim_id="c1", owner="claude"
        )
        loaded = bp.load_bindings(repo)
        assert loaded == (binding,)

    def test_bind_owner_mismatch_refused(self, repo: Path) -> None:
        _commit(repo, "a.txt")
        _checkout_new(repo, "claude/topic-20260829")
        with pytest.raises(bp.BranchPolicyError, match="does not match owner"):
            bp.bind_branch(
                repo, branch="claude/topic-20260829", claim_id="c1", owner="fable-5"
            )

    def test_rebinding_same_branch_replaces_not_duplicates(self, repo: Path) -> None:
        _commit(repo, "a.txt")
        _checkout_new(repo, "claude/topic-20260829")
        bp.bind_branch(
            repo, branch="claude/topic-20260829", claim_id="c1", owner="claude"
        )
        bp.bind_branch(
            repo, branch="claude/topic-20260829", claim_id="c1", owner="claude"
        )
        assert len(bp.load_bindings(repo)) == 1

    def test_second_live_branch_same_claim_is_refused(self, repo: Path) -> None:
        base = _commit(repo, "a.txt")
        _checkout_new(repo, "claude/topic-a-20260829", start=base)
        bp.bind_branch(
            repo, branch="claude/topic-a-20260829", claim_id="c1", owner="claude"
        )
        _git(repo, "checkout", "--quiet", "w7-trident-program")
        _checkout_new(repo, "claude/topic-b-20260829", start=base)
        with pytest.raises(bp.BranchPolicyError, match="already bound to live branch"):
            bp.bind_branch(
                repo, branch="claude/topic-b-20260829", claim_id="c1", owner="claude"
            )

    def test_second_branch_allowed_once_first_is_no_longer_live(
        self, repo: Path
    ) -> None:
        base = _commit(repo, "a.txt")
        _checkout_new(repo, "claude/topic-a-20260829", start=base)
        bp.bind_branch(
            repo, branch="claude/topic-a-20260829", claim_id="c1", owner="claude"
        )
        _git(repo, "checkout", "--quiet", "w7-trident-program")
        _git(repo, "branch", "-D", "claude/topic-a-20260829")
        _checkout_new(repo, "claude/topic-b-20260829", start=base)
        binding = bp.bind_branch(
            repo, branch="claude/topic-b-20260829", claim_id="c1", owner="claude"
        )
        assert binding.branch == "claude/topic-b-20260829"

    def test_unbind_removes_existing(self, repo: Path) -> None:
        _commit(repo, "a.txt")
        _checkout_new(repo, "claude/topic-20260829")
        bp.bind_branch(
            repo, branch="claude/topic-20260829", claim_id="c1", owner="claude"
        )
        assert bp.unbind_branch(repo, branch="claude/topic-20260829") is True
        assert bp.load_bindings(repo) == ()

    def test_unbind_noop_when_absent(self, repo: Path) -> None:
        assert bp.unbind_branch(repo, branch="claude/none-20260829") is False


class TestRecordPushAndStaleness:
    def test_record_push_updates_existing_binding(self, repo: Path) -> None:
        _commit(repo, "a.txt")
        _checkout_new(repo, "claude/topic-20260829")
        bp.bind_branch(
            repo, branch="claude/topic-20260829", claim_id="c1", owner="claude"
        )
        when = datetime(2026, 8, 29, 12, 0, tzinfo=UTC)
        updated = bp.record_push(repo, branch="claude/topic-20260829", when=when)
        assert updated is not None
        assert updated.last_push_at == when.isoformat()

    def test_record_push_noop_for_unbound_branch(self, repo: Path) -> None:
        assert bp.record_push(repo, branch="claude/none-20260829") is None

    def _binding(self, *, created_at: datetime) -> bp.BranchBinding:
        return bp.BranchBinding(
            branch="claude/topic-20260829",
            claim_id="c1",
            owner="claude",
            created_at=created_at.isoformat(),
            last_push_at=None,
            pr_number=None,
        )

    def test_binding_is_stale_just_under_boundary(self) -> None:
        now = datetime(2026, 8, 29, 12, 0, tzinfo=UTC)
        binding = self._binding(
            created_at=now - timedelta(hours=bp.STALE_PUSH_HOURS - 0.01)
        )
        assert bp.binding_is_stale(binding, now=now) is False

    def test_binding_is_stale_at_exact_boundary_is_not_stale(self) -> None:
        now = datetime(2026, 8, 29, 12, 0, tzinfo=UTC)
        binding = self._binding(created_at=now - timedelta(hours=bp.STALE_PUSH_HOURS))
        assert bp.binding_is_stale(binding, now=now) is False

    def test_binding_is_stale_just_over_boundary(self) -> None:
        now = datetime(2026, 8, 29, 12, 0, tzinfo=UTC)
        binding = self._binding(
            created_at=now - timedelta(hours=bp.STALE_PUSH_HOURS + 0.01)
        )
        assert bp.binding_is_stale(binding, now=now) is True


# --------------------------------------------------------------------------- force-mode detection


class TestForceModeFromTokens:
    def test_no_push_token_is_unknown(self) -> None:
        assert bp._force_mode_from_tokens(["git", "status"]) == "unknown"

    def test_push_with_no_flags_is_none(self) -> None:
        assert bp._force_mode_from_tokens(["git", "push", "origin", "main"]) == "none"

    def test_push_with_force_with_lease_is_lease(self) -> None:
        assert (
            bp._force_mode_from_tokens(["git", "push", "--force-with-lease"]) == "lease"
        )

    def test_push_with_force_with_lease_equals_ref_is_lease(self) -> None:
        assert (
            bp._force_mode_from_tokens(
                ["git", "push", "--force-with-lease=refs/heads/x:abc"]
            )
            == "lease"
        )

    def test_push_with_bare_long_force_is_bare(self) -> None:
        assert bp._force_mode_from_tokens(["git", "push", "--force"]) == "bare"

    def test_push_with_bare_short_force_is_bare(self) -> None:
        assert bp._force_mode_from_tokens(["git", "push", "-f"]) == "bare"

    def test_lease_checked_before_bare_when_both_present(self) -> None:
        # Malformed in practice, but proves lease detection isn't short-circuited by a
        # stray bare-force token earlier in argv.
        assert (
            bp._force_mode_from_tokens(["git", "push", "-f", "--force-with-lease"])
            == "lease"
        )


# --------------------------------------------------------------------------- evaluate_push: feature branches


class TestEvaluatePushFeatureBranch:
    def test_fast_forward_push_allowed_regardless_of_force_mode(
        self, repo: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        _commit(repo, "a.txt")
        _checkout_new(repo, "claude/topic-20260829")
        sha = _rev_parse(repo, "claude/topic-20260829")
        monkeypatch.setattr(bp, "detect_force_mode", lambda: "unknown")
        decision = bp.evaluate_push(
            repo, branch="claude/topic-20260829", remote_old=sha, remote_new=sha
        )
        assert decision.allowed is True

    def test_non_ff_with_declared_lease_is_allowed(self, repo: Path) -> None:
        base = _commit(repo, "a.txt")
        _checkout_new(repo, "claude/topic-20260829", start=base)
        old_tip = _commit(repo, "b.txt")
        _git(repo, "reset", "--hard", base)
        new_tip = _commit(repo, "c.txt")
        decision = bp.evaluate_push(
            repo,
            branch="claude/topic-20260829",
            remote_old=old_tip,
            remote_new=new_tip,
            declared_force_with_lease=True,
        )
        assert decision.allowed is True
        assert decision.force_mode == "lease"

    def test_non_ff_with_declared_bare_force_is_refused(self, repo: Path) -> None:
        base = _commit(repo, "a.txt")
        _checkout_new(repo, "claude/topic-20260829", start=base)
        old_tip = _commit(repo, "b.txt")
        _git(repo, "reset", "--hard", base)
        new_tip = _commit(repo, "c.txt")
        decision = bp.evaluate_push(
            repo,
            branch="claude/topic-20260829",
            remote_old=old_tip,
            remote_new=new_tip,
            declared_force=True,
        )
        assert decision.allowed is False
        assert any("bare --force" in r for r in decision.reasons)

    def test_non_ff_with_undeclared_none_force_mode_is_refused(
        self, repo: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        base = _commit(repo, "a.txt")
        _checkout_new(repo, "claude/topic-20260829", start=base)
        old_tip = _commit(repo, "b.txt")
        _git(repo, "reset", "--hard", base)
        new_tip = _commit(repo, "c.txt")
        monkeypatch.setattr(bp, "detect_force_mode", lambda: "none")
        decision = bp.evaluate_push(
            repo, branch="claude/topic-20260829", remote_old=old_tip, remote_new=new_tip
        )
        assert decision.allowed is False
        assert any("no force flag was declared" in r for r in decision.reasons)

    def test_non_ff_with_undetectable_force_mode_is_refused(
        self, repo: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        base = _commit(repo, "a.txt")
        _checkout_new(repo, "claude/topic-20260829", start=base)
        old_tip = _commit(repo, "b.txt")
        _git(repo, "reset", "--hard", base)
        new_tip = _commit(repo, "c.txt")
        monkeypatch.setattr(bp, "detect_force_mode", lambda: "unknown")
        decision = bp.evaluate_push(
            repo, branch="claude/topic-20260829", remote_old=old_tip, remote_new=new_tip
        )
        assert decision.allowed is False
        assert any("could not be established" in r for r in decision.reasons)

    def test_both_force_flags_declared_raises(self, repo: Path) -> None:
        sha = _commit(repo, "a.txt")
        with pytest.raises(bp.BranchPolicyError, match="at most one"):
            bp.evaluate_push(
                repo,
                branch="claude/topic-20260829",
                remote_old=sha,
                remote_new=sha,
                declared_force_with_lease=True,
                declared_force=True,
            )

    def test_invalid_branch_name_surfaces_as_a_reason(self, repo: Path) -> None:
        sha = _commit(repo, "a.txt")
        decision = bp.evaluate_push(
            repo, branch="Bad Name", remote_old=sha, remote_new=sha
        )
        assert decision.allowed is False
        assert any(
            "invalid agent slug" in r or "no '<agent>/' segment" in r
            for r in decision.reasons
        )

    def test_second_live_branch_same_claim_refused_at_push_time(
        self, repo: Path
    ) -> None:
        base = _commit(repo, "a.txt")
        _checkout_new(repo, "claude/topic-a-20260829", start=base)
        _git(repo, "checkout", "--quiet", "w7-trident-program")
        _checkout_new(repo, "claude/topic-b-20260829", start=base)
        _write_binding_row(
            repo, branch="claude/topic-a-20260829", claim_id="c1", owner="claude"
        )
        _write_binding_row(
            repo, branch="claude/topic-b-20260829", claim_id="c1", owner="claude"
        )
        tip = _rev_parse(repo, "claude/topic-b-20260829")
        decision = bp.evaluate_push(
            repo, branch="claude/topic-b-20260829", remote_old="", remote_new=tip
        )
        assert decision.allowed is False
        assert any("already bound to live branch" in r for r in decision.reasons)

    def test_no_conflict_when_the_other_bound_branch_is_gone(self, repo: Path) -> None:
        base = _commit(repo, "a.txt")
        _checkout_new(repo, "claude/topic-b-20260829", start=base)
        _write_binding_row(
            repo, branch="claude/topic-a-20260829", claim_id="c1", owner="claude"
        )
        _write_binding_row(
            repo, branch="claude/topic-b-20260829", claim_id="c1", owner="claude"
        )
        tip = _rev_parse(repo, "claude/topic-b-20260829")
        decision = bp.evaluate_push(
            repo, branch="claude/topic-b-20260829", remote_old="", remote_new=tip
        )
        assert decision.allowed is True


# --------------------------------------------------------------------------- evaluate_push: integration branches


class TestEvaluatePushIntegrationBranch:
    def test_non_ff_refused_regardless_of_provenance(self, repo: Path) -> None:
        old = _commit(repo, "a.txt")
        new = _commit(repo, "b.txt")
        # reversed roles => not a fast-forward
        decision = bp.evaluate_push(
            repo, branch="w7-trident-program", remote_old=new, remote_new=old
        )
        assert decision.allowed is False
        assert any("fast-forward" in r for r in decision.reasons)

    def test_ff_with_provenance_on_another_pushed_ref_is_allowed(
        self, repo: Path
    ) -> None:
        base = _commit(repo, "a.txt")
        _checkout_new(repo, "claude/feat-20260829", start=base)
        feat_sha = _commit(repo, "feature.txt")
        _git(repo, "update-ref", "refs/remotes/origin/claude/feat-20260829", feat_sha)
        _git(repo, "checkout", "--quiet", "w7-trident-program")
        _git(repo, "merge", "--ff-only", "--quiet", "claude/feat-20260829")
        decision = bp.evaluate_push(
            repo, branch="w7-trident-program", remote_old=base, remote_new=feat_sha
        )
        assert decision.allowed is True

    def test_ff_without_provenance_anywhere_else_is_refused(self, repo: Path) -> None:
        base = _commit(repo, "a.txt")
        direct = _commit(repo, "direct.txt")
        decision = bp.evaluate_push(
            repo, branch="w7-trident-program", remote_old=base, remote_new=direct
        )
        assert decision.allowed is False
        assert any(
            "not already present on any other pushed ref" in r for r in decision.reasons
        )

    def test_new_integration_ref_with_zero_old_and_no_other_refs_is_refused(
        self, repo: Path
    ) -> None:
        sha = _commit(repo, "a.txt")
        decision = bp.evaluate_push(
            repo, branch="w7-trident-program", remote_old="", remote_new=sha
        )
        assert decision.allowed is False

    def test_mirror_branch_is_treated_as_integration_too(self, repo: Path) -> None:
        old = _commit(repo, "a.txt")
        new = _commit(repo, "b.txt")
        decision = bp.evaluate_push(
            repo, branch="master", remote_old=new, remote_new=old
        )
        assert decision.allowed is False
        assert any("fast-forward" in r for r in decision.reasons)


# --------------------------------------------------------------------------- CLI


class TestCli:
    def test_check_branch_ok_exit_zero(
        self, capsys: pytest.CaptureFixture[str]
    ) -> None:
        rc = bp.main(["check-branch", "claude/topic-20260829"])
        assert rc == 0
        assert "OK" in capsys.readouterr().out

    def test_check_branch_refused_exit_one(
        self, capsys: pytest.CaptureFixture[str]
    ) -> None:
        rc = bp.main(["check-branch", "bad name"])
        assert rc == 1
        assert "REFUSED" in capsys.readouterr().out

    def test_check_branch_json_shape(self, capsys: pytest.CaptureFixture[str]) -> None:
        bp.main(["check-branch", "claude/topic-20260829", "--json"])
        payload = json.loads(capsys.readouterr().out)
        assert payload == {
            "ok": True,
            "branch": "claude/topic-20260829",
            "raw": "claude/topic-20260829",
            "agent": "claude",
            "topic": "topic",
            "date": "20260829",
        }

    def test_check_push_explicit_branch_allow(
        self,
        repo: Path,
        monkeypatch: pytest.MonkeyPatch,
        capsys: pytest.CaptureFixture[str],
    ) -> None:
        _commit(repo, "a.txt")
        _checkout_new(repo, "claude/topic-20260829")
        monkeypatch.chdir(repo)
        rc = bp.main(["check-push", "--branch", "claude/topic-20260829"])
        assert rc == 0
        assert "ALLOW" in capsys.readouterr().out
        assert bp.load_bindings(repo) == ()  # no binding exists, nothing to record

    def test_check_push_explicit_branch_refuse(
        self,
        repo: Path,
        monkeypatch: pytest.MonkeyPatch,
        capsys: pytest.CaptureFixture[str],
    ) -> None:
        monkeypatch.chdir(repo)
        rc = bp.main(["check-branch", "bad name"])
        assert rc == 1
        capsys.readouterr()
        rc = bp.main(["check-push", "--branch", "Bad Name"])
        assert rc != 0

    def test_check_push_stdin_protocol_allow(
        self,
        repo: Path,
        monkeypatch: pytest.MonkeyPatch,
        capsys: pytest.CaptureFixture[str],
    ) -> None:
        import io

        _commit(repo, "a.txt")
        _checkout_new(repo, "claude/topic-20260829")
        tip = _rev_parse(repo, "claude/topic-20260829")
        monkeypatch.chdir(repo)
        stdin_line = f"refs/heads/claude/topic-20260829 {tip} refs/heads/claude/topic-20260829 {'0' * 40}\n"
        fake_stdin = io.StringIO(stdin_line)
        fake_stdin.isatty = lambda: False  # type: ignore[method-assign]
        monkeypatch.setattr("sys.stdin", fake_stdin)
        rc = bp.main(["check-push"])
        assert rc == 0
        assert "ALLOW" in capsys.readouterr().out

    def test_check_push_no_stdin_and_no_branch_is_a_noop(
        self,
        repo: Path,
        monkeypatch: pytest.MonkeyPatch,
        capsys: pytest.CaptureFixture[str],
    ) -> None:
        import io

        monkeypatch.chdir(repo)
        fake_stdin = io.StringIO("")
        fake_stdin.isatty = lambda: False  # type: ignore[method-assign]
        monkeypatch.setattr("sys.stdin", fake_stdin)
        rc = bp.main(["check-push"])
        assert rc == 0
        assert "nothing to check" in capsys.readouterr().out

    def test_bind_and_unbind_cli(
        self,
        repo: Path,
        monkeypatch: pytest.MonkeyPatch,
        capsys: pytest.CaptureFixture[str],
    ) -> None:
        _commit(repo, "a.txt")
        _checkout_new(repo, "claude/topic-20260829")
        monkeypatch.chdir(repo)
        rc = bp.main(["bind", "--branch", "claude/topic-20260829", "--claim", "c1"])
        assert rc == 0
        assert "BOUND" in capsys.readouterr().out
        rc = bp.main(["unbind", "--branch", "claude/topic-20260829"])
        assert rc == 0
        assert "UNBOUND" in capsys.readouterr().out
        rc = bp.main(["unbind", "--branch", "claude/topic-20260829"])
        assert rc == 1
        assert "NO BINDING" in capsys.readouterr().out

    def test_status_reports_bindings(
        self,
        repo: Path,
        monkeypatch: pytest.MonkeyPatch,
        capsys: pytest.CaptureFixture[str],
    ) -> None:
        _commit(repo, "a.txt")
        _checkout_new(repo, "claude/topic-20260829")
        monkeypatch.chdir(repo)
        bp.main(["bind", "--branch", "claude/topic-20260829", "--claim", "c1"])
        capsys.readouterr()
        rc = bp.main(["status"])
        assert rc == 0
        out = capsys.readouterr().out
        assert "claude/topic-20260829" in out
        assert "c1" in out

    def test_status_empty_reports_no_bindings(
        self,
        repo: Path,
        monkeypatch: pytest.MonkeyPatch,
        capsys: pytest.CaptureFixture[str],
    ) -> None:
        monkeypatch.chdir(repo)
        rc = bp.main(["status"])
        assert rc == 0
        assert "no branch bindings" in capsys.readouterr().out

    def test_exposed_always_exits_zero_even_when_it_finds_things(
        self,
        repo: Path,
        monkeypatch: pytest.MonkeyPatch,
        capsys: pytest.CaptureFixture[str],
    ) -> None:
        _commit(repo, "a.txt")
        monkeypatch.chdir(repo)
        rc = bp.main(["exposed"])
        assert rc == 0
