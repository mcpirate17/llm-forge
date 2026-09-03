"""Timers that reclaim a claim its owner walked away from.

A claim carries two durations. `expected` is what the owner estimated; `max` is the
most it may hold the paths whatever happens. Neither keeps the path on its own: a
claim also lapses once nothing has been written under it for the idle window, and
crossing the expected time shrinks that window hard. An owner still inside its own
estimate gets patience; one that blew the estimate and went quiet is out in minutes.
"""

from __future__ import annotations

import json
import subprocess
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

from conductor.candidate_review.model import sha256_json
from conductor.candidate_review.ownership import (
    IDLE_LAPSE_MINUTES,
    MAX_ACTIVE_CLAIM_HOURS,
    OVERRUN_IDLE_MINUTES,
    OwnershipError,
    claim_activity_path,
    claim_store_path,
    create_claim,
    load_activity,
    load_claims,
    touch_claim,
)

CAP_MINUTES = MAX_ACTIVE_CLAIM_HOURS * 60.0


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    workdir = tmp_path / "repo"
    workdir.mkdir()
    subprocess.run(["git", "init", "--quiet"], cwd=workdir, check=True)
    return workdir


def _only(repo: Path):
    claims, _digest = load_claims(repo)
    assert len(claims) == 1
    return claims[0]


def _make(
    repo: Path,
    *,
    expected: float = 15.0,
    mx: float = 60.0,
    owner: str = "alpha",
    path: str = "pkg/mod.py",
):
    return create_claim(
        repo,
        owner=owner,
        paths=[path],
        justification="j",
        expected_minutes=expected,
        max_minutes=mx,
    )


def _store_a_legacy_claim(repo: Path, *, hours: float) -> str:
    """Rewrite the stored claim into the pre-`expected_at` shape.

    Claims written before this field existed are still on disk, and the store has
    to keep loading them: raising here reaches the write gate as "claim store
    unavailable" and fails closed for every lane at once.
    """
    created = datetime.now(timezone.utc)
    _make(repo)
    payload = json.loads(claim_store_path(repo).read_text(encoding="utf-8"))
    entry = payload["claims"][0]
    entry.pop("expected_at", None)
    entry["created_at"] = created.isoformat()
    entry["expires_at"] = (created + timedelta(hours=hours)).isoformat()
    identity = sha256_json(
        {
            "owner": entry["owner"],
            "paths": tuple(entry["paths"]),
            "justification": entry["justification"],
            "created_at": entry["created_at"],
            "expires_at": entry["expires_at"],
        }
    )
    entry["claim_id"] = f"claim-{identity[:20]}"
    claim_store_path(repo).write_text(json.dumps(payload), encoding="utf-8")
    return entry["claim_id"]


def test_create_refuses_a_max_above_the_ceiling(repo: Path) -> None:
    with pytest.raises(OwnershipError, match="claim max time must be"):
        _make(repo, expected=1.0, mx=CAP_MINUTES + 1.0)


def test_create_accepts_a_max_at_the_ceiling(repo: Path) -> None:
    claim = _make(repo, expected=1.0, mx=CAP_MINUTES)
    assert claim.expiry - claim.creation == timedelta(minutes=CAP_MINUTES)


def test_create_refuses_an_expected_beyond_its_own_max(repo: Path) -> None:
    with pytest.raises(OwnershipError, match="expected time must be"):
        _make(repo, expected=61.0, mx=60.0)


def test_create_stores_both_durations(repo: Path) -> None:
    claim = _make(repo, expected=15.0, mx=60.0)
    assert claim.expected - claim.creation == timedelta(minutes=15)
    assert claim.expiry - claim.creation == timedelta(minutes=60)
    assert claim.expected < claim.expiry


def test_an_estimate_that_produced_no_writes_lapses_at_the_estimate(repo: Path) -> None:
    """The headline: claim 15 minutes, write nothing, lose the path at 15 minutes.

    The estimate is only 15m and the overrun window is longer than nothing, so the
    binding moment is the estimate itself -- not the hour it asked for as a max.
    """
    claim = _make(repo, expected=15.0, mx=60.0)
    assert claim.active(claim.creation + timedelta(minutes=14))
    assert not claim.active(claim.creation + timedelta(minutes=16))
    assert claim.expiry > claim.creation + timedelta(minutes=59)


def test_an_on_time_claim_keeps_the_long_idle_window(repo: Path) -> None:
    claim = _make(repo, expected=CAP_MINUTES, mx=CAP_MINUTES)
    inside = claim.creation + timedelta(minutes=IDLE_LAPSE_MINUTES - 1)
    outside = claim.creation + timedelta(minutes=IDLE_LAPSE_MINUTES + 1)
    assert not claim.overrun(inside)
    assert claim.active(inside)
    assert not claim.active(outside)
    assert "idle since creation" in claim.lapse_reason(outside)


def test_overrunning_shrinks_the_idle_window(repo: Path) -> None:
    """Same claim, same last write -- only crossing the estimate changes the verdict."""
    claim = _make(repo, expected=15.0, mx=CAP_MINUTES)
    wrote_at = claim.creation + timedelta(minutes=12)
    touch_claim(repo, claim.claim_id, now=wrote_at)
    refreshed = _only(repo)
    on_time = wrote_at + timedelta(minutes=OVERRUN_IDLE_MINUTES - 10)
    overrun = wrote_at + timedelta(minutes=OVERRUN_IDLE_MINUTES + 1)
    assert refreshed.active(on_time)
    assert not refreshed.overrun(on_time)
    assert refreshed.overrun(overrun)
    assert not refreshed.active(overrun)
    # The long window would still have allowed it; only the overrun ends it.
    assert wrote_at + timedelta(minutes=IDLE_LAPSE_MINUTES) > overrun


def test_a_busy_overrun_claim_still_holds(repo: Path) -> None:
    """Overrun is a short leash, not eviction: an owner still writing keeps the path."""
    claim = _make(repo, expected=15.0, mx=CAP_MINUTES)
    now = claim.creation
    for _ in range(6):
        now = now + timedelta(minutes=OVERRUN_IDLE_MINUTES - 1)
        touch_claim(repo, claim.claim_id, now=now)
        assert _only(repo).active(now)
    refreshed = _only(repo)
    assert refreshed.overrun(now)
    assert refreshed.active(now)
    assert not refreshed.active(now + timedelta(minutes=OVERRUN_IDLE_MINUTES + 1))


def test_the_hard_cap_ends_even_a_busy_claim(repo: Path) -> None:
    claim_id = _store_a_legacy_claim(repo, hours=8.0)
    claim = _only(repo)
    cap = claim.creation + timedelta(hours=MAX_ACTIVE_CLAIM_HOURS)
    touch_claim(repo, claim_id, now=cap - timedelta(minutes=1))
    refreshed = _only(repo)
    beyond = cap + timedelta(minutes=1)
    assert refreshed.active(cap - timedelta(minutes=1))
    assert refreshed.expiry > beyond  # the stored lifetime would still allow it
    assert refreshed.idle_deadline(beyond) > beyond  # so would the idle window
    assert not refreshed.active(beyond)
    assert "expired at" in refreshed.lapse_reason(beyond)


def test_the_lapse_message_names_the_overrun(repo: Path) -> None:
    claim = _make(repo, expected=15.0, mx=CAP_MINUTES)
    touch_claim(repo, claim.claim_id, now=claim.creation + timedelta(minutes=12))
    refreshed = _only(repo)
    reason = refreshed.lapse_reason(claim.creation + timedelta(minutes=40))
    assert "overran its expected" in reason
    assert f"lapses after {OVERRUN_IDLE_MINUTES:g}m" in reason


def test_a_claim_stored_without_an_expected_time_still_loads(repo: Path) -> None:
    """Legacy shape: no soft phase, so expected collapses onto the hard deadline."""
    claim_id = _store_a_legacy_claim(repo, hours=1.0)
    claim = _only(repo)
    assert claim.claim_id == claim_id
    assert claim.expected_at is None
    assert claim.expected == claim.hard_deadline
    assert not claim.overrun(claim.creation + timedelta(minutes=1))


def test_a_legacy_claim_round_trips_without_gaining_a_null_field(repo: Path) -> None:
    """A rewrite must not add `expected_at: null` -- the id is hashed over the bytes."""
    claim_id = _store_a_legacy_claim(repo, hours=1.0)
    touch_claim(repo, claim_id, now=datetime.now(timezone.utc))
    _make(repo, owner="beta", path="pkg/other.py")  # a create rewrites the store
    entries = json.loads(claim_store_path(repo).read_text(encoding="utf-8"))["claims"]
    legacy = [item for item in entries if item["claim_id"] == claim_id]
    assert legacy and "expected_at" not in legacy[0]
    assert {claim.claim_id for claim in load_claims(repo)[0]} >= {claim_id}


def test_a_legacy_overlong_claim_is_still_capped(repo: Path) -> None:
    _store_a_legacy_claim(repo, hours=8.0)
    claim = _only(repo)
    assert claim.expiry - claim.creation == timedelta(hours=8)
    assert claim.hard_deadline == claim.creation + timedelta(
        hours=MAX_ACTIVE_CLAIM_HOURS
    )


def test_a_write_resets_the_idle_timer(repo: Path) -> None:
    claim = _make(repo, expected=CAP_MINUTES, mx=CAP_MINUTES)
    worked_at = claim.creation + timedelta(minutes=IDLE_LAPSE_MINUTES - 1)
    lapse = claim.creation + timedelta(minutes=IDLE_LAPSE_MINUTES + 1)
    assert touch_claim(repo, claim.claim_id, now=worked_at)
    refreshed = _only(repo)
    assert refreshed.activity == worked_at
    assert refreshed.active(lapse)
    assert not refreshed.active(worked_at + timedelta(minutes=IDLE_LAPSE_MINUTES + 1))


def test_touching_twice_in_quick_succession_writes_once(repo: Path) -> None:
    claim = _make(repo)
    first = claim.creation + timedelta(minutes=5)
    assert touch_claim(repo, claim.claim_id, now=first)
    assert not touch_claim(repo, claim.claim_id, now=first + timedelta(seconds=30))
    assert load_activity(repo)[claim.claim_id] == first.isoformat()
    assert touch_claim(repo, claim.claim_id, now=first + timedelta(seconds=90))


def test_a_lapsed_claim_stops_holding_the_path(repo: Path) -> None:
    stale = _make(repo, owner="alpha")
    past = datetime.now(timezone.utc) - timedelta(minutes=IDLE_LAPSE_MINUTES + 1)
    touch_claim(repo, stale.claim_id, now=past)
    taken = _make(repo, owner="beta")
    claims, _digest = load_claims(repo)
    assert [claim.claim_id for claim in claims] == [taken.claim_id]
    assert stale.claim_id not in load_activity(repo)


def test_a_live_claim_still_blocks_another_owner(repo: Path) -> None:
    _make(repo, owner="alpha")
    with pytest.raises(OwnershipError, match="overlaps active claim"):
        _make(repo, owner="beta")


def test_activity_never_enters_the_claim_store(repo: Path) -> None:
    claim = _make(repo)
    touch_claim(repo, claim.claim_id, now=claim.creation + timedelta(minutes=5))
    entry = json.loads(claim_store_path(repo).read_text(encoding="utf-8"))["claims"][0]
    assert "last_seen" not in entry
    assert _only(repo).claim_id == claim.claim_id  # identity binding survives
    assert claim_activity_path(repo).is_file()


def test_a_corrupt_activity_log_fails_loud(repo: Path) -> None:
    """A sidecar written by a future schema must not be read as "no activity".

    Silently ignoring it would restart every idle timer from creation and evict
    lanes that are working, so the load refuses instead of guessing.
    """
    _make(repo)
    claim_activity_path(repo).write_text(
        '{"schema_version": 99, "seen": {}}', encoding="utf-8"
    )
    with pytest.raises(OwnershipError, match="activity log schema version"):
        load_claims(repo)
