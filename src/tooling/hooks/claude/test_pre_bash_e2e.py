"""End-to-end: feed the real PreToolUse hook a payload and assert its JSON decision."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

HOOK = Path(__file__).resolve().with_name("pre-bash.sh")


def _decision(command: str) -> tuple[str, str]:
    payload = json.dumps({"tool_input": {"command": command}})
    proc = subprocess.run(
        [str(HOOK)],
        input=payload,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    out = json.loads(proc.stdout)["hookSpecificOutput"]
    return out["permissionDecision"], out.get("permissionDecisionReason", "")


def test_hook_denies_destructive_commands_with_a_reason() -> None:
    for command in (
        "git push --force origin master",
        "git reset --hard HEAD~1",
        "pip install numpy",
    ):
        decision, reason = _decision(command)
        assert decision == "deny", command
        assert reason.startswith("BLOCKED"), (command, reason)


def test_hook_allows_safe_commands_and_quoted_mentions() -> None:
    for command in (
        "git push --force-with-lease origin master",
        """echo '{"command":"git push --force"}'""",
        "uv pip install numpy",
        "ls -la",
        "git status",
    ):
        decision, _ = _decision(command)
        assert decision == "allow", command
