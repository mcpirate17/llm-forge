"""Durable, verified handoff envelopes with a replaceable live-context view.

The envelope is the immutable audit record.  The context returned by
``activate_handoff`` is a bounded, non-authoritative projection suitable for a
host that supports replacing its next-turn context.  It deliberately does not
release claims or mutate a provider conversation: those actions belong to the
host adapter, after it has accepted the projection.
"""

from __future__ import annotations

import argparse
import datetime as dt
import fcntl
import hashlib
import json
import os
import re
import tempfile
import uuid
from collections.abc import Mapping
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any, Final

from conductor.active_state import (
    ActiveState,
    generate_active_state,
    validate_active_state,
)
from conductor.context_envelope import fit_text

SCHEMA_VERSION: Final[int] = 1
MAX_CONTEXT_CHARS: Final[int] = 3_500
MAX_TASK_CHARS: Final[int] = 500
_SECRET_PATTERNS: Final[tuple[re.Pattern[str], ...]] = (
    re.compile(r"\bsk-[A-Za-z0-9_-]{12,}\b"),
    re.compile(r"(?i)(authorization\s*:\s*bearer\s+)[^\s]+"),
    re.compile(r"(?i)(api[_-]?key\s*[=:]\s*)[^\s,;]+"),
)


class HandoffError(RuntimeError):
    """A handoff cannot safely be prepared, loaded, or activated."""


def _utcnow() -> dt.datetime:
    return dt.datetime.now(dt.UTC)


def _canonical_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def _digest(value: Any) -> str:
    return hashlib.sha256(_canonical_json(value).encode("utf-8")).hexdigest()


def _state_payload(state: ActiveState) -> dict[str, Any]:
    """Return the full state snapshot used as durable audit evidence."""

    return state.to_dict()


def _state_fingerprint(payload: Mapping[str, Any]) -> str:
    """Fingerprint governance fields while ignoring refresh timestamp noise."""

    payload = dict(payload)
    payload.pop("last_updated", None)
    return _digest(payload)


def _redact(text: str) -> str:
    redacted = text
    for pattern in _SECRET_PATTERNS:
        redacted = pattern.sub(
            lambda match: (
                match.group(1) + "[REDACTED]" if match.lastindex else "[REDACTED]"
            ),
            redacted,
        )
    return redacted


def _validate_paths(paths: tuple[str, ...]) -> None:
    for path in paths:
        if not path or Path(path).is_absolute() or ".." in Path(path).parts:
            raise HandoffError(
                f"handoff path must be a non-empty relative path: {path!r}"
            )


def _validate_runtime(runtime: Mapping[str, Any]) -> dict[str, str | int | bool | None]:
    normalized: dict[str, str | int | bool | None] = {}
    for key, value in runtime.items():
        if not isinstance(key, str) or not key:
            raise HandoffError("runtime metadata keys must be non-empty strings")
        if not isinstance(value, (str, int, bool, type(None))):
            raise HandoffError(f"runtime metadata {key!r} must be a scalar")
        normalized[key] = value
    return normalized


@dataclass(frozen=True)
class HandoffEnvelope:
    """One durable handoff plus the bounded context it can activate."""

    handoff_id: str
    created_at: str
    task: str
    paths: tuple[str, ...]
    runtime: dict[str, str | int | bool | None]
    active_state: dict[str, Any]
    active_state_digest: str
    context: str
    status: str = "prepared"
    activated_at: str | None = None
    parent_handoff_id: str | None = None
    schema_version: int = SCHEMA_VERSION

    def payload(self) -> dict[str, Any]:
        return {
            "schema_version": self.schema_version,
            "handoff_id": self.handoff_id,
            "parent_handoff_id": self.parent_handoff_id,
            "created_at": self.created_at,
            "activated_at": self.activated_at,
            "status": self.status,
            "task": self.task,
            "paths": list(self.paths),
            "runtime": self.runtime,
            "active_state": self.active_state,
            "active_state_digest": self.active_state_digest,
            "context": self.context,
        }

    def to_dict(self) -> dict[str, Any]:
        payload = self.payload()
        payload["integrity_sha256"] = _digest(payload)
        return payload

    @classmethod
    def from_dict(cls, raw: Mapping[str, Any]) -> HandoffEnvelope:
        payload = dict(raw)
        received_digest = payload.pop("integrity_sha256", None)
        if not isinstance(received_digest, str) or received_digest != _digest(payload):
            raise HandoffError("handoff envelope integrity check failed")
        if payload.get("schema_version") != SCHEMA_VERSION:
            raise HandoffError("unsupported handoff envelope schema")
        try:
            paths = tuple(payload["paths"])
            runtime = _validate_runtime(payload["runtime"])
            envelope = cls(
                handoff_id=str(payload["handoff_id"]),
                parent_handoff_id=payload.get("parent_handoff_id"),
                created_at=str(payload["created_at"]),
                activated_at=payload.get("activated_at"),
                status=str(payload["status"]),
                task=str(payload["task"]),
                paths=paths,
                runtime=runtime,
                active_state=dict(payload["active_state"]),
                active_state_digest=str(payload["active_state_digest"]),
                context=str(payload["context"]),
                schema_version=int(payload["schema_version"]),
            )
        except (KeyError, TypeError, ValueError) as exc:
            raise HandoffError("handoff envelope has malformed fields") from exc
        envelope.validate()
        return envelope

    def validate(self) -> None:
        if self.status not in {"prepared", "active"}:
            raise HandoffError(f"unsupported handoff status: {self.status!r}")
        if (
            not self.handoff_id
            or not self.task.strip()
            or len(self.task) > MAX_TASK_CHARS
        ):
            raise HandoffError("handoff id and bounded task are required")
        _validate_paths(self.paths)
        if len(self.context) > MAX_CONTEXT_CHARS:
            raise HandoffError("handoff context exceeds its hard character cap")
        if self.active_state_digest != _state_fingerprint(self.active_state):
            raise HandoffError(
                "handoff active-state digest does not match its snapshot"
            )
        try:
            state = ActiveState(**self.active_state)
            created_at = dt.datetime.fromisoformat(self.created_at)
        except (TypeError, ValueError) as exc:
            raise HandoffError("handoff active-state snapshot is malformed") from exc
        if created_at.tzinfo is None:
            raise HandoffError("handoff created_at must include a timezone")
        # The immutable snapshot is checked at its creation time.  Activation
        # separately validates the live state at the time it is consumed.
        validate_active_state(state, now=created_at)

    def live_context(self) -> str:
        """Return the only content a host adapter may inject into a next turn."""

        runtime = (
            ", ".join(f"{key}={value}" for key, value in sorted(self.runtime.items()))
            or "none"
        )
        return fit_text(
            "\n".join(
                [
                    f"HANDOFF {self.handoff_id}: {self.task}",
                    "GOVERNANCE: revalidate live claims and approvals before mutation.",
                    f"RUNTIME: {runtime}",
                    "WORKING CONTEXT (non-authoritative; verify against durable sources):",
                    self.context,
                ]
            ),
            MAX_CONTEXT_CHARS,
        )


@dataclass(frozen=True)
class HandoffActivation:
    """The host-facing result of a successful, idempotent activation."""

    envelope: HandoffEnvelope
    context: str
    already_active: bool


def prepare_handoff(
    *,
    task: str,
    paths: tuple[str, ...] = (),
    runtime: Mapping[str, Any] | None = None,
    context: str | None = None,
    parent_handoff_id: str | None = None,
    state: ActiveState | None = None,
    now: dt.datetime | None = None,
) -> HandoffEnvelope:
    """Prepare a verified envelope without altering claims or a live session."""

    task = task.strip()
    if not task or len(task) > MAX_TASK_CHARS:
        raise HandoffError(f"task must contain 1..{MAX_TASK_CHARS} characters")
    paths = tuple(paths)
    _validate_paths(paths)
    live_state = state or generate_active_state()
    validate_active_state(live_state, now=now)
    if context is None:
        from conductor.session_brief import brief

        context = brief(task, list(paths))
    if not isinstance(context, str):
        raise HandoffError("handoff context must be text")
    snapshot = _state_payload(live_state)
    timestamp = (now or _utcnow()).astimezone(dt.UTC).isoformat()
    envelope = HandoffEnvelope(
        handoff_id=str(uuid.uuid4()),
        created_at=timestamp,
        parent_handoff_id=parent_handoff_id,
        task=task,
        paths=paths,
        runtime=_validate_runtime(runtime or {}),
        active_state=snapshot,
        active_state_digest=_state_fingerprint(snapshot),
        context=fit_text(_redact(context), MAX_CONTEXT_CHARS),
    )
    envelope.validate()
    return envelope


def _atomic_write(path: Path, payload: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary_name = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.")
    temporary = Path(temporary_name)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, ensure_ascii=False, indent=2, sort_keys=True)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def save_handoff(envelope: HandoffEnvelope, path: Path) -> None:
    """Atomically persist a validated handoff envelope."""

    envelope.validate()
    _atomic_write(path, envelope.to_dict())


def load_handoff(path: Path) -> HandoffEnvelope:
    """Load and validate a persisted envelope before it can reach a host."""

    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise HandoffError(f"cannot load handoff envelope {path}: {exc}") from exc
    if not isinstance(raw, Mapping):
        raise HandoffError("handoff envelope root must be an object")
    return HandoffEnvelope.from_dict(raw)


def activate_handoff(
    path: Path,
    *,
    state: ActiveState | None = None,
    now: dt.datetime | None = None,
) -> HandoffActivation:
    """Revalidate live governance state and atomically mark an envelope active."""

    lock_path = path.with_suffix(path.suffix + ".lock")
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with lock_path.open("a+", encoding="utf-8") as lock:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        try:
            envelope = load_handoff(path)
            current = state or generate_active_state()
            validate_active_state(current, now=now)
            current_digest = _state_fingerprint(_state_payload(current))
            if current_digest != envelope.active_state_digest:
                raise HandoffError(
                    "handoff is stale: live mandates, headings, or claims changed"
                )
            already_active = envelope.status == "active"
            if not already_active:
                timestamp = (now or _utcnow()).astimezone(dt.UTC).isoformat()
                envelope = replace(envelope, status="active", activated_at=timestamp)
                save_handoff(envelope, path)
            return HandoffActivation(
                envelope=envelope,
                context=envelope.live_context(),
                already_active=already_active,
            )
        finally:
            fcntl.flock(lock.fileno(), fcntl.LOCK_UN)


# --- host adapter: Claude Code SessionStart ---------------------------------
# `stage` writes the envelope this identity will resume from; the SessionStart
# hook (.claude/hooks/session-handoff.sh) calls `hook`, which activates a
# prepared envelope exactly once and returns its projection as
# additionalContext. The loop is: stage -> /clear (or compaction, restart) ->
# the next turn opens with the bounded projection instead of the old context.

from conductor.project_paths import host_root

ROOT: Final[Path] = host_root()
IDENTITY_ENV: Final[str] = "A2A_AGENT_NAME"
DEFAULT_IDENTITY: Final[str] = "claude"
_IDENTITY_RE: Final[re.Pattern[str]] = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}")
HOOK_EVENT: Final[str] = "SessionStart"
STAGE_RUNTIME: Final[dict[str, str | bool]] = {
    "adapter": "claude-code-session-start",
    "supports_inplace_replace": True,
}


def resolve_identity(
    explicit: str | None = None,
    *,
    environ: Mapping[str, str] | None = None,
    root: Path = ROOT,
) -> str:
    """Explicit name, else ``$A2A_AGENT_NAME``, else ``.agents/a2a/default_identity``, else ``claude``."""

    env = os.environ if environ is None else environ
    source = explicit if explicit is not None else env.get(IDENTITY_ENV, "")
    if not source.strip():
        default_file = root / ".agents" / "a2a" / "default_identity"
        try:
            source = default_file.read_text(encoding="utf-8").splitlines()[0]
        except (OSError, IndexError):
            source = DEFAULT_IDENTITY
    identity = source.strip()
    if _IDENTITY_RE.fullmatch(identity) is None:
        raise HandoffError(f"invalid handoff identity {source!r}")
    return identity


def staged_path(identity: str, *, root: Path = ROOT) -> Path:
    return root / ".agents" / "handoff" / identity / "pending.json"


def stage_handoff(
    *,
    identity: str,
    task: str,
    paths: tuple[str, ...] = (),
    context: str | None = None,
    root: Path = ROOT,
    state: ActiveState | None = None,
    now: dt.datetime | None = None,
) -> tuple[HandoffEnvelope, Path]:
    """Prepare the envelope the next session of *identity* will resume from.

    An envelope already at the staged path becomes the parent, so a chain of
    clears keeps its lineage in the audit record.
    """

    path = staged_path(identity, root=root)
    parent: str | None = None
    if path.is_file():
        parent = load_handoff(path).handoff_id
    envelope = prepare_handoff(
        task=task,
        paths=paths,
        runtime={**STAGE_RUNTIME, "identity": identity},
        context=context,
        parent_handoff_id=parent,
        state=state,
        now=now,
    )
    save_handoff(envelope, path)
    return envelope, path


def hook_context(
    identity: str,
    *,
    root: Path = ROOT,
    state: ActiveState | None = None,
    now: dt.datetime | None = None,
) -> str:
    """Context to inject for *identity*: the projection of a not-yet-active staged
    envelope, a loud diagnostic when it cannot be activated, else empty."""

    path = staged_path(identity, root=root)
    if not path.is_file():
        return ""
    try:
        activation = activate_handoff(path, state=state, now=now)
    except HandoffError as exc:
        return (
            f"HANDOFF NOT ACTIVATED for {identity}: {exc}. The staged envelope at "
            f"{path} was left untouched; re-stage with "
            "`python -m conductor.inplace_handoff stage --task ...` or delete it."
        )
    if activation.already_active:
        return ""
    return activation.context


def hook_output(context: str) -> dict[str, Any]:
    out: dict[str, Any] = {"hookSpecificOutput": {"hookEventName": HOOK_EVENT}}
    if context:
        out["hookSpecificOutput"]["additionalContext"] = context
    return out


def _add_identity(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--identity",
        default=None,
        help=f"defaults to ${IDENTITY_ENV}, then .agents/a2a/default_identity",
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    prepare = commands.add_parser("prepare", help="prepare and persist a handoff")
    prepare.add_argument("--task", required=True)
    prepare.add_argument("--paths", nargs="*", default=[])
    prepare.add_argument("--runtime-json", default="{}")
    prepare.add_argument("--context", default=None)
    prepare.add_argument("--output", type=Path, required=True)
    activate = commands.add_parser(
        "activate", help="activate and print replacement context"
    )
    activate.add_argument("--input", type=Path, required=True)
    stage = commands.add_parser(
        "stage", help="stage the envelope the next session resumes from"
    )
    stage.add_argument("--task", required=True)
    stage.add_argument("--paths", nargs="*", default=[])
    stage.add_argument(
        "--context",
        default=None,
        help="working notes; default: session_brief for the task",
    )
    stage.add_argument("--context-file", type=Path, default=None)
    _add_identity(stage)
    hook = commands.add_parser(
        "hook", help="SessionStart adapter: print hook JSON for the staged envelope"
    )
    _add_identity(hook)
    args = parser.parse_args(argv)
    try:
        if args.command == "prepare":
            runtime = json.loads(args.runtime_json)
            if not isinstance(runtime, Mapping):
                raise HandoffError("--runtime-json must decode to an object")
            envelope = prepare_handoff(
                task=args.task,
                paths=tuple(args.paths),
                runtime=runtime,
                context=args.context,
            )
            save_handoff(envelope, args.output)
            print(
                json.dumps(
                    {"handoff_id": envelope.handoff_id, "status": envelope.status}
                )
            )
            return 0
        if args.command == "stage":
            context = args.context
            if args.context_file is not None:
                context = args.context_file.read_text(encoding="utf-8")
            envelope, path = stage_handoff(
                identity=resolve_identity(args.identity),
                task=args.task,
                paths=tuple(args.paths),
                context=context,
            )
            print(
                json.dumps(
                    {
                        "handoff_id": envelope.handoff_id,
                        "staged": str(path),
                        "context_chars": len(envelope.context),
                    }
                )
            )
            return 0
        if args.command == "hook":
            print(
                json.dumps(hook_output(hook_context(resolve_identity(args.identity))))
            )
            return 0
        activation = activate_handoff(args.input)
        print(activation.context)
        return 0
    except (HandoffError, OSError, json.JSONDecodeError) as exc:
        parser.error(str(exc))
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
