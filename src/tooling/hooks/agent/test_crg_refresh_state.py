"""Tests for the asynchronous, freshness-guarded graph refresh (crg_refresh_state).

Covered contracts: a graph query waits (bounded) on a running refresh and
reports a timeout; the worker lock is honoured by both the hook path and a
second worker; a refresh that raises is recorded and surfaced, never swallowed;
the worker debounces and coalesces queued paths into one refresh; the doctor
row reports a stuck lock; one real detached worker drains an end-to-end queue.
"""

from __future__ import annotations

import fcntl
import importlib
import io
import json
import os
import sys
import time
from pathlib import Path

import pytest

HOOK_DIR = Path(__file__).resolve().parent
ROOT = HOOK_DIR.parents[2]
sys.path.insert(0, str(HOOK_DIR))
sys.path.insert(0, str(ROOT))

from crg_refresh_state import (  # noqa: E402
    Store,
    drain,
    record_failure,
    request,
    status,
    take_notices,
    take_pending,
    wait_for_fresh,
    worker_alive,
)

HOLD_LOCK = (
    "import fcntl, os, sys, time\n"
    "fd = os.open(sys.argv[1], os.O_RDWR | os.O_CREAT)\n"
    "fcntl.flock(fd, fcntl.LOCK_EX)\n"
    "time.sleep(float(sys.argv[2]))\n"
)
DRAIN_WORKER = (
    "import sys\n"
    f"sys.path.insert(0, {str(HOOK_DIR)!r})\n"
    "from pathlib import Path\n"
    "from crg_refresh_state import Store, drain\n"
    "out = Path(sys.argv[2])\n"
    "drain(Store(Path(sys.argv[1])), "
    "lambda paths: out.open('a').write(','.join(paths) + '\\n'), debounce=0.05)\n"
)


@pytest.fixture
def store(tmp_path: Path) -> Store:
    root = tmp_path / "store"
    root.mkdir()
    return Store(root)


@pytest.fixture
def held_lock(store: Store):
    """Hold the worker lock from this process on a separate open file description."""
    fd = os.open(store.lock, os.O_RDWR | os.O_CREAT, 0o644)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield fd
    finally:
        os.close(fd)


@pytest.fixture
def body(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    """The hook body wired to a scratch repo and store."""
    repo = tmp_path / "repo"
    (repo / "pkg").mkdir(parents=True)
    (repo / "pkg" / "mod.py").write_text("X = 1\n", encoding="utf-8")
    (repo / "notes.md").write_text("# n\n", encoding="utf-8")
    monkeypatch.setenv("CRG_DATA_DIR", str(tmp_path / "store"))
    module = importlib.import_module("crg_graph_refresh")
    monkeypatch.setattr(module, "REPO_ROOT", repo)
    monkeypatch.setattr(sys.modules["crg_gate"], "REPO_ROOT", repo)
    return module


class FakeClock:
    def __init__(self) -> None:
        self.now = 100.0
        self.sleeps: list[float] = []

    def __call__(self) -> float:
        return self.now

    def sleep(self, seconds: float) -> None:
        self.sleeps.append(seconds)
        self.now += seconds


# ── wait ─────────────────────────────────────────────────────────────────────


def test_wait_times_out_while_a_worker_holds_the_lock(store: Store, held_lock):
    clock = FakeClock()
    outcome = wait_for_fresh(
        store, timeout=3.0, poll=0.5, clock=clock, sleep=clock.sleep
    )
    assert outcome == "timeout"
    assert clock.sleeps == [0.5] * 6
    assert clock.now == pytest.approx(103.0)


def test_wait_returns_fresh_once_marker_and_worker_are_gone(store: Store):
    clock = FakeClock()
    store.pending.write_text("a.py\n", encoding="utf-8")
    calls: list[str] = []

    def respawn() -> None:
        calls.append("respawn")
        store.pending.unlink()

    outcome = wait_for_fresh(
        store, timeout=3.0, poll=0.5, clock=clock, sleep=clock.sleep, respawn=respawn
    )
    assert outcome == "fresh"
    assert calls == ["respawn"]
    assert clock.sleeps == [0.5]


def test_wait_output_reports_a_stale_graph_only_on_timeout(body):
    lock = body.store_for(body.REPO_ROOT).lock
    fd = os.open(lock, os.O_RDWR | os.O_CREAT, 0o644)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        output = body.wait_output(timeout=0.05)
    finally:
        os.close(fd)
    assert output is not None
    assert "STALE" in output["systemMessage"]
    assert output["hookSpecificOutput"]["hookEventName"] == "PreToolUse"
    assert body.wait_output(timeout=0.05) is None


# ── lock ─────────────────────────────────────────────────────────────────────


def test_request_spawns_one_worker_then_queues(store: Store, tmp_path: Path):
    argv = [sys.executable, "-c", HOLD_LOCK, str(store.lock), "2"]
    assert request(store, ["a.py"], worker_argv=argv, cwd=tmp_path) == "spawned"
    deadline = time.monotonic() + 5
    while not worker_alive(store) and time.monotonic() < deadline:
        time.sleep(0.02)
    assert worker_alive(store)
    assert request(store, ["b.py", "a.py"], worker_argv=argv, cwd=tmp_path) == "queued"
    assert take_pending(store) == ["a.py", "b.py"]
    assert not store.pending.exists()


def test_drain_refuses_while_another_worker_holds_the_lock(store: Store, held_lock):
    store.pending.write_text("a.py\n", encoding="utf-8")
    calls: list[list[str]] = []
    assert drain(store, calls.append, debounce=0.0, sleep=lambda _s: None) == 0
    assert calls == []
    assert store.pending.exists()


# ── failure ──────────────────────────────────────────────────────────────────


def test_drain_records_a_raising_refresh_and_keeps_going(store: Store):
    store.pending.write_text("a.py\nb.py\n", encoding="utf-8")
    seen: list[list[str]] = []

    def refresh(paths: list[str]) -> None:
        seen.append(paths)
        if "b.py" in paths:
            raise RuntimeError("boom")

    def sleep(_seconds: float) -> None:
        if len(seen) == 1:
            store.pending.write_text("c.py\n", encoding="utf-8")

    assert drain(store, refresh, debounce=0.0, sleep=sleep) == 2
    assert seen == [["a.py", "b.py"], ["c.py"]]
    assert not worker_alive(store)
    assert [n["text"] for n in take_notices(store)] == [
        "RuntimeError: boom while refreshing ['a.py', 'b.py']"
    ]
    assert take_notices(store) == []


def test_failure_output_surfaces_once_as_a_system_message(body):
    store = body.store_for(body.REPO_ROOT)
    assert body.failure_output("PreToolUse") is None
    record_failure(store, ["pkg/mod.py"], ValueError("bad db"))
    output = body.failure_output("PreToolUse")
    assert output is not None
    assert "ValueError: bad db" in output["systemMessage"]
    assert "pkg/mod.py" in output["hookSpecificOutput"]["additionalContext"]
    assert body.failure_output("PostToolUse") is None


# ── debounce / coalesce ──────────────────────────────────────────────────────


def test_drain_debounces_and_coalesces_into_one_refresh(store: Store):
    store.pending.write_text("a.py\n", encoding="utf-8")
    calls: list[list[str]] = []
    sleeps: list[float] = []

    def sleep(seconds: float) -> None:
        sleeps.append(seconds)
        if len(sleeps) == 1:
            with store.pending.open("a", encoding="utf-8") as handle:
                handle.write("b.py\na.py\n")

    assert drain(store, calls.append, debounce=0.25, sleep=sleep) == 1
    assert calls == [["a.py", "b.py"]]
    assert sleeps == [0.25, 0.25]


def test_hook_output_queues_only_graph_files(body, monkeypatch):
    queued: list[list[str]] = []
    monkeypatch.setattr(
        body, "request", lambda store, paths, **kw: queued.append(paths)
    )
    monkeypatch.setattr(body.shutil, "which", lambda name: "/usr/bin/code-review-graph")
    payload = {"tool_name": "Edit", "tool_input": {"file_path": "pkg/mod.py"}}
    assert body.hook_output(payload) == {
        "hookSpecificOutput": {"hookEventName": "PostToolUse"}
    }
    assert queued == [["pkg/mod.py"]]
    payload = {"tool_name": "Edit", "tool_input": {"file_path": "notes.md"}}
    assert body.hook_output(payload) == {
        "hookSpecificOutput": {"hookEventName": "PostToolUse"}
    }
    assert queued == [["pkg/mod.py"]]
    monkeypatch.setattr(body.shutil, "which", lambda name: None)
    context = body.full_update_output()["hookSpecificOutput"]["additionalContext"]
    assert "not installed" in context


# ── doctor ───────────────────────────────────────────────────────────────────


def test_status_and_doctor_report_a_stuck_lock(store: Store, held_lock, monkeypatch):
    from tooling.hooks.dispatch import doctor

    monkeypatch.setenv("CRG_DATA_DIR", str(store.root))
    now = store.lock.stat().st_mtime
    assert status(store, now=now)["stuck"] == ""
    stuck = status(store, now=now + 301)
    assert stuck["worker_alive"] is True
    assert "held" in stuck["stuck"]
    report = doctor.refresh_state_check(store.root.parent, now=now + 301)
    assert report.status == "DEAD"
    assert "stuck" in report.problems[0]
    line = doctor.render_state(report)
    assert line.startswith("graph-refresh | DEAD")


def test_doctor_warns_on_a_waiting_failure_and_orphaned_pending(
    store: Store, monkeypatch
):
    from tooling.hooks.dispatch import doctor

    monkeypatch.setenv("CRG_DATA_DIR", str(store.root))
    record_failure(store, ["a.py"], OSError("disk"))
    assert doctor.refresh_state_check(store.root.parent).status == "WARN"
    store.pending.write_text("a.py\n", encoding="utf-8")
    old = time.time() - 60
    os.utime(store.pending, (old, old))
    report = doctor.refresh_state_check(store.root.parent)
    assert report.status == "DEAD"
    assert "no worker" in report.problems[0]


# ── end to end ───────────────────────────────────────────────────────────────


def test_detached_worker_drains_and_wait_sees_fresh(store: Store, tmp_path: Path):
    out = tmp_path / "refreshed.txt"
    argv = [sys.executable, "-c", DRAIN_WORKER, str(store.root), str(out)]
    assert request(store, ["a.py"], worker_argv=argv, cwd=tmp_path) == "spawned"
    request(store, ["b.py"], worker_argv=argv, cwd=tmp_path)
    assert wait_for_fresh(store, timeout=10.0) == "fresh"
    assert not store.pending.exists()
    assert not worker_alive(store)
    batches = out.read_text(encoding="utf-8").splitlines()
    assert sorted(path for batch in batches for path in batch.split(",")) == [
        "a.py",
        "b.py",
    ]
    assert status(store)["failed_waiting"] is False
    assert json.loads(json.dumps(status(store)))["pending"] == 0


# ── bounded batch child, warnings, legacy wiring ─────────────────────────────

CHILD_SLEEP = "import time; time.sleep(5)"
CHILD_WARN = "print('embedding bridge unavailable: semantic search is stale')"
CHILD_FAIL = (
    "import sys; sys.exit('code-review-graph is not installed; graph NOT refreshed')"
)


def _child(monkeypatch, body, code: str) -> None:
    monkeypatch.setattr(
        body, "_batch_command", lambda paths: [sys.executable, "-c", code]
    )


def test_hung_batch_child_is_killed_and_recorded(body, monkeypatch):
    _child(monkeypatch, body, CHILD_SLEEP)
    monkeypatch.setattr(body, "BATCH_TIMEOUT_SECONDS", 0.3)
    store = body.store_for(body.REPO_ROOT)
    store.pending.write_text("pkg/mod.py\n", encoding="utf-8")
    started = time.monotonic()
    assert drain(store, body._refresh_batch, debounce=0.0, sleep=lambda _s: None) == 1
    assert time.monotonic() - started < 3.0
    assert not worker_alive(store)
    (notice,) = take_notices(store)
    assert notice["kind"] == "failure"
    assert "was killed" in notice["text"]


def test_batch_child_warning_reaches_the_next_hook_event(body, monkeypatch):
    _child(monkeypatch, body, CHILD_WARN)
    body._refresh_batch(["pkg/mod.py"])
    output = body.failure_output("PostToolUse")
    assert output is not None
    assert "semantic search is stale" in output["systemMessage"]
    assert "FAILED" not in output["systemMessage"]
    assert body.failure_output("PostToolUse") is None


def test_batch_child_failure_is_recorded_not_lost(body, monkeypatch):
    _child(monkeypatch, body, CHILD_FAIL)
    store = body.store_for(body.REPO_ROOT)
    store.pending.write_text("pkg/mod.py\n", encoding="utf-8")
    assert drain(store, body._refresh_batch, debounce=0.0, sleep=lambda _s: None) == 1
    output = body.failure_output("PreToolUse")
    assert output is not None
    assert "not installed" in output["systemMessage"]
    assert "STALE" in output["systemMessage"]


def test_legacy_wiring_refreshes_synchronously(body, monkeypatch, tmp_path, capsys):
    marker = tmp_path / "refreshed"
    _child(monkeypatch, body, f"open({str(marker)!r}, 'w').write('x')")
    monkeypatch.setattr(body.shutil, "which", lambda name: None)
    monkeypatch.setattr(sys, "argv", ["crg_graph_refresh.py"])
    payload = json.dumps(
        {"tool_name": "Edit", "tool_input": {"file_path": "pkg/mod.py"}}
    )
    monkeypatch.setattr(sys, "stdin", io.StringIO(payload))
    assert body.main() == 0
    assert marker.is_file()
    assert not body.store_for(body.REPO_ROOT).pending.exists()
    assert json.loads(capsys.readouterr().out) == {
        "hookSpecificOutput": {"hookEventName": "PostToolUse"}
    }
    _child(monkeypatch, body, CHILD_FAIL)
    context = body.sync_output(json.loads(payload))["hookSpecificOutput"][
        "additionalContext"
    ]
    assert "FAILED" in context and "not installed" in context
