from __future__ import annotations

import json
import shlex
from pathlib import Path

import pytest

from conductor import hook_installer as installer


def _config_path(root: Path, provider: str) -> Path:
    return root / installer.PROVIDERS[provider].relative_path


def _write_config(root: Path, provider: str, payload: dict[str, object]) -> Path:
    path = _config_path(root, provider)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=4) + "\n")
    return path


def _managed_commands(payload: dict[str, object]) -> list[str]:
    commands: list[str] = []
    hooks = payload.get("hooks", {})
    assert isinstance(hooks, dict)
    for groups in hooks.values():
        assert isinstance(groups, list)
        for group in groups:
            if not isinstance(group, dict) or not isinstance(group.get("hooks"), list):
                continue
            for hook in group["hooks"]:
                if isinstance(hook, dict) and installer._is_managed_hook(hook):
                    command = hook.get("command")
                    assert isinstance(command, str)
                    commands.append(command)
    return commands


@pytest.mark.parametrize("provider", sorted(installer.PROVIDERS))
def test_merge_install_is_idempotent_and_preserves_unrelated_settings(
    provider: str,
) -> None:
    spec = installer.PROVIDERS[provider]
    original = {
        "permissions": {"allow": ["Read"]},
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": "Read",
                    "hooks": [{"type": "command", "command": "keep-me", "timeout": 7}],
                }
            ]
        },
    }
    command = installer.startup_command(
        spec, interpreter="/runtime/python", identity="codex-efficiency"
    )
    once = installer.merge_install(original, spec, command)
    twice = installer.merge_install(once, spec, command)
    assert twice == once
    assert once["permissions"] == original["permissions"]
    assert once["hooks"]["PreToolUse"] == original["hooks"]["PreToolUse"]
    assert _managed_commands(once) == [command]


def test_startup_command_uses_current_interpreter_and_fixed_context_bounds() -> None:
    command = installer.startup_command(
        installer.PROVIDERS["codex"], interpreter="/runtime/python"
    )
    parts = shlex.split(command)
    assert parts[:3] == ["/runtime/python", "-m", "conductor.a2a_session_start"]
    assert parts[parts.index("--max-messages") + 1] == "8"
    assert parts[parts.index("--preview-chars") + 1] == "140"
    assert parts[parts.index("--max-chars") + 1] == "1200"
    assert "/home/" not in command


def test_grok_uses_first_turn_path_not_session_start_injection() -> None:
    spec = installer.PROVIDERS["grok"]
    command = installer.startup_command(spec, interpreter="python")
    updated = installer.merge_install(
        {"hooks": {"SessionStart": [{"hooks": [{"command": "keep"}]}]}},
        spec,
        command,
    )
    assert updated["hooks"]["SessionStart"] == [{"hooks": [{"command": "keep"}]}]
    assert len(updated["hooks"]["UserPromptSubmit"]) == 1
    assert "--once-per-session" in command


def test_default_install_is_dry_run(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    path = _write_config(tmp_path, "codex", {"unrelated": {"answer": 42}})
    before = path.read_bytes()
    assert (
        installer.main(
            [
                "install",
                "--provider",
                "codex",
                "--root",
                str(tmp_path),
                "--interpreter",
                "/runtime/python",
            ]
        )
        == 0
    )
    summary = json.loads(capsys.readouterr().out)
    assert summary["applied"] is False
    assert summary["providers"][0]["changed"] is True
    assert path.read_bytes() == before
    assert not installer.backup_path(path).exists()
    _assert_invalid_json_fails_without_writes(tmp_path)


def test_apply_is_idempotent_and_uninstall_preserves_other_hooks(
    tmp_path: Path,
) -> None:
    keep = {"type": "command", "command": "keep-me", "timeout": 3}
    path = _write_config(
        tmp_path,
        "claude",
        {
            "theme": "dark",
            "hooks": {"SessionStart": [{"matcher": "", "hooks": [keep]}]},
        },
    )
    args = [
        "install",
        "--provider",
        "claude",
        "--root",
        str(tmp_path),
        "--interpreter",
        "/runtime/python",
        "--identity",
        "codex-efficiency",
        "--apply",
    ]
    assert installer.main(args) == 0
    installed = path.read_text()
    assert installer.main(args) == 0
    assert path.read_text() == installed
    payload = json.loads(installed)
    assert payload["theme"] == "dark"
    assert _managed_commands(payload) and "keep-me" in installed

    assert (
        installer.main(
            [
                "uninstall",
                "--provider",
                "claude",
                "--root",
                str(tmp_path),
                "--apply",
            ]
        )
        == 0
    )
    uninstalled = json.loads(path.read_text())
    assert uninstalled["theme"] == "dark"
    assert not _managed_commands(uninstalled)
    assert "keep-me" in path.read_text()


def test_rollback_restores_exact_preinstall_bytes(tmp_path: Path) -> None:
    path = _write_config(
        tmp_path,
        "qwen",
        {"mcpServers": {"existing": {"command": "server"}}, "hooks": {}},
    )
    original = path.read_bytes()
    common = ["--provider", "qwen", "--root", str(tmp_path), "--apply"]
    assert installer.main(["install", *common]) == 0
    assert path.read_bytes() != original
    assert installer.backup_path(path).is_file()

    assert installer.main(["rollback", *common]) == 0
    assert path.read_bytes() == original
    # Rollback swaps the states, so a second rollback safely redoes install.
    assert installer.main(["rollback", *common]) == 0
    assert _managed_commands(json.loads(path.read_text()))


def test_explicit_backup_and_missing_file_rollback(tmp_path: Path) -> None:
    path = _config_path(tmp_path, "grok")
    common = ["--provider", "grok", "--root", str(tmp_path), "--apply"]
    assert installer.main(["backup", *common]) == 0
    assert installer.backup_path(path).is_file()

    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text('{"temporary": true}\n')
    assert installer.main(["rollback", *common]) == 0
    assert not path.exists()


def _assert_invalid_json_fails_without_writes(tmp_path: Path) -> None:
    path = _config_path(tmp_path, "codex")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("{not json\n")
    assert (
        installer.main(
            [
                "install",
                "--provider",
                "codex",
                "--root",
                str(tmp_path),
                "--apply",
            ]
        )
        == 2
    )
    assert path.read_text() == "{not json\n"
    assert not installer.backup_path(path).exists()
