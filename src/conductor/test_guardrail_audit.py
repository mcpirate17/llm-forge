from __future__ import annotations

import subprocess
from pathlib import Path

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
    def timeout(*args, **kwargs):
        raise subprocess.TimeoutExpired(args[0], timeout=7, output=b"partial")

    monkeypatch.setattr(guardrail_audit.subprocess, "run", timeout)

    returncode, output = guardrail_audit._run_tool(
        ["pylint", "research"], timeout_seconds=7
    )

    assert returncode == 124
    assert "timed out" in output
    assert "partial" in output


def _stub_external_tool_results(
    monkeypatch, results: tuple[tuple[int, str], ...]
) -> None:
    result_iter = iter(results)
    monkeypatch.setattr(guardrail_audit, "_iter_files", lambda *args, **kwargs: [])
    monkeypatch.setattr(
        guardrail_audit,
        "_resolve_tool_command",
        lambda tool, *args: [tool, *args],
    )
    monkeypatch.setattr(
        guardrail_audit,
        "_run_tool",
        lambda *args, **kwargs: next(result_iter),
    )


def test_incomplete_external_tools_fail_closed(monkeypatch) -> None:
    _stub_external_tool_results(
        monkeypatch,
        (
            (127, "missing tool: vulture"),
            (124, "timed out: pylint research"),
        ),
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
    _stub_external_tool_results(
        monkeypatch,
        (
            (3, "research/example.py:1: unused function 'old' (90% confidence)"),
            (8, "R0801: Similar lines in 2 files (duplicate-code)"),
        ),
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
    monkeypatch.setattr(
        guardrail_audit,
        "resolve_audit_root",
        lambda _explicit_root: guardrail_audit.ROOT,
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


def _run_git(repo: Path, *args: str) -> None:
    completed = subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True, text=True, check=False
    )
    assert completed.returncode == 0, completed.stderr


def _new_repo(parent: Path, name: str) -> Path:
    repo = parent / name
    repo.mkdir(parents=True, exist_ok=True)
    commands = (
        ("init", "-b", "main"),
        ("config", "user.email", "guardrail-tests@example.invalid"),
        ("config", "user.name", "Guardrail Tests"),
        ("config", "commit.gpgsign", "false"),
    )
    for command in commands:
        _run_git(repo, *command)
    return repo


def _oversized_source(repo: Path, relative: str) -> None:
    assignments = "\n".join(f"    value = {number}" for number in range(105))
    path = repo / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f"def big():\n{assignments}\n    return value\n", encoding="utf-8")


def test_explicit_root_scans_the_named_repo_not_cwd(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    target = _new_repo(tmp_path, "target")
    decoy = _new_repo(tmp_path, "decoy")
    _oversized_source(target, "research/candidate.py")
    _run_git(target, "add", "--all")
    _run_git(target, "commit", "-m", "base")

    monkeypatch.chdir(decoy)
    assert guardrail_audit.resolve_audit_root(target) == target.resolve()
    assert guardrail_audit.main(["--root", str(target), "--check"]) == 1


def test_default_root_uses_cwd_toplevel_not_module_location(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The bug: Path(__file__)-derived resolution would always point at the
    checkout that supplied the imported module, so a god_function only
    present in this throwaway repo would be invisible. Finding it proves the
    tool followed cwd, not its own install location.
    """
    repo = _new_repo(tmp_path, "repo")
    _oversized_source(repo, "research/candidate.py")
    _run_git(repo, "add", "--all")
    _run_git(repo, "commit", "-m", "base")

    monkeypatch.chdir(repo)
    assert guardrail_audit.resolve_audit_root(None) == repo.resolve()
    assert guardrail_audit.main(["--check"]) == 1
    assert guardrail_audit.ROOT == repo.resolve()


def test_cwd_outside_worktree_refuses_rather_than_falling_back(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    outside = tmp_path / "not_a_repo"
    outside.mkdir()
    monkeypatch.chdir(outside)

    assert guardrail_audit.main([]) == 2


def test_resolved_root_is_printed(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    repo = _new_repo(tmp_path, "repo")
    _run_git(repo, "commit", "--allow-empty", "-m", "base")

    guardrail_audit.print_audit_provenance("guardrail-audit", repo, cwd=repo)
    out = capsys.readouterr().out
    assert f"root={repo.resolve()}" in out


def test_root_mismatch_warns(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    target = _new_repo(tmp_path, "target")
    decoy = _new_repo(tmp_path, "decoy")
    _run_git(target, "commit", "--allow-empty", "-m", "base")

    guardrail_audit.print_audit_provenance("guardrail-audit", target, cwd=decoy)
    err = capsys.readouterr().err
    assert "WARNING" in err
    assert str(target.resolve()) in err
