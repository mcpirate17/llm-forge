from __future__ import annotations

import json
import shlex
import subprocess
import sys
from pathlib import Path

import pytest

from conductor import harness_provisioning as hp
from conductor import project_init as pi
from conductor.hook_installer import PROVIDERS


@pytest.mark.parametrize("provider", ["codex", "qwen", "grok"])
def test_provider_bootstrap_is_idempotent_and_check_catches_drift(
    tmp_path: Path,
    provider: str,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    (tmp_path / ".git").mkdir()
    monkeypatch.setattr(pi, "_crg_importable", lambda python: True)
    config = pi.InitConfig(
        project_dir=tmp_path, python=Path(sys.executable), providers=(provider,)
    )
    assert pi.run(config) == 0
    assert not pi.plan(config).changed
    assert pi.run(config.model_copy(update={"check": True})) == 0
    path = tmp_path / PROVIDERS[provider].relative_path
    payload = json.loads(path.read_text())
    event = PROVIDERS[provider].event
    payload["hooks"][event] = []
    path.write_text(json.dumps(payload))
    assert hp.check_provider(provider, Path(sys.executable), tmp_path)
    assert not (tmp_path / ".claude/settings.json").exists()


def test_codex_wires_explicit_protocol_root_and_interpreter_with_spaces(
    tmp_path: Path,
) -> None:
    python = Path("/runtime with spaces/bin/python")
    hooks = hp.provider_hooks("codex", python, tmp_path)
    command = hooks["PreToolUse"][0]["hooks"][0]["command"]
    assert shlex.split(command) == [
        str(python),
        "-m",
        "tooling.hooks.dispatch",
        "PreToolUse",
        "--project-dir",
        str(tmp_path),
        "--protocol",
        "codex",
    ]


def test_grok_uses_first_prompt_and_preserves_foreign_hooks(tmp_path: Path) -> None:
    path = tmp_path / PROVIDERS["grok"].relative_path
    path.parent.mkdir(parents=True)
    foreign = {"hooks": [{"type": "command", "command": "keep-me"}]}
    path.write_text(json.dumps({"hooks": {"UserPromptSubmit": [foreign]}}))
    config = pi.InitConfig(project_dir=tmp_path, providers=("grok",))
    action = pi._provider_actions(config)[0]
    hooks = json.loads(action.after)["hooks"]
    assert foreign in hooks["UserPromptSubmit"]
    assert "SessionStart" not in hooks
    assert "--once-per-session" in action.after
    assert "tool guards are not provisioned" in hp.CAPABILITIES["grok"]


def test_check_only_imports_adapters_without_starting_services(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    path = tmp_path / PROVIDERS["qwen"].relative_path
    path.parent.mkdir(parents=True)
    path.write_text(
        json.dumps({"hooks": hp.provider_hooks("qwen", Path(sys.executable), tmp_path)})
    )
    calls = []

    def probe(argv: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        calls.append(argv)
        return subprocess.CompletedProcess(argv, 0, "", "")

    monkeypatch.setattr(hp.subprocess, "run", probe)
    assert hp.check_provider("qwen", Path(sys.executable), tmp_path) == []
    assert len(calls) == 1 and calls[0][1] == "-c"
    assert calls[0][2].startswith("import conductor.a2a_session_start;")


def test_selected_interpreter_is_bound_in_native_commands(tmp_path: Path) -> None:
    binary = tmp_path / "tool directory" / "forge"
    python = tmp_path / "virtual environment" / "bin" / "python"
    settings = json.loads(pi.render_settings(None, False, binary, python))
    for event, groups in settings["hooks"].items():
        assert shlex.split(groups[0]["hooks"][0]["command"]) == [
            "env",
            f"CONDUCTOR_PYTHON={python}",
            str(binary),
            "hook",
            event,
        ]


def test_provider_selection_defaults_to_claude_and_all_is_explicit(
    tmp_path: Path,
) -> None:
    assert pi.parse_args([str(tmp_path)]).providers == ("claude",)
    assert set(pi.parse_args([str(tmp_path), "--provider", "all"]).providers) == set(
        hp.CAPABILITIES
    )
