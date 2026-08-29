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
