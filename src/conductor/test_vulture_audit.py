"""Focused fail-closed tests for the Vulture baseline adapter."""

from __future__ import annotations

import json
import subprocess
from datetime import date, timedelta
from pathlib import Path

import pytest

from conductor.candidate_review.vulture_audit import (
    VultureAuditError,
    _iso_date,
    _key,
    _load_baseline,
    _parse_output,
    _validate_entry,
    main,
    run_audit,
)


def _baseline(entries: dict[str, dict[str, str]]) -> dict[str, object]:
    return {
        "schema_version": 1,
        "generated_from_tree": [str(number) * 8 for number in range(5)],
        "expires": (date.today() + timedelta(days=30)).isoformat(),
        "count": len(entries),
        "entries": entries,
    }


def _entry(path: str, message: str) -> tuple[str, dict[str, str]]:
    key = _key(path, message)
    return key, {
        "path": path,
        "message": message,
        "owner": "governance-test",
        "justification": "Exact test-only debt with a bounded expiration date.",
        "expires": (date.today() + timedelta(days=30)).isoformat(),
    }


def test_vulture_baseline_rejects_count_and_key_mismatch(tmp_path: Path) -> None:
    key, entry = _entry("sample.py", "unused variable 'value' (100% confidence)")
    path = tmp_path / "baseline.json"
    payload = _baseline({key: entry})
    payload["count"] = 2
    path.write_text(json.dumps(payload), encoding="utf-8")
    with pytest.raises(VultureAuditError, match="count"):
        _load_baseline(path)

    payload["count"] = 1
    payload["entries"] = {"wrong-key": entry}
    path.write_text(json.dumps(payload), encoding="utf-8")
    with pytest.raises(VultureAuditError, match="key mismatch"):
        _load_baseline(path)


def test_vulture_audit_blocks_new_real_finding(tmp_path: Path) -> None:
    source = tmp_path / "candidate.py"
    source.write_text(
        "def live():\n    if False:\n        return 1\n    return 2\n",
        encoding="utf-8",
    )
    baseline = tmp_path / "baseline.json"
    baseline.write_text(json.dumps(_baseline({})), encoding="utf-8")

    assert run_audit(baseline, [str(source)]) == 1


def test_vulture_audit_rejects_resolved_stale_entry(tmp_path: Path) -> None:
    source = tmp_path / "candidate.py"
    source.write_text("VALUE = 1\n", encoding="utf-8")
    message = "unused variable 'removed' (100% confidence)"
    key, entry = _entry(str(source), message)
    baseline = tmp_path / "baseline.json"
    baseline.write_text(json.dumps(_baseline({key: entry})), encoding="utf-8")

    with pytest.raises(VultureAuditError, match="resolved findings"):
        run_audit(baseline, [str(source)])


def test_vulture_rejects_unbound_tree_and_untrusted_analyzer(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    baseline = tmp_path / "baseline.json"
    payload = _baseline({})
    payload["generated_from_tree"] = ["too-short"]
    baseline.write_text(json.dumps(payload), encoding="utf-8")
    with pytest.raises(VultureAuditError, match="Git OID chunks"):
        _load_baseline(baseline)

    baseline.write_text(json.dumps(_baseline({})), encoding="utf-8")
    monkeypatch.setattr(
        subprocess,
        "run",
        lambda *args, **kwargs: subprocess.CompletedProcess(
            args[0], 9, stdout="", stderr="analyzer crashed"
        ),
    )
    with pytest.raises(VultureAuditError, match="findings are untrusted"):
        run_audit(baseline, [str(tmp_path / "source.py")])


def test_vulture_rejects_unrecognized_success_output(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    baseline = tmp_path / "baseline.json"
    baseline.write_text(json.dumps(_baseline({})), encoding="utf-8")
    monkeypatch.setattr(
        subprocess,
        "run",
        lambda *args, **kwargs: subprocess.CompletedProcess(
            args[0], 3, stdout="not a finding\n", stderr=""
        ),
    )
    with pytest.raises(VultureAuditError, match="unrecognized Vulture output"):
        run_audit(baseline, [str(tmp_path / "source.py")])


def test_vulture_baseline_schema_and_entry_failures_are_blocking(
    tmp_path: Path,
) -> None:
    path = tmp_path / "baseline.json"
    malformed_payloads = [
        {},
        {**_baseline({}), "schema_version": 2},
        {
            **_baseline({}),
            "expires": (date.today() - timedelta(days=1)).isoformat(),
        },
    ]
    for payload in malformed_payloads:
        path.write_text(json.dumps(payload), encoding="utf-8")
        with pytest.raises(VultureAuditError):
            _load_baseline(path)
    with pytest.raises(VultureAuditError, match="unreadable"):
        _load_baseline(tmp_path / "missing.json")
    with pytest.raises(VultureAuditError, match="ISO date"):
        _iso_date(None, field="probe")
    with pytest.raises(VultureAuditError, match="ISO date"):
        _iso_date("not-a-date", field="probe")
    assert _parse_output("\n") == {}

    key, valid = _entry("sample.py", "unused variable 'value' (100% confidence)")
    invalid_entries: list[object] = [
        {},
        {**valid, "path": ""},
        {**valid, "message": ""},
        {**valid, "owner": ""},
        {**valid, "justification": "short"},
        {
            **valid,
            "expires": (date.today() - timedelta(days=1)).isoformat(),
        },
    ]
    for entry in invalid_entries:
        with pytest.raises(VultureAuditError):
            _validate_entry(key, entry)


def test_vulture_main_returns_audit_error_for_missing_baseline(tmp_path: Path) -> None:
    assert main(["--baseline", str(tmp_path / "missing.json"), "source.py"]) == 2


def _stub_vulture_output(monkeypatch: pytest.MonkeyPatch, stdout: str) -> None:
    monkeypatch.setattr(
        subprocess,
        "run",
        lambda *args, **kwargs: subprocess.CompletedProcess(
            args[0], 0, stdout=stdout, stderr=""
        ),
    )


def test_vulture_changed_file_caused_blocks(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    baseline = tmp_path / "baseline.json"
    baseline.write_text(json.dumps(_baseline({})), encoding="utf-8")
    _stub_vulture_output(
        monkeypatch,
        "changed.py:5: unused variable 'value' (100% confidence)\n",
    )

    exit_code = run_audit(
        baseline, ["changed.py"], changed_files=frozenset({"changed.py"})
    )

    assert exit_code == 1


def test_vulture_inherited_finding_does_not_block(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    baseline = tmp_path / "baseline.json"
    baseline.write_text(json.dumps(_baseline({})), encoding="utf-8")
    _stub_vulture_output(
        monkeypatch,
        "untouched.py:5: unused variable 'value' (100% confidence)\n",
    )

    exit_code = run_audit(
        baseline, ["untouched.py"], changed_files=frozenset({"changed.py"})
    )

    assert exit_code == 0


def test_vulture_no_changed_files_blocks_every_new_finding(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    baseline = tmp_path / "baseline.json"
    baseline.write_text(json.dumps(_baseline({})), encoding="utf-8")
    _stub_vulture_output(
        monkeypatch,
        "untouched.py:5: unused variable 'value' (100% confidence)\n",
    )

    exit_code = run_audit(baseline, ["untouched.py"], changed_files=None)

    assert exit_code == 1


def test_vulture_no_new_findings_exits_zero_either_way(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    message = "unused variable 'value' (100% confidence)"
    key, entry = _entry("known.py", message)
    baseline = tmp_path / "baseline.json"
    baseline.write_text(json.dumps(_baseline({key: entry})), encoding="utf-8")
    _stub_vulture_output(monkeypatch, f"known.py:5: {message}\n")

    assert run_audit(baseline, ["known.py"], changed_files=None) == 0
    assert run_audit(baseline, ["known.py"], changed_files=frozenset({"known.py"})) == 0


def test_vulture_changed_baseline_only_file_not_reported_as_caused(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A --changed-file naming a *baselined* (non-new) finding's file must not
    spuriously attribute an unrelated NEW finding on a different file."""
    known_message = "unused variable 'known' (100% confidence)"
    key, entry = _entry("baselined.py", known_message)
    baseline = tmp_path / "baseline.json"
    baseline.write_text(json.dumps(_baseline({key: entry})), encoding="utf-8")
    _stub_vulture_output(
        monkeypatch,
        f"baselined.py:5: {known_message}\n"
        "unrelated.py:9: unused variable 'other' (100% confidence)\n",
    )

    exit_code = run_audit(
        baseline,
        ["baselined.py", "unrelated.py"],
        # "baselined.py" is changed, but its only finding is already in the
        # baseline (not new); it must not cause unrelated.py's NEW finding.
        changed_files=frozenset({"baselined.py"}),
    )

    assert exit_code == 0


def test_run_audit_reports_a_missing_vulture_as_an_audit_error(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """An absent `vulture` binary is an audit error, not a raw OSError.

    run_audit shells out to `vulture`, so before 2026-08-30 a missing tool escaped
    as FileNotFoundError from Popen -- an exception this module never documents and
    no caller catches. It surfaced as two tests failing with a traceback through
    subprocess._execute_child, which reads as a logic defect rather than an absent
    dependency, and that is exactly how it was misread. The tool being missing must
    fail loud in this module's own currency, and say how to fix it.
    """
    monkeypatch.setattr(
        "conductor.candidate_review.vulture_audit.shutil.which", lambda _name: None
    )
    baseline = tmp_path / "baseline.json"
    baseline.write_text(json.dumps(_baseline({})), encoding="utf-8")
    source = tmp_path / "candidate.py"
    source.write_text("def live():\n    return 1\n", encoding="utf-8")

    with pytest.raises(VultureAuditError, match="not installed or not on PATH"):
        run_audit(baseline, [str(source)])
