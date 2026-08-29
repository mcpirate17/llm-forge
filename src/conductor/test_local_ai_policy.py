from __future__ import annotations

from pathlib import Path

import pytest

from conductor import local_ai_policy as policy


REPO_ROOT = Path(__file__).resolve().parents[1]


def test_only_user_or_frontier_may_approve() -> None:
    assert policy.approval_authority_allowed("user") is True
    assert policy.approval_authority_allowed("frontier_model") is True
    assert policy.approval_authority_allowed("local_model") is False
    assert policy.approval_authority_allowed("unknown") is False


def test_clerical_classes_accept_low_risk_prompts() -> None:
    for task in policy.ALLOWED_LOCAL_TASKS:
        assert policy.require_clerical_task(task, "Summarize and organize these notes")


@pytest.mark.parametrize(
    "prompt",
    [
        "Approve the post-smoke run",
        "Can we conduct another training run?",
        "Return FINAL VERDICT: PASS if optimizer steps may start",
        "Recommend whether to resume the experiment",
        "Give permission to launch evaluation",
    ],
)
def test_clerical_label_cannot_hide_authority_request(prompt: str) -> None:
    with pytest.raises(policy.LocalAIPolicyError, match="zero approval authority"):
        policy.require_clerical_task("summary", prompt)


def test_hook_requires_explicit_clerical_class_for_local_chat() -> None:
    assert (
        policy.deny_local_ai_command('ollama run qwen3.5:9b "summarize these notes"')
        == policy.UNCLASSIFIED_REASON
    )
    assert (
        policy.deny_local_ai_command(
            'curl http://127.0.0.1:11434/api/chat -d \'{"prompt":"organize"}\''
        )
        == policy.UNCLASSIFIED_REASON
    )


def test_hook_allows_classified_low_risk_local_chat() -> None:
    assert (
        policy.deny_local_ai_command(
            'LOCAL_AI_TASK=summary ollama run qwen3.5:9b "summarize these notes"'
        )
        is None
    )
    assert (
        policy.deny_local_ai_command(
            "env LOCAL_AI_TASK=organization curl "
            'http://localhost:11434/api/chat -d \'{"prompt":"organize notes"}\''
        )
        is None
    )


def test_hook_denies_authority_request_even_when_classified() -> None:
    reason = policy.deny_local_ai_command(
        'LOCAL_AI_TASK=summary ollama run qwen3.5:9b "Should we approve the '
        'post-smoke training run?"'
    )
    assert reason == policy.DENY_REASON


def test_local_agent_runtime_cannot_send_approval_verdict() -> None:
    reason = policy.deny_local_ai_command(
        "python -m conductor.agent_a2a send --to codex-phase22 "
        "--body 'AI_POWERED: true FINAL VERDICT: PASS; training may start'",
        local_runtime=True,
    )
    assert reason == policy.DENY_REASON


def test_frontier_runtime_command_is_not_misclassified_as_local() -> None:
    command = (
        "python -m conductor.agent_a2a send --to codex-phase22 "
        "--body 'FINAL VERDICT: PASS'"
    )
    assert policy.deny_local_ai_command(command, local_runtime=False) is None


def test_hook_ignores_mentions_embeddings_and_non_inference_commands() -> None:
    for command in (
        'echo "ollama run qwen3.5:9b is a local command"',
        "ollama ps",
        "ollama stop qwen3.5:9b",
        "curl http://127.0.0.1:11434/api/embed -d '{} '",
        'rg "qwen3.5:9b" conductor/',
    ):
        assert policy.deny_local_ai_command(command) is None, command


@pytest.mark.parametrize(
    ("config_path", "hook_path"),
    [
        (".codex/hooks.json", ".codex/hooks/pre-edit.sh"),
        (".claude/settings.json", ".claude/hooks/pre-edit.sh"),
        (".qwen/settings.json", ".qwen/hooks/pre-edit.sh"),
        (".grok/hooks/workspace.json", ".grok/hooks/pre-edit.sh"),
    ],
)
def test_every_agent_shell_hook_reaches_shared_policy(
    config_path: str,
    hook_path: str,
) -> None:
    config = (REPO_ROOT / config_path).read_text(encoding="utf-8")
    hook = (REPO_ROOT / hook_path).read_text(encoding="utf-8")

    assert Path(hook_path).name in config
    assert "conductor.current_work_guard" in hook


def test_qwen_clerk_hook_declares_local_runtime() -> None:
    hook = (REPO_ROOT / ".qwen/hooks/pre-edit.sh").read_text(encoding="utf-8")
    assert "LOCAL_AI_RUNTIME=1" in hook
