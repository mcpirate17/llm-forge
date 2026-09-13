"""Execution engine: run every matching hook once, in parallel, bounded, captured.

In-process adapters run on daemon threads with thread-local stdin/stdout so a
body that ``print``s its JSON or ``json.load(sys.stdin)``s the payload works
unchanged; a subprocess body gets the raw payload on its stdin. Each hook has
its own timeout; a hook that raises, times out, exits non-zero or writes
non-JSON becomes a ``HookOutcome`` with ``error`` set, never a silent skip.
"""

from __future__ import annotations

import io
import json
import os
import subprocess
import sys
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from tooling.hooks.dispatch import adapters
from tooling.hooks.dispatch.merge import HookOutcome, merge
from tooling.hooks.dispatch.paths import body_path, interpreter_bin
from tooling.hooks.dispatch.registry import HookSpec, hooks_for, natively_served


@dataclass
class Context:
    root: Path
    event: str
    payload: dict[str, Any]
    raw: bytes
    env: dict[str, str] = field(default_factory=dict)


class _ThreadOut:
    """``sys.stdout`` stand-in: each bound thread writes to its own buffer."""

    def __init__(self) -> None:
        self._local = threading.local()
        self.fallback: Any = None

    def bind(self) -> io.StringIO:
        self._local.buffer = io.StringIO()
        return self._local.buffer

    def write(self, text: str) -> int:
        buffer = getattr(self._local, "buffer", None)
        if buffer is not None:
            return buffer.write(text)
        if (
            self.fallback is not None
            and threading.current_thread() is threading.main_thread()
        ):
            return self.fallback.write(text)
        raise RuntimeError(
            "hook stdout written from a thread with no bound buffer "
            f"({threading.current_thread().name}): output would be lost"
        )

    def flush(self) -> None:
        return None

    def isatty(self) -> bool:
        return False


class _ThreadIn:
    """``sys.stdin`` stand-in: each bound thread reads its own payload once."""

    def __init__(self) -> None:
        self._local = threading.local()

    def bind(self, text: str) -> None:
        self._local.stream = io.StringIO(text)

    def _stream(self) -> io.StringIO:
        stream = getattr(self._local, "stream", None)
        return stream if stream is not None else io.StringIO()

    def read(self, size: int = -1) -> str:
        return self._stream().read(size)

    def readline(self, size: int = -1) -> str:
        return self._stream().readline(size)

    def isatty(self) -> bool:
        return False


_OUT = _ThreadOut()
_IN = _ThreadIn()


def install_thread_io() -> None:
    """Route stdout/stdin through per-thread buffers; the main thread keeps the real stdout."""
    if sys.stdout is not _OUT:
        _OUT.fallback = sys.stdout
        sys.stdout = _OUT  # type: ignore[assignment]
    if sys.stdin is not _IN:
        sys.stdin = _IN  # type: ignore[assignment]


def subject_of(event: str, payload: dict[str, Any]) -> str:
    if event == "SessionStart":
        return str(payload.get("source") or "")
    if event == "SessionEnd":
        return str(payload.get("reason") or "")
    return str(payload.get("tool_name") or payload.get("toolName") or "")


def select(event: str, payload: dict[str, Any]) -> tuple[HookSpec, ...]:
    """Hooks to run for this event/payload: registry matches minus whatever
    ``FORGE_NATIVE_HOOKS`` says a caller already served natively (see
    ``registry.natively_served`` -- dormant by default, opt-in per hook name).
    """
    subject = subject_of(event, payload)
    served = natively_served()
    return tuple(
        spec
        for spec in hooks_for(event)
        if spec.matches(subject) and spec.name not in served
    )


def parse_output(text: str) -> tuple[dict[str, Any] | None, str | None]:
    if not text.strip():
        return None, None
    try:
        data = json.loads(text)
    except ValueError:
        return None, f"stdout is not JSON: {text.strip()[:200]!r}"
    if not isinstance(data, dict):
        return None, f"stdout JSON is not an object: {text.strip()[:200]!r}"
    return data, None


def _run_subprocess(
    spec: HookSpec, ctx: Context
) -> tuple[dict[str, Any] | None, str | None]:
    body = body_path(ctx.root, spec.argv[0])
    argv = [sys.executable, str(body)] if body.suffix == ".py" else [str(body)]
    argv.extend(spec.argv[1:])
    proc = subprocess.run(
        argv,
        input=ctx.raw,
        capture_output=True,
        cwd=ctx.root,
        env=ctx.env,
        timeout=spec.timeout,
        check=False,
    )
    stdout = proc.stdout.decode("utf-8", "replace")
    stderr = proc.stderr.decode("utf-8", "replace")
    if stderr.strip():
        sys.stderr.write(stderr)
    if proc.returncode != 0:
        return None, f"exit {proc.returncode}: {stderr.strip()[-300:] or '<no stderr>'}"
    return parse_output(stdout)


def _run_adapter(
    spec: HookSpec, ctx: Context
) -> tuple[dict[str, Any] | None, str | None]:
    install_thread_io()
    buffer = _OUT.bind()
    _IN.bind(ctx.raw.decode("utf-8", "replace"))
    fn = getattr(adapters, spec.adapter)
    error: str | None = None
    returned: Any = None
    try:
        returned = fn(ctx)
    except SystemExit as exc:
        if exc.code not in (None, 0):
            error = f"SystemExit({exc.code})"
    if isinstance(returned, dict):
        return returned, error
    output, parse_error = parse_output(buffer.getvalue())
    return output, error or parse_error


def run_one(spec: HookSpec, ctx: Context) -> HookOutcome:
    started = time.perf_counter()
    try:
        if spec.adapter:
            output, error = _run_adapter(spec, ctx)
        else:
            output, error = _run_subprocess(spec, ctx)
    except subprocess.TimeoutExpired:
        output, error = None, f"timed out after {spec.timeout}s"
    except BaseException as exc:  # noqa: BLE001 - every failure must surface
        output, error = None, f"{type(exc).__name__}: {exc}"
    return HookOutcome(
        spec.name,
        output,
        error,
        spec.fail_closed,
        (time.perf_counter() - started) * 1000,
    )


def run_all(specs: tuple[HookSpec, ...], ctx: Context) -> list[HookOutcome]:
    install_thread_io()
    boxes: list[dict[str, HookOutcome]] = [{} for _ in specs]
    threads: list[threading.Thread] = []
    started = time.perf_counter()
    for spec, box in zip(specs, boxes, strict=True):

        def target(spec: HookSpec = spec, box: dict[str, HookOutcome] = box) -> None:
            box["outcome"] = run_one(spec, ctx)

        thread = threading.Thread(target=target, name=spec.name, daemon=True)
        thread.start()
        threads.append(thread)
    outcomes: list[HookOutcome] = []
    for spec, thread, box in zip(specs, threads, boxes, strict=True):
        thread.join(max(0.0, spec.timeout - (time.perf_counter() - started)))
        outcome = box.get("outcome")
        if thread.is_alive() or outcome is None:
            outcome = HookOutcome(
                spec.name,
                None,
                f"timed out after {spec.timeout}s",
                spec.fail_closed,
                (time.perf_counter() - started) * 1000,
            )
        outcomes.append(outcome)
    return outcomes


def build_context(event: str, raw: bytes, root: Path) -> Context:
    try:
        payload = json.loads(raw)
    except ValueError:
        payload = {}
    if not isinstance(payload, dict):
        payload = {}
    env = dict(os.environ)
    env["PROJECT_DIR"] = str(root)
    env.setdefault("CLAUDE_PROJECT_DIR", str(root))
    # Shell bodies call ``python3 -m conductor.*``: resolve it to this interpreter
    # first, the one that carries the tooling in a foreign project.
    env["PATH"] = os.pathsep.join([interpreter_bin(), *filter(None, [env.get("PATH")])])
    os.environ["PROJECT_DIR"] = str(root)
    return Context(root, event, payload, raw, env)


def dispatch(
    event: str, raw: bytes, root: Path
) -> tuple[dict[str, Any], list[HookOutcome]]:
    ctx = build_context(event, raw, root)
    outcomes = run_all(select(event, ctx.payload), ctx)
    return merge(event, outcomes), outcomes
