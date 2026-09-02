"""Decision-merge contracts (mutation campaign: claude_hook_dispatch_merge)."""

from __future__ import annotations

from tooling.hooks.dispatch.merge import SEPARATOR, HookOutcome, error_line, merge


def _pre(decision: str | None = None, reason: str = "", context: str = "") -> dict:
    specific: dict = {"hookEventName": "PreToolUse"}
    if decision:
        specific["permissionDecision"] = decision
    if reason:
        specific["permissionDecisionReason"] = reason
    if context:
        specific["additionalContext"] = context
    return {"hookSpecificOutput": specific}


def _out(name: str, output: dict | None, **kw) -> HookOutcome:
    return HookOutcome(name, output, **kw)


def test_any_deny_wins_over_allow_and_ask():
    result = merge(
        "PreToolUse",
        [
            _out("a", _pre("allow")),
            _out("b", _pre("deny", "no")),
            _out("c", _pre("ask", "maybe")),
            _out("d", _pre("allow")),
        ],
    )
    specific = result["hookSpecificOutput"]
    assert specific["permissionDecision"] == "deny"
    assert specific["permissionDecisionReason"] == "no"


def test_ask_beats_allow_when_nobody_denies():
    result = merge(
        "PreToolUse", [_out("a", _pre("allow")), _out("b", _pre("ask", "why"))]
    )
    specific = result["hookSpecificOutput"]
    assert specific["permissionDecision"] == "ask"
    assert specific["permissionDecisionReason"] == "why"


def test_all_allow_is_allow_and_quiet_hooks_do_not_vote():
    result = merge(
        "PreToolUse", [_out("a", _pre("allow")), _out("b", None), _out("c", _pre())]
    )
    assert result["hookSpecificOutput"]["permissionDecision"] == "allow"
    assert result == {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
        }
    }


def test_no_votes_means_no_decision_key():
    result = merge(
        "PostToolUse",
        [
            _out("a", None),
            _out("b", {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}),
        ],
    )
    assert result == {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}


def test_every_denying_reason_is_kept_in_order():
    result = merge(
        "PreToolUse",
        [
            _out("a", _pre("deny", "first")),
            _out("b", _pre("allow")),
            _out("c", _pre("deny", "second")),
        ],
    )
    assert (
        result["hookSpecificOutput"]["permissionDecisionReason"]
        == f"first{SEPARATOR}second"
    )


def test_losing_reasons_are_dropped():
    result = merge(
        "PreToolUse",
        [_out("a", _pre("ask", "ask-reason")), _out("b", _pre("deny", "deny-reason"))],
    )
    assert result["hookSpecificOutput"]["permissionDecisionReason"] == "deny-reason"


def test_additional_context_concatenates_in_hook_order():
    result = merge(
        "PostToolUse",
        [
            _out("a", {"hookSpecificOutput": {"additionalContext": "one"}}),
            _out("b", None),
            _out("c", {"hookSpecificOutput": {"additionalContext": "two"}}),
        ],
    )
    assert result["hookSpecificOutput"]["additionalContext"] == f"one{SEPARATOR}two"


def test_error_is_visible_in_system_message_and_context():
    failed = _out("read_budget", None, error="ValueError: boom")
    result = merge("PostToolUse", [_out("ok", _pre()), failed])
    assert error_line(failed) in result["systemMessage"]
    assert error_line(failed) in result["hookSpecificOutput"]["additionalContext"]
    assert "permissionDecision" not in result["hookSpecificOutput"]


def test_fail_closed_pretooluse_error_denies():
    failed = _out("crg_gate_verify", None, error="timed out after 5s", fail_closed=True)
    result = merge("PreToolUse", [_out("ok", _pre("allow")), failed])
    specific = result["hookSpecificOutput"]
    assert specific["permissionDecision"] == "deny"
    assert specific["permissionDecisionReason"] == error_line(failed)
    assert result["systemMessage"] == error_line(failed)


def test_fail_open_pretooluse_error_does_not_deny():
    failed = _out("post_edit", None, error="exit 1")
    result = merge("PreToolUse", [_out("ok", _pre("allow")), failed])
    assert result["hookSpecificOutput"]["permissionDecision"] == "allow"
    assert error_line(failed) in result["systemMessage"]


def test_fail_closed_only_applies_to_pretooluse():
    failed = _out("x", None, error="exit 1", fail_closed=True)
    result = merge("PostToolUse", [failed])
    assert "permissionDecision" not in result["hookSpecificOutput"]
    assert error_line(failed) in result["hookSpecificOutput"]["additionalContext"]


def test_legacy_top_level_deny_counts_as_deny_on_pretooluse():
    result = merge(
        "PreToolUse",
        [
            _out("a", _pre("allow")),
            _out("guard", {"decision": "deny", "reason": "claim"}),
        ],
    )
    specific = result["hookSpecificOutput"]
    assert specific["permissionDecision"] == "deny"
    assert specific["permissionDecisionReason"] == "claim"


def test_post_tool_block_survives_with_reason():
    result = merge(
        "PostToolUse",
        [_out("a", None), _out("b", {"decision": "block", "reason": "bad"})],
    )
    assert result["decision"] == "block"
    assert result["reason"] == "bad"


def test_continue_false_wins_and_stop_reasons_join():
    result = merge(
        "PostToolUse",
        [
            _out("a", {"continue": False, "stopReason": "halt"}),
            _out("b", {"continue": True}),
            _out("c", {"systemMessage": "note", "suppressOutput": True}),
        ],
    )
    assert result["continue"] is False
    assert result["stopReason"] == "halt"
    assert result["suppressOutput"] is True
    assert result["systemMessage"] == "note"


def test_updated_tool_output_last_writer_wins_and_conflict_is_loud():
    result = merge(
        "PostToolUse",
        [
            _out("a", {"hookSpecificOutput": {"updatedToolOutput": "first"}}),
            _out("b", {"hookSpecificOutput": {"updatedToolOutput": "second"}}),
        ],
    )
    assert result["hookSpecificOutput"]["updatedToolOutput"] == "second"
    assert "HOOK CONFLICT [b]" in result["systemMessage"]


def test_event_name_is_always_the_dispatched_event():
    result = merge(
        "SessionEnd",
        [_out("a", {"hookSpecificOutput": {"hookEventName": "PreToolUse"}})],
    )
    assert result["hookSpecificOutput"]["hookEventName"] == "SessionEnd"


def test_non_dict_output_is_ignored_without_crashing():
    result = merge(
        "PreToolUse", [_out("a", None), HookOutcome("b", ["not", "a", "dict"])]
    )  # type: ignore[arg-type]
    assert result == {"hookSpecificOutput": {"hookEventName": "PreToolUse"}}
