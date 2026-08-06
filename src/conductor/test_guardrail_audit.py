from __future__ import annotations

import subprocess

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
