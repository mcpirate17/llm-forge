"""Structured, expiring ownership claims shared by linked Git worktrees."""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass, replace
from datetime import datetime, timedelta, timezone
from pathlib import Path, PurePosixPath
from typing import Sequence

from conductor.candidate_review.git_source import git_common_dir
from conductor.candidate_review.model import sha256_json, write_json_atomic

CLAIM_SCHEMA_VERSION = 1
ACTIVITY_SCHEMA_VERSION = 1
# The stored schema still admits the legacy 24 h lifetime so an existing store keeps
# loading after this cap landed. The two timers below are what actually bind: no new
# claim may outlive MAX_ACTIVE_CLAIM_HOURS, and any claim -- however long it asked
# for -- lapses IDLE_LAPSE_HOURS after the last write recorded under it. A claim its
# owner walked away from stops holding the path without anyone releasing it.
MAX_CLAIM_HOURS = 24.0
MAX_ACTIVE_CLAIM_HOURS = 4.0
IDLE_LAPSE_HOURS = 1.5
_TOUCH_DEBOUNCE_SECONDS = 60.0
_BROAD_ROOTS = frozenset(
    {
        ".",
        ".github",
        "aria_core",
        "aria_designer",
        "component_fab",
        "conductor",
        "research",
    }
)


class OwnershipError(RuntimeError):
    """Ownership state is malformed, unsafe, or conflicts with an active claim."""


def _instant(raw: str, claim_id: str, label: str) -> datetime:
    try:
        value = datetime.fromisoformat(raw)
    except ValueError as exc:
        raise OwnershipError(
            f"claim {claim_id} {label} is unparseable: {raw!r}"
        ) from exc
    if value.tzinfo is None:
        raise OwnershipError(f"claim {claim_id} {label} lacks a timezone")
    return value.astimezone(timezone.utc)


@dataclass(frozen=True, slots=True)
class OwnershipClaim:
    claim_id: str
    owner: str
    paths: tuple[str, ...]
    justification: str
    created_at: str
    expires_at: str
    # Attached by load_claims from the activity sidecar; never part of the claim's
    # identity hash and never written back into the claim store.
    last_seen: str | None = None

    @property
    def expiry(self) -> datetime:
        return _instant(self.expires_at, self.claim_id, "expiry")

    @property
    def creation(self) -> datetime:
        return _instant(self.created_at, self.claim_id, "creation time")

    @property
    def activity(self) -> datetime | None:
        if self.last_seen is None:
            return None
        return _instant(self.last_seen, self.claim_id, "activity stamp")

    @property
    def deadline(self) -> datetime:
        """The earliest of the three timers: requested expiry, hard cap, idle lapse."""
        created = self.creation
        capped = min(self.expiry, created + timedelta(hours=MAX_ACTIVE_CLAIM_HOURS))
        return min(
            capped, (self.activity or created) + timedelta(hours=IDLE_LAPSE_HOURS)
        )

    def active(self, now: datetime) -> bool:
        return self.deadline > now

    def lapse_reason(self, now: datetime) -> str:
        """Why this claim is no longer active -- for the message that denies a write."""
        if self.active(now):
            return ""
        idle_deadline = (self.activity or self.creation) + timedelta(
            hours=IDLE_LAPSE_HOURS
        )
        if idle_deadline <= now and idle_deadline <= self.expiry:
            since = self.activity or self.creation
            what = "last write" if self.activity else "creation"
            return (
                f"idle since {what} at {since:%Y-%m-%d %H:%M}Z "
                f"(lapses after {IDLE_LAPSE_HOURS:g}h without a write)"
            )
        return f"expired at {self.deadline:%Y-%m-%d %H:%M}Z"


def claim_store_path(repo: Path) -> Path:
    return git_common_dir(repo) / "governance" / "ownership-claims.json"


def claim_activity_path(repo: Path) -> Path:
    """Last-write stamps, beside the claim store and shared by every worktree.

    Kept out of the claim store on purpose: a claim's id is a hash of its own
    fields, so stamping activity into it would break that binding on every write.
    """
    return git_common_dir(repo) / "governance" / "claim-activity.json"


def load_activity(repo: Path) -> dict[str, str]:
    path = claim_activity_path(repo)
    if not path.is_file():
        return {}
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise OwnershipError(f"claim activity log is unreadable: {exc}") from exc
    if not isinstance(payload, dict) or set(payload) != {"schema_version", "seen"}:
        raise OwnershipError("claim activity log has an invalid top-level schema")
    if payload["schema_version"] != ACTIVITY_SCHEMA_VERSION or not isinstance(
        payload["seen"], dict
    ):
        raise OwnershipError("claim activity log schema version or entries are invalid")
    return {str(key): str(value) for key, value in payload["seen"].items()}


def _write_activity(repo: Path, seen: dict[str, str]) -> None:
    write_json_atomic(
        claim_activity_path(repo),
        {"schema_version": ACTIVITY_SCHEMA_VERSION, "seen": seen},
    )


def touch_claim(repo: Path, claim_id: str, *, now: datetime | None = None) -> bool:
    """Record a write under *claim_id*, resetting its idle timer.

    Debounced: this runs inside the write hook, so a burst of edits under one claim
    costs a single file write. Returns whether the stamp was persisted.
    """
    moment = now or datetime.now(timezone.utc)
    seen = load_activity(repo)
    previous = seen.get(claim_id)
    if previous is not None:
        try:
            last = _instant(previous, claim_id, "activity stamp")
        except OwnershipError:
            last = None
        if (
            last is not None
            and 0 <= (moment - last).total_seconds() < _TOUCH_DEBOUNCE_SECONDS
        ):
            return False
    seen[claim_id] = moment.isoformat()
    _write_activity(repo, seen)
    return True


def normalize_claim_path(raw: str) -> str:
    value = raw.strip().rstrip("/")
    path = PurePosixPath(value)
    if (
        not value
        or path.is_absolute()
        or value in _BROAD_ROOTS
        or any(part in {"", ".", ".."} for part in path.parts)
        or any(character in value for character in "*?[")
    ):
        raise OwnershipError(
            f"claim path must be narrow and repository-relative: {raw!r}"
        )
    return path.as_posix()


def paths_overlap(first: str, second: str) -> bool:
    return (
        first == second
        or first.startswith(f"{second}/")
        or second.startswith(f"{first}/")
    )


def _claim_from_payload(payload: object) -> OwnershipClaim:
    if not isinstance(payload, dict) or set(payload) != {
        "claim_id",
        "owner",
        "paths",
        "justification",
        "created_at",
        "expires_at",
    }:
        raise OwnershipError("ownership claim has an invalid schema")
    paths = payload["paths"]
    if not isinstance(paths, list) or not paths:
        raise OwnershipError("ownership claim paths must be a non-empty array")
    normalized = tuple(normalize_claim_path(str(path)) for path in paths)
    if len(normalized) != len(set(normalized)):
        raise OwnershipError("ownership claim contains duplicate paths")
    claim = OwnershipClaim(
        claim_id=str(payload["claim_id"]),
        owner=str(payload["owner"]).strip(),
        paths=normalized,
        justification=str(payload["justification"]).strip(),
        created_at=str(payload["created_at"]),
        expires_at=str(payload["expires_at"]),
    )
    if not claim.claim_id or not claim.owner or not claim.justification:
        raise OwnershipError(
            "ownership claim identity, owner, and justification are required"
        )
    created = datetime.fromisoformat(claim.created_at)
    if created.tzinfo is None:
        raise OwnershipError(f"claim {claim.claim_id} creation time lacks a timezone")
    created = created.astimezone(timezone.utc)
    expiry = claim.expiry
    if expiry <= created or expiry - created > timedelta(hours=MAX_CLAIM_HOURS):
        raise OwnershipError(f"claim {claim.claim_id} has an invalid lifetime")
    identity = sha256_json(
        {
            "owner": claim.owner,
            "paths": claim.paths,
            "justification": claim.justification,
            "created_at": claim.created_at,
            "expires_at": claim.expires_at,
        }
    )
    if claim.claim_id != f"claim-{identity[:20]}":
        raise OwnershipError(f"claim {claim.claim_id} is not bound to its content")
    return claim


def load_claims(repo: Path) -> tuple[tuple[OwnershipClaim, ...], str]:
    path = claim_store_path(repo)
    if not path.is_file():
        return (), sha256_json({"schema_version": CLAIM_SCHEMA_VERSION, "claims": []})
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise OwnershipError(f"ownership claim store is unreadable: {exc}") from exc
    if not isinstance(payload, dict) or set(payload) != {"schema_version", "claims"}:
        raise OwnershipError("ownership claim store has an invalid top-level schema")
    if payload["schema_version"] != CLAIM_SCHEMA_VERSION or not isinstance(
        payload["claims"], list
    ):
        raise OwnershipError(
            "ownership claim store schema version or claims are invalid"
        )
    claims = tuple(_claim_from_payload(item) for item in payload["claims"])
    identifiers = [claim.claim_id for claim in claims]
    if len(identifiers) != len(set(identifiers)):
        raise OwnershipError("ownership claim store contains duplicate claim IDs")
    seen = load_activity(repo)
    claims = tuple(
        replace(claim, last_seen=seen[claim.claim_id])
        if claim.claim_id in seen
        else claim
        for claim in claims
    )
    # The digest covers the claim store alone: activity stamps must not perturb a
    # hash that receipts and the write gate bind to.
    return claims, sha256_json(payload)


def _write_claims(repo: Path, claims: Sequence[OwnershipClaim]) -> Path:
    path = claim_store_path(repo)
    stored = []
    for claim in claims:
        payload = asdict(claim)
        payload.pop("last_seen", None)
        stored.append({**payload, "paths": list(claim.paths)})
    write_json_atomic(
        path,
        {"schema_version": CLAIM_SCHEMA_VERSION, "claims": stored},
    )
    retained = {claim.claim_id for claim in claims}
    seen = load_activity(repo)
    surviving = {key: value for key, value in seen.items() if key in retained}
    if surviving != seen:
        _write_activity(repo, surviving)
    return path


def create_claim(
    repo: Path,
    *,
    owner: str,
    paths: Sequence[str],
    justification: str,
    hours: float,
) -> OwnershipClaim:
    owner = owner.strip()
    justification = justification.strip()
    normalized = tuple(sorted({normalize_claim_path(path) for path in paths}))
    if not owner or not justification or not normalized:
        raise OwnershipError(
            "owner, justification, and at least one exact path are required"
        )
    if hours <= 0 or hours > MAX_ACTIVE_CLAIM_HOURS:
        raise OwnershipError(
            f"claim duration must be > 0 and <= {MAX_ACTIVE_CLAIM_HOURS:g} hours"
        )
    now = datetime.now(timezone.utc)
    claims, _digest = load_claims(repo)
    active = [claim for claim in claims if claim.active(now)]
    for claim in active:
        for requested in normalized:
            for existing in claim.paths:
                if paths_overlap(requested, existing):
                    raise OwnershipError(
                        f"path {requested!r} overlaps active claim {claim.claim_id} "
                        f"owned by {claim.owner!r} at {existing!r}"
                    )
    created_at = now.isoformat()
    expires_at = (now + timedelta(hours=hours)).isoformat()
    identity = sha256_json(
        {
            "owner": owner,
            "paths": normalized,
            "justification": justification,
            "created_at": created_at,
            "expires_at": expires_at,
        }
    )
    claim = OwnershipClaim(
        claim_id=f"claim-{identity[:20]}",
        owner=owner,
        paths=normalized,
        justification=justification,
        created_at=created_at,
        expires_at=expires_at,
    )
    _write_claims(repo, [*active, claim])
    return claim


def release_claim(repo: Path, *, claim_id: str, owner: str) -> bool:
    claims, _digest = load_claims(repo)
    retained: list[OwnershipClaim] = []
    removed = False
    for claim in claims:
        if claim.claim_id == claim_id:
            if claim.owner.casefold() != owner.strip().casefold():
                raise OwnershipError(
                    f"claim {claim_id} is owned by {claim.owner!r}, not {owner!r}"
                )
            removed = True
        else:
            retained.append(claim)
    if removed:
        _write_claims(repo, retained)
    return removed
