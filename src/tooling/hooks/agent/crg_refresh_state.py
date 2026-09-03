"""Asynchronous, freshness-guarded code-review-graph refresh: the orchestration half.

State lives in the graph store directory, beside ``graph.db``:

- ``refresh.pending``    the "refresh pending" marker: one queued path per line
  (``*`` = whole-tree ``code-review-graph update``); absent when nothing is queued
- ``refresh.lock``       ``flock`` held by the one live worker for its whole life,
  so a dead worker can never leave it held (the kernel releases it)
- ``refresh.queue.lock`` ``flock`` serialising every edit of the marker with the
  worker's exit decision, so no request slips between its last check and its exit
- ``refresh.failed``     the last failed refreshes; read once by the next hook event
- ``refresh.log``        worker stderr

The hook path (:func:`request`) appends to the marker and spawns one detached
worker when none holds the lock — a few stats plus one fork. The worker
(:func:`drain`) debounces, coalesces every queued path into one refresh, repeats
while requests keep arriving and exits only when the queue is empty under the
queue lock. A graph query (:func:`wait_for_fresh`) blocks, bounded, until neither
marker nor worker remains, so a query never reads a graph an edit has outrun.
The refresh itself is code-review-graph's; this module never imports it.
"""

from __future__ import annotations

import contextlib
import fcntl
import json
import os
import subprocess
import sys
import time
from collections.abc import Callable, Iterator
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Final

FULL_UPDATE: Final[str] = "*"
DEBOUNCE_SECONDS: Final[float] = 0.25
WAIT_SECONDS: Final[float] = 3.0
POLL_SECONDS: Final[float] = 0.02
STUCK_WORKER_SECONDS: Final[float] = 300.0
ORPHANED_PENDING_SECONDS: Final[float] = 30.0


@dataclass(frozen=True)
class Store:
    """The marker and lock paths for one graph store directory."""

    root: Path

    @property
    def pending(self) -> Path:
        return self.root / "refresh.pending"

    @property
    def lock(self) -> Path:
        return self.root / "refresh.lock"

    @property
    def queue_lock(self) -> Path:
        return self.root / "refresh.queue.lock"

    @property
    def failed(self) -> Path:
        return self.root / "refresh.failed"

    @property
    def log(self) -> Path:
        return self.root / "refresh.log"


def store_for(repo_root: Path) -> Store:
    """``CRG_DATA_DIR`` when set, else ``<repo>/.code-review-graph`` (crg's default)."""
    override = os.environ.get("CRG_DATA_DIR", "").strip()
    root = Path(override).expanduser() if override else repo_root / ".code-review-graph"
    root.mkdir(parents=True, exist_ok=True)
    return Store(root.resolve())


@contextlib.contextmanager
def _flock(path: Path, *, blocking: bool) -> Iterator[int | None]:
    """Yield a locked fd, or ``None`` when non-blocking and another holder exists."""
    fd = os.open(path, os.O_RDWR | os.O_CREAT, 0o644)
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | (0 if blocking else fcntl.LOCK_NB))
        except BlockingIOError:
            yield None
            return
        yield fd
    finally:
        os.close(fd)


def worker_alive(store: Store) -> bool:
    """True while some process holds the worker lock."""
    with _flock(store.lock, blocking=False) as fd:
        return fd is None


def _pending_lines(store: Store) -> list[str]:
    try:
        text = store.pending.read_text(encoding="utf-8")
    except FileNotFoundError:
        return []
    return [line for line in text.splitlines() if line]


def _spawn(argv: list[str], store: Store, cwd: Path, env: dict[str, str]) -> None:
    with store.log.open("ab") as log:
        subprocess.Popen(  # noqa: S603 - detached worker, argv built by the caller
            argv,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=log,
            cwd=cwd,
            env=env,
            start_new_session=True,
            close_fds=True,
        )


def request(
    store: Store,
    paths: list[str],
    *,
    worker_argv: list[str],
    cwd: Path,
    env: dict[str, str] | None = None,
) -> str:
    """Queue *paths* and make sure a worker will drain them; returns immediately.

    The append and the lock probe happen under the queue lock: either the live
    worker sees the new lines before it decides to exit, or the lock is already
    free here and a new worker is spawned. Returns ``"spawned"`` or ``"queued"``.
    """
    with _flock(store.queue_lock, blocking=True):
        with store.pending.open("a", encoding="utf-8") as handle:
            handle.write("".join(f"{path}\n" for path in paths))
        if worker_alive(store):
            return "queued"
        _spawn(worker_argv, store, cwd, env if env is not None else dict(os.environ))
        return "spawned"


def take_pending(store: Store) -> list[str]:
    """Remove and return every queued path, coalesced, first occurrence first."""
    with _flock(store.queue_lock, blocking=True):
        lines = _pending_lines(store)
        store.pending.unlink(missing_ok=True)
    return list(dict.fromkeys(lines))


def _record(store: Store, kind: str, paths: list[str], text: str) -> None:
    line = json.dumps({"at": time.time(), "kind": kind, "paths": paths, "text": text})
    with store.failed.open("a", encoding="utf-8") as handle:
        handle.write(line + "\n")


def record_failure(store: Store, paths: list[str], exc: BaseException) -> None:
    """A refresh that raised: reported as STALE by the next hook event."""
    _record(store, "failure", paths, f"{type(exc).__name__}: {exc}")


def record_warning(store: Store, paths: list[str], text: str) -> None:
    """A refresh that succeeded with a warning (e.g. embeddings skipped)."""
    _record(store, "warning", paths, text)


def take_notices(store: Store) -> list[dict[str, Any]]:
    """Failures and warnings waiting for the next hook event, consumed; ``[]`` if none.

    Each item: ``{"kind": "failure"|"warning", "paths": [...], "text": "..."}``,
    text already phrased for a message (``<error> while refreshing [...]``).
    """
    try:
        text = store.failed.read_text(encoding="utf-8")
    except FileNotFoundError:
        return []
    store.failed.unlink(missing_ok=True)
    notices: list[dict[str, Any]] = []
    for raw in text.splitlines():
        try:
            item = json.loads(raw)
        except ValueError:
            notices.append({"kind": "failure", "paths": [], "text": raw})
            continue
        kind = item.get("kind", "failure")
        body = item.get("text", item.get("error"))
        suffix = f" while refreshing {item['paths']}" if kind == "failure" else ""
        notices.append(
            {"kind": kind, "paths": item.get("paths", []), "text": f"{body}{suffix}"}
        )
    return notices


def drain(
    store: Store,
    refresh: Callable[[list[str]], None],
    *,
    debounce: float = DEBOUNCE_SECONDS,
    sleep: Callable[[float], None] = time.sleep,
) -> int:
    """Worker body: hold the lock, debounce, refresh coalesced batches until idle.

    Returns the number of batches refreshed; ``0`` immediately when another
    worker already holds the lock. A refresh that raises is recorded through
    :func:`record_failure` and never stops the loop.
    """
    with _flock(store.lock, blocking=False) as fd:
        if fd is None:
            return 0
        os.ftruncate(fd, 0)
        os.write(fd, f"{os.getpid()} {time.time():.3f}\n".encode())
        batches = 0
        while True:
            sleep(debounce)
            paths = take_pending(store)
            if paths:
                try:
                    refresh(paths)
                except Exception as exc:  # noqa: BLE001 - reported, never swallowed
                    record_failure(store, paths, exc)
                batches += 1
                continue
            with _flock(store.queue_lock, blocking=True):
                if not _pending_lines(store):
                    fcntl.flock(fd, fcntl.LOCK_UN)
                    return batches


def wait_for_fresh(
    store: Store,
    *,
    timeout: float = WAIT_SECONDS,
    poll: float = POLL_SECONDS,
    clock: Callable[[], float] = time.monotonic,
    sleep: Callable[[float], None] = time.sleep,
    respawn: Callable[[], None] | None = None,
) -> str:
    """Block until no refresh is pending or running, bounded by *timeout*.

    Returns ``"fresh"`` when the graph is current, ``"timeout"`` when a refresh
    still runs at the deadline. A marker with no live worker (the worker died
    before draining) calls *respawn* once so the wait can still succeed.
    """
    deadline = clock() + timeout
    respawned = False
    while True:
        pending = store.pending.exists()
        alive = worker_alive(store)
        if not pending and not alive:
            return "fresh"
        if pending and not alive and respawn is not None and not respawned:
            respawn()
            respawned = True
        if clock() >= deadline:
            return "timeout"
        sleep(poll)


def _age(path: Path, now: float) -> float | None:
    try:
        return now - path.stat().st_mtime
    except FileNotFoundError:
        return None


def status(store: Store, *, now: float | None = None) -> dict[str, object]:
    """Doctor view: what is queued, whether a worker holds the lock, and if it is stuck."""
    now = time.time() if now is None else now
    pending = _pending_lines(store)
    alive = worker_alive(store)
    lock_age = _age(store.lock, now) if alive else None
    pending_age = _age(store.pending, now)
    stuck = ""
    if alive and lock_age is not None and lock_age > STUCK_WORKER_SECONDS:
        stuck = f"worker has held {store.lock} for {lock_age:.0f}s"
    elif (
        pending
        and not alive
        and pending_age is not None
        and pending_age > ORPHANED_PENDING_SECONDS
    ):
        stuck = f"{len(pending)} queued path(s) with no worker for {pending_age:.0f}s"
    return {
        "store": str(store.root),
        "pending": len(pending),
        "worker_alive": alive,
        "lock_age_s": lock_age,
        "failed_waiting": store.failed.exists(),
        "stuck": stuck,
    }


def worker_command(body: Path, repo_root: Path) -> list[str]:
    return [sys.executable, str(body), "--worker", "--repo", str(repo_root)]
