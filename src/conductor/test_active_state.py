from __future__ import annotations

import json
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

from conductor import active_state


def test_parse_top_headings(tmp_path: Path, monkeypatch) -> None:
    current_work = tmp_path / ".current_work.md"
    current_work.write_text(
        "# Active Coordination\n"
        "## ✅ Task 1: Finished\n"
        "Details here...\n"
        "## ➡️ Task 2: In Progress\n"
        "More details...\n"
        "## 🛑 Task 3: Blocked\n"
    )
    monkeypatch.setattr(active_state, "CURRENT_WORK_PATH", current_work)
    headings = active_state.parse_top_headings(limit=2)
    assert len(headings) == 2
    assert "Task 1" in headings[0]
    assert "Task 2" in headings[1]


def test_generate_and_save_active_state(tmp_path: Path, monkeypatch) -> None:
    target_json = tmp_path / "active_state.json"
    monkeypatch.setattr(active_state, "ACTIVE_STATE_PATH", target_json)

    state = active_state.save_active_state(target_json)
    assert target_json.exists()
    payload = json.loads(target_json.read_text(encoding="utf-8"))
    assert payload["schema_version"] == 1
    assert len(payload["standing_mandates"]) >= 3
    assert any("MEMORY_RETRIEVE" in item for item in payload["standing_mandates"])
    assert any("handoff append" in item for item in payload["standing_mandates"])
    assert any(
        "LOCAL_AI_CLERICAL_ONLY" in item for item in payload["standing_mandates"]
    )
    assert any(
        "zero approval authority" in item for item in payload["standing_mandates"]
    )
    assert state.schema_version == 1
    assert not list(tmp_path.glob(".active_state.json.*.tmp"))


def test_validate_active_state_rejects_expired_claim() -> None:
    now = datetime.now(timezone.utc)
    state = active_state.ActiveState(
        last_updated=now.isoformat(),
        active_claims=[
            {
                "claim_id": "claim-expired",
                "expires_at": (now - timedelta(seconds=1)).isoformat(),
            }
        ],
    )

    with pytest.raises(active_state.ActiveStateError, match="expired claim"):
        active_state.validate_active_state(state, now=now)


def test_failed_generation_does_not_overwrite_last_good_state(
    tmp_path: Path, monkeypatch
) -> None:
    target = tmp_path / "active_state.json"
    target.write_text('{"last_good": true}\n', encoding="utf-8")

    def fail() -> active_state.ActiveState:
        raise active_state.ActiveStateError("claim store unreadable")

    monkeypatch.setattr(active_state, "generate_active_state", fail)
    with pytest.raises(active_state.ActiveStateError, match="claim store unreadable"):
        active_state.save_active_state(target)

    assert json.loads(target.read_text(encoding="utf-8")) == {"last_good": True}
