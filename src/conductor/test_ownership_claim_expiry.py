"""Timers that reclaim a claim its owner walked away from.

Three deadlines bind a claim, and the earliest wins: the duration it asked for, a
hard cap on any new claim, and an idle lapse measured from the last write recorded
under it. The idle one is the point -- an agent that claims a path and wanders off
stops holding it without anyone remembering to release.
"""

from __future__ import annotations

import json
import subprocess
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

from conductor.candidate_review.ownership import (
    IDLE_LAPSE_HOURS,
    MAX_ACTIVE_CLAIM_HOURS,
    OwnershipError,
    claim_activity_path,
    claim_store_path,
    create_claim,
    load_activity,
    load_claims,
    touch_claim,
)


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


def _store_a_claim_of(repo: Path, *, hours: float, owner: str = "alpha") -> None:
    """Write a claim with an arbitrary stored lifetime, bypassing the create cap.

    Claims created before the cap landed are still on disk with an eight hour
    lifetime; the store has to keep loading them.
    """
    created = datetime.now(timezone.utc)
    claim = create_claim(
        repo, owner=owner, paths=["pkg/mod.py"], justification="legacy", hours=1
    )
    payload = json.loads(claim_store_path(repo).read_text(encoding="utf-8"))
    entry = payload["claims"][0]
    entry["created_at"] = created.isoformat()
    entry["expires_at"] = (created + timedelta(hours=hours)).isoformat()
    from conductor.candidate_review.model import sha256_json

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
    assert claim.claim_id != entry["claim_id"]


def test_create_refuses_a_duration_above_the_active_cap(repo: Path) -> None:
    with pytest.raises(OwnershipError, match="claim duration must be"):
        create_claim(
            repo,
            owner="alpha",
            paths=["pkg/mod.py"],
            justification="too long",
            hours=MAX_ACTIVE_CLAIM_HOURS + 0.5,
        )


def test_create_accepts_a_duration_at_the_active_cap(repo: Path) -> None:
    claim = create_claim(
        repo,
        owner="alpha",
        paths=["pkg/mod.py"],
        justification="at the cap",
        hours=MAX_ACTIVE_CLAIM_HOURS,
    )
    assert claim.expiry - claim.creation == timedelta(hours=MAX_ACTIVE_CLAIM_HOURS)


def test_a_legacy_overlong_claim_still_loads_and_is_capped(repo: Path) -> None:
    _store_a_claim_of(repo, hours=8.0)
    claim = _only(repo)
    assert claim.expiry - claim.creation == timedelta(hours=8)
    assert claim.deadline <= claim.creation + timedelta(hours=MAX_ACTIVE_CLAIM_HOURS)


def test_an_unworked_claim_lapses_after_the_idle_window(repo: Path) -> None:
    claim = create_claim(
        repo, owner="alpha", paths=["pkg/mod.py"], justification="j", hours=4
    )
    just_inside = (
        claim.creation + timedelta(hours=IDLE_LAPSE_HOURS) - timedelta(minutes=1)
    )
    just_outside = (
        claim.creation + timedelta(hours=IDLE_LAPSE_HOURS) + timedelta(minutes=1)
    )
    assert claim.active(just_inside)
    assert not claim.active(just_outside)
    assert "idle since creation" in claim.lapse_reason(just_outside)


def test_a_write_resets_the_idle_timer(repo: Path) -> None:
    claim = create_claim(
        repo, owner="alpha", paths=["pkg/mod.py"], justification="j", hours=4
    )
    lapse = claim.creation + timedelta(hours=IDLE_LAPSE_HOURS) + timedelta(minutes=1)
    worked_at = (
        claim.creation + timedelta(hours=IDLE_LAPSE_HOURS) - timedelta(minutes=1)
    )
    assert touch_claim(repo, claim.claim_id, now=worked_at)
    refreshed = _only(repo)
    assert refreshed.activity == worked_at
    assert refreshed.active(lapse)
    assert not refreshed.active(
        worked_at + timedelta(hours=IDLE_LAPSE_HOURS, minutes=1)
    )


def test_the_cap_still_binds_a_claim_worked_on_continuously(repo: Path) -> None:
    """Only the cap can end this one: the claim asked for longer than the cap, and a
    write just before the cap leaves the idle timer with time still on it."""
    _store_a_claim_of(repo, hours=8.0)
    claim = _only(repo)
    cap = claim.creation + timedelta(hours=MAX_ACTIVE_CLAIM_HOURS)
    touch_claim(repo, claim.claim_id, now=cap - timedelta(minutes=1))
    refreshed = _only(repo)
    beyond = cap + timedelta(minutes=1)
    assert refreshed.active(cap - timedelta(minutes=1))
    # The other two timers would both still allow this write.
    assert refreshed.expiry > beyond
    assert (refreshed.activity or refreshed.creation) + timedelta(
        hours=IDLE_LAPSE_HOURS
    ) > beyond
    assert not refreshed.active(beyond)
    assert "expired at" in refreshed.lapse_reason(beyond)


def test_touching_twice_in_quick_succession_writes_once(repo: Path) -> None:
    claim = create_claim(
        repo, owner="alpha", paths=["pkg/mod.py"], justification="j", hours=2
    )
    first = claim.creation + timedelta(minutes=5)
    assert touch_claim(repo, claim.claim_id, now=first)
    assert not touch_claim(repo, claim.claim_id, now=first + timedelta(seconds=30))
    assert load_activity(repo)[claim.claim_id] == first.isoformat()
    assert touch_claim(repo, claim.claim_id, now=first + timedelta(seconds=90))


def test_a_lapsed_claim_stops_holding_the_path(repo: Path) -> None:
    stale = create_claim(
        repo, owner="alpha", paths=["pkg/mod.py"], justification="abandoned", hours=4
    )
    past = datetime.now(timezone.utc) - timedelta(hours=IDLE_LAPSE_HOURS + 1)
    touch_claim(repo, stale.claim_id, now=past)
    taken = create_claim(
        repo, owner="beta", paths=["pkg/mod.py"], justification="reclaimed", hours=1
    )
    claims, _digest = load_claims(repo)
    assert [claim.claim_id for claim in claims] == [taken.claim_id]
    assert stale.claim_id not in load_activity(repo)


def test_a_live_claim_still_blocks_another_owner(repo: Path) -> None:
    create_claim(
        repo, owner="alpha", paths=["pkg/mod.py"], justification="working", hours=2
    )
    with pytest.raises(OwnershipError, match="overlaps active claim"):
        create_claim(
            repo, owner="beta", paths=["pkg/mod.py"], justification="poach", hours=1
        )


def test_activity_never_enters_the_claim_store(repo: Path) -> None:
    claim = create_claim(
        repo, owner="alpha", paths=["pkg/mod.py"], justification="j", hours=2
    )
    touch_claim(repo, claim.claim_id, now=claim.creation + timedelta(minutes=5))
    entry = json.loads(claim_store_path(repo).read_text(encoding="utf-8"))["claims"][0]
    assert "last_seen" not in entry
    assert _only(repo).claim_id == claim.claim_id  # identity binding survives
    assert claim_activity_path(repo).is_file()


def test_a_corrupt_activity_log_fails_loud(repo: Path) -> None:
    create_claim(repo, owner="alpha", paths=["pkg/mod.py"], justification="j", hours=2)
    claim_activity_path(repo).write_text(
        '{"schema_version": 99, "seen": {}}', encoding="utf-8"
    )
    with pytest.raises(OwnershipError, match="activity log schema version"):
        load_claims(repo)
