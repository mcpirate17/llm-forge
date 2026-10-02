"""Explicit, finite A2A retry supervision; only owned children are stopped."""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import json
import os
import signal
import subprocess
import time
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from conductor.a2a_registry import (
    A2aError,
    AgentRecord,
    _atomic_json,
    _utc_now,
    load_registry,
)
from conductor.a2a_session_start import (
    _card_is_valid,
    _port_is_open,
    _stop_process,
    sender_flush_command,
    serve_command,
)


@dataclass(frozen=True)
class SupervisorConfig:
    state_dir: Path
    identity: str
    duration: float = 300
    interval: float = 5
    max_messages: int = 100
    serve: bool = False
    max_restarts: int = 3

    def validate(self) -> None:
        if not 1 <= self.duration <= 3600:
            raise A2aError("supervisor duration must be between 1 and 3600 seconds")
        if not 0.1 <= self.interval <= 60:
            raise A2aError("supervisor interval must be between 0.1 and 60 seconds")
        if not 1 <= self.max_messages <= 1000:
            raise A2aError("max_messages must be between 1 and 1000")
        if not 0 <= self.max_restarts <= 10:
            raise A2aError("max_restarts must be between 0 and 10")


@contextlib.contextmanager
def _lease(directory: Path) -> Iterator[None]:
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (directory / ".supervisor.lock").open("a+") as handle:
        os.chmod(handle.name, 0o600)
        try:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise A2aError(f"supervisor already active for {directory.name!r}") from exc
        try:
            yield
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def _start_endpoint(
    config: SupervisorConfig, record: AgentRecord, deadline: float
) -> subprocess.Popen[bytes] | None:
    if _card_is_valid(record):
        return None
    if _port_is_open(record):
        raise A2aError(f"port {record.port} is occupied by an invalid endpoint")
    log_path = config.state_dir / config.identity / "supervised-serve.log"
    with log_path.open("ab") as log:
        os.chmod(log_path, 0o600)
        process = subprocess.Popen(
            serve_command(identity=config.identity, state_dir=config.state_dir),
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
        )
    try:
        ready_by = min(deadline, time.monotonic() + 5)
        while time.monotonic() < ready_by:
            if process.poll() is not None:
                raise A2aError(
                    f"owned A2A endpoint exited {process.returncode}; inspect {log_path}"
                )
            if _card_is_valid(record):
                return process
            time.sleep(min(0.05, max(0, ready_by - time.monotonic())))
        raise A2aError(f"owned A2A endpoint did not become ready; inspect {log_path}")
    except (A2aError, OSError, KeyboardInterrupt):
        _stop_process(process)
        raise


def _flush(config: SupervisorConfig, remaining: float) -> dict[str, Any]:
    command = sender_flush_command(identity=config.identity, state_dir=config.state_dir)
    command.extend(["--max-messages", str(config.max_messages)])
    try:
        result = subprocess.run(
            command,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=min(15.0, remaining),
            check=False,
        )
    except subprocess.TimeoutExpired:
        return {"status": "timeout", "detail": "pending sends are retained for retry"}
    if result.returncode not in (0, 3):
        raise A2aError(
            f"A2A flush exited {result.returncode}: {result.stderr.strip()[:500]}"
        )
    rows = json.loads(result.stdout)
    if not isinstance(rows, list):
        raise A2aError("A2A flush returned a non-list receipt")
    counts = {
        status: sum(row["status"] == status for row in rows)
        for status in ("delivered", "queued", "failed")
    }
    return {"status": "complete", "exit_code": result.returncode, "counts": counts}


def supervise(config: SupervisorConfig) -> dict[str, Any]:
    """Run foreground until the budget ends; retain queue and lifecycle evidence."""
    config.validate()
    records = load_registry(config.state_dir)
    if config.identity not in records:
        raise A2aError(f"unknown supervisor identity {config.identity!r}")
    directory = config.state_dir / config.identity
    with _lease(directory):
        return _run(config, records[config.identity], directory / "supervisor.json")


def _run(
    config: SupervisorConfig, record: AgentRecord, state_path: Path
) -> dict[str, Any]:
    deadline = time.monotonic() + config.duration
    process: subprocess.Popen[bytes] | None = None
    starts = 0
    state: dict[str, Any] = {
        "schema_version": 1,
        "identity": config.identity,
        "pid": os.getpid(),
        "started_at": _utc_now(),
        "duration_seconds": config.duration,
        "status": "running",
        "cycles": 0,
        "endpoint_pid": None,
    }
    _atomic_json(state_path, state)
    from conductor.a2a_wait import MailboxWakeup

    wakeup = MailboxWakeup(state_path.parent)
    last_written = time.monotonic()
    previous_flush: dict[str, Any] | None = None
    try:
        while time.monotonic() < deadline:
            if process is not None and process.poll() is not None:
                process = None
            if config.serve and process is None and not _card_is_valid(record):
                if starts > config.max_restarts:
                    raise A2aError("owned A2A endpoint exhausted restart budget")
                process = _start_endpoint(config, record, deadline)
                starts += int(process is not None)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            state["last_flush"] = _flush(config, remaining)
            state.update(
                cycles=state["cycles"] + 1,
                updated_at=_utc_now(),
                endpoint_pid=process.pid if process else None,
            )
            changed = state["last_flush"] != previous_flush
            flush = state["last_flush"]
            idle = flush.get("exit_code") == 0 and not any(
                flush.get("counts", {}).values()
            )
            if idle and previous_flush is None:
                changed = False
            if changed or time.monotonic() - last_written >= 60:
                _atomic_json(state_path, state)
                last_written = time.monotonic()
            previous_flush = state["last_flush"]
            interval = 60.0 if idle else config.interval
            wakeup.wait(min(interval, max(0, deadline - time.monotonic())))
        state["status"] = "completed"
    except KeyboardInterrupt:
        state["status"] = "interrupted"
        raise
    except (A2aError, OSError, ValueError) as exc:
        state.update(status="failed", error=str(exc)[:500])
        raise
    finally:
        wakeup.close()
        if process is not None:
            _stop_process(process)
        state.update(ended_at=_utc_now(), endpoint_pid=None)
        _atomic_json(state_path, state)
    return state


def add_parser(sub: argparse._SubParsersAction[Any]) -> None:
    parser = sub.add_parser("supervise", help="run finite foreground delivery retries")
    parser.add_argument("--as-name", required=True)
    parser.add_argument(
        "--duration", type=float, default=300, help="seconds, maximum 3600"
    )
    parser.add_argument("--interval", type=float, default=5)
    parser.add_argument("--max-messages", type=int, default=100)
    parser.add_argument(
        "--serve", action="store_true", help="own an endpoint only while running"
    )
    parser.add_argument("--max-restarts", type=int, default=3)


def command(args: argparse.Namespace) -> int:
    config = SupervisorConfig(
        state_dir=args.state_dir.resolve(),
        identity=args.as_name,
        duration=args.duration,
        interval=args.interval,
        max_messages=args.max_messages,
        serve=args.serve,
        max_restarts=args.max_restarts,
    )

    def terminate(_signal: int, _frame: object) -> None:
        raise KeyboardInterrupt

    previous = signal.signal(signal.SIGTERM, terminate)
    try:
        try:
            result = supervise(config)
        except KeyboardInterrupt:
            return 130
    finally:
        signal.signal(signal.SIGTERM, previous)
    print(json.dumps(result, sort_keys=True))
    return 0
