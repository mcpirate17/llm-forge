"""Paired tests for the dispatch entrypoint's telemetry wiring.

``_record_hook_timings`` is the seam that routes ``runner.dispatch``'s
per-hook ``elapsed_ms`` (previously visible only under
``HOOK_DISPATCH_TRACE``) into ``conductor.context_telemetry`` -- see
``src/conductor/context_telemetry.py``'s ``hook_timing_event``.
"""

from __future__ import annotations

import json
from pathlib import Path

from tooling.hooks.dispatch import __main__ as entrypoint
from tooling.hooks.dispatch.merge import HookOutcome


def test_session_id_reads_the_payload_and_tolerates_bad_json() -> None:
    assert entrypoint._session_id(b'{"session_id": "abc-123"}') == "abc-123"
    assert entrypoint._session_id(b"not json") == ""
    assert entrypoint._session_id(b'{"session_id": 7}') == ""
    assert entrypoint._session_id(b"[]") == ""


def test_record_hook_timings_writes_one_event_per_outcome(
    tmp_path: Path, monkeypatch
) -> None:
    from conductor import context_telemetry

    path = tmp_path / "events.jsonl"
    monkeypatch.setattr(context_telemetry, "DEFAULT_PATH", path)

    outcomes = [
        HookOutcome(name="pre-read-skeleton", output={"ok": True}, elapsed_ms=12.5),
        HookOutcome(name="bash-guard", output=None, error="boom", elapsed_ms=3.0),
    ]
    entrypoint._record_hook_timings("PreToolUse", outcomes, "sess-1")

    lines = [json.loads(line) for line in path.read_text().splitlines()]
    assert len(lines) == 2
    by_tool = {item["tool"]: item for item in lines}
    assert by_tool["pre-read-skeleton"]["elapsed_ms"] == 12.5
    assert by_tool["pre-read-skeleton"]["status"] == "json"
    assert by_tool["pre-read-skeleton"]["session_id"] == "sess-1"
    assert by_tool["bash-guard"]["status"] == "boom"
    assert by_tool["bash-guard"]["hook_event"] == "PreToolUse"


def test_record_hook_timings_survives_a_telemetry_failure(monkeypatch, capsys) -> None:
    from conductor import context_telemetry

    def _boom(*_args, **_kwargs):
        raise OSError("disk full")

    monkeypatch.setattr(context_telemetry, "record", _boom)
    outcomes = [HookOutcome(name="pre-read-skeleton", output={}, elapsed_ms=1.0)]

    entrypoint._record_hook_timings("PreToolUse", outcomes, "")

    assert "context telemetry unavailable" in capsys.readouterr().err
