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
from tooling.hooks.dispatch.registry import (
    HookSpec,
    hooks_for,
    native_answers,
    natively_served,
)


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
    name = str(payload.get("tool_name") or payload.get("toolName") or "")
    return "Bash" if _is_shell_tool(name) else name


def _is_shell_tool(name: str) -> bool:
    """Recognize provider shell spellings without matching arbitrary MCP tools."""
    return name.lower().removeprefix("functions.") in {
        "bash",
        "shell",
        "exec_command",
        "shell_command",
        "run_shell_command",
        "run_terminal_command",
    }


class _ShellPayloadError(ValueError):
    """A recognized shell tool supplied an unsupported command payload."""


def _normalize_shell_payload(event: str, payload: dict[str, Any]) -> dict[str, Any]:
    """Give all shell hooks one input shape, including bodies that read stdin."""
    if event not in {"PreToolUse", "PostToolUse"}:
        return payload
    name = str(payload.get("tool_name") or payload.get("toolName") or "")
    if not _is_shell_tool(name):
        return payload
    tool_input = payload.get("tool_input")
    if not isinstance(tool_input, dict):
        raise _ShellPayloadError(f"shell tool {name!r} requires an object tool_input")
    commands = [tool_input[key] for key in ("command", "cmd") if key in tool_input]
    if not commands or any(not isinstance(command, str) for command in commands):
        raise _ShellPayloadError(
            f"shell tool {name!r} requires a string tool_input.command or cmd; "
            "argv arrays are unsupported"
        )
    if len(commands) == 2 and commands[0] != commands[1]:
        raise _ShellPayloadError(
            f"shell tool {name!r} has conflicting command and cmd values"
        )
    return {
        **payload,
        "tool_name": "Bash",
        "tool_input": {**tool_input, "command": commands[0]},
    }


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
    normalized = _normalize_shell_payload(event, payload)
    if normalized != payload:
        raw = json.dumps(normalized).encode("utf-8")
        payload = normalized
    env = dict(os.environ)
    env["PROJECT_DIR"] = str(root)
    env.setdefault("CLAUDE_PROJECT_DIR", str(root))
    # Shell bodies call ``python3 -m conductor.*``: resolve it to this interpreter
    # first, the one that carries the tooling in a foreign project.
    env["PATH"] = os.pathsep.join([interpreter_bin(), *filter(None, [env.get("PATH")])])
    os.environ["PROJECT_DIR"] = str(root)
    return Context(root, event, payload, raw, env)


def _splice_native_outcomes(
    event: str, payload: dict[str, Any], outcomes: list[HookOutcome]
) -> list[HookOutcome]:
    """Adds one `HookOutcome` per hook `select()` dropped as natively served,
    carrying forge's precomputed answer instead of re-running its adapter, then
    restores registry order -- `merge()`'s decision algebra is order-dependent,
    so a served hook must land back where it would have run, not at the end.

    Fails loud (`ValueError`) if a served name has no matching answer: a served
    hook silently vanishing from the merged result, with no trace anywhere, is
    exactly the bug this splice exists to prevent.
    """
    served = natively_served()
    if not served:
        return outcomes
    subject = subject_of(event, payload)
    all_specs = hooks_for(event)
    served_specs = [
        spec for spec in all_specs if spec.matches(subject) and spec.name in served
    ]
    if not served_specs:
        return outcomes
    answers = native_answers()
    spliced = list(outcomes)
    for spec in served_specs:
        if spec.name not in answers:
            raise ValueError(
                f"FORGE_NATIVE_HOOKS names {spec.name!r} as served natively for "
                f"event {event!r}, but FORGE_NATIVE_ANSWERS has no entry for it "
                "-- a served hook must never run twice and never be dropped "
                "without a native replacement"
            )
        spliced.append(
            HookOutcome(spec.name, answers[spec.name], None, spec.fail_closed, 0.0)
        )
    order = {spec.name: index for index, spec in enumerate(all_specs)}
    spliced.sort(key=lambda outcome: order.get(outcome.name, len(order)))
    return spliced


def dispatch(
    event: str, raw: bytes, root: Path
) -> tuple[dict[str, Any], list[HookOutcome]]:
    try:
        ctx = build_context(event, raw, root)
    except _ShellPayloadError as exc:
        if event != "PreToolUse":
            raise
        outcome = HookOutcome(
            "shell_input",
            {
                "hookSpecificOutput": {
                    "permissionDecision": "deny",
                    "permissionDecisionReason": str(exc),
                }
            },
            None,
            True,
            0.0,
        )
        return merge(event, [outcome]), [outcome]
    outcomes = run_all(select(event, ctx.payload), ctx)
    outcomes = _splice_native_outcomes(event, ctx.payload, outcomes)
    return merge(event, outcomes), outcomes
