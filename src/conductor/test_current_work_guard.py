from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

from conductor import current_work_guard as guard
from conductor import local_ai_policy


def test_allows_unrelated_files() -> None:
    assert (
        guard.evaluate_payload(
            {"tool_input": {"file_path": "conductor/handoff.py", "content": "x" * 5000}}
        )
        is None
    )


def test_denies_direct_read_of_current_work(tmp_path: Path) -> None:
    path = tmp_path / ".current_work.md"
    reason = guard.evaluate_payload(
        {"tool_name": "Read", "tool_input": {"file_path": str(path)}}
    )
    assert reason is not None
    assert "cannot access" in reason


def test_denies_direct_write_even_when_short(tmp_path: Path) -> None:
    path = tmp_path / ".current_work.md"
    reason = guard.evaluate_payload(
        {
            "tool_name": "Write",
            "tool_input": {"file_path": str(path), "content": "short status"},
        }
    )
    assert reason is not None
    assert "BLOCKED" in reason
    assert "conductor.handoff" in reason


def test_denies_obvious_shell_reads() -> None:
    for command in (
        "cat .current_work.md",
        "sed -n '1,20p' /repo/.current_work.md",
        "rg heading .current_work.md",
    ):
        reason = guard.evaluate_payload(
            {"tool_name": "Bash", "tool_input": {"command": command}}
        )
        assert reason is not None, command
        assert "direct shell access" in reason


def test_allows_safe_shell_mentions_and_bounded_utilities() -> None:
    for command in (
        "echo '.current_work.md is a compatibility log'",
        "rg -n '\\.current_work\\.md' README.md",
        "python -m conductor.handoff append --owner codex --title x --body y",
        "python -m conductor.active_state update",
    ):
        assert (
            guard.evaluate_payload(
                {"tool_name": "Bash", "tool_input": {"command": command}}
            )
            is None
        ), command


def test_denies_local_ai_approval_through_shared_agent_hook() -> None:
    reason = guard.evaluate_payload(
        {
            "tool_name": "Bash",
            "tool_input": {
                "command": (
                    "LOCAL_AI_TASK=summary ollama run qwen3.5:9b "
                    "'Approve the post-smoke training run'"
                )
            },
        }
    )
    assert reason is not None
    assert "zero approval authority" in reason


def test_allows_classified_local_note_summary_through_shared_agent_hook() -> None:
    reason = guard.evaluate_payload(
        {
            "tool_name": "Bash",
            "tool_input": {
                "command": (
                    "LOCAL_AI_TASK=summary ollama run qwen3.5:9b "
                    "'Summarize these implementation notes'"
                )
            },
        }
    )
    assert reason is None


def test_local_agent_runtime_cannot_send_approval_receipt(monkeypatch) -> None:
    monkeypatch.setenv("LOCAL_AI_RUNTIME", "1")
    reason = guard.evaluate_payload(
        {
            "tool_name": "Bash",
            "tool_input": {
                "command": (
                    "python -m conductor.agent_a2a send --to codex-phase22 "
                    "--body 'AI_POWERED: true FINAL VERDICT: PASS'"
                )
            },
        }
    )
    assert reason is not None
    assert "zero approval authority" in reason


def test_hook_response_deny_shape() -> None:
    payload = guard.hook_response("BLOCKED: dump")
    assert payload is not None
    out = payload["hookSpecificOutput"]
    assert set(payload) == {"hookSpecificOutput"}
    assert out["permissionDecision"] == "deny"
    assert "BLOCKED" in out["permissionDecisionReason"]
    json.dumps(payload)


def test_hook_response_is_silent_on_success() -> None:
    assert guard.hook_response(None) is None
    assert guard.hook_response(None, protocol="grok") is None


def test_hook_response_uses_grok_deny_shape_for_camel_case_payloads() -> None:
    payload = {
        "hookEventName": "pre_tool_use",
        "toolName": "write",
        "toolInput": {},
    }
    assert guard.hook_protocol(payload) == "grok"
    assert guard.hook_response("BLOCKED: dump", protocol="grok") == {
        "decision": "deny",
        "reason": "BLOCKED: dump",
    }


def test_cli_is_silent_on_success() -> None:
    result = subprocess.run(
        [sys.executable, "-m", "conductor.current_work_guard"],
        input=json.dumps(
            {"tool_input": {"file_path": "conductor/handoff.py", "content": "x"}}
        ),
        text=True,
        capture_output=True,
        check=True,
    )

    assert result.stdout == ""
    assert result.stderr == ""


def test_denies_grok_camelcase_tool_input(tmp_path: Path) -> None:
    path = tmp_path / ".current_work.md"
    path.write_text("# Active Coordination\n\n## Old\n\nbody\n", encoding="utf-8")
    reason = guard.evaluate_payload(
        {
            "hookEventName": "pre_tool_use",
            "toolName": "read_file",
            "toolInput": {"file_path": str(path)},
        }
    )
    assert reason is not None
    assert "BLOCKED" in reason


def test_advisory_reminds_on_test_file_writes() -> None:
    payload = {
        "tool_name": "Write",
        "tool_input": {"file_path": "research/tests/test_new.py", "content": "x"},
    }
    assert guard.evaluate_payload(payload) is None
    advisory = guard.advisory_for_payload(payload)
    assert advisory is not None
    assert "mutation campaign" in advisory
    response = guard.hook_response(None, protocol="codex", advisory=advisory)
    assert response is not None
    assert "mutation campaign" in response["hookSpecificOutput"]["additionalContext"]


def test_advisory_covers_javascript_specs_and_skips_non_tests() -> None:
    spec = {
        "tool_name": "Write",
        "tool_input": {"file_path": "aria_designer/e2e/designer.spec.js"},
    }
    assert guard.advisory_for_payload(spec) is not None
    assert (
        guard.advisory_for_payload(
            {"tool_name": "Write", "tool_input": {"file_path": "conductor/handoff.py"}}
        )
        is None
    )


def test_deny_still_wins_over_test_file_advisory(tmp_path: Path) -> None:
    path = tmp_path / ".current_work.md"
    payload = {
        "tool_name": "Write",
        "tool_input": {"file_path": str(path), "content": "status"},
    }
    assert guard.evaluate_payload(payload) is not None
    assert guard.advisory_for_payload(payload) is None


def test_hook_protocol_grok_and_unsupported_advisory() -> None:
    assert guard.hook_protocol({"toolName": "Write"}) == "grok"
    deny = guard.hook_response("BLOCKED: x", protocol="grok")
    assert deny == {"decision": "deny", "reason": "BLOCKED: x"}
    advisory = guard.hook_response(None, protocol="grok", advisory="hint")
    assert advisory is not None
    with pytest.raises(ValueError, match="unsupported"):
        guard.hook_response("BLOCKED: x", protocol="other")
    with pytest.raises(ValueError, match="unsupported"):
        guard.hook_response(None, protocol="other", advisory="hint")


def test_main_reads_stdin_and_emits_advisory(monkeypatch, capsys) -> None:
    monkeypatch.setattr(
        "sys.stdin",
        __import__("io").StringIO(
            json.dumps({"tool_input": {"file_path": "research/tests/test_x.py"}})
        ),
    )
    assert guard.main() == 0
    out = capsys.readouterr().out
    assert "mutation campaign" in out
    monkeypatch.setattr("sys.stdin", __import__("io").StringIO("not-json"))
    assert guard.main() == 0
    monkeypatch.setattr("sys.stdin", __import__("io").StringIO("[]"))
    assert guard.main() == 0


def test_shell_segment_edge_cases() -> None:
    assert guard.deny_shell_access("cat 'unterminated") is None
    reason = guard.evaluate_payload(
        {"tool_name": "Bash", "tool_input": {"command": "env cat .current_work.md"}}
    )
    assert reason is not None
    reason = guard.evaluate_payload(
        {"tool_name": "Bash", "tool_input": {"command": "rg foo .current_work.md"}}
    )
    assert reason is not None


def test_shell_unquoted_and_env_assignment() -> None:
    assert guard.deny_shell_access("cat '.current_work.md") is None
    reason = guard.evaluate_payload(
        {
            "tool_name": "Bash",
            "tool_input": {"command": "FOO=1 cat .current_work.md"},
        }
    )
    assert reason is not None
    reason = guard.evaluate_payload(
        {
            "tool_name": "Bash",
            "tool_input": {"command": "rg foo -- .current_work.md"},
        }
    )
    assert reason is not None
    reason = guard.evaluate_payload(
        {
            "tool_name": "Bash",
            "tool_input": {"command": "sudo cat .current_work.md"},
        }
    )
    assert reason is not None
    reason = guard.evaluate_payload(
        {
            "tool_name": "Bash",
            "tool_input": {"command": "echo x; cat .current_work.md"},
        }
    )
    assert reason is not None
    reason = guard.evaluate_payload(
        {
            "tool_name": "Bash",
            "tool_input": {"command": "FOO=.current_work.md sudo"},
        }
    )
    assert reason is None
    reason = guard.evaluate_payload(
        {
            "tool_name": "Bash",
            "tool_input": {"command": "rg .current_work.md --"},
        }
    )
    assert reason is None


def test_local_ai_policy_fail_closed_edge_cases() -> None:
    assert not local_ai_policy.approval_authority_allowed("unverified")
    with pytest.raises(local_ai_policy.LocalAIPolicyError, match="not clerical"):
        local_ai_policy.require_clerical_task("approval", "summarize notes")
    assert (
        local_ai_policy.deny_local_ai_command('ollama run "unterminated')
        == local_ai_policy.UNCLASSIFIED_REASON
    )
