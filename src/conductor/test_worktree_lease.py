"""Contracts for worktree leases: a tree without one has to be reportable."""

from __future__ import annotations

import json
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from conductor.worktree_lease import (
    LEASE_FILENAME,
    LeaseError,
    is_linked_worktree,
    lease_state,
    main,
    open_lease,
    read_lease,
)

OPENED = datetime(2026, 9, 7, 12, 0, tzinfo=UTC)


def _leased(tmp_path: Path, hours: float = 8.0) -> Path:
    tree = tmp_path / "tree"
    tree.mkdir()
    open_lease(tree, "llm-d5", "land the research pile", hours, branch="x", now=OPENED)
    return tree


def test_a_lease_records_the_owner_purpose_and_a_deadline_it_computed(tmp_path):
    record = open_lease(
        tmp_path, "llm-d5", "  land the research pile  ", 6.0, now=OPENED
    )

    assert record["owner"] == "llm-d5"
    assert record["purpose"] == "land the research pile"
    assert datetime.fromisoformat(str(record["expires_at"])) == OPENED + timedelta(
        hours=6
    )
    assert json.loads((tmp_path / LEASE_FILENAME).read_text()) == record


def test_an_unexplained_or_unowned_lease_is_refused(tmp_path):
    with pytest.raises(LeaseError, match="purpose"):
        open_lease(tmp_path, "llm-d5", "   ")
    with pytest.raises(LeaseError, match="owner"):
        open_lease(tmp_path, "  ", "land the research pile")
    assert not (tmp_path / LEASE_FILENAME).exists()


@pytest.mark.parametrize("hours", [0.0, -1.0, 169.0])
def test_a_lease_longer_than_a_week_or_no_time_at_all_is_refused(tmp_path, hours):
    with pytest.raises(LeaseError, match="hours"):
        open_lease(tmp_path, "llm-d5", "land the research pile", hours)


def test_a_tree_with_no_lease_reads_as_none_but_a_broken_one_raises(tmp_path):
    assert read_lease(tmp_path) is None

    (tmp_path / LEASE_FILENAME).write_text("{not json")
    with pytest.raises(LeaseError, match="cannot read"):
        read_lease(tmp_path)

    (tmp_path / LEASE_FILENAME).write_text(json.dumps({"schema": "something.else"}))
    with pytest.raises(LeaseError, match="not a worktree-lease.v1"):
        read_lease(tmp_path)


def test_a_lease_missing_its_deadline_is_broken_not_merely_empty(tmp_path):
    record = open_lease(tmp_path, "llm-d5", "land the research pile", now=OPENED)
    del record["expires_at"]
    (tmp_path / LEASE_FILENAME).write_text(json.dumps(record))

    with pytest.raises(LeaseError, match="no expires_at"):
        read_lease(tmp_path)


def test_state_separates_a_live_lease_from_an_expired_one_and_from_none(tmp_path):
    live = _leased(tmp_path)
    bare = tmp_path / "bare"
    bare.mkdir()

    within = lease_state([live, bare], now=OPENED + timedelta(hours=7))
    assert [row["status"] for row in within] == ["leased", "unleased"]
    assert within[0]["overdue_minutes"] == 0

    past = lease_state([live], now=OPENED + timedelta(hours=9, minutes=30))
    assert past[0]["status"] == "expired"
    assert past[0]["overdue_minutes"] == 90


def test_state_skips_a_registration_whose_directory_is_gone(tmp_path):
    assert lease_state([tmp_path / "never-existed"]) == []


def test_the_main_checkout_is_not_a_disposable_worktree(tmp_path):
    checkout = tmp_path / "checkout"
    (checkout / ".git").mkdir(parents=True)
    linked = tmp_path / "linked"
    linked.mkdir()
    (linked / ".git").write_text("gitdir: /elsewhere/.git/worktrees/linked\n")

    assert is_linked_worktree(linked)
    assert not is_linked_worktree(checkout)


def test_the_cli_defaults_the_owner_to_the_tree_it_is_leasing(
    tmp_path, capsys, monkeypatch
):
    monkeypatch.delenv("GOVERNANCE_OWNER", raising=False)
    tree = tmp_path / "llm-d5-research-pile"
    tree.mkdir()

    assert main(["open", str(tree), "--purpose", "land it"]) == 0

    assert json.loads(capsys.readouterr().out)["owner"] == "llm-d5-research-pile"
