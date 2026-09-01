"""Tests for the one-call session brief."""

from __future__ import annotations

import subprocess
import sqlite3
import json
import sys
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
COMPACT_INBOX = {
    "schema_version": 1,
    "authority": "bounded-a2a-inbox",
    "agent": "fable-5",
    "unread_only": True,
    "total": 1,
    "shown": 1,
    "omitted": 0,
    "raw_bytes_not_injected": 12,
    "messages": [
        {
            "id": "aaa",
            "from": "codex",
            "at": "t1",
            "thread": "thread-1",
            "status": "open",
            "requires_response": True,
            "summary": "short",
            "raw_bytes": 12,
        }
    ],
}


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

    def fake_run(command: list[str], **_kwargs: Any) -> SimpleNamespace:
        assert command == [
            sys.executable,
            "-m",
            "conductor.agent_a2a",
            "inbox",
            "--as-name",
            "fable-5",
            "--unread",
            "--compact",
            "--max-messages",
            "8",
            "--preview-chars",
            "140",
            "--max-chars",
            "1200",
            "--json",
        ]
        return SimpleNamespace(returncode=0, stdout=json.dumps(COMPACT_INBOX))

    monkeypatch.setattr(sb.subprocess, "run", fake_run)
    out = sb.inbox_preview("fable-5")
    compact = json.dumps(
        COMPACT_INBOX, ensure_ascii=False, separators=(",", ":"), sort_keys=True
    )
    assert out == f"A2A unread (fable-5); full message by explicit show:\n{compact}"
    empty = {
        **COMPACT_INBOX,
        "total": 0,
        "shown": 0,
        "messages": [],
        "raw_bytes_not_injected": 0,
    }
    monkeypatch.setattr(
        sb.subprocess,
        "run",
        lambda *a, **k: SimpleNamespace(returncode=0, stdout=json.dumps(empty)),
    )
    assert sb.inbox_preview("fable-5") == "A2A: no unread for fable-5"
    monkeypatch.setattr(
        sb.subprocess, "run", lambda *a, **k: SimpleNamespace(returncode=3, stdout="")
    )
    assert sb.inbox_preview("fable-5") == "A2A: inbox unavailable (exit 3)"


def test_inbox_preview_rejects_untrusted_compact_envelopes(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    wrong_schema = {**COMPACT_INBOX, "schema_version": 2}
    wrong_agent = {**COMPACT_INBOX, "agent": "other"}
    wrong_count = {**COMPACT_INBOX, "omitted": 1}
    wrong_integer = {**COMPACT_INBOX, "total": True}
    raw_content = json.loads(json.dumps(COMPACT_INBOX))
    raw_content["messages"][0]["body"] = "full body must not cross the boundary"
    oversized = {**COMPACT_INBOX, "extension": "x" * sb.MAX_INBOX_CHARS}

    for payload in (
        [],
        wrong_schema,
        wrong_agent,
        wrong_count,
        wrong_integer,
        raw_content,
        oversized,
    ):
        monkeypatch.setattr(
            sb.subprocess,
            "run",
            lambda *a, _payload=payload, **k: SimpleNamespace(
                returncode=0, stdout=json.dumps(_payload)
            ),
        )
        assert sb.inbox_preview("fable-5") == (
            "A2A: inbox unavailable (untrusted compact response)"
        )

    monkeypatch.setattr(
        sb.subprocess,
        "run",
        lambda *a, **k: SimpleNamespace(returncode=0, stdout="not-json"),
    )
    assert sb.inbox_preview("fable-5") == (
        "A2A: inbox unavailable (invalid compact response)"
    )


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
    monkeypatch.setattr(
        sb.kb_retrieve,
        "query_index",
        lambda task, rows, top_k, embedder: [card],
    )
    assert sb.top_cards("do it") == ["- kb_a.md: # Title Body line."]


def test_brief_degrades_cleanly_when_local_indexes_are_missing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        sb, "load_state", lambda refresh: {"standing_mandates": ["GRAPH_GATE: call"]}
    )
    monkeypatch.setattr(sb, "claims_for_paths", lambda paths: "CLAIMS: none")
    monkeypatch.setattr(sb, "inbox_preview", lambda agent: "")
    monkeypatch.setattr(
        sb.kb_retrieve,
        "load_index",
        lambda: (_ for _ in ()).throw(FileNotFoundError("clean checkout")),
    )
    monkeypatch.setattr(
        sb.memory_vectors,
        "load_sidecar",
        lambda: (_ for _ in ()).throw(FileNotFoundError("clean checkout")),
    )
    monkeypatch.setattr(sb, "task_previews", lambda task: [])

    out = sb.brief("repair clean checkout", ["conductor/example.py"])

    assert "TASK: repair clean checkout" in out
    assert "MANDATES: GRAPH_GATE" in out
    assert "CLAIMS: none" in out


def test_kb_and_memory_retrieval_share_one_query_embedding(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    sb._query_vector.cache_clear()
    calls: list[tuple[str, str]] = []
    vector = [0.25, 0.75]

    def fake_embed(text: str, *, purpose: str) -> list[float]:
        calls.append((text, purpose))
        return vector

    def fake_query(_task, _rows, *, top_k, embedder):
        assert top_k == sb.CARDS_K
        assert embedder("ignored") is vector
        return []

    def fake_search(query, _rows, _matrix, *, top_k):
        assert top_k == sb.MEMORY_K
        assert query is vector
        return []

    monkeypatch.setattr(sb.kb_retrieve, "embed_text", fake_embed)
    monkeypatch.setattr(sb.kb_retrieve, "load_index", lambda: ["kb-row"])
    monkeypatch.setattr(sb.kb_retrieve, "query_index", fake_query)
    monkeypatch.setattr(sb.memory_vectors, "load_sidecar", lambda: (["row"], "matrix"))
    monkeypatch.setattr(sb.memory_vectors, "search", fake_search)

    assert sb.top_cards("shared task") == []
    assert sb.memory_previews("shared task") == []
    assert calls == [(sb.kb_retrieve.QUERY_INSTRUCT + "shared task", "query")]
    sb._query_vector.cache_clear()


def test_task_previews_opens_index_read_only(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from research.tools import index_notes

    database = tmp_path / "notes.sqlite"
    sqlite3.connect(database).close()

    def fake_search(connection, task, *, limit, source):
        assert task == "pending work"
        assert limit == 3
        assert source == "tasks"
        with pytest.raises(sqlite3.OperationalError, match="readonly"):
            connection.execute("CREATE TABLE forbidden_write (value INTEGER)")
        return [{"title": "Task", "snippet": "Pending", "path": "task.md"}]

    monkeypatch.setattr(index_notes, "DB_PATH", database)
    monkeypatch.setattr(index_notes, "search_notes", fake_search)

    assert sb.task_previews("pending work") == ["- Task: Pending (task.md)"]


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
    monkeypatch.setattr(sb, "memory_previews", lambda task: [])
    monkeypatch.setattr(sb, "task_previews", lambda task: [])
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


def test_compact_inbox_passes_already_compact_input_through() -> None:
    compact = (
        "A2A compact agent=fable-5 total=2 shown=2 omitted=0\n"
        "[open] aaa from=codex thread=t1 response=yes\n  first summary\n"
        "[working] bbb from=helm thread=t2\n  second summary\n"
        "raw bytes withheld from context: 999\n"
    )
    assert sb.is_compact_inbox(compact) and not sb.is_compact_inbox(INBOX)
    assert sb.compact_inbox(compact) == compact.strip()
    assert sb.compact_inbox(compact, max_msgs=1) == compact.strip()
