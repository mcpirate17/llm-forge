from __future__ import annotations

import json
from pathlib import Path

import pytest

from conductor import doctor
from tooling.hooks.dispatch import registry


def _healthy() -> dict[str, object]:
    payload: dict[str, object] = dict(registry.settings_block())
    payload["env"] = {"BASH_QUIET_LIMIT_BYTES": "8000"}
    payload["subagentPromptCacheTtl"] = "1h"
    return payload


def _write(root: Path, payload: dict[str, object]) -> Path:
    path = root / ".claude" / "settings.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    return path


def _run(
    project: Path,
    home: Path,
    *flags: str,
    payload: dict[str, object] | None = None,
    home_payload: dict[str, object] | None = None,
) -> int:
    if payload is not None:
        _write(project, payload)
    if home_payload is not None:
        _write(home, home_payload)
    return doctor.main(["--harness", "--project-dir", str(project), "--home", str(home), *flags])


def test_all_checks_pass_on_the_canonical_settings(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    exit_code = _run(
        tmp_path / "p", tmp_path / "u", payload=_healthy(), home_payload=_healthy()
    )
    assert exit_code == 0
    out = capsys.readouterr().out
    assert "harness-doctor | PASS fails=0" in out
    for check in ("subagentPromptCacheTtl", "hooks-dispatcher", "BASH_QUIET_LIMIT_BYTES", "model"):
        assert f"| {check} | PASS" in out


def test_short_prompt_cache_ttl_fails_and_fix_sets_one_hour(tmp_path: Path) -> None:
    project, home = tmp_path / "p", tmp_path / "u"
    payload = _healthy()
    payload["subagentPromptCacheTtl"] = "5m"
    assert _run(project, home, payload=payload, home_payload=_healthy()) == 1
    assert _run(project, home, "--fix", payload=None) == 0
    fixed = json.loads((project / ".claude" / "settings.json").read_text(encoding="utf-8"))
    assert fixed["subagentPromptCacheTtl"] == "1h"


def test_missing_prompt_cache_ttl_counts_as_a_failure(tmp_path: Path) -> None:
    payload = _healthy()
    del payload["subagentPromptCacheTtl"]
    assert _run(tmp_path / "p", tmp_path / "u", payload=payload, home_payload=_healthy()) == 1


def test_stray_hook_command_fails_and_fix_canonicalizes(tmp_path: Path) -> None:
    project, home = tmp_path / "p", tmp_path / "u"
    payload = _healthy()
    payload["hooks"]["PreToolUse"][0]["hooks"].append(
        {"type": "command", "command": "python3 .claude/hooks/my_hook.py", "timeout": 5}
    )
    assert _run(project, home, payload=payload, home_payload=_healthy()) == 1
    assert _run(project, home, "--fix") == 0
    fixed = json.loads((project / ".claude" / "settings.json").read_text(encoding="utf-8"))
    assert fixed["hooks"] == registry.settings_block()["hooks"]


def test_an_unwired_event_fails(tmp_path: Path) -> None:
    payload = _healthy()
    del payload["hooks"]["SessionEnd"]
    assert _run(tmp_path / "p", tmp_path / "u", payload=payload, home_payload=_healthy()) == 1


def test_the_future_forge_hook_command_is_accepted(tmp_path: Path) -> None:
    payload = _healthy()
    payload["hooks"]["PostToolUse"][0]["hooks"][0]["command"] = "forge hook PostToolUse"
    assert _run(tmp_path / "p", tmp_path / "u", payload=payload, home_payload=_healthy()) == 0


def test_unset_bash_quiet_limit_fails_and_fix_sets_the_default(tmp_path: Path) -> None:
    project, home = tmp_path / "p", tmp_path / "u"
    payload = _healthy()
    payload["env"] = {}
    assert _run(project, home, payload=payload, home_payload=_healthy()) == 1
    assert _run(project, home, "--fix") == 0
    fixed = json.loads((project / ".claude" / "settings.json").read_text(encoding="utf-8"))
    assert fixed["env"]["BASH_QUIET_LIMIT_BYTES"] == "8000"


@pytest.mark.parametrize("bad", ["zero", "0", "-1", ""])
def test_non_positive_limit_values_fail(bad: str, tmp_path: Path) -> None:
    payload = _healthy()
    payload["env"] = {"BASH_QUIET_LIMIT_BYTES": bad}
    assert _run(tmp_path / "p", tmp_path / "u", payload=payload, home_payload=_healthy()) == 1


def test_foreign_model_id_fails_and_fix_removes_the_key(tmp_path: Path) -> None:
    project, home = tmp_path / "p", tmp_path / "u"
    payload = _healthy()
    payload["model"] = "glm-5.3-flash"
    assert _run(project, home, payload=payload, home_payload=_healthy()) == 1
    assert _run(project, home, "--fix") == 0
    fixed = json.loads((project / ".claude" / "settings.json").read_text(encoding="utf-8"))
    assert "model" not in fixed


@pytest.mark.parametrize("good", ["claude-opus-5", "sonnet", "claude-fable-5-1"])
def test_known_model_ids_pass(good: str, tmp_path: Path) -> None:
    payload = _healthy()
    payload["model"] = good
    assert _run(tmp_path / "p", tmp_path / "u", payload=payload, home_payload=_healthy()) == 0


def test_a_non_string_model_value_in_user_settings_fails(tmp_path: Path) -> None:
    assert _run(tmp_path / "p", tmp_path / "u", payload=_healthy(), home_payload={"model": 7}) == 1


def test_a_model_with_a_thinking_budget_suffix_passes(tmp_path: Path) -> None:
    payload = _healthy()
    payload["model"] = "claude-fable-5-1[1m]"
    assert _run(tmp_path / "p", tmp_path / "u", payload=payload, home_payload=_healthy()) == 0


def test_user_settings_without_dispatcher_hooks_pass(tmp_path: Path) -> None:
    # A real user settings file: an empty hooks block (wiring is the project
    # file's job), a budget-suffixed model, the cost keys present.
    user = _healthy()
    user["hooks"] = {}
    user["model"] = "claude-fable-5-1[1m]"
    assert _run(tmp_path / "p", tmp_path / "u", payload=_healthy(), home_payload=user) == 0


def test_a_stray_command_in_user_settings_hook_events_fails(tmp_path: Path) -> None:
    stray = {
        "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "bash mine.sh"}]}]}
    }
    assert _run(tmp_path / "p", tmp_path / "u", payload=_healthy(), home_payload=stray) == 1


def test_missing_user_settings_reports_one_skip_not_a_failure(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert _run(tmp_path / "p", tmp_path / "u", payload=_healthy()) == 0
    out = capsys.readouterr().out
    assert "user | (file) | SKIP" in out


def test_missing_project_settings_fails_and_fix_writes_the_canonical_block(tmp_path: Path) -> None:
    project, home = tmp_path / "p", tmp_path / "u"
    _write(home, _healthy())
    assert doctor.main(["--harness", "--project-dir", str(project), "--home", str(home)]) == 1
    assert doctor.main(["--harness", "--project-dir", str(project), "--home", str(home), "--fix"]) == 0
    created = json.loads((project / ".claude" / "settings.json").read_text(encoding="utf-8"))
    assert created["hooks"] == registry.settings_block()["hooks"]
    assert created["subagentPromptCacheTtl"] == "1h"
    assert created["env"]["BASH_QUIET_LIMIT_BYTES"] == "8000"


def test_malformed_settings_json_fails_loud_with_exit_two(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    project = tmp_path / "p"
    path = project / ".claude" / "settings.json"
    path.parent.mkdir(parents=True)
    path.write_text("{ not json", encoding="utf-8")
    exit_code = doctor.main(
        ["--harness", "--project-dir", str(project), "--home", str(tmp_path / "u")]
    )
    assert exit_code == 2
    assert "not readable JSON" in capsys.readouterr().err


def test_fix_is_idempotent_a_second_run_changes_nothing(tmp_path: Path) -> None:
    project, home = tmp_path / "p", tmp_path / "u"
    payload = _healthy()
    payload["model"] = "glm-5.3-flash"
    payload["subagentPromptCacheTtl"] = "5m"
    _write(project, payload)
    _write(home, {"model": "glm-4.9"})
    assert doctor.main(["--harness", "--project-dir", str(project), "--home", str(home), "--fix"]) == 0
    snapshot = (project / ".claude" / "settings.json").read_text(encoding="utf-8")
    user_snapshot = (home / ".claude" / "settings.json").read_text(encoding="utf-8")
    assert doctor.main(["--harness", "--project-dir", str(project), "--home", str(home), "--fix"]) == 0
    assert (project / ".claude" / "settings.json").read_text(encoding="utf-8") == snapshot
    assert (home / ".claude" / "settings.json").read_text(encoding="utf-8") == user_snapshot


def test_roots_come_from_the_environment_when_flags_are_absent(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    project, home = tmp_path / "p", tmp_path / "u"
    _write(project, _healthy())
    _write(home, _healthy())
    monkeypatch.setenv("CLAUDE_PROJECT_DIR", str(project))
    monkeypatch.setenv("HOME", str(home))
    assert doctor.main(["--harness"]) == 0


def test_a_mode_is_required(tmp_path: Path) -> None:
    assert doctor.main(["--project-dir", str(tmp_path), "--home", str(tmp_path)]) == 2


def test_json_mode_reports_findings_and_the_fix_count(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    project, home = tmp_path / "p", tmp_path / "u"
    payload = _healthy()
    payload["model"] = "glm-5.3-flash"
    _write(project, payload)
    _write(home, _healthy())
    exit_code = doctor.main(
        ["--harness", "--fix", "--json", "--project-dir", str(project), "--home", str(home)]
    )
    assert exit_code == 0
    report = json.loads(capsys.readouterr().out)
    # --fix reports the post-fix state: the edit count is the trace of what
    # moved, the findings are what is true afterwards.
    assert report["fixed"] == 1
    assert all(f["status"] in {"PASS", "SKIP"} for f in report["findings"])
