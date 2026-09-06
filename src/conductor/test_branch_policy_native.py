"""Contract tests for the Rust-backed branch-policy adapter.

``conductor/branch_policy.py`` is a thin adapter over ``conductor_native``'s
``branch_policy`` module (see ``tooling/native/conductor-native/src/branch_policy.rs``).
These tests pin the boundary: the adapter passes the right facts to Rust, maps Rust's
``ValueError``/``TypeError`` exceptions onto the Python-level contract, and the Rust
decision text survives the trip. They complement -- and never duplicate --
``conductor/test_branch_policy.py`` (the full behavioural suite, unchanged by the port).
"""

from __future__ import annotations

import json
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from conductor import branch_policy as bp
from conductor.branch_policy import BranchBinding, BranchPolicyError


def _today() -> str:
    return datetime.now(UTC).strftime("%Y%m%d")


class TestNamingBoundary:
    def test_valueerror_from_rust_maps_to_policy_error(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        def boom(name: str, today: str) -> tuple[str, str, str, str]:
            raise ValueError("no 'agent/' segment")

        monkeypatch.setattr(bp, "branch_policy_validate_name_native", boom)
        with pytest.raises(BranchPolicyError, match="no 'agent/' segment"):
            bp.validate_branch_name("nope")

    def test_fields_returned_by_rust_become_parsed_branch(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setattr(
            bp,
            "branch_policy_validate_name_native",
            lambda name, today: (name, "agent", "topic", "20260101"),
        )
        parsed = bp.validate_branch_name("agent/topic-20260101")
        assert parsed == bp.ParsedBranch(
            raw="agent/topic-20260101", agent="agent", topic="topic", date="20260101"
        )

    def test_today_flows_from_clock_to_rust(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        seen: dict[str, str] = {}

        def record(name: str, today: str) -> tuple[str, str, str, str]:
            seen["today"] = today
            return name, "a", "t", today

        monkeypatch.setattr(bp, "branch_policy_validate_name_native", record)
        bp.validate_branch_name("a/t-20260101")
        assert seen["today"] == _today()


class TestStalenessBoundary:
    def _binding(self, created_at: str, last_push_at: str | None) -> BranchBinding:
        return BranchBinding(
            branch="claude/topic-20260101",
            claim_id="c1",
            owner="claude",
            created_at=created_at,
            last_push_at=last_push_at,
            pr_number=None,
        )

    def test_exactly_stale_push_hours_is_not_stale(self) -> None:
        now = datetime.now(UTC)
        created = (now - timedelta(hours=6)).isoformat()
        binding = self._binding(created, None)
        assert bp.binding_is_stale(binding, now=now) is False

    def test_one_microsecond_past_the_boundary_is_stale(self) -> None:
        now = datetime.now(UTC)
        created = (now - timedelta(hours=6, microseconds=1)).isoformat()
        binding = self._binding(created, None)
        assert bp.binding_is_stale(binding, now=now) is True

    def test_recent_push_rescues_an_old_binding(self) -> None:
        now = datetime.now(UTC)
        created = (now - timedelta(hours=48)).isoformat()
        pushed = (now - timedelta(minutes=5)).isoformat()
        binding = self._binding(created, pushed)
        assert bp.binding_is_stale(binding, now=now) is False

    def test_naive_aware_mix_propagates_typeerror(self) -> None:
        binding = self._binding("2026-01-01T00:00:00", None)
        with pytest.raises(TypeError):
            bp.binding_is_stale(binding, now=datetime.now(UTC))


class TestBindingStoreBoundary:
    @pytest.fixture
    def store_repo(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
        monkeypatch.setattr(bp, "git_common_dir", lambda repo: tmp_path)
        return tmp_path

    @staticmethod
    def _store(repo: Path, payload: object) -> None:
        path = bp.binding_store_path(repo)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(payload), encoding="utf-8")

    def test_roundtrip_preserves_every_field(self, store_repo: Path) -> None:
        now = datetime.now(UTC).isoformat()
        self._store(
            store_repo,
            {
                "schema_version": 1,
                "bindings": [
                    {
                        "branch": "codex/topic-20260101",
                        "claim_id": "c9",
                        "owner": "codex",
                        "created_at": now,
                        "last_push_at": now,
                        "pr_number": 42,
                    }
                ],
            },
        )
        (loaded,) = bp.load_bindings(store_repo)
        assert loaded == BranchBinding(
            branch="codex/topic-20260101",
            claim_id="c9",
            owner="codex",
            created_at=now,
            last_push_at=now,
            pr_number=42,
        )

    def test_missing_store_is_empty_tuple(self, store_repo: Path) -> None:
        assert bp.load_bindings(store_repo) == ()

    def test_duplicate_branches_refused(self, store_repo: Path) -> None:
        row = {
            "branch": "claude/topic-20260101",
            "claim_id": "c1",
            "owner": "claude",
            "created_at": "2026-01-01T00:00:00+00:00",
            "last_push_at": None,
            "pr_number": None,
        }
        self._store(store_repo, {"schema_version": 1, "bindings": [row, row]})
        with pytest.raises(BranchPolicyError, match="duplicate branches"):
            bp.load_bindings(store_repo)

    def test_wrong_schema_version_refused(self, store_repo: Path) -> None:
        self._store(store_repo, {"schema_version": 2, "bindings": []})
        with pytest.raises(BranchPolicyError, match="schema version"):
            bp.load_bindings(store_repo)

    def test_unreadable_bytes_refused_as_unreadable(self, store_repo: Path) -> None:
        path = bp.binding_store_path(store_repo)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"\xff\xfe\x9c")
        with pytest.raises(BranchPolicyError, match="unreadable"):
            bp.load_bindings(store_repo)

    def test_bind_conflict_facts_passed_to_rust(
        self, store_repo: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        seen: dict[str, object] = {}

        def record(live, store_text, branch, claim_id):
            seen.update(
                live=live, store_text=store_text, branch=branch, claim_id=claim_id
            )
            return "conflict!"

        monkeypatch.setattr(bp, "branch_policy_second_branch_conflict_native", record)
        monkeypatch.setattr(bp, "_refs", lambda repo, pattern: ["refs/heads/a/b-1"])
        with pytest.raises(BranchPolicyError, match="conflict!"):
            bp.bind_branch(
                store_repo, branch="claude/t-20260101", claim_id="c1", owner="claude"
            )
        assert seen == {
            "live": ["a/b-1"],
            "store_text": None,
            "branch": "claude/t-20260101",
            "claim_id": "c1",
        }
