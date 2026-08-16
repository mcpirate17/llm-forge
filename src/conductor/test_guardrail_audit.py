from __future__ import annotations

import subprocess

import pytest

from conductor import guardrail_audit


def test_iter_files_accepts_explicit_file_target() -> None:
    files = guardrail_audit._iter_files(
        ["research/tools/vault_health.py"], staged_only=False
    )

    assert [path.name for path in files] == ["vault_health.py"]


def test_resolve_tool_command_prefers_running_environment(
    tmp_path, monkeypatch
) -> None:
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    python = bin_dir / "python"
    python.touch(mode=0o755)
    tool = bin_dir / "vulture"
    tool.touch(mode=0o755)
    monkeypatch.setattr(guardrail_audit.sys, "executable", str(python))

    command = guardrail_audit._resolve_tool_command("vulture", "research")

    assert command == [str(tool), "research"]


def test_run_tool_reports_timeout_without_raising(monkeypatch) -> None:
    def timeout(*args, **kwargs):  # noqa: ANN002, ANN003, ARG001
        raise subprocess.TimeoutExpired(args[0], timeout=7, output=b"partial")

    monkeypatch.setattr(guardrail_audit.subprocess, "run", timeout)

    returncode, output = guardrail_audit._run_tool(
        ["pylint", "research"], timeout_seconds=7
    )

    assert returncode == 124
    assert "timed out" in output
    assert "partial" in output


def test_incomplete_external_tools_fail_closed(monkeypatch) -> None:
    results = iter(
        (
            (127, "missing tool: vulture"),
            (124, "timed out: pylint research"),
        )
    )
    monkeypatch.setattr(guardrail_audit, "_iter_files", lambda *args, **kwargs: [])
    monkeypatch.setattr(
        guardrail_audit,
        "_resolve_tool_command",
        lambda tool, *args: [tool, *args],
    )
    monkeypatch.setattr(
        guardrail_audit,
        "_run_tool",
        lambda *args, **kwargs: next(results),
    )

    issues, summary = guardrail_audit.collect_issues(("research",))
    report = guardrail_audit.build_markdown_report(issues, summary)

    incomplete = [issue for issue in issues if issue.kind == "audit_incomplete"]
    assert len(incomplete) == 2
    assert all(issue.severity == "critical" for issue in incomplete)
    assert summary["audit_complete"] is False
    assert summary["dead_code_hits"] == 0
    assert summary["duplicate_hits"] == 0
    assert "external tool audit complete: False" in report


def test_expected_tool_finding_exit_codes_are_complete(monkeypatch) -> None:
    results = iter(
        (
            (3, "research/example.py:1: unused function 'old' (90% confidence)"),
            (8, "R0801: Similar lines in 2 files (duplicate-code)"),
        )
    )
    monkeypatch.setattr(guardrail_audit, "_iter_files", lambda *args, **kwargs: [])
    monkeypatch.setattr(
        guardrail_audit,
        "_resolve_tool_command",
        lambda tool, *args: [tool, *args],
    )
    monkeypatch.setattr(
        guardrail_audit,
        "_run_tool",
        lambda *args, **kwargs: next(results),
    )

    issues, summary = guardrail_audit.collect_issues(("research",))

    assert summary["audit_complete"] is True
    assert summary["dead_code_hits"] == 1
    assert summary["duplicate_hits"] == 1
    assert {issue.kind for issue in issues} == {"dead_code", "duplicate_code"}


def test_check_mode_blocks_high_severity_findings(monkeypatch) -> None:
    issue = guardrail_audit.Issue(
        kind="complexity",
        severity="high",
        path="research/example.py",
        symbol="example",
        message="Function complexity is high.",
        recommendation="Extract a helper.",
        metric={},
    )
    summary = {
        "files_scanned": 1,
        "python_files_scanned": 1,
        "dead_code_hits": 0,
        "duplicate_hits": 0,
        "audit_complete": True,
        "tool_failures": [],
    }
    monkeypatch.setattr(
        guardrail_audit,
        "collect_issues",
        lambda *args, **kwargs: ([issue], summary),
    )

    assert guardrail_audit.main(["--check"]) == 1


def test_ref_selection_and_structural_parse_fail_closed(tmp_path, monkeypatch) -> None:
    with pytest.raises(ValueError, match="select exactly one"):
        guardrail_audit._git_changed_paths(
            ("conductor",), staged_only=False, from_ref=None
        )
    with pytest.raises(ValueError, match="mutually exclusive"):
        guardrail_audit.collect_issues(
            ("conductor",), staged_only=True, from_ref="HEAD^"
        )

    monkeypatch.setattr(guardrail_audit, "ROOT", tmp_path)
    text_path = tmp_path / "probe.txt"
    text_path.write_text("not Python\n", encoding="utf-8")
    python_path = tmp_path / "probe.py"
    python_path.write_text("def broken(:\n", encoding="utf-8")
    issues, python_count = guardrail_audit._structural_issues(
        [text_path, python_path], staged_only=False, from_ref=None
    )
    assert python_count == 1
    assert [issue.kind for issue in issues] == ["syntax_error"]


def test_candidate_text_has_deterministic_latin1_fallback(
    tmp_path, monkeypatch
) -> None:
    monkeypatch.setattr(guardrail_audit, "ROOT", tmp_path)
    path = tmp_path / "probe.py"
    path.write_bytes(b"value = '\xff'\n")
    assert "ÿ" in guardrail_audit._read_candidate_text(
        path, staged_only=False, from_ref=None
    )
    monkeypatch.setattr(
        guardrail_audit.subprocess,
        "run",
        lambda *_args, **_kwargs: subprocess.CompletedProcess([], 0, stdout=b"\xff"),
    )
    assert (
        guardrail_audit._read_candidate_text(path, staged_only=True, from_ref=None)
        == "ÿ"
    )
