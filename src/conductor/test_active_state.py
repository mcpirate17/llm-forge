from __future__ import annotations

import json
from datetime import UTC, datetime, timedelta
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
    monkeypatch.setattr(
        "conductor.candidate_review.ownership.load_claims",
        lambda _root: ((), "empty-claim-store"),
    )

    state = active_state.save_active_state(target_json)
    assert target_json.exists()
    payload = json.loads(target_json.read_text(encoding="utf-8"))
    assert payload["schema_version"] == 1
    assert payload["standing_mandates"] == []
    assert state.schema_version == 1
    assert not list(tmp_path.glob(".active_state.json.*.tmp"))


def test_generate_active_state_uses_selected_repository_policy(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repository = tmp_path / "repo"
    repository.mkdir()
    (repository / "pyproject.toml").write_text(
        "[tool.conductor.session]\n"
        'preamble = ["PROJECT: policy"]\n'
        'standing_mandates = ["PROJECT_RULE: required"]\n',
        encoding="utf-8",
    )
    monkeypatch.setattr(active_state, "parse_active_claims", lambda _repo: [])
    state = active_state.generate_active_state(repository)
    assert state.standing_mandates == ["PROJECT_RULE: required"]


def test_generate_active_state_uses_alternate_repo_headings(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    alternate_repo = tmp_path / "alternate"
    alternate_repo.mkdir()
    (alternate_repo / ".current_work.md").write_text(
        "# Active Coordination\n## Alternate repository task\n",
        encoding="utf-8",
    )
    global_work = tmp_path / "global.md"
    global_work.write_text("## Wrong repository task\n", encoding="utf-8")
    monkeypatch.setattr(active_state, "CURRENT_WORK_PATH", global_work)
    seen_repos: list[Path] = []

    def fake_claims(repo: Path) -> list[dict[str, object]]:
        seen_repos.append(repo)
        return []

    monkeypatch.setattr(active_state, "parse_active_claims", fake_claims)
    state = active_state.generate_active_state(alternate_repo)

    assert state.active_headings == ["Alternate repository task"]
    assert seen_repos == [alternate_repo]


def test_validate_active_state_rejects_expired_claim() -> None:
    now = datetime.now(UTC)
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
