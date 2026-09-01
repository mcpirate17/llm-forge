"""Focused tests for bounded, attribution-safe context telemetry."""

from __future__ import annotations

import json
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from conductor import context_telemetry as telemetry


def test_native_usage_ignores_downstream_tool_output() -> None:
    record = telemetry.event(
        {
            "usage": {"prompt_tokens": 7, "completion_tokens": 3},
            "tool_output": {"usage": {"prompt_tokens": 900, "completion_tokens": 800}},
        }
    )
    assert record["usage_source"] == "native"
    assert record["native_usage_path"] == "usage"
    assert record["input_tokens"] == 7
    assert record["output_tokens"] == 3

    downstream_only = telemetry.event(
        {"tool_output": {"usage": {"prompt_tokens": 900}}}
    )
    assert downstream_only["usage_source"] == "none"
    assert downstream_only["native_usage_path"] is None
    assert downstream_only["input_tokens"] is None


def test_append_accepts_exact_byte_cap_and_refuses_overflow(
    tmp_path: Path, monkeypatch
) -> None:
    path = tmp_path / "events.jsonl"
    record = {"id": 1, "value": "é"}
    encoded = (
        json.dumps(record, ensure_ascii=False, separators=(",", ":")) + "\n"
    ).encode("utf-8")
    monkeypatch.setattr(telemetry, "MAX_LOG_BYTES", len(encoded))

    assert telemetry.append(record, path) is True
    first_contents = path.read_bytes()
    assert first_contents == encoded
    assert telemetry.append({"id": 2}, path) is False
    assert path.read_bytes() == first_contents


def test_concurrent_append_keeps_each_record_intact(tmp_path: Path) -> None:
    path = tmp_path / "events.jsonl"
    records = [{"id": number, "value": f"event-{number}"} for number in range(64)]

    with ThreadPoolExecutor(max_workers=16) as pool:
        results = list(pool.map(lambda record: telemetry.append(record, path), records))

    assert all(results)
    decoded = [
        json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()
    ]
    assert len(decoded) == len(records)
    assert {item["id"] for item in decoded} == set(range(len(records)))


def test_event_handles_nonserializable_values_without_retaining_them() -> None:
    record = telemetry.event(
        {"tool_input": {"bad": {1, 2}}, "tool_output": {"bad": object()}}
    )
    assert record["input_bytes"] == 0
    assert record["output_bytes"] == 0
    assert record["tool_input_tokens_estimate"] == 0
    assert record["output_tokens_estimate"] == 0


def test_output_bounded_requires_a_structured_marker() -> None:
    literal = telemetry.event({"tool_output": {"message": "the word elided"}})
    structured = telemetry.event({"tool_output": {"truncated": True}})

    assert literal["output_bounded"] is False
    assert structured["output_bounded"] is True


def test_hook_context_event_measures_only_the_injected_context() -> None:
    record = telemetry.hook_context_event(
        "session-start",
        {
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": "héllo",
            }
        },
    )
    assert record["event"] == "HookContext"
    assert record["tool"] == "session-start"
    assert record["hook_event"] == "SessionStart"
    assert record["output_bytes"] == 6
    assert record["output_tokens_estimate"] == 2

    quiet = telemetry.hook_context_event(
        "pre-read-skeleton",
        {"hookSpecificOutput": {"hookEventName": "PreToolUse"}},
        event_name="Override",
    )
    assert quiet["output_bytes"] == 0
    assert quiet["hook_event"] == "Override"
    assert (
        telemetry.hook_context_event(
            "x", {"hookSpecificOutput": {"hookEventName": "PreToolUse"}}
        )["hook_event"]
        == "PreToolUse"
    )
    assert (
        telemetry.hook_context_event("x", "not json", event_name="E")["hook_event"]
        == "E"
    )


def test_record_rotates_a_full_log_instead_of_dropping(
    tmp_path: Path, monkeypatch
) -> None:
    path = tmp_path / "events.jsonl"
    encoded = (
        json.dumps({"id": 1}, ensure_ascii=False, separators=(",", ":")) + "\n"
    ).encode("utf-8")
    monkeypatch.setattr(telemetry, "MAX_LOG_BYTES", len(encoded))

    telemetry.record({"id": 1}, path)
    telemetry.record({"id": 2}, path)

    rotated = list(tmp_path.glob("events.jsonl.*.full"))
    assert len(rotated) == 1
    assert rotated[0].read_bytes() == encoded
    assert json.loads(path.read_text(encoding="utf-8")) == {"id": 2}


def test_summary_groups_by_event_and_tool_and_counts_over_bound(
    tmp_path: Path,
) -> None:
    path = tmp_path / "events.jsonl"
    records = [
        telemetry.event({"tool_name": "Bash", "tool_response": "x" * 30}),
        telemetry.event({"tool_name": "Bash", "tool_response": "x" * 8}),
        telemetry.event({"tool_name": "Read", "tool_response": "y" * 10}),
        telemetry.hook_context_event(
            "session-start",
            {
                "hookSpecificOutput": {
                    "hookEventName": "SessionStart",
                    "additionalContext": "abcd",
                }
            },
        ),
    ]
    for item in records:
        telemetry.append(item, path)
    path.write_bytes(path.read_bytes() + b"not json\n")

    summary = telemetry.summarize([path], bound_bytes=12)

    assert summary["events"] == 4
    assert summary["hook_context_bytes"] == 4
    rows = {(row["event"], row["tool"]): row for row in summary["rows"]}
    bash = rows[("PostToolUse", "Bash")]
    assert bash["count"] == 2 and bash["output_bytes"] == 32 + 10
    assert bash["over_bound"] == 1 and bash["over_bound_bytes"] == 32 - 12
    assert rows[("PostToolUse", "Read")]["over_bound"] == 0
    assert summary["rows"][0]["tool"] == "Bash"
    assert abs(sum(row["share"] for row in summary["rows"]) - 1.0) < 1e-3


def test_event_measures_edit_and_write_as_the_agent_sees_them() -> None:
    big = "x" * 10_000
    edit = telemetry.event(
        {
            "tool_name": "Edit",
            "tool_response": {
                "filePath": "a.py",
                "oldString": "a",
                "newString": "b",
                "originalFile": big,
                "structuredPatch": [{"lines": ["-a", "+b"]}],
                "userModified": False,
            },
        }
    )
    write = telemetry.event(
        {
            "tool_name": "Write",
            "tool_response": {"type": "create", "filePath": "a.py", "content": big},
        }
    )
    bash = telemetry.event({"tool_name": "Bash", "tool_response": {"stdout": big}})
    assert edit["output_bytes"] < 200
    assert write["output_bytes"] < 100
    assert bash["output_bytes"] > 10_000
    assert telemetry.model_visible_output("Edit", "not a mapping") == "not a mapping"


def test_hook_context_event_counts_a_deny_reason_as_injected_context() -> None:
    denied = telemetry.hook_context_event(
        "pre-read-skeleton",
        {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": "PRE-READ DENIED: 812 lines",
            }
        },
    )
    assert denied["output_bytes"] == len("PRE-READ DENIED: 812 lines")
