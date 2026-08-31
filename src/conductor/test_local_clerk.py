"""Focused security and provenance tests for the bounded local clerk."""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from conductor import local_clerk as clerk


@pytest.fixture
def clerk_root(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setattr(clerk, "ROOT", tmp_path)
    monkeypatch.setattr(clerk, "ALLOWED_ROOTS", (tmp_path,))
    monkeypatch.setattr(clerk, "OUTPUT_ROOT", tmp_path / "outputs")
    return tmp_path


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        ("http://127.0.0.1:11434", ("127.0.0.1", 11434, "/api/generate")),
        ("http://localhost", ("localhost", 80, "/api/generate")),
        ("http://[::1]:11434", ("::1", 11434, "/api/generate")),
    ],
)
def test_ollama_endpoint_accepts_only_plain_loopback_origins(
    raw: str, expected: tuple[str, int, str], monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("OLLAMA_HOST", raw)
    assert clerk._ollama_endpoint() == expected


@pytest.mark.parametrize(
    "raw",
    [
        "https://127.0.0.1:11434",
        "http://example.com:11434",
        "http://user:placeholder@localhost:11434",  # pragma: allowlist secret
        "http://localhost:11434/api/generate",
        "http://localhost:11434?model=other",
    ],
)
def test_ollama_endpoint_rejects_non_loopback_or_ambient_request_data(
    raw: str, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("OLLAMA_HOST", raw)
    with pytest.raises(clerk.ClerkError, match="plain loopback HTTP origin"):
        clerk._ollama_endpoint()


def test_output_paths_and_atomic_writer_never_clobber_existing_files(
    clerk_root: Path,
) -> None:
    source = clerk.OUTPUT_ROOT / "source.json"
    source.parent.mkdir(parents=True)
    source.write_text("source", encoding="utf-8")
    sources = [{"path": str(source)}]

    with pytest.raises(clerk.ClerkError, match="collides with a source"):
        clerk._safe_output_path(source, sources=sources)

    existing = clerk.OUTPUT_ROOT / "existing.json"
    existing.write_text("preserve me", encoding="utf-8")
    with pytest.raises(clerk.ClerkError, match="already exists"):
        clerk._safe_output_path(existing, sources=[])
    with pytest.raises(clerk.ClerkError, match="already exists"):
        clerk._write_json(existing, {"replacement": True})
    assert existing.read_text(encoding="utf-8") == "preserve me"


def test_read_source_rejects_a_file_that_changes_during_read(
    clerk_root: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = clerk_root / "note.md"
    source.write_text("# Stable title\nbody\n", encoding="utf-8")
    stat = source.stat()
    stats = [
        SimpleNamespace(
            st_dev=stat.st_dev,
            st_ino=stat.st_ino,
            st_size=stat.st_size,
            st_mtime_ns=stat.st_mtime_ns,
        ),
        SimpleNamespace(
            st_dev=stat.st_dev,
            st_ino=stat.st_ino,
            st_size=stat.st_size,
            st_mtime_ns=stat.st_mtime_ns + 1,
        ),
    ]
    monkeypatch.setattr(clerk.os, "fstat", lambda _fd: stats.pop(0))

    with pytest.raises(clerk.ClerkError, match="changed while being read"):
        clerk._read_source(source)


def test_multi_source_prompt_is_bounded_complete_and_valid_json(tmp_path: Path) -> None:
    sources = [
        clerk.Source(
            path=tmp_path / f"source-{number}.md",
            sha256=str(number) * 64,
            size_bytes=clerk.MAX_SOURCE_TEXT_CHARS,
            title=f"Source {number}",
            text=str(number) * clerk.MAX_SOURCE_TEXT_CHARS,
            truncated=False,
        )
        for number in range(clerk.MAX_SOURCE_COUNT)
    ]

    prompt, prompt_chars = clerk._prompt(sources)
    envelope = json.loads(prompt[prompt.index("{") :])

    assert len(prompt) <= clerk.MAX_PROMPT_CHARS
    assert len(envelope["sources"]) == clerk.MAX_SOURCE_COUNT
    assert [item["path"] for item in envelope["sources"]] == [
        str(source.path) for source in sources
    ]
    assert prompt_chars == [len(item["text"]) for item in envelope["sources"]]
    assert all(chars >= clerk.MIN_PROMPT_SOURCE_CHARS for chars in prompt_chars)


def _valid_model_draft() -> dict[str, object]:
    return {
        "summary": "A bounded summary.",
        "source_decisions": [],
        "open_tasks": ["Review the result."],
        "todo_items": [],
        "duplicate_candidates": [],
    }


def test_draft_provenance_round_trips_and_rejects_tampering(
    clerk_root: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = clerk_root / "note.md"
    source.write_text("# Note\n" + "bounded facts " * 40, encoding="utf-8")
    monkeypatch.setattr(
        clerk, "_generate", lambda _prompt, *, model: (_valid_model_draft(), 12)
    )

    payload = clerk.draft([source])
    draft_path = clerk_root / "draft.json"
    draft_path.write_text(json.dumps(payload), encoding="utf-8")
    assert clerk.validate_document(draft_path) == {
        "valid": True,
        "sources": 1,
        "path": str(draft_path),
    }

    tampered = json.loads(json.dumps(payload))
    tampered["sources"][0]["sha256"] = "0" * 64
    tampered_path = clerk_root / "tampered.json"
    tampered_path.write_text(json.dumps(tampered), encoding="utf-8")
    with pytest.raises(clerk.ClerkError, match="source changed since draft"):
        clerk.validate_document(tampered_path)
