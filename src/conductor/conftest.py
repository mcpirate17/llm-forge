"""Shared fixtures for conductor tests that exercise agent hook wiring.

The runtime matrix and the local-AI policy tests verify the shell hooks every
agent launcher (Codex, Claude, Qwen, Grok) routes through the shared guard.
Those hook suites are per-host configuration, absent from a clean checkout, so
the tests build an equivalent suite under ``tmp_path`` and point the code at it
through its ``root`` parameters instead of reading the host's files.
"""

from __future__ import annotations

import json
import shlex
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from conductor import active_state

REPO_ROOT = Path(__file__).resolve().parents[1]

# (agent, read tool, shell tool, extra exports in the pre-edit hook)
_AGENTS: tuple[tuple[str, str, str, str], ...] = (
    ("codex", "Read", "Bash", ""),
    ("claude", "Read", "Bash", ""),
    ("qwen", "read_file", "run_shell_command", "export LOCAL_AI_RUNTIME=1\n"),
    ("grok", "read_file", "run_shell_command", ""),
)
_CONFIG_PATHS = {
    "codex": Path(".codex/hooks.json"),
    "claude": Path(".claude/settings.json"),
    "qwen": Path(".qwen/settings.json"),
    "grok": Path(".grok/hooks/workspace.json"),
}
_PRE_EDIT_HOOK = """#!/usr/bin/env bash
# PreToolUse: route every read and edit through the shared current-work guard.
set -euo pipefail
{exports}export PYTHONPATH={repo}${{PYTHONPATH:+:$PYTHONPATH}}
exec {python} -m conductor.current_work_guard
"""
_PRE_BASH_HOOK = """#!/usr/bin/env bash
# PreToolUse/Bash: deny history-destroying git commands.
set -euo pipefail
payload=$(cat)
case "$payload" in
  *'git reset --hard'*) echo 'BLOCKED: git reset --hard destroys uncommitted work' ;;
esac
"""
_POST_HOOK = """#!/usr/bin/env bash
# PostToolUse: bounded no-op.
set -euo pipefail
cat >/dev/null
echo '{"hookSpecificOutput":{"hookEventName":"PostToolUse"}}'
"""
_OBSIDIAN_SYNC = """#!{python}
# PostToolUse no-op standing in for the vault mirror.
import sys

sys.stdin.read()
print('{{"hookSpecificOutput":{{"hookEventName":"PostToolUse"}}}}')
"""


def _write_program(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8")
    path.chmod(0o755)


def _hook_config(
    root: Path, agent: str, read_tool: str, shell_tool: str
) -> dict[str, object]:
    hooks = root / f".{agent}" / "hooks"
    gate = f"GOVERNANCE_OWNER={agent} {root / '.agent_hooks' / 'crg_gate.py'} verify"
    return {
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": read_tool,
                    "hooks": [
                        {"type": "command", "command": str(hooks / "pre-edit.sh")}
                    ],
                },
                {
                    "matcher": shell_tool,
                    "hooks": [{"type": "command", "command": gate}],
                },
            ]
        }
    }


@pytest.fixture
def hook_repo(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A git repository carrying the four agent hook suites the matrix checks."""
    root = tmp_path / "repo"
    root.mkdir()
    subprocess.run(["git", "init", "-q", str(root)], check=True)
    python = shlex.quote(sys.executable)
    for agent, read_tool, shell_tool, exports in _AGENTS:
        hooks = root / f".{agent}" / "hooks"
        hooks.mkdir(parents=True)
        _write_program(
            hooks / "pre-edit.sh",
            _PRE_EDIT_HOOK.format(
                exports=exports, repo=shlex.quote(str(REPO_ROOT)), python=python
            ),
        )
        (root / _CONFIG_PATHS[agent]).write_text(
            json.dumps(_hook_config(root, agent, read_tool, shell_tool), indent=2)
            + "\n",
            encoding="utf-8",
        )
    for agent in ("codex", "claude"):
        hooks = root / f".{agent}" / "hooks"
        _write_program(hooks / "pre-bash.sh", _PRE_BASH_HOOK)
        _write_program(hooks / "post-edit.sh", _POST_HOOK)
        _write_program(
            hooks / "obsidian_sync.py", _OBSIDIAN_SYNC.format(python=sys.executable)
        )
    _write_program(root / ".claude" / "hooks" / "post-bash-graph.sh", _POST_HOOK)
    (root / ".agent_hooks").mkdir()
    shutil.copy(
        REPO_ROOT / ".agent_hooks" / "crg_gate.py",
        root / ".agent_hooks" / "crg_gate.py",
    )
    monkeypatch.setenv(
        "GROK_INSPECT_COMMAND",
        f"{python} -m conductor.grok_inspect_stub {shlex.quote(str(root))}",
    )
    monkeypatch.setattr(active_state, "ROOT", root)
    active_state.save_active_state(root / "conductor" / "active_state.json")
    return root
