from __future__ import annotations

from pathlib import Path

import pytest

from conductor import handoff


@pytest.mark.parametrize(
    ("line_count", "should_raise"),
    [(handoff.MAX_BODY_LINES, False), (handoff.MAX_BODY_LINES + 1, True)],
)
def test_rejects_oversized_body(line_count: int, should_raise: bool) -> None:
    body = "\n".join(f"line {i}" for i in range(line_count))
    if not should_raise:
        handoff.validate_entry("grok", "exactly at limit", body)
        return
    with pytest.raises(handoff.HandoffError, match="12 lines"):
        handoff.validate_entry(
            "grok", "too long", "\n".join(f"line {i}" for i in range(13))
        )


def test_rejects_empty_owner() -> None:
    with pytest.raises(handoff.HandoffError, match="owner"):
        handoff.validate_entry("  ", "title", "body")


def test_append_inserts_newest_first(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path = tmp_path / ".current_work.md"
    path.write_text(
        "# Active Coordination\n\n## Old heading — 2026-08-22, glm-5.3\n\nOld body.\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(handoff, "CURRENT_WORK_PATH", path)
    entry = handoff.append_status(
        "grok",
        "Dump cap landed",
        "Pre-edit now denies research dumps. Use this helper.",
        path=path,
        refresh_state=lambda: None,
    )
    text = path.read_text(encoding="utf-8")
    assert text.startswith("# Active Coordination\n")
    assert entry.splitlines()[0] in text
    assert text.index("Dump cap landed") < text.index("Old heading")


# ------------------------------------------- the refresh behind the append


def test_a_failed_state_refresh_is_reported_not_swallowed(tmp_path: Path) -> None:
    """The entry is written either way; whether the state behind it is stale is not.

    Unwinding the append because a derived cache could not be rebuilt would lose the
    status the caller came here to record, so the refresh stays best-effort. Silent
    is the part that was wrong: the coordination log then describes work that the
    state file every other agent reads knows nothing about, and nothing said so.
    """
    seen: list[str] = []
    log = tmp_path / "log.md"

    def explode() -> None:
        raise OSError("state file is read-only")

    entry = handoff.append_status(
        "claude",
        "title",
        "body",
        path=log,
        refresh_state=explode,
        on_refresh_error=seen.append,
    )
    assert "title" in entry
    assert "title" in log.read_text(encoding="utf-8"), "the append must still stand"
    assert seen and "state file is read-only" in seen[0]
    assert "OSError" in seen[0], "the report must name what failed"


def test_a_successful_refresh_reports_nothing(tmp_path: Path) -> None:
    """Or the caller learns to ignore the channel and a real failure rides along."""
    seen: list[str] = []
    calls: list[int] = []
    handoff.append_status(
        "claude",
        "title",
        "body",
        path=tmp_path / "log.md",
        refresh_state=lambda: calls.append(1),
        on_refresh_error=seen.append,
    )
    assert calls == [1] and seen == []


def test_the_cli_separates_a_stale_state_from_a_clean_append(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    """Exit EXIT_STALE_STATE, not 0 and not 2.

    0 would say the fleet's state is current when it is not. 2 is the code for a
    REJECTED append, and a caller retrying on it would write a second copy of an
    entry that is already on disk.
    """
    log = tmp_path / "log.md"
    real = handoff.append_status

    def failing(*args: object, **kwargs: object) -> str:
        def explode() -> None:
            raise RuntimeError("no")

        return real(*args, **{**kwargs, "path": log, "refresh_state": explode})

    monkeypatch.setattr(handoff, "append_status", failing)
    code = handoff.main(["append", "--owner", "claude", "--title", "t", "--body", "b"])
    assert code == handoff.EXIT_STALE_STATE
    captured = capsys.readouterr()
    assert captured.out.startswith("## t"), "the written entry is still reported"
    assert "refresh failed" in captured.err

    def clean(*args: object, **kwargs: object) -> str:
        return real(*args, **{**kwargs, "path": log, "refresh_state": lambda: None})

    monkeypatch.setattr(handoff, "append_status", clean)
    assert (
        handoff.main(["append", "--owner", "claude", "--title", "t", "--body", "b"])
        == 0
    )
    assert handoff.EXIT_STALE_STATE not in (0, 2)
