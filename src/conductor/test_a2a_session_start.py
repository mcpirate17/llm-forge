from __future__ import annotations

import json
import subprocess
from pathlib import Path
from typing import Any, cast

import pytest

from conductor import a2a_session_start as startup
from conductor.agent_a2a import A2aStore, AgentRecord


def _valid_envelope(identity: str = "one") -> dict[str, Any]:
    return {
        "schema_version": 1,
        "authority": "bounded-a2a-inbox",
        "agent": identity,
        "unread_only": True,
        "total": 1,
        "shown": 1,
        "omitted": 0,
        "raw_bytes_not_injected": 12,
        "messages": [
            {
                "id": "00000000-0000-4000-8000-000000000001",
                "from": "peer",
                "at": "2026-08-30T12:00:00+00:00",
                "thread": "thread-1",
                "status": "open",
                "requires_response": True,
                "summary": "short",
                "raw_bytes": 12,
            }
        ],
    }


def _assert_identity_resolution() -> None:
    assert startup.resolve_identity(
        "codex-explicit", {startup.IDENTITY_ENV: "env"}
    ) == ("codex-explicit")
    assert startup.resolve_identity(None, {startup.IDENTITY_ENV: " codex-env "}) == (
        "codex-env"
    )
    with pytest.raises(startup.SessionStartError, match="identity missing"):
        startup.resolve_identity(None, {})
    with pytest.raises(startup.SessionStartError, match="invalid A2A identity"):
        startup.resolve_identity("bad identity", {})


def _assert_compact_inbox_command(tmp_path: Path) -> None:
    command = startup.compact_inbox_command(
        identity="codex-efficiency", state_dir=tmp_path, interpreter="python-current"
    )
    assert command == [
        "python-current",
        "-m",
        "conductor.agent_a2a",
        "--state-dir",
        str(tmp_path),
        "inbox",
        "--as-name",
        "codex-efficiency",
        "--unread",
        "--compact",
        "--max-messages",
        "8",
        "--preview-chars",
        "140",
        "--max-chars",
        "1200",
        "--json",
    ]
    assert "--limit" not in command
    for kwargs in (
        {"max_messages": 9},
        {"preview_chars": 141},
        {"max_chars": 1201},
        {"max_messages": 0},
        {"preview_chars": 31},
        {"max_chars": 255},
    ):
        with pytest.raises(startup.SessionStartError):
            startup.compact_inbox_command(
                identity="codex-efficiency",
                state_dir=tmp_path,
                **cast(dict[str, Any], kwargs),
            )


def test_flush_is_always_sender_scoped(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    seen: dict[str, Any] = {}

    def fake_run(command: list[str], **kwargs: Any) -> subprocess.CompletedProcess[str]:
        seen["command"] = command
        seen["kwargs"] = kwargs
        return subprocess.CompletedProcess(command, 3, "", "")

    monkeypatch.setattr(startup.subprocess, "run", fake_run)
    assert startup.flush_sender_queue(identity="one", state_dir=tmp_path) == 3
    command = seen["command"]
    assert command[-3:] == ["flush", "--as-name", "one"]
    assert seen["kwargs"]["stdout"] is subprocess.DEVNULL


def test_preview_failure_never_falls_back_to_full_inbox(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _assert_compact_inbox_command(tmp_path)
    _assert_preview_command_integrates_with_cli(tmp_path)
    _assert_preview_bounds(monkeypatch, tmp_path)
    calls: list[list[str]] = []

    def fake_run(
        command: list[str], **_kwargs: Any
    ) -> subprocess.CompletedProcess[str]:
        calls.append(command)
        return subprocess.CompletedProcess(command, 2, "", "compact unavailable")

    monkeypatch.setattr(startup.subprocess, "run", fake_run)
    with pytest.raises(startup.SessionStartError, match="bounded A2A preview exited 2"):
        startup.request_compact_preview(identity="one", state_dir=tmp_path)
    assert len(calls) == 1
    assert "--compact" in calls[0]


def _assert_preview_bounds(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    payload = _valid_envelope()

    def valid_run(
        command: list[str], **_kwargs: Any
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.CompletedProcess(command, 0, json.dumps(payload), "")

    monkeypatch.setattr(startup.subprocess, "run", valid_run)
    parsed, text = startup.request_compact_preview(
        identity="one", state_dir=tmp_path, max_chars=500
    )
    assert parsed == payload
    assert len(text) <= 500

    def oversized_run(
        command: list[str], **_kwargs: Any
    ) -> subprocess.CompletedProcess[str]:
        oversized = _valid_envelope()
        return subprocess.CompletedProcess(command, 0, json.dumps(oversized), "")

    monkeypatch.setattr(startup.subprocess, "run", oversized_run)
    with pytest.raises(startup.SessionStartError, match="chars; limit is 256"):
        startup.request_compact_preview(
            identity="one", state_dir=tmp_path, max_chars=256
        )


def test_compact_envelope_rejects_untrusted_shapes() -> None:
    _assert_identity_resolution()
    cases: list[tuple[Any, str]] = []
    cases.append(([], "JSON object"))
    for field, value, match in (
        ("schema_version", 2, "schema_version"),
        ("schema_version", 1.0, "schema_version"),
        ("authority", "peer-claimed", "authority"),
        ("agent", "other", "does not match"),
        ("unread_only", False, "unread_only"),
        ("total", True, "total must be"),
        ("shown", "1", "shown must be"),
        ("omitted", -1, "omitted must be"),
        ("raw_bytes_not_injected", "12", "raw_bytes_not_injected"),
        ("messages", {}, "messages must be"),
    ):
        payload = _valid_envelope()
        payload[field] = value
        cases.append((payload, match))

    mismatched_shown = _valid_envelope()
    mismatched_shown["shown"] = 0
    cases.append((mismatched_shown, "does not match"))
    mismatched_total = _valid_envelope()
    mismatched_total["total"] = 2
    cases.append((mismatched_total, "count mismatch"))
    too_many = _valid_envelope()
    too_many["shown"] = 2
    too_many["total"] = 2
    too_many["messages"] = too_many["messages"] * 2
    cases.append((too_many, "exceeds requested maximum"))
    raw_underflow = _valid_envelope()
    raw_underflow["raw_bytes_not_injected"] = 11
    cases.append((raw_underflow, "smaller than shown"))
    raw_mismatch = _valid_envelope()
    raw_mismatch["raw_bytes_not_injected"] = 13
    cases.append((raw_mismatch, "raw byte mismatch"))
    unknown_field = _valid_envelope()
    unknown_field["extension"] = "schema changes require a version bump"
    cases.append((unknown_field, "keys do not match"))

    for payload, match in cases:
        with pytest.raises(startup.SessionStartError, match=match):
            startup.validate_compact_envelope(
                payload, identity="one", max_messages=1, preview_chars=140
            )
    _assert_malformed_message_metadata()


@pytest.mark.parametrize("forbidden", ["body", "data", "data_json"])
def test_compact_envelope_rejects_raw_content_keys_recursively(
    forbidden: str,
) -> None:
    payload = _valid_envelope()
    payload["messages"][0]["nested"] = {forbidden: "must not cross boundary"}
    with pytest.raises(startup.SessionStartError, match="forbidden raw-content key"):
        startup.validate_compact_envelope(
            payload, identity="one", max_messages=8, preview_chars=140
        )


def _assert_malformed_message_metadata() -> None:
    cases = (
        ({"summary": 7}, "summary must be a string"),
        ({"requires_response": 1}, "requires_response must be boolean"),
        ({"raw_bytes": -1}, "raw_bytes must be"),
        ({"summary": "x" * 141}, "summary exceeds 140"),
    )
    for changes, match in cases:
        payload = _valid_envelope()
        payload["messages"][0].update(changes)
        with pytest.raises(startup.SessionStartError, match=match):
            startup.validate_compact_envelope(
                payload, identity="one", max_messages=8, preview_chars=140
            )


def _assert_preview_command_integrates_with_cli(tmp_path: Path) -> None:
    store = A2aStore(tmp_path, "one")
    full_body = "evidence " * 10_000
    store.record_inbound(
        message_id="00000000-0000-4000-8000-000000000001",
        sender="peer",
        recipient="one",
        body=full_body,
        data_json=None,
    )
    payload, text = startup.request_compact_preview(identity="one", state_dir=tmp_path)
    assert payload["authority"] == "bounded-a2a-inbox"
    assert payload["total"] == 1
    assert len(text) <= startup.DEFAULT_MAX_CHARS
    assert full_body not in text


def _assert_ensure_serve_reuses_valid_endpoint(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    record = AgentRecord("one", 7310, "x" * 16)
    monkeypatch.setattr(startup, "load_registry", lambda _state: {"one": record})
    monkeypatch.setattr(startup, "_card_is_valid", lambda _record: True)

    def no_spawn(*_args: Any, **_kwargs: Any) -> None:
        pytest.fail("valid endpoint must not spawn another serve process")

    monkeypatch.setattr(startup.subprocess, "Popen", no_spawn)
    assert startup.ensure_serve(identity="one", state_dir=tmp_path) == "already-running"


def _assert_ensure_serve_refuses_occupied_invalid_port(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    record = AgentRecord("one", 7310, "x" * 16)
    monkeypatch.setattr(startup, "load_registry", lambda _state: {"one": record})
    monkeypatch.setattr(startup, "_card_is_valid", lambda _record: False)
    monkeypatch.setattr(startup, "_port_is_open", lambda _record: True)

    with pytest.raises(
        startup.SessionStartError, match="occupied by an invalid endpoint"
    ):
        startup.ensure_serve(identity="one", state_dir=tmp_path)


def _assert_ensure_serve_starts_detached(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    record = AgentRecord("one", 7310, "x" * 16)
    probes = iter((False, False, True))
    spawned: dict[str, Any] = {}

    class FakeProcess:
        returncode: int | None = None

        def poll(self) -> int | None:
            return self.returncode

    def fake_popen(command: list[str], **kwargs: Any) -> FakeProcess:
        spawned["command"] = command
        spawned["kwargs"] = kwargs
        return FakeProcess()

    monkeypatch.setattr(startup, "load_registry", lambda _state: {"one": record})
    monkeypatch.setattr(startup, "_card_is_valid", lambda _record: next(probes))
    monkeypatch.setattr(startup, "_port_is_open", lambda _record: False)
    monkeypatch.setattr(startup.subprocess, "Popen", fake_popen)
    monkeypatch.setattr(startup.time, "sleep", lambda _delay: None)

    assert startup.ensure_serve(identity="one", state_dir=tmp_path) == "started"
    assert spawned["command"][-3:] == ["serve", "--name", "one"]
    assert spawned["kwargs"]["start_new_session"] is True
    assert spawned["kwargs"]["stdin"] is subprocess.DEVNULL


def test_ensure_serve_lifecycle(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _assert_ensure_serve_reuses_valid_endpoint(monkeypatch, tmp_path)
    _assert_ensure_serve_refuses_occupied_invalid_port(monkeypatch, tmp_path)
    _assert_ensure_serve_starts_detached(monkeypatch, tmp_path)


def test_first_invocation_marks_only_a_successful_session(tmp_path: Path) -> None:
    with startup.first_invocation(
        state_dir=tmp_path, identity="one", session_id="session-a"
    ) as should_run:
        assert should_run is True
    with startup.first_invocation(
        state_dir=tmp_path, identity="one", session_id="session-a"
    ) as should_run:
        assert should_run is False

    with pytest.raises(RuntimeError, match="boom"):
        with startup.first_invocation(
            state_dir=tmp_path, identity="one", session_id="session-b"
        ) as should_run:
            assert should_run is True
            raise RuntimeError("boom")
    with startup.first_invocation(
        state_dir=tmp_path, identity="one", session_id="session-b"
    ) as should_run:
        assert should_run is True
    _assert_grok_hook_output_uses_user_prompt_event()


def _assert_grok_hook_output_uses_user_prompt_event() -> None:
    result = startup.StartupResult("grok", "started", 0, {"total": 1}, '{"total":1}')
    with pytest.MonkeyPatch.context() as monkeypatch:
        output: list[str] = []
        monkeypatch.setattr("builtins.print", output.append)
        startup._emit(
            result, output="hook-json", event_name=startup.PROVIDER_EVENT["grok"]
        )
    payload = json.loads(output[0])
    assert payload["hookSpecificOutput"] == {
        "hookEventName": "UserPromptSubmit",
        "additionalContext": '{"total":1}',
    }
