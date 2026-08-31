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
