"""`cost_budget_audit.phase`: maps `forge ledger audit`'s verdict onto a gate phase.

No `forge` binary is spawned here -- `run_forge_ledger_audit` is stubbed with a
fixture `CompletedProcess`, the same boundary `audit.rs`'s own unit tests stop
at from the other side. These tests cover the JSON-to-`PhaseResult` mapping
only: `ok` is `False` only on a `REGRESSION` metric -- `PASS`, `RATCHET_HELD`,
`NO_BASELINE` and `NO_DATA` (including the hard-empty exit-3 case) are all
`ok` so a fresh clone or CI runner with no ledger data yet does not turn
`make gate` permanently red, while `detail` still names every metric's status
verbatim, never rounded up to `PASS`.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from conductor import cost_budget_audit


def _completed(
    payload: dict, *, returncode: int = 0, stderr: str = ""
) -> subprocess.CompletedProcess[str]:
    return subprocess.CompletedProcess(
        args=["forge", "ledger", "audit"],
        returncode=returncode,
        stdout=json.dumps(payload),
        stderr=stderr,
    )


def _metric(status: str, *, value: float = 1.0, baseline: float | None = 0.9) -> dict:
    return {
        "value": value,
        "n": 10,
        "baseline": baseline,
        "delta_pct": None if baseline is None else (value - baseline) / baseline * 100,
        "status": status,
    }


def _payload(status: str, metric_status: str) -> dict:
    """All five Phase 3 step 4 metrics, `median_hook_ms` /
    `resend_bytes_per_session` / `tokens_per_landed_pr` from step 3 plus
    `cap_breach_rate` / `cheap_tier_rework_rate` -- a real `forge ledger
    audit` JSON always carries all five keys now (`METRIC_NAMES`,
    `audit.rs`), so a fixture with fewer would no longer match production
    shape.
    """
    return {
        "window": {"from": "2026-09-06", "to": "2026-09-13", "days": 7},
        "metrics": {
            "median_hook_ms": _metric(metric_status),
            "resend_bytes_per_session": _metric(metric_status),
            "tokens_per_landed_pr": _metric(metric_status),
            "cap_breach_rate": _metric(metric_status),
            "cheap_tier_rework_rate": _metric(metric_status),
        },
        "status": status,
    }


@pytest.fixture(autouse=True)
def _fake_forge_binary(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> Path:
    """`phase()` refuses loudly with no binary; give it one that is never run."""

    binary = tmp_path / "forge"
    binary.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    binary.chmod(0o755)
    monkeypatch.setattr(cost_budget_audit, "resolve_forge_binary", lambda root: binary)
    return binary


def test_pass_status_is_ok(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setattr(
        cost_budget_audit,
        "run_forge_ledger_audit",
        lambda **kwargs: _completed(_payload("PASS", "PASS")),
    )
    result = cost_budget_audit.phase(tmp_path)
    assert result.ok
    assert result.name == "cost-budget-audit"
    assert "PASS" in result.detail


def test_ratchet_held_is_ok_but_not_pass(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setattr(
        cost_budget_audit,
        "run_forge_ledger_audit",
        lambda **kwargs: _completed(_payload("RATCHET_HELD", "RATCHET_HELD")),
    )
    result = cost_budget_audit.phase(tmp_path)
    assert result.ok
    assert result.evidence["status"] == "RATCHET_HELD"
    assert result.evidence["status"] != "PASS"


def test_regression_status_is_not_ok(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Synthetic regression fixture (design step 5's exit criterion)."""

    payload = _payload("REGRESSION", "RATCHET_HELD")
    payload["metrics"]["tokens_per_landed_pr"] = _metric(
        "REGRESSION", value=500_000.0, baseline=150_000.0
    )
    monkeypatch.setattr(
        cost_budget_audit,
        "run_forge_ledger_audit",
        lambda **kwargs: _completed(payload),
    )
    result = cost_budget_audit.phase(tmp_path)
    assert not result.ok
    assert "REGRESSION" in result.detail
    assert result.evidence["metrics"]["tokens_per_landed_pr"]["status"] == "REGRESSION"


def test_no_baseline_status_is_ok(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """No recorded baseline yet is not a regression -- nothing to regress against."""

    monkeypatch.setattr(
        cost_budget_audit,
        "run_forge_ledger_audit",
        lambda **kwargs: _completed(_payload("NO_BASELINE", "NO_BASELINE")),
    )
    result = cost_budget_audit.phase(tmp_path)
    assert result.ok
    assert "NO_BASELINE" in result.detail


def test_no_data_exit_code_is_ok_with_status_in_detail(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Exit 3 (every table empty, e.g. a fresh clone/CI runner) must not turn
    `make gate` permanently red -- it is `ok`, but `NO_DATA` still shows up
    verbatim in `detail` rather than being hidden or rounded up to `PASS`.
    """

    monkeypatch.setattr(
        cost_budget_audit,
        "run_forge_ledger_audit",
        lambda **kwargs: _completed({}, returncode=3, stderr="window has zero rows"),
    )
    result = cost_budget_audit.phase(tmp_path)
    assert result.ok
    assert "NO_DATA" in result.detail
    assert result.evidence["status"] == "NO_DATA"
    # stderr, not the (always-JSON, so always-truthy) stdout, is the message
    # source when both are present -- pins the `or` in `phase()`, not `and`.
    assert "window has zero rows" in result.detail
    assert result.evidence["message"] == "window has zero rows"


def test_no_data_falls_back_to_stdout_when_stderr_is_empty(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setattr(
        cost_budget_audit,
        "run_forge_ledger_audit",
        lambda **kwargs: subprocess.CompletedProcess(
            args=["forge"], returncode=3, stdout="stdout says zero rows too", stderr=""
        ),
    )
    result = cost_budget_audit.phase(tmp_path)
    assert result.ok
    assert result.evidence["message"] == "stdout says zero rows too"


def test_single_regression_metric_makes_phase_not_ok(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """`ok` is decided per-metric, not off the overall status string alone."""

    payload = _payload("REGRESSION", "PASS")
    payload["metrics"]["resend_bytes_per_session"] = _metric(
        "REGRESSION", value=20_000_000.0, baseline=8_800_000.0
    )
    monkeypatch.setattr(
        cost_budget_audit,
        "run_forge_ledger_audit",
        lambda **kwargs: _completed(payload),
    )
    result = cost_budget_audit.phase(tmp_path)
    assert not result.ok


def test_five_metric_payload_with_no_baseline_metric_is_ok(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """`cheap_tier_rework_rate`'s debt case (Phase 3 step 4, item 2): a real
    recorded baseline can carry `null` for it until the `ci_history` cache
    file exists, which `audit.rs` reports as a `NO_BASELINE` metric on every
    subsequent check, not `NO_DATA`, and not as this metric's own
    `REGRESSION` -- `phase()` must stay `ok` and must still name the status
    verbatim in `detail`.
    """

    payload = _payload("NO_BASELINE", "RATCHET_HELD")
    payload["metrics"]["cheap_tier_rework_rate"] = _metric(
        "NO_BASELINE", value=None, baseline=None
    )
    payload["status"] = "NO_BASELINE"
    assert len(payload["metrics"]) == 5
    monkeypatch.setattr(
        cost_budget_audit,
        "run_forge_ledger_audit",
        lambda **kwargs: _completed(payload),
    )
    result = cost_budget_audit.phase(tmp_path)
    assert result.ok
    assert "cheap_tier_rework_rate=NO_BASELINE" in result.detail
    assert result.evidence["metrics"]["cheap_tier_rework_rate"]["value"] is None
    assert result.evidence["metrics"]["cheap_tier_rework_rate"]["baseline"] is None


def test_missing_forge_binary_raises(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setattr(cost_budget_audit, "resolve_forge_binary", lambda root: None)
    with pytest.raises(cost_budget_audit.CostBudgetAuditError, match="no forge binary"):
        cost_budget_audit.phase(tmp_path)


def test_malformed_json_raises(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setattr(
        cost_budget_audit,
        "run_forge_ledger_audit",
        lambda **kwargs: subprocess.CompletedProcess(
            args=["forge"], returncode=0, stdout="not json", stderr=""
        ),
    )
    with pytest.raises(cost_budget_audit.CostBudgetAuditError, match="no JSON"):
        cost_budget_audit.phase(tmp_path)


def test_run_forge_ledger_audit_builds_the_expected_command(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    captured: dict = {}

    def _fake_run(command, **kwargs):
        captured["command"] = command
        return _completed(_payload("PASS", "PASS"))

    monkeypatch.setattr(subprocess, "run", _fake_run)
    cost_budget_audit.run_forge_ledger_audit(
        forge_binary=Path("/usr/local/bin/forge"),
        baseline=tmp_path / "baseline.json",
        ledger_root=tmp_path / "ledger",
        window_days=14,
        record=True,
    )
    command = captured["command"]
    assert command[:3] == ["/usr/local/bin/forge", "ledger", "audit"]
    assert "--record" in command
    assert "--window-days" in command
    assert str(14) in command
    assert "--ledger-root" in command


def test_default_baseline_path_is_ledger_relative(tmp_path: Path) -> None:
    assert cost_budget_audit.default_baseline_path(tmp_path) == (
        tmp_path / "ledger" / "cost_budget_baseline.json"
    )
