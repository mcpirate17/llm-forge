"""Tests for conductor.session_close."""

from __future__ import annotations

from contextlib import contextmanager
import json
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest

from conductor.candidate_review.ownership import (
    create_claim,
    load_claims,
)
from conductor.session_close import (
    SessionCloseError,
    close_session,
    format_summary,
    main,
    release_owner_claims,
)


@pytest.fixture
def session_repo(tmp_path: Path) -> Path:
    repo = tmp_path / "session_ws"
    repo.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "--quiet", str(repo)], check=True)
    subprocess.run(
        ["git", "config", "user.name", "session-tester"], cwd=repo, check=True
    )
    subprocess.run(
        ["git", "config", "user.email", "session@test.org"], cwd=repo, check=True
    )
    (repo / ".git" / "governance").mkdir(parents=True, exist_ok=True)
    (repo / "conductor").mkdir(parents=True, exist_ok=True)
    (repo / ".current_work.md").write_text(
        "# Active Coordination\n\n", encoding="utf-8"
    )
    return repo


def test_release_owner_claims_requires_explicit_scope(session_repo: Path) -> None:
    c1 = create_claim(
        session_repo,
        owner="agent-alpha",
        paths=["conductor/foo.py"],
        justification="alpha task",
        hours=1.0,
    )
    with pytest.raises(
        SessionCloseError,
        match="specify --claim-id <id>... or explicit --all-owner-claims",
    ):
        release_owner_claims(session_repo, owner="agent-alpha")

    released = release_owner_claims(
        session_repo, owner="agent-alpha", all_owner_claims=True
    )
    assert released == (c1.claim_id,)


def test_release_specific_claim_id(session_repo: Path) -> None:
    c1 = create_claim(
        session_repo,
        owner="agent-alpha",
        paths=["conductor/foo.py"],
        justification="alpha task 1",
        hours=1.0,
    )
    c2 = create_claim(
        session_repo,
        owner="agent-alpha",
        paths=["conductor/baz.py"],
        justification="alpha task 2",
        hours=1.0,
    )

    released = release_owner_claims(
        session_repo, owner="agent-alpha", claim_ids=(c1.claim_id,)
    )
    assert released == (c1.claim_id,)

    claims, _ = load_claims(session_repo)
    assert len(claims) == 1
    assert claims[0].claim_id == c2.claim_id


@pytest.mark.host_path(
    "conductor.active_state.generate_active_state() takes no repo argument -- "
    "parse_active_claims()/load_claims(ROOT) always reads THIS worktree's real "
    "git-common-dir governance/ownership-claims.json (shared across linked "
    "worktrees by design, CLAUDE.md), never the session_repo tmp_path fixture "
    "close_session() is otherwise isolated to. Pre-existing test-isolation gap "
    "surfaced by the deliverable-5 path guard, not introduced by it; fixing it "
    "needs generate_active_state/parse_active_claims/parse_top_headings to "
    "accept a repo param, which is out of this deliverable's scope."
)
def test_close_session_full_flow(session_repo: Path) -> None:
    c1 = create_claim(
        session_repo,
        owner="agent-close",
        paths=["conductor/file1.py"],
        justification="close task",
        hours=1.0,
    )

    res = close_session(
        session_repo,
        owner="agent-close",
        title="Completed test task",
        body="All tests passed cleanly.",
        claim_ids=(c1.claim_id,),
        sync_memory_index=False,
    )

    assert res.owner == "agent-close"
    assert res.claims_released == (c1.claim_id,)
    assert res.handoff_entry is not None
    assert "Completed test task" in res.handoff_entry

    claims, _ = load_claims(session_repo)
    assert len(claims) == 0

    active_json = session_repo / "conductor" / "active_state.json"
    assert active_json.is_file()
    payload = json.loads(active_json.read_text(encoding="utf-8"))
    assert payload["schema_version"] == 1

    summary = format_summary(res)
    assert "session-close SUCCESS" in summary
    assert "Completed test task" in summary


@pytest.mark.parametrize(
    ("owner", "title", "body", "expected_err"),
    [
        ("", "Title", "Body", "owner is required"),
        (
            "test-agent",
            "Title only",
            None,
            "both --title and --body must be provided together",
        ),
        (
            "test-agent",
            None,
            "Body only",
            "both --title and --body must be provided together",
        ),
    ],
)
def test_close_session_validation_errors(
    session_repo: Path,
    owner: str,
    title: str | None,
    body: str | None,
    expected_err: str,
) -> None:
    with pytest.raises(SessionCloseError, match=expected_err):
        close_session(session_repo, owner=owner, title=title, body=body)


@pytest.mark.host_path(
    "same generate_active_state() repo-isolation gap as test_close_session_full_flow "
    "-- reads the real worktree's git-common-dir governance/ownership-claims.json"
)
def test_main_cli(session_repo: Path, capsys: pytest.CaptureFixture[str]) -> None:
    c1 = create_claim(
        session_repo,
        owner="cli-agent",
        paths=["conductor/cli_file.py"],
        justification="cli task",
        hours=1.0,
    )

    code = main(
        [
            "--repo",
            str(session_repo),
            "--owner",
            "cli-agent",
            "--title",
            "CLI Close",
            "--body",
            "Done via CLI.",
            "--claim-id",
            c1.claim_id,
            "--no-memory-index",
            "--json",
        ]
    )
    assert code == 0
    out, _ = capsys.readouterr()
    data = json.loads(out)
    assert data["owner"] == "cli-agent"
    assert len(data["claims_released"]) == 1


def test_main_cli_error(session_repo: Path, capsys: pytest.CaptureFixture[str]) -> None:
    code = main(
        [
            "--repo",
            str(session_repo),
            "--owner",
            "cli-agent",
            "--title",
            "Only Title",
        ]
    )
    assert code == 2
    _, err = capsys.readouterr()
    assert "session-close FAILED" in err


@pytest.mark.host_path(
    "same generate_active_state() repo-isolation gap as test_close_session_full_flow "
    "-- reads the real worktree's git-common-dir governance/ownership-claims.json"
)
def test_close_session_memory_index_sync(
    session_repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    called: list[Path] = []
    lock_paths: list[Path] = []
    lock_held = False

    @contextmanager
    def fake_lock(path: Path):
        nonlocal lock_held
        lock_paths.append(path)
        lock_held = True
        try:
            yield
        finally:
            lock_held = False

    def fake_build_index(repo: Path) -> None:
        assert lock_held
        called.append(repo)

    import conductor.memory_index

    monkeypatch.setattr(conductor.memory_index, "build_index", fake_build_index)
    monkeypatch.setattr(conductor.memory_index, "index_write_lock", fake_lock)

    res = close_session(
        session_repo,
        owner="mem-agent",
        all_owner_claims=True,
        sync_memory_index=True,
    )
    assert res.memory_status == "ok"
    assert called == [session_repo]
    assert lock_paths == [session_repo / "research" / "cache" / "memory_index.jsonl"]


def test_close_session_unchanged_memory_result_does_not_save(
    session_repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    import conductor.memory_index

    index_path = session_repo / "research" / "cache" / "memory_index.jsonl"
    built: list[Path] = []
    lock_paths: list[Path] = []
    saved: list[object] = []

    @contextmanager
    def fake_lock(path: Path):
        lock_paths.append(path)
        yield

    def fake_result(*, index_path: Path) -> SimpleNamespace:
        built.append(index_path)
        return SimpleNamespace(changed=False)

    monkeypatch.setattr(conductor.memory_index, "build_index", lambda: None)
    monkeypatch.setattr(conductor.memory_index, "index_write_lock", fake_lock)
    monkeypatch.setattr(conductor.memory_index, "build_index_result", fake_result)
    monkeypatch.setattr(
        conductor.memory_index,
        "save_index_result",
        lambda result, path: saved.append((result, path)),
    )

    res = close_session(
        session_repo,
        owner="mem-agent",
        all_owner_claims=True,
        sync_memory_index=True,
    )

    assert res.memory_status == "ok"
    assert lock_paths == [index_path]
    assert built == [index_path]
    assert saved == []
