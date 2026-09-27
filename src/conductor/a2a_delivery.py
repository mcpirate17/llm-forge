"""Compatibility calls into Forge's native local A2A delivery engine."""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
from pathlib import Path
from typing import Any

from conductor.a2a_registry import A2aError
from conductor.project_init import resolve_forge_binary
from conductor.project_paths import host_root


def _forge_binary() -> Path:
    explicit = os.environ.get("FORGE_BIN")
    if explicit:
        path = Path(explicit)
        if not path.is_file() or not os.access(path, os.X_OK):
            raise A2aError(f"FORGE_BIN is not an executable file: {path}")
        return path
    found = resolve_forge_binary(host_root())
    if found is None:
        raise A2aError("forge binary is required for A2A delivery")
    return found


def _run(
    state_dir: Path,
    action: str,
    args: list[str],
    *,
    body: str | None = None,
    data_payload: dict[str, Any] | None = None,
) -> tuple[Any, int]:
    command = [
        str(_forge_binary()),
        "mailbox",
        "--state-dir",
        str(state_dir),
        action,
        *args,
    ]
    # An unnamed temporary descriptor keeps structured data out of argv and
    # leaves no path to clean up after a process crash.
    with tempfile.TemporaryFile(mode="w+b") as data_file:
        pass_fds: tuple[int, ...] = ()
        if data_payload is not None:
            try:
                encoded = json.dumps(
                    data_payload, ensure_ascii=False, sort_keys=True
                ).encode("utf-8")
            except (TypeError, ValueError) as exc:
                raise A2aError(f"invalid A2A data payload: {exc}") from exc
            if len(encoded) > 1_048_576:
                raise A2aError("serialized data payload exceeds 1048576 bytes")
            data_file.write(encoded)
            data_file.flush()
            data_file.seek(0)
            command.extend(("--data-file", f"/proc/self/fd/{data_file.fileno()}"))
            pass_fds = (data_file.fileno(),)
        try:
            completed = subprocess.run(
                command,
                input=body,
                text=True,
                capture_output=True,
                check=False,
                pass_fds=pass_fds,
            )
        except OSError as exc:
            raise A2aError(f"cannot run native A2A {action}: {exc}") from exc
    if completed.returncode not in (0, 3):
        raise A2aError(completed.stderr.strip() or f"native A2A {action} failed")
    try:
        return json.loads(completed.stdout), completed.returncode
    except json.JSONDecodeError as exc:
        raise A2aError(f"invalid native A2A {action} response: {exc}") from exc


def send_message(
    from_name: str,
    to_name: str,
    body: str,
    data_payload: dict[str, Any] | None,
    state_dir: Path,
    queue_on_unreachable: bool = True,
) -> dict[str, Any]:
    """Store and attempt one send in native Forge, retaining the Python API."""
    if not isinstance(body, str):
        raise A2aError("body must be a string")
    args = ["--from-name", from_name, "--to", to_name, "--stdin"]
    if not queue_on_unreachable:
        args.append("--no-queue")
    payload, _ = _run(state_dir, "send", args, body=body, data_payload=data_payload)
    if (
        not isinstance(payload, dict)
        or payload.get("schema_version") != 1
        or payload.get("authority") != "a2a-delivery-receipt"
        or payload.get("delivery_status") not in {"delivered", "queued"}
    ):
        raise A2aError("invalid native A2A send receipt")
    return payload


def flush_queued(
    state_dir: Path,
    from_name: str | None = None,
    to_name: str | None = None,
    max_messages: int = 100,
) -> list[dict[str, Any]]:
    """Retry a bounded batch through the native sender locks and store."""
    args = ["--max-messages", str(max_messages)]
    if from_name is not None:
        args.extend(("--as-name", from_name))
    if to_name is not None:
        args.extend(("--to", to_name))
    payload, _ = _run(state_dir, "flush", args)
    if not isinstance(payload, list) or any(
        not isinstance(row, dict)
        or row.get("status") not in {"delivered", "queued", "failed"}
        or not isinstance(row.get("message_id"), str)
        for row in payload
    ):
        raise A2aError("invalid native A2A flush response")
    return payload


def history(
    state_dir: Path, identity: str, *, message_id: str | None = None, limit: int = 20
) -> dict[str, Any]:
    """Read bounded native delivery evidence without creating a mailbox."""
    args = ["--as-name", identity, "--limit", str(limit)]
    if message_id is not None:
        args.extend(("--message-id", message_id))
    payload, _ = _run(state_dir, "history", args)
    if (
        not isinstance(payload, dict)
        or payload.get("schema_version") != 1
        or not isinstance(payload.get("available"), bool)
        or not isinstance(payload.get("events"), list)
    ):
        raise A2aError("invalid native A2A history response")
    return payload
