"""Tests for the one-call session brief."""

from __future__ import annotations

import subprocess
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from conductor import session_brief as sb
from conductor.candidate_review.ownership import create_claim

INBOX = (
    "[UNREAD] aaa from=codex at=t1\n"
    + "word " * 60
    + "\nsecond line\n\n[UNREAD] bbb from=helm at=t2\nshort\n\n[UNREAD] ccc from=x at=t3\n\n"
)


def test_snippet_strips_frontmatter_and_truncates() -> None:
    assert sb.snippet("---\nid: X\n---\n\n# T\n\na  b\n") == "# T a b"
    out = sb.snippet("w " * 300, limit=20)
    assert len(out) == 20 and out.endswith("…")


def test_compact_inbox_previews_headers_and_caps_messages() -> None:
    out = sb.compact_inbox(INBOX)
    lines = out.splitlines()
    assert lines[0] == "[UNREAD] aaa from=codex at=t1"
    assert lines[1].startswith("  word word") and lines[1].endswith("…")
    assert len(lines[1]) == 2 + sb.PREVIEW_CHARS
    assert "[UNREAD] bbb from=helm at=t2\n  short" in out
    assert out.endswith("[UNREAD] ccc from=x at=t3")
    capped = sb.compact_inbox(INBOX, max_msgs=1)
    assert capped.endswith("(+2 more unread)")
    assert sb.compact_inbox("") == ""


@pytest.fixture
def repo(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    r = tmp_path / "repo"
    r.mkdir()
    subprocess.run(["git", "init", "--quiet"], cwd=r, check=True)
    for name in ("a.py", "b.py"):
        (r / name).write_text("X = 1\n", encoding="utf-8")
    create_claim(r, owner="codex", paths=["a.py"], justification="j", hours=1)
    monkeypatch.setattr(sb, "ROOT", r)
    return r


def test_claims_for_paths_boundary_reports_load_failure(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    def boom(_repo: Path) -> Any:
        raise OSError("claims store unreadable")

    monkeypatch.setattr(sb, "load_claims", boom)
    out = sb.claims_for_paths(["a.py"], repo=repo)
    assert out == "CLAIMS: unavailable (claims store unreadable)"


def test_claims_for_paths_reports_overlap_or_absence(repo: Path) -> None:
    now = datetime.now(timezone.utc)
    text = sb.claims_for_paths(["a.py"], repo=repo, now=now)
    assert text.startswith("CLAIMS overlapping your paths:\nclaims: 1 active")
    assert " codex " in text and "\n    a.py" in text
    assert sb.claims_for_paths(["b.py"], repo=repo, now=now) == (
        "CLAIMS: none overlap b.py — claim before editing"
    )
    assert sb.claims_for_paths([], repo=repo) == "CLAIMS: no paths given"
    later = now + timedelta(hours=2)
    assert "none overlap" in sb.claims_for_paths(["a.py"], repo=repo, now=later)


def test_inbox_preview_handles_missing_agent_and_failures(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    assert sb.inbox_preview(None) == ""

    def fake_run(*_a: Any, **_k: Any) -> SimpleNamespace:
        return SimpleNamespace(returncode=0, stdout=INBOX)

    monkeypatch.setattr(sb.subprocess, "run", fake_run)
    out = sb.inbox_preview("fable-5")
    assert out.startswith("A2A unread (fable-5):\n[UNREAD] aaa")
    monkeypatch.setattr(
        sb.subprocess, "run", lambda *a, **k: SimpleNamespace(returncode=0, stdout="")
    )
    assert sb.inbox_preview("fable-5") == "A2A: no unread for fable-5"
    monkeypatch.setattr(
        sb.subprocess, "run", lambda *a, **k: SimpleNamespace(returncode=3, stdout="")
    )
    assert sb.inbox_preview("fable-5") == "A2A: inbox unavailable (exit 3)"


def test_inbox_preview_reports_subprocess_failure(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def boom(*_a: Any, **_k: Any) -> SimpleNamespace:
        raise subprocess.TimeoutExpired(cmd="agent_a2a", timeout=10)

    monkeypatch.setattr(sb.subprocess, "run", boom)
    out = sb.inbox_preview("fable-5")
    assert out.startswith("A2A: inbox unavailable (")


def test_top_cards_formats_kb_retrieve_hits(monkeypatch: pytest.MonkeyPatch) -> None:
    card = SimpleNamespace(
        name="kb_a.md", text="---\nid: X\n---\n\n# Title\n\nBody line."
    )
    monkeypatch.setattr(sb.kb_retrieve, "load_index", lambda: ["row"])
    monkeypatch.setattr(sb.kb_retrieve, "query_index", lambda task, rows, top_k: [card])
    assert sb.top_cards("do it") == ["- kb_a.md: # Title Body line."]


def test_build_brief_assembles_sections_and_bounds_size() -> None:
    state = {
        "standing_mandates": ["GRAPH_GATE: call the graph", "CLAIM_REQUIRED: claim", 7],
        "active_headings": ["h1", "h2", "h3", "h4", "h5"],
    }
    text = sb.build_brief(
        task="fix x",
        state=state,
        claims_text="CLAIMS: none",
        inbox_text="A2A: no unread for me",
        cards=["- kb_a.md: alpha", "- kb_b.md: beta"],
    )
    assert text.splitlines()[:2] == [
        "TASK: fix x",
        "MANDATES: GRAPH_GATE, CLAIM_REQUIRED",
    ]
    assert "- h4" in text and "- h5" not in text
    assert text.index("CLAIMS: none") < text.index("A2A:") < text.index("CARDS:")
    huge = sb.build_brief(
        task="t", state={}, claims_text="x" * 5000, inbox_text="", cards=[]
    )
    assert len(huge) == sb.MAX_BRIEF_CHARS and huge.endswith("…")
    assert "HEADINGS" not in huge


def test_brief_orchestrates_and_main_prints(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(
        sb, "load_state", lambda refresh: {"standing_mandates": ["M: x"]}
    )
    monkeypatch.setattr(sb, "claims_for_paths", lambda paths: f"CLAIMS for {paths}")
    monkeypatch.setattr(sb, "inbox_preview", lambda agent: f"A2A for {agent}")
    monkeypatch.setattr(sb, "top_cards", lambda task: [f"- card for {task}"])
    monkeypatch.setenv("A2A_AGENT_NAME", "env-agent")
    out = sb.brief("do it", ["p.py"])
    assert "CLAIMS for ['p.py']" in out and "A2A for env-agent" in out
    assert "- card for do it" in out
    with pytest.raises(ValueError, match="empty"):
        sb.brief(" ")
    assert (
        sb.main(["brief", "--task", "do it", "--paths", "p.py", "--agent", "me"]) == 0
    )
    assert "A2A for me" in capsys.readouterr().out
    monkeypatch.setattr("sys.stdin", __import__("io").StringIO(INBOX))
    assert sb.main(["a2a-compact"]) == 0
    assert capsys.readouterr().out.startswith("[UNREAD] aaa")
