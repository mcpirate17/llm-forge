#!/usr/bin/env python3
"""Provider-neutral A2A session bootstrap with a bounded inbox view.

The transport and durable journal remain lossless.  This entrypoint starts the
named local endpoint when necessary, retries only that sender's queued
messages, and asks :mod:`conductor.agent_a2a` for its compact JSON view.  It
never falls back to the full ``inbox`` output.
"""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import hashlib
import json
import os
import socket
import stat
import subprocess
import sys
import time
from collections.abc import Iterator, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Final

import httpx

from conductor.agent_a2a import (
    BIND_HOST,
    DEFAULT_STATE_DIR,
    IDENTITY_RE,
    A2aError,
    AgentRecord,
    fetch_card,
    load_registry,
)

from conductor.project_paths import host_root
ROOT: Final[Path] = host_root()
IDENTITY_ENV: Final[str] = "A2A_AGENT_NAME"
DEFAULT_MAX_MESSAGES: Final[int] = 8
DEFAULT_PREVIEW_CHARS: Final[int] = 140
DEFAULT_MAX_CHARS: Final[int] = 1200
MIN_PREVIEW_CHARS: Final[int] = 32
MIN_MAX_CHARS: Final[int] = 256
MAX_MAX_MESSAGES: Final[int] = 8
MAX_PREVIEW_CHARS: Final[int] = 140
MAX_MAX_CHARS: Final[int] = 1200
DEFAULT_SERVE_TIMEOUT_S: Final[float] = 5.0
COMMAND_TIMEOUT_S: Final[float] = 15.0
ONCE_MARKER_MAX_AGE_S: Final[int] = 24 * 60 * 60
PROVIDER_EVENT: Final[dict[str, str]] = {
    "codex": "SessionStart",
    "claude": "SessionStart",
    "qwen": "SessionStart",
    # Grok ignores SessionStart stdout.  The installer therefore invokes this
    # from UserPromptSubmit, once per session, where hook output is consumable.
    "grok": "UserPromptSubmit",
}
FORBIDDEN_COMPACT_KEYS: Final[frozenset[str]] = frozenset({"body", "data", "data_json"})
COMPACT_ENVELOPE_KEYS: Final[frozenset[str]] = frozenset(
    {
        "schema_version",
        "authority",
        "agent",
        "unread_only",
        "total",
        "shown",
        "omitted",
        "raw_bytes_not_injected",
        "messages",
    }
)
COMPACT_MESSAGE_KEYS: Final[frozenset[str]] = frozenset(
    {
        "id",
        "from",
        "at",
        "thread",
        "status",
        "requires_response",
        "summary",
        "raw_bytes",
    }
)


class SessionStartError(RuntimeError):
    """A2A startup could not complete without violating its bounded contract."""


@dataclass(frozen=True)
class StartupResult:
    """Structured result from one session bootstrap."""

    identity: str
    serve_status: str
    flush_status: int
    preview: dict[str, Any]
    preview_text: str


def resolve_identity(
    explicit: str | None, environ: Mapping[str, str] | None = None
) -> str:
    """Resolve an explicit identity first, then ``A2A_AGENT_NAME``."""

    source = (
        explicit if explicit is not None else (environ or os.environ).get(IDENTITY_ENV)
    )
    if source is None:
        raise SessionStartError(
            f"A2A identity missing; pass --identity or set {IDENTITY_ENV}"
        )
    identity = source.strip()
    if not identity or IDENTITY_RE.fullmatch(identity) is None:
        raise SessionStartError(f"invalid A2A identity {source!r}")
    return identity


def validate_preview_bounds(
    max_messages: int, preview_chars: int, max_chars: int
) -> None:
    """Reject a startup request that exceeds the repository context budget."""

    bounds = (
        ("max_messages", max_messages, 1, MAX_MAX_MESSAGES),
        (
            "preview_chars",
            preview_chars,
            MIN_PREVIEW_CHARS,
            MAX_PREVIEW_CHARS,
        ),
        ("max_chars", max_chars, MIN_MAX_CHARS, MAX_MAX_CHARS),
    )
    for label, value, lower, upper in bounds:
        if isinstance(value, bool) or not lower <= value <= upper:
            raise SessionStartError(
                f"{label} must be between {lower} and {upper}, got {value}"
            )


def compact_inbox_command(
    *,
    identity: str,
    state_dir: Path,
    max_messages: int = DEFAULT_MAX_MESSAGES,
    preview_chars: int = DEFAULT_PREVIEW_CHARS,
    max_chars: int = DEFAULT_MAX_CHARS,
    interpreter: str = sys.executable,
) -> list[str]:
    """Return the sole allowed session-start inbox command."""

    validate_preview_bounds(max_messages, preview_chars, max_chars)
    return [
        interpreter,
        "-m",
        "conductor.agent_a2a",
        "--state-dir",
        str(state_dir),
        "inbox",
        "--as-name",
        identity,
        "--unread",
        "--compact",
        "--max-messages",
        str(max_messages),
        "--preview-chars",
        str(preview_chars),
        "--max-chars",
        str(max_chars),
        "--json",
    ]


def sender_flush_command(
    *, identity: str, state_dir: Path, interpreter: str = sys.executable
) -> list[str]:
    """Build a flush command scoped to exactly one sender."""

    return [
        interpreter,
        "-m",
        "conductor.agent_a2a",
        "--state-dir",
        str(state_dir),
        "flush",
        "--as-name",
        identity,
    ]


def serve_command(
    *, identity: str, state_dir: Path, interpreter: str = sys.executable
) -> list[str]:
    """Build the detached endpoint command for one registered identity."""

    return [
        interpreter,
        "-m",
        "conductor.agent_a2a",
        "--state-dir",
        str(state_dir),
        "serve",
        "--name",
        identity,
    ]


def _port_is_open(record: AgentRecord) -> bool:
    try:
        with socket.create_connection((BIND_HOST, record.port), timeout=0.3):
            return True
    except OSError:
        return False


def _card_is_valid(record: AgentRecord) -> bool:
    try:
        fetch_card(record, timeout=0.5)
        return True
    except (A2aError, httpx.HTTPError, OSError, ValueError):
        return False


def _stop_process(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=1.0)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=1.0)


def ensure_serve(
    *,
    identity: str,
    state_dir: Path,
    timeout_s: float = DEFAULT_SERVE_TIMEOUT_S,
    interpreter: str = sys.executable,
) -> str:
    """Ensure the registered identity has a valid endpoint, without port takeover."""

    if timeout_s <= 0:
        raise SessionStartError(f"serve timeout must be positive, got {timeout_s}")
    try:
        records = load_registry(state_dir)
    except (A2aError, OSError, ValueError) as exc:
        raise SessionStartError(f"cannot load A2A registry: {exc}") from exc
    record = records.get(identity)
    if record is None:
        raise SessionStartError(
            f"identity {identity!r} is not registered in {state_dir / 'agents.json'}"
        )
    if _card_is_valid(record):
        return "already-running"
    if _port_is_open(record):
        raise SessionStartError(
            f"port {record.port} for {identity!r} is occupied by an invalid endpoint"
        )

    endpoint_dir = state_dir / identity
    endpoint_dir.mkdir(parents=True, exist_ok=True)
    os.chmod(endpoint_dir, stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR)
    log_path = endpoint_dir / "serve.log"
    try:
        with log_path.open("ab") as log_handle:
            os.chmod(log_path, stat.S_IRUSR | stat.S_IWUSR)
            process = subprocess.Popen(
                serve_command(
                    identity=identity, state_dir=state_dir, interpreter=interpreter
                ),
                cwd=ROOT,
                stdin=subprocess.DEVNULL,
                stdout=log_handle,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
    except OSError as exc:
        raise SessionStartError(
            f"cannot start A2A serve for {identity!r}: {exc}"
        ) from exc

    deadline = time.monotonic() + timeout_s
    child_exit: int | None = None
    while time.monotonic() < deadline:
        if _card_is_valid(record):
            return "started" if child_exit is None else "already-running"
        polled = process.poll()
        if polled is not None:
            # A concurrent bootstrap can win the bind after this child loses.
            # Keep probing until the same readiness deadline instead of
            # failing during that small bind/card publication window.
            child_exit = polled
        time.sleep(0.05)

    _stop_process(process)
    exit_detail = f" (child exited {child_exit})" if child_exit is not None else ""
    raise SessionStartError(
        f"A2A serve for {identity!r} did not become ready within {timeout_s:g}s"
        f"{exit_detail}; "
        f"inspect {log_path}"
    )


def flush_sender_queue(
    *, identity: str, state_dir: Path, interpreter: str = sys.executable
) -> int:
    """Flush only ``identity`` and return 0 or the expected still-queued code 3."""

    try:
        completed = subprocess.run(
            sender_flush_command(
                identity=identity, state_dir=state_dir, interpreter=interpreter
            ),
            cwd=ROOT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            timeout=COMMAND_TIMEOUT_S,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise SessionStartError(f"sender-scoped A2A flush failed: {exc}") from exc
    if completed.returncode not in (0, 3):
        detail = completed.stderr.strip()[:500]
        raise SessionStartError(
            f"sender-scoped A2A flush exited {completed.returncode}: {detail}"
        )
    return completed.returncode


def _non_negative_int(value: Any, *, field: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise SessionStartError(
            f"compact envelope {field} must be a non-negative integer"
        )
    return value


def _reject_raw_content_keys(value: Any, *, path: str = "envelope") -> None:
    if isinstance(value, dict):
        for key, nested in value.items():
            if key in FORBIDDEN_COMPACT_KEYS:
                raise SessionStartError(
                    f"compact envelope contains forbidden raw-content key {path}.{key}"
                )
            _reject_raw_content_keys(nested, path=f"{path}.{key}")
    elif isinstance(value, list):
        for index, nested in enumerate(value):
            _reject_raw_content_keys(nested, path=f"{path}[{index}]")


def validate_compact_envelope(
    payload: Any,
    *,
    identity: str,
    max_messages: int,
    preview_chars: int,
) -> dict[str, Any]:
    """Validate the complete compact-inbox trust boundary before injection."""

    if not isinstance(payload, dict):
        raise SessionStartError("compact envelope must be a JSON object")
    _reject_raw_content_keys(payload)
    if set(payload) != COMPACT_ENVELOPE_KEYS:
        raise SessionStartError("compact envelope keys do not match schema version 1")
    schema_version = payload.get("schema_version")
    if (
        isinstance(schema_version, bool)
        or not isinstance(schema_version, int)
        or schema_version != 1
    ):
        raise SessionStartError("compact envelope schema_version must be integer 1")
    if payload.get("authority") != "bounded-a2a-inbox":
        raise SessionStartError("compact envelope authority is not bounded-a2a-inbox")
    if payload.get("agent") != identity:
        raise SessionStartError(
            f"compact envelope agent {payload.get('agent')!r} does not match {identity!r}"
        )
    if payload.get("unread_only") is not True:
        raise SessionStartError("compact envelope unread_only must be true")

    total = _non_negative_int(payload.get("total"), field="total")
    shown = _non_negative_int(payload.get("shown"), field="shown")
    omitted = _non_negative_int(payload.get("omitted"), field="omitted")
    total_raw_bytes = _non_negative_int(
        payload.get("raw_bytes_not_injected"), field="raw_bytes_not_injected"
    )
    messages = payload.get("messages")
    if not isinstance(messages, list):
        raise SessionStartError("compact envelope messages must be a JSON array")
    if shown != len(messages):
        raise SessionStartError(
            f"compact envelope shown={shown} does not match {len(messages)} messages"
        )
    if shown > max_messages:
        raise SessionStartError(
            f"compact envelope shown={shown} exceeds requested maximum {max_messages}"
        )
    if total != shown + omitted:
        raise SessionStartError(
            "compact envelope count mismatch: total must equal shown plus omitted"
        )

    shown_raw_bytes = 0
    text_fields = ("id", "from", "at", "thread", "status", "summary")
    for index, message in enumerate(messages):
        if not isinstance(message, dict):
            raise SessionStartError(
                f"compact envelope messages[{index}] must be an object"
            )
        if set(message) != COMPACT_MESSAGE_KEYS:
            raise SessionStartError(
                f"compact envelope messages[{index}] keys do not match schema version 1"
            )
        for field in text_fields:
            if not isinstance(message.get(field), str):
                raise SessionStartError(
                    f"compact envelope messages[{index}].{field} must be a string"
                )
        if not isinstance(message.get("requires_response"), bool):
            raise SessionStartError(
                f"compact envelope messages[{index}].requires_response must be boolean"
            )
        summary = message["summary"]
        if len(summary) > preview_chars:
            raise SessionStartError(
                f"compact envelope messages[{index}].summary exceeds {preview_chars} chars"
            )
        shown_raw_bytes += _non_negative_int(
            message.get("raw_bytes"), field=f"messages[{index}].raw_bytes"
        )
    if total_raw_bytes < shown_raw_bytes:
        raise SessionStartError(
            "compact envelope raw_bytes_not_injected is smaller than shown raw bytes"
        )
    if omitted == 0 and total_raw_bytes != shown_raw_bytes:
        raise SessionStartError(
            "compact envelope raw byte mismatch when no messages are omitted"
        )
    return payload


def request_compact_preview(
    *,
    identity: str,
    state_dir: Path,
    max_messages: int = DEFAULT_MAX_MESSAGES,
    preview_chars: int = DEFAULT_PREVIEW_CHARS,
    max_chars: int = DEFAULT_MAX_CHARS,
    interpreter: str = sys.executable,
) -> tuple[dict[str, Any], str]:
    """Request and validate compact JSON; never retry with the full inbox view."""

    command = compact_inbox_command(
        identity=identity,
        state_dir=state_dir,
        max_messages=max_messages,
        preview_chars=preview_chars,
        max_chars=max_chars,
        interpreter=interpreter,
    )
    try:
        completed = subprocess.run(
            command,
            cwd=ROOT,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=COMMAND_TIMEOUT_S,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise SessionStartError(f"bounded A2A preview failed: {exc}") from exc
    if completed.returncode != 0:
        detail = completed.stderr.strip()[:500]
        raise SessionStartError(
            f"bounded A2A preview exited {completed.returncode}: {detail}"
        )
    raw = completed.stdout.strip()
    if not raw:
        raise SessionStartError("bounded A2A preview returned empty output")
    try:
        payload = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise SessionStartError("bounded A2A preview returned invalid JSON") from exc
    envelope = validate_compact_envelope(
        payload,
        identity=identity,
        max_messages=max_messages,
        preview_chars=preview_chars,
    )
    preview_text = json.dumps(
        envelope, ensure_ascii=False, separators=(",", ":"), sort_keys=True
    )
    if len(preview_text) > max_chars:
        raise SessionStartError(
            f"bounded A2A preview returned {len(preview_text)} chars; limit is {max_chars}"
        )
    return envelope, preview_text


def run_startup(
    *,
    identity: str,
    state_dir: Path = DEFAULT_STATE_DIR,
    max_messages: int = DEFAULT_MAX_MESSAGES,
    preview_chars: int = DEFAULT_PREVIEW_CHARS,
    max_chars: int = DEFAULT_MAX_CHARS,
    serve_timeout_s: float = DEFAULT_SERVE_TIMEOUT_S,
    interpreter: str = sys.executable,
) -> StartupResult:
    """Start receive, flush this sender, and fetch the bounded unread view."""

    serve_status = ensure_serve(
        identity=identity,
        state_dir=state_dir,
        timeout_s=serve_timeout_s,
        interpreter=interpreter,
    )
    flush_status = flush_sender_queue(
        identity=identity, state_dir=state_dir, interpreter=interpreter
    )
    preview, preview_text = request_compact_preview(
        identity=identity,
        state_dir=state_dir,
        max_messages=max_messages,
        preview_chars=preview_chars,
        max_chars=max_chars,
        interpreter=interpreter,
    )
    return StartupResult(
        identity=identity,
        serve_status=serve_status,
        flush_status=flush_status,
        preview=preview,
        preview_text=preview_text,
    )


def _hook_session_id(stdin_text: str) -> str:
    try:
        payload = json.loads(stdin_text)
    except json.JSONDecodeError as exc:
        raise SessionStartError(
            "once-per-session hook input is not valid JSON"
        ) from exc
    session_id = payload.get("session_id") if isinstance(payload, dict) else None
    if not isinstance(session_id, str) or not session_id.strip():
        raise SessionStartError("once-per-session hook input has no session_id")
    return session_id


@contextlib.contextmanager
def first_invocation(
    *, state_dir: Path, identity: str, session_id: str
) -> Iterator[bool]:
    """Serialize a provider hook and mark one successful invocation per session."""

    marker_dir = state_dir / identity / ".session-start"
    marker_dir.mkdir(parents=True, exist_ok=True)
    os.chmod(marker_dir, stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR)
    now = time.time()
    for pattern in ("*.done", "*.lock"):
        for old in marker_dir.glob(pattern):
            with contextlib.suppress(OSError):
                if now - old.stat().st_mtime > ONCE_MARKER_MAX_AGE_S:
                    old.unlink()
    digest = hashlib.sha256(session_id.encode()).hexdigest()
    marker_path = marker_dir / f"{digest}.done"
    lock_path = marker_dir / f"{digest}.lock"
    with lock_path.open("a+") as lock_handle:
        os.chmod(lock_path, stat.S_IRUSR | stat.S_IWUSR)
        fcntl.flock(lock_handle.fileno(), fcntl.LOCK_EX)
        try:
            if marker_path.exists():
                yield False
                return
            yield True
            marker_path.write_text("ok\n")
            os.chmod(marker_path, stat.S_IRUSR | stat.S_IWUSR)
        finally:
            fcntl.flock(lock_handle.fileno(), fcntl.LOCK_UN)


def _emit(result: StartupResult, *, output: str, event_name: str) -> None:
    if output == "none":
        return
    if output == "text":
        print(result.preview_text)
        return
    if output == "hook-json":
        print(
            json.dumps(
                {
                    "hookSpecificOutput": {
                        "hookEventName": event_name,
                        "additionalContext": result.preview_text,
                    }
                },
                ensure_ascii=False,
                separators=(",", ":"),
            )
        )
        return
    print(
        json.dumps(
            {
                "identity": result.identity,
                "serve_status": result.serve_status,
                "flush_status": result.flush_status,
                "preview": result.preview,
            },
            ensure_ascii=False,
            sort_keys=True,
        )
    )


def build_parser() -> argparse.ArgumentParser:
    """Build the command-line parser."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--identity", help=f"A2A name; defaults to ${IDENTITY_ENV}")
    parser.add_argument("--state-dir", type=Path, default=DEFAULT_STATE_DIR)
    parser.add_argument("--provider", choices=sorted(PROVIDER_EVENT), default="codex")
    parser.add_argument(
        "--output", choices=("hook-json", "json", "none", "text"), default="hook-json"
    )
    parser.add_argument("--max-messages", type=int, default=DEFAULT_MAX_MESSAGES)
    parser.add_argument("--preview-chars", type=int, default=DEFAULT_PREVIEW_CHARS)
    parser.add_argument("--max-chars", type=int, default=DEFAULT_MAX_CHARS)
    parser.add_argument("--serve-timeout", type=float, default=DEFAULT_SERVE_TIMEOUT_S)
    parser.add_argument(
        "--once-per-session",
        action="store_true",
        help="consume hook JSON on stdin and emit only once for its session_id",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    """CLI entrypoint used by provider hook configurations."""

    args = build_parser().parse_args(argv)
    try:
        identity = resolve_identity(args.identity)
        validate_preview_bounds(args.max_messages, args.preview_chars, args.max_chars)

        def execute() -> None:
            result = run_startup(
                identity=identity,
                state_dir=args.state_dir,
                max_messages=args.max_messages,
                preview_chars=args.preview_chars,
                max_chars=args.max_chars,
                serve_timeout_s=args.serve_timeout,
            )
            _emit(result, output=args.output, event_name=PROVIDER_EVENT[args.provider])

        if args.once_per_session:
            session_id = _hook_session_id(sys.stdin.read())
            with first_invocation(
                state_dir=args.state_dir, identity=identity, session_id=session_id
            ) as should_run:
                if should_run:
                    execute()
        else:
            execute()
        return 0
    except (SessionStartError, OSError) as exc:
        print(f"a2a-session-start: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
