from __future__ import annotations

import json
from pathlib import Path

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
    assert state.schema_version == 1


def test_error_and_cli_paths_fail_closed(tmp_path: Path, monkeypatch, capsys) -> None:
    unreadable = tmp_path / "active-state-directory"
    unreadable.mkdir()
    monkeypatch.setattr(active_state, "CURRENT_WORK_PATH", unreadable)
    assert active_state.parse_top_headings() == []

    def fail_claims(_root: Path):
        raise OSError("claim store unavailable")

    monkeypatch.setattr("conductor.candidate_review.ownership.load_claims", fail_claims)
    assert active_state.parse_active_claims() == []

    saved: list[bool] = []
    monkeypatch.setattr(active_state, "save_active_state", lambda: saved.append(True))
    assert active_state.main(["update"]) == 0
    assert saved == [True]

    state = active_state.ActiveState(active_headings=["heading"])
    monkeypatch.setattr(active_state, "generate_active_state", lambda: state)
    assert active_state.main(["dump"]) == 0
    assert json.loads(capsys.readouterr().out)["active_headings"] == ["heading"]
