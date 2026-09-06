"""Tests for the required-check promotion and demotion policy.

Both thresholds get a fixture on each side. A policy whose tests only prove the
happy path is how `candidate-review` became required without ever having been green.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from conductor import gate_rollout
from conductor.gate_rollout import CheckRun, RolloutError


def _run(
    *,
    check: str = "candidate-review",
    pr: int = 1,
    conclusion: str = "success",
    required: bool = False,
    merged_at: str = "2026-08-01T00:00:00+00:00",
    unrelated: bool = False,
) -> CheckRun:
    return CheckRun(
        check=check,
        pr_number=pr,
        conclusion=conclusion,
        required=required,
        merged_at=merged_at,
        unrelated_to_diff=unrelated,
    )


def _greens(count: int, *, required: bool = False) -> list[CheckRun]:
    return [
        _run(
            pr=index,
            merged_at=f"2026-08-{index + 1:02d}T00:00:00+00:00",
            required=required,
        )
        for index in range(1, count + 1)
    ]


# ---------------------------------------------------------------------------
# Promotion threshold -- both sides of >= PROMOTION_GREEN_RUNS
# ---------------------------------------------------------------------------


def test_promotion_is_refused_one_run_below_the_threshold() -> None:
    runs = _greens(gate_rollout.PROMOTION_GREEN_RUNS - 1)
    allowed, reason = gate_rollout.may_promote(runs, "candidate-review")
    assert allowed is False
    assert str(gate_rollout.PROMOTION_GREEN_RUNS) in reason


def test_promotion_is_allowed_exactly_at_the_threshold() -> None:
    runs = _greens(gate_rollout.PROMOTION_GREEN_RUNS)
    allowed, _reason = gate_rollout.may_promote(runs, "candidate-review")
    assert allowed is True


def test_promotion_is_allowed_above_the_threshold() -> None:
    runs = _greens(gate_rollout.PROMOTION_GREEN_RUNS + 3)
    allowed, _reason = gate_rollout.may_promote(runs, "candidate-review")
    assert allowed is True


def test_a_red_resets_the_promotion_count() -> None:
    """Five greens with a red among them is flakiness, not readiness."""
    runs = _greens(gate_rollout.PROMOTION_GREEN_RUNS)
    runs.append(
        _run(pr=99, conclusion="failure", merged_at="2026-08-20T00:00:00+00:00")
    )
    runs.append(_run(pr=100, merged_at="2026-08-21T00:00:00+00:00"))
    assert gate_rollout.consecutive_green_as_advisory(runs, "candidate-review") == 1
    allowed, _reason = gate_rollout.may_promote(runs, "candidate-review")
    assert allowed is False


def test_required_runs_do_not_count_toward_promotion() -> None:
    """The threshold is about ADVISORY history; required greens are a different thing."""
    runs = _greens(gate_rollout.PROMOTION_GREEN_RUNS, required=True)
    assert gate_rollout.consecutive_green_as_advisory(runs, "candidate-review") == 0


def test_promotion_counts_are_per_check() -> None:
    runs = _greens(gate_rollout.PROMOTION_GREEN_RUNS)
    allowed, _reason = gate_rollout.may_promote(runs, "some-other-check")
    assert allowed is False


# ---------------------------------------------------------------------------
# Demotion threshold -- both sides of >= DEMOTION_CONSECUTIVE_REDS
# ---------------------------------------------------------------------------


def test_one_unrelated_red_does_not_demote() -> None:
    runs = [_run(pr=1, conclusion="failure", required=True, unrelated=True)]
    needed, _reason = gate_rollout.must_demote(runs, "candidate-review")
    assert needed is False


def test_two_consecutive_unrelated_reds_demote() -> None:
    runs = [
        _run(
            pr=1,
            conclusion="failure",
            required=True,
            unrelated=True,
            merged_at="2026-08-01T00:00:00+00:00",
        ),
        _run(
            pr=2,
            conclusion="failure",
            required=True,
            unrelated=True,
            merged_at="2026-08-02T00:00:00+00:00",
        ),
    ]
    needed, reason = gate_rollout.must_demote(runs, "candidate-review")
    assert needed is True
    assert "#2" in reason


def test_a_red_related_to_its_own_diff_does_not_count() -> None:
    """The other side of `unrelated_to_diff`: a real defect must keep blocking."""
    runs = [
        _run(
            pr=1,
            conclusion="failure",
            required=True,
            unrelated=True,
            merged_at="2026-08-01T00:00:00+00:00",
        ),
        _run(
            pr=2,
            conclusion="failure",
            required=True,
            unrelated=False,
            merged_at="2026-08-02T00:00:00+00:00",
        ),
    ]
    needed, _reason = gate_rollout.must_demote(runs, "candidate-review")
    assert needed is False


def test_a_green_between_reds_breaks_the_demotion_streak() -> None:
    runs = [
        _run(
            pr=1,
            conclusion="failure",
            required=True,
            unrelated=True,
            merged_at="2026-08-01T00:00:00+00:00",
        ),
        _run(
            pr=2,
            conclusion="success",
            required=True,
            merged_at="2026-08-02T00:00:00+00:00",
        ),
        _run(
            pr=3,
            conclusion="failure",
            required=True,
            unrelated=True,
            merged_at="2026-08-03T00:00:00+00:00",
        ),
    ]
    needed, _reason = gate_rollout.must_demote(runs, "candidate-review")
    assert needed is False


def test_advisory_reds_never_demote() -> None:
    """A non-required check cannot be demoted; it is already where demotion lands."""
    runs = [
        _run(
            pr=1,
            conclusion="failure",
            required=False,
            unrelated=True,
            merged_at="2026-08-01T00:00:00+00:00",
        ),
        _run(
            pr=2,
            conclusion="failure",
            required=False,
            unrelated=True,
            merged_at="2026-08-02T00:00:00+00:00",
        ),
    ]
    needed, _reason = gate_rollout.must_demote(runs, "candidate-review")
    assert needed is False


# ---------------------------------------------------------------------------
# Ledger round-trip
# ---------------------------------------------------------------------------


def test_ledger_round_trips(tmp_path: Path) -> None:
    ledger = tmp_path / "ledger.json"
    runs = _greens(3)
    gate_rollout.save_ledger(ledger, runs)
    assert gate_rollout.load_ledger(ledger) == runs


def test_missing_ledger_is_empty_not_an_error(tmp_path: Path) -> None:
    assert gate_rollout.load_ledger(tmp_path / "absent.json") == []


def test_unknown_ledger_schema_is_refused(tmp_path: Path) -> None:
    ledger = tmp_path / "ledger.json"
    ledger.write_text(json.dumps({"schema_version": 999, "runs": []}), encoding="utf-8")
    with pytest.raises(RolloutError):
        gate_rollout.load_ledger(ledger)


def test_promote_refuses_without_the_owner_acknowledgement(
    tmp_path: Path, capsys
) -> None:
    """Meeting the threshold makes a check eligible, never promoted."""
    ledger = tmp_path / "ledger.json"
    gate_rollout.save_ledger(ledger, _greens(gate_rollout.PROMOTION_GREEN_RUNS))
    exit_code = gate_rollout.main(
        [
            "--ledger",
            str(ledger),
            "promote",
            "--check",
            "candidate-review",
            "--ruleset",
            "1",
        ]
    )
    assert exit_code == 1
    assert "owner's decision" in capsys.readouterr().err


# ---------------------------------------------------------------------------
# GitHub ruleset mutation
#
# `promote` and `demote` rewrite a branch protection rule. Nothing here had a
# test: the module's whole `gh`-shelling half was reachable only by running it
# against the real repository, which is the one thing a test must never do.
# ---------------------------------------------------------------------------


class _Completed:
    def __init__(self, returncode: int, stdout: str = "", stderr: str = "") -> None:
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


def _stub_gh(
    monkeypatch, ruleset: dict, *, put_returncode: int = 0, put_stderr: str = ""
):
    """Answer the GET with `ruleset` and collect the body of every PUT."""
    sent: list[dict] = []

    def fake_run(argv, **kwargs):
        if "--method" not in argv:
            return _Completed(0, stdout=json.dumps(ruleset))
        sent.append(json.loads(kwargs["input"]))
        return _Completed(put_returncode, stderr=put_stderr)

    monkeypatch.setattr(gate_rollout.subprocess, "run", fake_run)
    return sent


def test_a_failed_gh_call_names_the_command_and_carries_its_stderr(
    monkeypatch,
) -> None:
    monkeypatch.setattr(
        gate_rollout.subprocess,
        "run",
        lambda *_, **__: _Completed(1, stderr="  HTTP 404: Not Found  \n"),
    )
    with pytest.raises(RolloutError) as raised:
        gate_rollout._gh_api(["repos/o/r/rulesets/1"])
    message = str(raised.value)
    assert "gh api repos/o/r/rulesets/1" in message
    assert "HTTP 404: Not Found" in message
    assert not message.endswith(" ")


def test_setting_required_checks_replaces_only_the_status_check_rule(
    monkeypatch,
) -> None:
    """Every other rule, and the ruleset's conditions, survive the rewrite."""
    existing = {
        "name": "default",
        "enforcement": "active",
        "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"]}},
        "rules": [
            {"type": "deletion"},
            {
                "type": "required_status_checks",
                "parameters": {"required_status_checks": [{"context": "old"}]},
            },
        ],
    }
    sent = _stub_gh(monkeypatch, existing)
    gate_rollout.set_ruleset_required_checks("o/r", 1, ["candidate-review"])

    (body,) = sent
    assert body["conditions"] == existing["conditions"]
    assert {rule["type"] for rule in body["rules"]} == {
        "deletion",
        "required_status_checks",
    }
    checks = next(
        rule for rule in body["rules"] if rule["type"] == "required_status_checks"
    )
    assert checks["parameters"]["required_status_checks"] == [
        {"context": "candidate-review"}
    ]
    assert checks["parameters"]["strict_required_status_checks_policy"] is False


def test_an_empty_context_list_removes_the_rule_rather_than_emptying_it(
    monkeypatch,
) -> None:
    """A rule with no contexts blocks nothing; demotion must delete it outright."""
    existing = {
        "name": "default",
        "enforcement": "active",
        "rules": [
            {
                "type": "required_status_checks",
                "parameters": {"required_status_checks": [{"context": "old"}]},
            }
        ],
    }
    sent = _stub_gh(monkeypatch, existing)
    gate_rollout.set_ruleset_required_checks("o/r", 1, [])

    (body,) = sent
    assert body["rules"] == []
    assert "conditions" not in body


def test_a_rejected_ruleset_update_is_raised_not_swallowed(monkeypatch) -> None:
    existing = {"name": "default", "enforcement": "active", "rules": []}
    _stub_gh(
        monkeypatch,
        existing,
        put_returncode=1,
        put_stderr="Resource not accessible by integration",
    )
    with pytest.raises(RolloutError, match="Resource not accessible"):
        gate_rollout.set_ruleset_required_checks("o/r", 1, ["candidate-review"])


def test_reading_required_checks_returns_the_contexts_in_the_rule(
    monkeypatch,
) -> None:
    payload = {
        "rules": [
            {"type": "deletion"},
            {
                "type": "required_status_checks",
                "parameters": {
                    "required_status_checks": [{"context": "a"}, {"context": "b"}]
                },
            },
        ]
    }
    monkeypatch.setattr(
        gate_rollout.subprocess,
        "run",
        lambda *_, **__: _Completed(0, stdout=json.dumps(payload)),
    )
    assert gate_rollout.ruleset_required_checks("o/r", 1) == ["a", "b"]


def test_a_ruleset_with_no_status_check_rule_reads_as_no_required_checks(
    monkeypatch,
) -> None:
    monkeypatch.setattr(
        gate_rollout.subprocess,
        "run",
        lambda *_, **__: _Completed(
            0, stdout=json.dumps({"rules": [{"type": "deletion"}]})
        ),
    )
    assert gate_rollout.ruleset_required_checks("o/r", 1) == []


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def test_record_appends_a_run_and_status_reads_it_back(tmp_path: Path, capsys) -> None:
    ledger = tmp_path / "ledger.json"
    assert (
        gate_rollout.main(
            [
                "--ledger",
                str(ledger),
                "record",
                "--check",
                "candidate-review",
                "--pr",
                "7",
                "--conclusion",
                "success",
                "--merged-at",
                "2026-08-01T00:00:00+00:00",
            ]
        )
        == 0
    )
    assert "recorded candidate-review on #7" in capsys.readouterr().out
    assert gate_rollout.main(["--ledger", str(ledger), "status"]) == 0
    out = capsys.readouterr().out
    assert "candidate-review:" in out
    assert "promotion: blocked" in out
    assert "demotion:  not needed" in out


def test_status_on_an_empty_ledger_says_so_instead_of_printing_nothing(
    tmp_path: Path, capsys
) -> None:
    ledger = tmp_path / "ledger.json"
    assert gate_rollout.main(["--ledger", str(ledger), "status"]) == 0
    assert "ledger is empty" in capsys.readouterr().out
