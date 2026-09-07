"""Read the optional project-owned SessionStart policy.

A package install has no project policy.  A host opts in by putting complete
``[tool.conductor.session]`` text in its root ``pyproject.toml``; this module
selects and validates those strings without interpreting them as authority.
"""

from __future__ import annotations

import stat
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Final

_CONFIG_LIMIT: Final[int] = 64 * 1024
_TABLE_KEYS: Final[frozenset[str]] = frozenset({"preamble", "standing_mandates"})


class SessionPolicyError(ValueError):
    """Project SessionStart policy is unreadable or invalid."""


@dataclass(frozen=True, slots=True)
class SessionPolicy:
    """Immutable project text selected for a session inject."""

    preamble: tuple[str, ...] = ()
    standing_mandates: tuple[str, ...] = ()


EMPTY_SESSION_POLICY: Final[SessionPolicy] = SessionPolicy()


def _error(path: Path, message: str) -> SessionPolicyError:
    return SessionPolicyError(f"{path}: {message}")


def _read_config(path: Path) -> dict[str, object] | None:
    try:
        metadata = path.stat()
    except FileNotFoundError:
        return None
    except OSError as exc:
        raise _error(path, f"cannot inspect configuration: {exc}") from exc
    if not stat.S_ISREG(metadata.st_mode):
        raise _error(path, "configuration must be a regular file")
    if metadata.st_size > _CONFIG_LIMIT:
        raise _error(path, "configuration exceeds 64 KiB")
    try:
        with path.open("rb") as stream:
            raw = stream.read(_CONFIG_LIMIT + 1)
    except OSError as exc:
        raise _error(path, f"cannot read configuration: {exc}") from exc
    if len(raw) > _CONFIG_LIMIT:
        raise _error(path, "configuration exceeds 64 KiB")
    try:
        payload = tomllib.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as exc:
        raise _error(path, f"invalid TOML configuration: {exc}") from exc
    if not isinstance(payload, dict):
        raise _error(path, "configuration must be a table")
    return payload


def _table(value: object, path: Path, label: str) -> dict[str, object] | None:
    if value is None:
        return None
    if not isinstance(value, dict):
        raise _error(path, f"{label} must be a table")
    return value


def _strings(value: object, path: Path, field: str) -> tuple[str, ...]:
    if not isinstance(value, list):
        raise _error(path, f"{field} must be a list of nonempty strings")
    if any(not isinstance(item, str) or not item.strip() for item in value):
        raise _error(path, f"{field} must be a list of nonempty strings")
    return tuple(value)


def load_session_policy(repo: Path) -> SessionPolicy:
    """Return the complete project session policy, or the generic empty policy."""
    if not isinstance(repo, Path):
        raise TypeError("repo must be a pathlib.Path")
    if not repo.is_dir():
        raise SessionPolicyError(f"{repo}: repository must be an existing directory")
    path = repo / "pyproject.toml"
    payload = _read_config(path)
    if payload is None:
        return EMPTY_SESSION_POLICY
    tool = _table(payload.get("tool"), path, "[tool]")
    conductor = _table(
        None if tool is None else tool.get("conductor"), path, "[tool.conductor]"
    )
    session = _table(
        None if conductor is None else conductor.get("session"),
        path,
        "[tool.conductor.session]",
    )
    if session is None:
        return EMPTY_SESSION_POLICY
    if set(session) != _TABLE_KEYS:
        raise _error(
            path,
            "[tool.conductor.session] must contain exactly preamble and standing_mandates",
        )
    return SessionPolicy(
        preamble=_strings(session["preamble"], path, "preamble"),
        standing_mandates=_strings(
            session["standing_mandates"], path, "standing_mandates"
        ),
    )
