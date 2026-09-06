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


def _init_repo(path: Path, *, initial_branch: str = "master") -> Path:
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


def _write_binding_row(repo: Path, *, branch: str, claim_id: str, owner: str) -> None:
    """Append a binding row straight to the store, bypassing bind_branch's own guard.

    Used to simulate a fan-out that already happened (e.g. a hand-edited store, or a
    binding written before the guard existed) so ``audit_repo``'s independent,
    after-the-fact re-check of the same rule can be exercised on its own.
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


class TestIsIntegrationBranch:
    def test_true_for_primary(self) -> None:
        assert bp.is_integration_branch("master") is True

    def test_the_constant_names_the_current_line(self) -> None:
        # The regression this guards: the constant lagged the integration line by four
        # days, so every caller that defaulted to it resolved a ref no checkout had.
        assert bp.INTEGRATION_BRANCH == "master"
        assert bp.INTEGRATION_BRANCHES[0] == bp.INTEGRATION_BRANCH

    def test_true_for_a_retired_integration_line(self) -> None:
        # w7-trident-program stopped being the integration line on 2026-08-30. A name
        # that was once the line must still never be classified as a deletable feature
        # branch if it turns up on an old worktree or a stale remote.
        assert bp.RETIRED_INTEGRATION_BRANCHES == ("w7-trident-program",)
        for retired in bp.RETIRED_INTEGRATION_BRANCHES:
            assert bp.is_integration_branch(retired) is True
            assert retired in bp.INTEGRATION_BRANCHES

    def test_false_for_feature_branch(self) -> None:
        assert bp.is_integration_branch("claude/topic-20260829") is False

    def test_false_for_near_miss_substring(self) -> None:
        assert bp.is_integration_branch("mastered") is False
        assert bp.is_integration_branch("w7-trident-program-old") is False


# --------------------------------------------------------------------------- fast-forward


class TestIsFastForward:
    def test_equal_shas_is_ff(
        self, repo: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        sha = _commit(repo, "a.txt")

        # git would answer True here anyway, so asserting the return value alone
        # tests nothing. What the shortcut buys is skipping the subprocess.
        def boom(*args: object, **kwargs: object) -> object:
            raise AssertionError("equal shas must not shell out to git")

        monkeypatch.setattr(bp.subprocess, "run", boom)
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
        _git(repo, "checkout", "--quiet", "master")
        main_tip = _commit(repo, "b.txt")
        assert bp.is_fast_forward(repo, old=side_tip, new=main_tip) is False


# --------------------------------------------------------------------------- local-only commits


class TestLocalOnlyCommits:
    def test_reports_everything_when_nothing_is_excluded(self, repo: Path) -> None:
        first = _commit(repo, "a.txt")
        second = _commit(repo, "b.txt")
        rows = bp.local_only_commits(repo, "master")
        # the whole unpushed run, newest first -- not just the tip
        assert [row["sha"] for row in rows] == [second, first]

    def test_excluded_by_remote_ref(self, repo: Path) -> None:
        sha = _commit(repo, "a.txt")
        _git(repo, "update-ref", "refs/remotes/origin/master", sha)
        assert bp.local_only_commits(repo, "master") == ()

    def test_excluded_by_snapshot_ref(self, repo: Path) -> None:
        sha = _commit(repo, "a.txt")
        _git(repo, "update-ref", "refs/snapshots/backup-1", sha)
        assert bp.local_only_commits(repo, "master") == ()

    def test_only_the_unpushed_tail_is_reported(self, repo: Path) -> None:
        base = _commit(repo, "a.txt")
        _git(repo, "update-ref", "refs/remotes/origin/master", base)
        newer = _commit(repo, "b.txt")
        rows = bp.local_only_commits(repo, "master")
        assert [row["sha"] for row in rows] == [newer]


# --------------------------------------------------------------------------- binding store


class TestBindingStore:
    def test_store_lives_at_the_governance_path_other_tooling_reads(
        self, repo: Path
    ) -> None:
        """The location is a contract, not an implementation detail.

        Every test below reaches the store through ``binding_store_path``, so a
        mutant that empties either path segment moves the store and stays green in
        all of them. This is the one test that names the location outright.
        """
        assert bp.binding_store_path(repo) == (
            bp.git_common_dir(repo) / "governance" / "branch-bindings.json"
        )

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
        _git(repo, "checkout", "--quiet", "master")
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
        _git(repo, "checkout", "--quiet", "master")
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


class TestBindingStaleness:
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


# --------------------------------------------------------------------------- at-rest audit


class TestAuditRepo:
    """``audit_repo`` replaces the deleted pre-push guard for the rules that survive.

    Rule 4 (force mode) has no test because it has no implementation: it cannot be
    decided without the push command line, and the module says so rather than
    guessing from ``/proc``.
    """

    def test_clean_repo_has_no_findings(self, repo: Path) -> None:
        _commit(repo, "a.txt")
        assert bp.audit_repo(repo) == ()

    def test_integration_branch_ahead_of_its_remote_is_silent(self, repo: Path) -> None:
        """Ahead is a fast-forward, which is exactly what rule 1 permits."""
        base = _commit(repo, "a.txt")
        _git(repo, "update-ref", "refs/remotes/origin/master", base)
        _commit(repo, "b.txt")
        assert bp.audit_repo(repo) == ()

    def test_integration_branch_diverged_from_its_remote_is_reported(
        self, repo: Path
    ) -> None:
        base = _commit(repo, "a.txt")
        _git(repo, "update-ref", "refs/remotes/origin/master", base)
        # Rewrite the tip: origin/master is no longer an ancestor of master, so
        # landing this would rewrite the integration line rather than extend it.
        _git(repo, "commit", "--quiet", "--amend", "-m", "rewritten")
        findings = bp.audit_repo(repo)
        assert len(findings) == 1
        assert "rule 1" in findings[0]
        assert "diverged" in findings[0]

    def test_integration_branch_with_no_remote_counterpart_is_silent(
        self, repo: Path
    ) -> None:
        """A local-only master has nothing to have diverged from -- not a violation."""
        _commit(repo, "a.txt")
        assert bp.audit_repo(repo) == ()

    def test_misnamed_feature_branch_is_reported(self, repo: Path) -> None:
        _commit(repo, "a.txt")
        _git(repo, "branch", "not-a-valid-name")
        findings = bp.audit_repo(repo)
        assert len(findings) == 1
        assert "rule 2" in findings[0]
        assert "not-a-valid-name" in findings[0]
        # The refusal's own suggestion is the actionable half: a name to rename *to*.
        assert "try 'not-a-valid-name/topic-" in findings[0]

    def test_well_named_feature_branch_is_silent(self, repo: Path) -> None:
        _commit(repo, "a.txt")
        _git(repo, "branch", "claude/topic-20260906")
        assert bp.audit_repo(repo) == ()

    def test_two_live_branches_on_one_claim_are_reported(self, repo: Path) -> None:
        _commit(repo, "a.txt")
        for branch in ("claude/first-20260906", "claude/second-20260906"):
            _git(repo, "branch", branch)
            _write_binding_row(repo, branch=branch, claim_id="claim-x", owner="claude")
        findings = bp.audit_repo(repo)
        assert len(findings) == 1
        assert "rule 3" in findings[0]
        assert "claim-x" in findings[0]
        assert "claude/first-20260906" in findings[0]
        assert "claude/second-20260906" in findings[0]

    def test_one_claim_one_branch_is_silent(self, repo: Path) -> None:
        _commit(repo, "a.txt")
        _git(repo, "branch", "claude/only-20260906")
        _write_binding_row(
            repo, branch="claude/only-20260906", claim_id="claim-x", owner="claude"
        )
        assert bp.audit_repo(repo) == ()

    def test_a_binding_whose_branch_is_gone_does_not_count_as_fan_out(
        self, repo: Path
    ) -> None:
        """Only *live* branches count -- a stale row for a deleted branch is not a
        second branch, which is the whole point of intersecting with refs/heads."""
        _commit(repo, "a.txt")
        _git(repo, "branch", "claude/live-20260906")
        _write_binding_row(
            repo, branch="claude/live-20260906", claim_id="claim-x", owner="claude"
        )
        _write_binding_row(
            repo, branch="claude/deleted-20260906", claim_id="claim-x", owner="claude"
        )
        assert bp.audit_repo(repo) == ()


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

    def test_audit_exits_zero_and_says_so_on_a_clean_repo(
        self,
        repo: Path,
        monkeypatch: pytest.MonkeyPatch,
        capsys: pytest.CaptureFixture[str],
    ) -> None:
        _commit(repo, "a.txt")
        monkeypatch.chdir(repo)
        assert bp.main(["audit"]) == 0
        assert capsys.readouterr().out.startswith("OK:")

    def test_audit_exits_one_and_prints_each_violation(
        self,
        repo: Path,
        monkeypatch: pytest.MonkeyPatch,
        capsys: pytest.CaptureFixture[str],
    ) -> None:
        _commit(repo, "a.txt")
        _git(repo, "branch", "Not-A-Valid-Name")
        monkeypatch.chdir(repo)
        assert bp.main(["audit"]) == 1
        out = capsys.readouterr().out
        assert "VIOLATIONS: 1" in out
        assert "Not-A-Valid-Name" in out
