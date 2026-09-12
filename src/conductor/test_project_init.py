"""``conductor init``: merge by key, refuse conflicts, idempotent, doctor fails loud."""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
import tomllib
from datetime import date
from pathlib import Path

import pytest

from conductor import project_init as pi
from conductor.candidate_review.policy import load_policy
from tooling.hooks.dispatch.registry import EVENTS, settings_block

TEMPLATE_HOOKS = settings_block()["hooks"]


def _repo(tmp_path: Path) -> Path:
    project = tmp_path / "proj"
    project.mkdir()
    (project / ".git").mkdir()
    return project


def _config(project: Path, **kw: object) -> pi.InitConfig:
    return pi.InitConfig(project_dir=project, python=Path(sys.executable), **kw)


@pytest.fixture
def quiet_doctor(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(pi, "run_doctor", lambda config: 0)
    monkeypatch.setattr(pi, "_crg_importable", lambda python: True)


def _write(project: Path, rel: str, text: str) -> None:
    path = project / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


# ── settings merge ──────────────────────────────────────────────────────────


def test_settings_merge_keeps_non_hook_keys_and_foreign_events() -> None:
    existing = {
        "permissions": {"allow": ["Bash(ls:*)"]},
        "env": {"FOO": "1"},
        "hooks": {"Notification": [{"hooks": [{"type": "command", "command": "x"}]}]},
    }
    merged = json.loads(pi.render_settings(json.dumps(existing), force=False))
    assert merged["permissions"] == existing["permissions"]
    assert merged["env"] == existing["env"]
    assert merged["hooks"]["Notification"] == existing["hooks"]["Notification"]
    for event in EVENTS:
        assert merged["hooks"][event] == TEMPLATE_HOOKS[event]
    assert (
        pi.render_settings(None, force=False)
        == json.dumps({"hooks": TEMPLATE_HOOKS}, indent=2) + "\n"
    )


def test_settings_conflict_refused_without_force_replaced_with_force() -> None:
    existing = {
        "model": "opus",
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "legacy.sh"}],
                }
            ]
        },
    }
    text = json.dumps(existing)
    with pytest.raises(pi.InitError, match=r"hooks\.PreToolUse.*--force"):
        pi.render_settings(text, force=False)
    forced = json.loads(pi.render_settings(text, force=True))
    assert forced["model"] == "opus"
    assert forced["hooks"] == TEMPLATE_HOOKS


def test_settings_identical_event_is_not_a_conflict() -> None:
    existing = {"hooks": {"PreToolUse": TEMPLATE_HOOKS["PreToolUse"]}, "k": 1}
    merged = json.loads(pi.render_settings(json.dumps(existing), force=False))
    assert merged == {"hooks": TEMPLATE_HOOKS, "k": 1}


def test_settings_malformed_json_refused() -> None:
    with pytest.raises(pi.InitError, match="not JSON"):
        pi.render_settings("{not json", force=True)
    with pytest.raises(pi.InitError, match="JSON object"):
        pi.render_settings("[]", force=True)


def test_an_already_wired_settings_file_is_returned_byte_for_byte() -> None:
    # A foreign project formats its own files. Wiring in nothing must cost the
    # owner nothing -- not an indent change, not a reordered key, not a newline.
    existing = (
        '{\n\t"model": "opus",\n\t"hooks": ' + json.dumps(TEMPLATE_HOOKS) + "\n}\t\n\n"
    )
    assert pi.render_settings(existing, force=False) == existing
    assert pi.render_settings(existing, force=True) == existing


def test_wiring_in_a_hook_does_not_escape_the_rest_of_the_file() -> None:
    existing = {"env": {"GREETING": "café → ☕"}}
    rendered = pi.render_settings(json.dumps(existing, ensure_ascii=False), force=False)
    assert "café → ☕" in rendered
    assert json.loads(rendered)["env"] == existing["env"]


def test_a_partially_wired_settings_file_is_still_rewritten() -> None:
    existing = json.dumps({"hooks": {"PreToolUse": TEMPLATE_HOOKS["PreToolUse"]}})
    rendered = pi.render_settings(existing, force=False)
    assert rendered != existing
    assert json.loads(rendered)["hooks"] == TEMPLATE_HOOKS


def test_an_absent_settings_file_is_created_not_preserved() -> None:
    assert pi.render_settings(None, force=False) != ""
    assert json.loads(pi.render_settings(None, force=False)) == {
        "hooks": TEMPLATE_HOOKS
    }


# ── mcp merge ───────────────────────────────────────────────────────────────


def test_mcp_merge_keeps_other_servers_and_refuses_conflict(tmp_path: Path) -> None:
    project = _repo(tmp_path)
    config = _config(project)
    existing = {
        "mcpServers": {"other": {"command": "x"}, pi.MCP_SERVER: {"command": "stale"}},
        "extra": True,
    }
    with pytest.raises(pi.InitError, match=r"mcpServers\.code-review-graph"):
        pi.render_mcp(json.dumps(existing), config)
    merged = json.loads(
        pi.render_mcp(json.dumps(existing), _config(project, force=True))
    )
    assert merged["extra"] is True
    assert merged["mcpServers"]["other"] == {"command": "x"}
    entry = merged["mcpServers"][pi.MCP_SERVER]
    assert entry["args"] == ["-m", "conductor.crg_server", "--repo", str(project)]
    assert entry["command"] == sys.executable
    assert entry["env"] == {"CRG_ROLE": "review"}


def test_an_already_wired_mcp_file_is_returned_byte_for_byte(tmp_path: Path) -> None:
    project = _repo(tmp_path)
    config = _config(project)
    existing = (
        '{"mcpServers": {"'
        + pi.MCP_SERVER
        + '": '
        + json.dumps(pi.mcp_entry(project, config.python))
        + "}}"
    )
    assert pi.render_mcp(existing, config) == existing
    assert pi.render_mcp(existing, _config(project, force=True)) == existing


# ── gitignore block ─────────────────────────────────────────────────────────


def test_gitignore_block_appended_once_and_replaced_when_stale() -> None:
    first = pi.render_gitignore("*.pyc\n")
    assert first.startswith("*.pyc\n\n" + pi.MARK_BEGIN)
    assert first.count(pi.MARK_BEGIN) == 1
    assert pi.render_gitignore(first) == first
    stale = first.replace(".mcp.json\n", "") + "build/\n"
    rewritten = pi.render_gitignore(stale)
    assert rewritten.count(pi.MARK_BEGIN) == 1
    assert ".mcp.json\n" in rewritten
    assert rewritten.endswith(pi.MARK_END + "\nbuild/\n")
    assert pi.render_gitignore(None).count(pi.MARK_END) == 1


# ── policy template ─────────────────────────────────────────────────────────


def test_policy_template_is_loadable(tmp_path: Path) -> None:
    path = tmp_path / "candidate_policy.toml"
    path.write_text(pi.render_policy(date.today()), encoding="utf-8")
    policy = load_policy(path)
    assert policy.block_at == "high"
    assert {c.check_id for c in policy.checks} == {
        "candidate-integrity",
        "config-parse",
        "python-ast",
    }


# ── plan / apply / idempotency ──────────────────────────────────────────────


@pytest.mark.usefixtures("quiet_doctor")
def test_apply_then_second_run_is_idempotent(tmp_path: Path) -> None:
    project = _repo(tmp_path)
    _write(project, ".gitignore", "node_modules/\n")
    assert pi.run(_config(project)) == 0
    settings = json.loads((project / pi.SETTINGS).read_text())
    assert settings["hooks"] == TEMPLATE_HOOKS
    launcher = project / pi.LAUNCHER
    assert launcher.stat().st_mode & stat.S_IXUSR
    assert launcher.read_text().startswith(f"#!{sys.executable}\n")
    assert (project / pi.REGISTRY).is_file()
    for keep in pi.GITKEEPS:
        assert (project / keep).is_file()
    second = pi.plan(_config(project))
    assert second.changed == []
    assert {a.status for a in second.actions} == {"unchanged"}
    assert pi.run(_config(project, check=True)) == 0
    assert (project / ".gitignore").read_text().count(pi.MARK_BEGIN) == 1


@pytest.mark.usefixtures("quiet_doctor")
def test_project_owned_files_are_never_rewritten(tmp_path: Path) -> None:
    project = _repo(tmp_path)
    assert pi.run(_config(project)) == 0
    for rel in (pi.POLICY, pi.PREAUTH, pi.REGISTRY):
        _write(project, rel, "owner edit\n")
    assert pi.run(_config(project, force=True)) == 0
    for rel in (pi.POLICY, pi.PREAUTH, pi.REGISTRY):
        assert (project / rel).read_text() == "owner edit\n"


@pytest.mark.usefixtures("quiet_doctor")
def test_dry_run_writes_nothing_and_prints_diff(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    project = _repo(tmp_path)
    assert pi.run(_config(project, dry_run=True)) == 0
    assert not (project / ".claude").exists()
    assert not (project / "conductor").exists()
    out = capsys.readouterr().out
    assert f"+++ b/{pi.SETTINGS}" in out
    assert "create    .claude/hooks/dispatch.py" in out


@pytest.mark.usefixtures("quiet_doctor")
def test_check_reports_drift(tmp_path: Path) -> None:
    project = _repo(tmp_path)
    assert pi.run(_config(project, check=True)) == 1
    assert not (project / pi.SETTINGS).exists()
    assert pi.run(_config(project)) == 0
    assert pi.run(_config(project, check=True)) == 0
    _write(project, pi.SETTINGS, json.dumps({"hooks": {}}))
    assert pi.run(_config(project, check=True)) == 1


@pytest.mark.usefixtures("quiet_doctor")
def test_conflict_refuses_before_any_write(tmp_path: Path) -> None:
    project = _repo(tmp_path)
    _write(
        project,
        pi.SETTINGS,
        json.dumps({"hooks": {"SessionStart": [{"hooks": [{"command": "z"}]}]}}),
    )
    with pytest.raises(pi.InitError, match="SessionStart"):
        pi.run(_config(project))
    assert not (project / pi.LAUNCHER).exists()
    assert pi.run(_config(project, force=True)) == 0


def test_dead_hook_fails_loud(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    project = _repo(tmp_path)
    monkeypatch.setattr(pi, "_crg_importable", lambda python: True)
    monkeypatch.setattr(pi, "run_doctor", lambda config: 1)
    with pytest.raises(pi.InitError, match="dead hook"):
        pi.run(_config(project))
    assert (project / pi.SETTINGS).is_file()
    assert pi.run(_config(project, check=True)) == 1


@pytest.mark.usefixtures("quiet_doctor")
def test_not_a_git_repo_refused(tmp_path: Path) -> None:
    with pytest.raises(pi.InitError, match="git repository"):
        pi.run(_config(tmp_path))


def test_missing_crg_is_a_warning_not_a_refusal(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    project = _repo(tmp_path)
    monkeypatch.setattr(pi, "run_doctor", lambda config: 0)
    monkeypatch.setattr(pi, "_crg_importable", lambda python: False)
    assert pi.run(_config(project)) == 0
    assert "code_review_graph is not importable" in capsys.readouterr().err


def test_cli_parses_and_refuses_exclusive_flags(tmp_path: Path) -> None:
    config = pi.parse_args([str(tmp_path), "--force", "--dry-run"])
    assert config.project_dir == tmp_path.resolve()
    assert config.force and config.dry_run and not config.check
    # a venv interpreter is a symlink out of the venv: keep the path, never resolve it
    venv_python = tmp_path / ".venv" / "bin" / "python"
    venv_python.parent.mkdir(parents=True)
    venv_python.symlink_to(sys.executable)
    assert (
        pi.parse_args([str(tmp_path), "--python", str(venv_python)]).python
        == venv_python
    )
    with pytest.raises(SystemExit):
        pi.parse_args([str(tmp_path), "--dry-run", "--check"])


def test_main_reports_refusal_as_exit_2(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    from conductor.__main__ import main

    assert main(["init", str(tmp_path), "--dry-run"]) == 2
    assert "REFUSED" in capsys.readouterr().err
    assert main(["bogus"]) == 2


# ── the real doctor and dispatcher, end to end ──────────────────────────────


def test_init_runs_doctor_and_dispatcher_denies_force_push(tmp_path: Path) -> None:
    project = _repo(tmp_path)
    subprocess.run(["git", "init", "-q", str(project)], check=True, timeout=30)
    assert pi.run(_config(project)) == 0
    env = {k: v for k, v in os.environ.items() if k != "PYTHONPATH"}
    env["PYTHONPATH"] = str(pi.TOOLING_ROOT)
    env["CLAUDE_PROJECT_DIR"] = str(project)
    env["CRG_SKIP_EMBED"] = "1"
    payload = {
        "session_id": "init-test",
        "cwd": str(project),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": "git push --force origin master"},
    }
    proc = subprocess.run(
        [str(project / pi.LAUNCHER), "PreToolUse"],
        input=json.dumps(payload),
        capture_output=True,
        text=True,
        cwd=project,
        env=env,
        timeout=60,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    out = json.loads(proc.stdout)
    assert out["hookSpecificOutput"]["permissionDecision"] == "deny", out


# ── weekly-audit.yml / env.sh templates ──────────────────────────────────────


@pytest.mark.usefixtures("quiet_doctor")
def test_workflow_and_env_stub_are_written_once(tmp_path: Path) -> None:
    project = _repo(tmp_path)
    assert pi.run(_config(project)) == 0
    workflow = project / pi.WORKFLOW
    env_stub = project / pi.ENV_STUB
    assert workflow.read_text() == pi.WORKFLOW_TEXT
    assert env_stub.read_text() == pi.ENV_STUB_TEXT
    assert "conductor.guardrail_audit" in workflow.read_text()
    assert "conductor.radon_complexity" in workflow.read_text()
    assert "BASH_QUIET_SAVE_DIR" in env_stub.read_text()

    second = pi.plan(_config(project))
    assert second.changed == []

    for rel in (pi.WORKFLOW, pi.ENV_STUB):
        _write(project, rel, "owner edit\n")
    assert pi.run(_config(project, force=True)) == 0
    for rel in (pi.WORKFLOW, pi.ENV_STUB):
        assert (project / rel).read_text() == "owner edit\n"


# ── [tool.conductor] stanza warning ──────────────────────────────────────────


def test_pyproject_warning_when_manifest_absent(tmp_path: Path) -> None:
    project = _repo(tmp_path)
    warnings = pi._pyproject_conductor_warnings(project)
    assert len(warnings) == 1
    assert "pyproject.toml does not exist" in warnings[0]


def test_pyproject_warning_lists_missing_keys(tmp_path: Path) -> None:
    project = _repo(tmp_path)
    _write(project, "pyproject.toml", '[tool.conductor]\ncandidate_policy = "x.toml"\n')
    warnings = pi._pyproject_conductor_warnings(project)
    assert len(warnings) == 1
    assert "mutation_registry" in warnings[0]
    assert "package_root" in warnings[0]
    assert "candidate_policy" not in warnings[0].split("missing", 1)[1]


def test_pyproject_no_warning_when_stanza_is_complete(tmp_path: Path) -> None:
    project = _repo(tmp_path)
    _write(
        project,
        "pyproject.toml",
        "[tool.conductor]\n"
        'candidate_policy = "candidate_policy.toml"\n'
        'mutation_registry = "campaigns/registry.json"\n'
        'package_root = "src/conductor"\n',
    )
    assert pi._pyproject_conductor_warnings(project) == []


def test_pyproject_warning_on_malformed_toml(tmp_path: Path) -> None:
    project = _repo(tmp_path)
    _write(project, "pyproject.toml", "[tool.conductor\n")
    warnings = pi._pyproject_conductor_warnings(project)
    assert len(warnings) == 1
    assert "could not be parsed" in warnings[0]


def test_pyproject_warning_reaches_the_plan_without_writing_pyproject(
    tmp_path: Path, quiet_doctor: None
) -> None:
    project = _repo(tmp_path)
    plan_ = pi.plan(_config(project))
    assert any("pyproject.toml does not exist" in w for w in plan_.warnings)
    assert not (project / "pyproject.toml").exists()


def test_conductor_stanza_keys_round_trip_tomllib() -> None:
    # Sanity: the constant this module warns about really is what tomllib parses
    # a [tool.conductor] table into, not a typo that would never match.
    parsed = tomllib.loads(
        '[tool.conductor]\ncandidate_policy = "a"\nmutation_registry = "b"\n'
        'package_root = "c"\n'
    )
    assert set(pi.CONDUCTOR_STANZA_KEYS) <= set(parsed["tool"]["conductor"])
