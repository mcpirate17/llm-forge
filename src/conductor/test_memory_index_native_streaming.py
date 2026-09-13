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


def _sidecar_index_rows(count: int, dimension: int = 4) -> list[dict[str, object]]:
    """Deterministic pseudo-random rows: same LCG as the Rust sidecar tests."""

    def lcg(seed: int, step: int) -> float:
        state = (
            (seed * 6_364_136_223_846_793_005 + 1_442_695_040_888_963_407 + step * 7)
            & 0xFFFFFFFFFFFFFFFF
        )
        state = ((state << 13) | (state >> 51)) & 0xFFFFFFFFFFFFFFFF
        return (state >> 40) / (1 << 24) * 2.0 - 1.0

    rows = []
    for index in range(count):
        rows.append(
            {
                "schema_version": memory_index.SCHEMA_VERSION,
                "embedding": {
                    "fingerprint": "sha256:sidecar-parity",
                    "dimension": dimension,
                    "paid": True,
                },
                "source_sha256": f"{index:064x}",
                "source": "notes",
                "path": f"row-{index:04}.md",
                "title": f"row {index}",
                "text": f"chunk {index}",
                "vector": [round(lcg(index, step), 9) for step in range(dimension)],
            }
        )
    return rows


def _write_rows(path: Path, rows: list[dict[str, object]]) -> None:
    path.write_text(
        "\n".join(json.dumps(row) for row in rows) + "\n", encoding="utf-8"
    )


def test_first_query_builds_the_sidecar_and_second_query_reuses_it(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    path = tmp_path / "memory.jsonl"
    _write_rows(path, _sidecar_index_rows(3, dimension=2))

    first = memory_index.query_index_file(
        "q", path, top_k=2, embedder=lambda _text: [1.0, 0.0]
    )
    err = capsys.readouterr().err
    assert err.count("memory_index: building sidecar (3 rows)") == 1, err
    sidecar = tmp_path / "memory.jsonl.sidecar"
    assert sidecar.is_file(), "the first query must leave the sidecar behind"

    second = memory_index.query_index_file(
        "q", path, top_k=2, embedder=lambda _text: [1.0, 0.0]
    )
    assert capsys.readouterr().err == "", "the second query must reuse, not rebuild"
    assert first == second


def test_appending_a_row_rebuilds_the_sidecar_and_ranks_it(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    rows = _sidecar_index_rows(3, dimension=2)
    orthogonal = [[0.0, 1.0] for _ in rows]  # score 0 against the [1, 0] query
    for row, vector in zip(rows, orthogonal):
        row["vector"] = vector
    path = tmp_path / "memory.jsonl"
    _write_rows(path, rows)

    hits = memory_index.query_index_file(
        "q", path, top_k=1, embedder=lambda _text: [1.0, 0.0]
    )
    assert capsys.readouterr().err.count("building sidecar") == 1
    assert hits[0]["score"] == 0.0  # everything is orthogonal so far

    appended = _sidecar_index_rows(1, dimension=2)[0]
    appended["vector"] = [1.0, 0.0]
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(appended) + "\n")

    hits = memory_index.query_index_file(
        "q", path, top_k=1, embedder=lambda _text: [1.0, 0.0]
    )
    assert capsys.readouterr().err.count("building sidecar (4 rows)") == 1
    assert hits[0]["path"] == "row-0000.md"
    assert hits[0]["score"] == 1.0


def test_sidecar_query_matches_the_full_scan_reference(tmp_path: Path) -> None:
    rows = _sidecar_index_rows(120, dimension=4)
    path = tmp_path / "memory.jsonl"
    _write_rows(path, rows)

    def embed(_text: str) -> list[float]:
        return [0.31, -0.7, 0.11, 0.9]

    # Give the eventual winner a 600-char body so truncation is compared too:
    # the reference path ranks first (it never builds a sidecar), then the
    # index is rewritten before either sidecar-path query runs.
    reference = memory_index._query_index_file_scan(
        "parity", path, top_k=10, embedder=embed
    )
    winner = next(row for row in rows if row["path"] == reference[0]["path"])
    winner["text"] = "λ" * 600
    _write_rows(path, rows)
    sidecar_hits = memory_index.query_index_file(
        "parity", path, top_k=10, embedder=embed
    )
    reference = memory_index._query_index_file_scan(
        "parity", path, top_k=10, embedder=embed
    )
    assert [hit["path"] for hit in sidecar_hits] == [
        hit["path"] for hit in reference
    ]
    for sidecar_hit, reference_hit in zip(sidecar_hits, reference):
        assert abs(sidecar_hit["score"] - reference_hit["score"]) < 1e-5
    assert sidecar_hits[0]["text"] == "λ" * 500
