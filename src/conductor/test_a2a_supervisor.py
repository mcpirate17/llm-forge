"""Bounded supervisor lifetime and ownership tests; never launch a service."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from conductor import a2a_supervisor as supervisor
from conductor.a2a_registry import A2aError, init_registry


@pytest.fixture
def config(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    init_registry(tmp_path, name="tester", port=7442)
    now = [0.0]
    monkeypatch.setattr(supervisor.time, "monotonic", lambda: now[0])
    monkeypatch.setattr(
        supervisor.time, "sleep", lambda seconds: now.__setitem__(0, now[0] + seconds)
    )
    return supervisor.SupervisorConfig(tmp_path, "tester", duration=2, interval=1)


def test_supervisor_finishes_at_budget_with_durable_state(config, monkeypatch):
    calls = []

    def flush(configuration, remaining):
        calls.append(remaining)
        return {"status": "complete", "counts": {"queued": 1}}

    monkeypatch.setattr(supervisor, "_flush", flush)
    result = supervisor.supervise(config)
    assert calls == [2, 1]
    assert result["cycles"] == 2 and result["status"] == "completed"
    assert (
        json.loads((config.state_dir / "tester/supervisor.json").read_text()) == result
    )


def test_supervisor_does_not_stop_existing_endpoint(config, monkeypatch):
    from dataclasses import replace

    monkeypatch.setattr(supervisor, "_card_is_valid", lambda _: True)
    monkeypatch.setattr(supervisor, "_flush", lambda *a: {"status": "complete"})
    monkeypatch.setattr(
        supervisor, "_stop_process", lambda _: pytest.fail("not our endpoint")
    )
    monkeypatch.setattr(
        supervisor, "_start_endpoint", lambda *a: pytest.fail("already healthy")
    )
    supervisor.supervise(replace(config, serve=True))


def test_supervisor_stops_owned_endpoint_even_when_flush_fails(config, monkeypatch):
    from dataclasses import replace

    class Child:
        pid = 992

        def poll(self):
            return None

    child = Child()
    stopped = []
    monkeypatch.setattr(supervisor, "_card_is_valid", lambda _: False)
    monkeypatch.setattr(supervisor, "_start_endpoint", lambda *a: child)
    monkeypatch.setattr(supervisor, "_stop_process", stopped.append)

    def flush(*args):
        raise A2aError("bad flush")

    monkeypatch.setattr(supervisor, "_flush", flush)
    with pytest.raises(A2aError, match="bad flush"):
        supervisor.supervise(replace(config, serve=True))
    assert stopped == [child]
    state = json.loads((config.state_dir / "tester/supervisor.json").read_text())
    assert state["status"] == "failed" and state["endpoint_pid"] is None


def test_flush_timeout_is_bounded_and_preserves_retry_evidence(config, monkeypatch):
    def run(command, **kwargs):
        assert kwargs["timeout"] == 0.5
        assert command[-2:] == ["--max-messages", "100"]
        raise subprocess.TimeoutExpired(command, kwargs["timeout"])

    monkeypatch.setattr(supervisor.subprocess, "run", run)
    assert supervisor._flush(config, 0.5)["status"] == "timeout"


def test_supervisor_rejects_duplicate_owner_and_invalid_budget(config):
    from dataclasses import replace

    with (
        supervisor._lease(config.state_dir / "tester"),
        pytest.raises(A2aError, match="already active"),
    ):
        supervisor.supervise(config)
    with pytest.raises(A2aError, match="duration"):
        supervisor.supervise(replace(config, duration=float("nan")))
