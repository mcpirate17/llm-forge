from __future__ import annotations

import json
from pathlib import Path

import pytest

from conductor import memory_index


def test_query_index_file_streams_with_stable_exact_contract(tmp_path: Path) -> None:
    metadata = {
        "fingerprint": "sha256:test-space",
        "dimension": 2,
        "paid": True,
    }
    long_text = "λ" * 501
    rows = [
        {
            "schema_version": memory_index.SCHEMA_VERSION,
            "embedding": metadata,
            "source_sha256": "a" * 64,
            "source": "notes",
            "path": "first.md",
            "title": "first",
            "text": long_text,
            "vector": [1.0, 0.0],
        },
        {
            "schema_version": memory_index.SCHEMA_VERSION,
            "embedding": metadata,
            "source_sha256": "b" * 64,
            "source": "cards",
            "path": "second.md",
            "title": "second",
            "text": "second",
            "vector": [1.0, 0.0],
        },
        {
            "schema_version": memory_index.SCHEMA_VERSION,
            "embedding": metadata,
            "source_sha256": "c" * 64,
            "source": "notes",
            "path": "low.md",
            "title": "low",
            "text": "low",
            "vector": [0.0, 1.0],
        },
    ]
    path = tmp_path / "memory.jsonl"
    path.write_text(
        "\n".join(json.dumps(row, ensure_ascii=False) for row in rows) + "\n",
        encoding="utf-8",
    )
    embedded: list[str] = []

    def embed(text: str) -> list[float]:
        embedded.append(text)
        return [1.0, 0.0]

    hits = memory_index.query_index_file(
        "  stable ties  ", path, top_k=2, embedder=embed
    )

    assert embedded == [memory_index.QUERY_INSTRUCT + "stable ties"]
    assert [hit["path"] for hit in hits] == ["first.md", "second.md"]
    assert [hit["score"] for hit in hits] == [1.0, 1.0]
    assert hits[0]["text"] == "λ" * 500
    assert set(hits[0]) == {"score", "source", "path", "title", "text"}

    with pytest.raises(memory_index.RetrieveError, match="query dimension 1 !="):
        memory_index.query_index_file("q", path, embedder=lambda _text: [1.0])
