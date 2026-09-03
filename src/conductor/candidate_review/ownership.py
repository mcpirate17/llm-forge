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
# A claim carries two durations. `expected_at` is what the owner estimated the work
# would take; `expires_at` is the most it may hold the path even if the estimate was
# wrong. Neither is a promise the path stays held: a claim also lapses once nothing
# has been written under it for the idle window, and crossing the expected time
# shrinks that window from IDLE_LAPSE_MINUTES to OVERRUN_IDLE_MINUTES. An owner
# still inside its own estimate gets patience; one that blew the estimate and then
# went quiet loses the path in minutes.
#
# The stored schema still admits the legacy 24 h lifetime so an existing store keeps
# loading -- lowering MAX_CLAIM_HOURS raises OwnershipError, which the write gate
# reports as "claim store unavailable" and fails closed for every lane at once.
# MAX_ACTIVE_CLAIM_HOURS is the bound that actually binds: it is applied at creation
# and clamped again on read, so it reaches already-stored claims with no migration.
MAX_CLAIM_HOURS = 24.0
MAX_ACTIVE_CLAIM_HOURS = 2.0
EXPECTED_DEFAULT_MINUTES = 15.0
MAX_DEFAULT_MINUTES = 60.0
IDLE_LAPSE_MINUTES = 45.0
OVERRUN_IDLE_MINUTES = 10.0
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
    # When the owner estimated the work would be done. Optional so a store written
    # before this field existed still loads: absent means the claim never has a soft
    # phase and its expected time is its hard one.
    expected_at: str | None = None
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
    def expected(self) -> datetime:
        """When the owner said it would be done -- soft, and never after the hard cap."""
        if self.expected_at is None:
            return self.hard_deadline
        return min(
            _instant(self.expected_at, self.claim_id, "expected time"),
            self.hard_deadline,
        )

    @property
    def activity(self) -> datetime | None:
        if self.last_seen is None:
            return None
        return _instant(self.last_seen, self.claim_id, "activity stamp")

    @property
    def hard_deadline(self) -> datetime:
        """The longest this claim may hold, whatever it asked for and however busy."""
        return min(self.expiry, self.creation + timedelta(hours=MAX_ACTIVE_CLAIM_HOURS))

    def overrun(self, now: datetime) -> bool:
        """Past the estimate. Still holds the path, but on a much shorter fuse."""
        return now > self.expected

    def idle_window(self, now: datetime) -> timedelta:
        minutes = OVERRUN_IDLE_MINUTES if self.overrun(now) else IDLE_LAPSE_MINUTES
        return timedelta(minutes=minutes)

    def idle_deadline(self, now: datetime) -> datetime:
        return (self.activity or self.creation) + self.idle_window(now)

    def deadline(self, now: datetime) -> datetime:
        """The earliest of the hard cap and the idle window in force at *now*."""
        return min(self.hard_deadline, self.idle_deadline(now))

    def active(self, now: datetime) -> bool:
        return self.deadline(now) > now

    def lapse_reason(self, now: datetime) -> str:
        """Why this claim is no longer active -- for the message that denies a write."""
        if self.active(now):
            return ""
        idle_deadline = self.idle_deadline(now)
        if idle_deadline <= now and idle_deadline <= self.hard_deadline:
            since = self.activity or self.creation
            what = "last write" if self.activity else "creation"
            window = self.idle_window(now).total_seconds() / 60.0
            overran = (
                f"overran its expected {self.expected:%H:%M}Z, so it "
                if self.overrun(now)
                else ""
            )
            return (
                f"idle since {what} at {since:%Y-%m-%d %H:%M}Z "
                f"({overran}lapses after {window:g}m without a write)"
            )
        return f"expired at {self.hard_deadline:%Y-%m-%d %H:%M}Z"


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


_REQUIRED_CLAIM_FIELDS = frozenset(
    {"claim_id", "owner", "paths", "justification", "created_at", "expires_at"}
)


def _claim_from_payload(payload: object) -> OwnershipClaim:
    # `expected_at` is optional: a store written before it existed must keep loading,
    # because a raise here reaches the write gate as "claim store unavailable" and
    # fails closed for every lane at once.
    if not isinstance(payload, dict) or not (
        _REQUIRED_CLAIM_FIELDS
        <= set(payload)
        <= _REQUIRED_CLAIM_FIELDS | {"expected_at"}
    ):
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
        expected_at=(
            str(payload["expected_at"])
            if payload.get("expected_at") is not None
            else None
        ),
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
    if claim.expected_at is not None:
        expected = _instant(claim.expected_at, claim.claim_id, "expected time")
        if expected <= created or expected > expiry:
            raise OwnershipError(
                f"claim {claim.claim_id} expects to finish outside its own lifetime"
            )
    fields = {
        "owner": claim.owner,
        "paths": claim.paths,
        "justification": claim.justification,
        "created_at": claim.created_at,
        "expires_at": claim.expires_at,
    }
    # Bound into the identity only when present, so claims stored before the field
    # existed still hash to the id they were written under.
    if claim.expected_at is not None:
        fields["expected_at"] = claim.expected_at
    identity = sha256_json(fields)
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
        # Omit rather than store null, so a claim written before `expected_at`
        # existed round-trips to the exact bytes its id was hashed over.
        if payload.get("expected_at") is None:
            payload.pop("expected_at", None)
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
    expected_minutes: float = EXPECTED_DEFAULT_MINUTES,
    max_minutes: float = MAX_DEFAULT_MINUTES,
) -> OwnershipClaim:
    owner = owner.strip()
    justification = justification.strip()
    normalized = tuple(sorted({normalize_claim_path(path) for path in paths}))
    if not owner or not justification or not normalized:
        raise OwnershipError(
            "owner, justification, and at least one exact path are required"
        )
    cap = MAX_ACTIVE_CLAIM_HOURS * 60.0
    if max_minutes <= 0 or max_minutes > cap:
        raise OwnershipError(f"claim max time must be > 0 and <= {cap:g} minutes")
    if expected_minutes <= 0 or expected_minutes > max_minutes:
        raise OwnershipError(
            "claim expected time must be > 0 and no later than its max time "
            f"({max_minutes:g} minutes)"
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
    expires_at = (now + timedelta(minutes=max_minutes)).isoformat()
    expected_at = (now + timedelta(minutes=expected_minutes)).isoformat()
    identity = sha256_json(
        {
            "owner": owner,
            "paths": normalized,
            "justification": justification,
            "created_at": created_at,
            "expires_at": expires_at,
            "expected_at": expected_at,
        }
    )
    claim = OwnershipClaim(
        claim_id=f"claim-{identity[:20]}",
        owner=owner,
        paths=normalized,
        justification=justification,
        created_at=created_at,
        expires_at=expires_at,
        expected_at=expected_at,
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
