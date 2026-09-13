#!/usr/bin/env python3
"""Active state manager for AVO Tier-0 token-conscious state compression.

Generates and maintains ``conductor/active_state.json``, which provides a compact
(<500 token) summary of active coordination, standing mandates, and claims so
agents do not need to parse large markdown files on every turn.
"""

from __future__ import annotations

import argparse
import datetime as dt
import inspect
import json
import os
import re
import sys
import tempfile
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any, Final

from conductor.project_paths import host_root
from conductor.session_policy import load_session_policy

ROOT: Final[Path] = host_root()
ACTIVE_STATE_PATH: Final[Path] = ROOT / "conductor" / "active_state.json"
CURRENT_WORK_PATH: Final[Path] = ROOT / ".current_work.md"

HEADING_RE: Final = re.compile(r"^##\s+(.+)$")


class ActiveStateError(RuntimeError):
    """The compact coordination state could not be generated or validated."""


@dataclass
class ActiveState:
    """Compact machine-readable state representation for agent sessions."""

    schema_version: int = 1
    last_updated: str = field(
        default_factory=lambda: dt.datetime.now(dt.timezone.utc).isoformat()
    )
    standing_mandates: list[str] = field(default_factory=list)
    active_headings: list[str] = field(default_factory=list)
    active_claims: list[dict[str, Any]] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


def parse_top_headings(
    limit: int = 4, *, current_work_path: Path | None = None
) -> list[str]:
    """Parse top active headings from .current_work.md."""
    path = current_work_path or CURRENT_WORK_PATH
    if not path.exists():
        return []
    headings: list[str] = []
    try:
        with path.open("r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                m = HEADING_RE.match(line)
                if m:
                    heading_text = m.group(1).strip()
                    if heading_text.lower() != "active coordination":
                        headings.append(heading_text)
                        if len(headings) >= limit:
                            break
    except OSError:
        pass
    return headings


def parse_active_claims(repo: Path = ROOT) -> list[dict[str, Any]]:
    """Read unexpired claims from the governance ownership store."""
    from conductor.candidate_review.ownership import load_claims

    claims, _ = load_claims(repo)
    now = dt.datetime.now(dt.timezone.utc)
    active: list[dict[str, Any]] = []
    for claim in claims:
        if claim.active(now):
            active.append(
                {
                    "claim_id": claim.claim_id,
                    "owner": claim.owner,
                    "paths": list(claim.paths),
                    "justification": claim.justification,
                    "expires_at": claim.expires_at,
                }
            )
    return active


def generate_active_state(repo: Path = ROOT) -> ActiveState:
    """Construct an updated ActiveState object from live repo sources."""
    headings = parse_top_headings(limit=4, current_work_path=repo / ".current_work.md")
    claims = parse_active_claims(repo)
    policy = load_session_policy(repo)
    return ActiveState(
        standing_mandates=list(policy.standing_mandates),
        active_headings=headings,
        active_claims=claims,
    )


def validate_active_state(
    state: ActiveState,
    *,
    now: dt.datetime | None = None,
) -> None:
    """Reject stale or malformed authorization data before it reaches a session."""
    if state.schema_version != 1:
        raise ActiveStateError(
            f"unsupported active-state schema: {state.schema_version!r} (expected 1)"
        )
    reference = now or dt.datetime.now(dt.timezone.utc)
    if reference.tzinfo is None:
        raise ActiveStateError("active-state validation time must include a timezone")
    try:
        updated = dt.datetime.fromisoformat(state.last_updated)
    except ValueError as exc:
        raise ActiveStateError(
            f"active-state last_updated is invalid: {state.last_updated!r}"
        ) from exc
    if updated.tzinfo is None:
        raise ActiveStateError("active-state last_updated must include a timezone")
    if updated.astimezone(dt.timezone.utc) > reference.astimezone(
        dt.timezone.utc
    ) + dt.timedelta(minutes=1):
        raise ActiveStateError("active-state last_updated is implausibly in the future")
    if len(state.active_headings) > 4:
        raise ActiveStateError(
            f"active-state contains {len(state.active_headings)} headings (maximum 4)"
        )
    for claim in state.active_claims:
        try:
            expiry = dt.datetime.fromisoformat(str(claim["expires_at"]))
        except (KeyError, TypeError, ValueError) as exc:
            raise ActiveStateError(
                "active-state claim has an invalid expires_at"
            ) from exc
        if expiry.tzinfo is None or expiry.astimezone(
            dt.timezone.utc
        ) <= reference.astimezone(dt.timezone.utc):
            raise ActiveStateError(
                f"active-state contains expired claim {claim.get('claim_id', '<unknown>')}"
            )


def _write_state_atomic(path: Path, payload: dict[str, Any]) -> None:
    """Write one validated state snapshot without exposing a partial JSON file."""
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, raw_temporary = tempfile.mkstemp(
        dir=path.parent,
        prefix=f".{path.name}.",
        suffix=".tmp",
    )
    temporary = Path(raw_temporary)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, indent=2)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        directory_fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        temporary.unlink(missing_ok=True)


def save_active_state(path: Path = ACTIVE_STATE_PATH) -> ActiveState:
    """Generate, validate, and atomically write ``active_state.json``."""
    repo = path.resolve().parent.parent
    # Keep zero-argument test doubles compatible with the public helper while
    # passing the target repository to the real implementation.
    if inspect.signature(generate_active_state).parameters:
        state = generate_active_state(repo)
    else:
        state = generate_active_state()
    validate_active_state(state)
    _write_state_atomic(path, state.to_dict())
    return state


def cmd_update() -> int:
    save_active_state()
    return 0


def cmd_dump() -> int:
    state = generate_active_state()
    print(json.dumps(state.to_dict(), indent=2))
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Manage AVO Tier-0 active state.")
    parser.add_argument(
        "action",
        nargs="?",
        choices=["update", "dump"],
        default="update",
        help="Action to perform (default: update)",
    )
    args = parser.parse_args(argv)
    if args.action == "dump":
        return cmd_dump()
    return cmd_update()


if __name__ == "__main__":
    sys.exit(main())
