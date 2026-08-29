"""Tests for the memory-index vector sidecar."""

from __future__ import annotations

import json
import os
import time
from pathlib import Path

import numpy as np
import pytest

from conductor import memory_vectors as mv


def _row(path: str, vec: list[float], title: str = "t", text: str = "body") -> dict:
    return {
        "source": "notes",
        "path": path,
        "title": title,
        "text": text,
        "vector": vec,
    }


@pytest.fixture
def index(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    old = tmp_path / "old.md"
    new = tmp_path / "new.md"
    old.write_text("old", encoding="utf-8")
    new.write_text("new", encoding="utf-8")
    week = 7 * 86400
    os.utime(old, (time.time() - 200 * 86400, time.time() - 200 * 86400))
    os.utime(new, (time.time() - week, time.time() - week))
    rows = [
        _row(str(old), [1.0, 0.0, 0.0], "old-exact"),
        _row(str(new), [0.95, 0.31, 0.0], "new-close"),
        _row(str(old), [1.0, 0.001, 0.0], "old-duplicate"),
        _row(str(tmp_path / "gone.md"), [0.0, 1.0, 0.0], "orthogonal"),
    ]
    path = tmp_path / "memory_index.jsonl"
    path.write_text("\n".join(json.dumps(r) for r in rows) + "\n", encoding="utf-8")
    monkeypatch.setattr(mv.memory_index, "load_index", lambda p=path: rows)
    return path


def test_sidecar_builds_once_and_reloads_from_mmap(index: Path) -> None:
    rows, matrix = mv.load_sidecar(index)
    vectors_path, meta_path = mv.sidecar_paths(index)
    assert vectors_path.is_file() and meta_path.is_file()
    assert matrix.shape == (4, 3) and matrix.dtype == np.float32
    assert rows[0]["title"] == "old-exact" and "vector" not in rows[0]
    stamp = meta_path.stat().st_mtime_ns
    rows2, matrix2 = mv.load_sidecar(index)
    assert meta_path.stat().st_mtime_ns == stamp  # not rebuilt
    assert isinstance(matrix2, np.memmap) and len(rows2) == 4


def test_sidecar_rebuilds_when_index_changes(index: Path) -> None:
    mv.load_sidecar(index)
    _, meta_path = mv.sidecar_paths(index)
    before = meta_path.read_text(encoding="utf-8")
    time.sleep(0.01)
    index.write_text(index.read_text(encoding="utf-8") + "\n", encoding="utf-8")
    mv.load_sidecar(index)
    assert meta_path.read_text(encoding="utf-8") != before
    meta_path.write_text("{broken", encoding="utf-8")
    rows, _ = mv.load_sidecar(index)
    assert len(rows) == 4


def test_search_applies_recency_and_dedup(index: Path) -> None:
    rows, matrix = mv.load_sidecar(index)
    hits = mv.search([1.0, 0.0, 0.0], rows, matrix, top_k=3)
    titles = [h["title"] for h in hits]
    # exact old match scores 1.0 but the week-old near match wins on recency;
    # the near-identical duplicate of the old chunk is collapsed.
    assert titles == ["new-close", "old-exact", "orthogonal"]
    assert hits[1]["score"] == pytest.approx(1.0)
    assert hits[0]["weighted_score"] > hits[1]["weighted_score"]
    plain = mv.search(
        [1.0, 0.0, 0.0], rows, matrix, top_k=3, boost=0.0, dedup_cosine=1.01
    )
    assert [h["title"] for h in plain] == ["old-exact", "old-duplicate", "new-close"]


def test_search_validates_inputs(index: Path) -> None:
    rows, matrix = mv.load_sidecar(index)
    with pytest.raises(ValueError, match="top_k"):
        mv.search([1.0, 0.0, 0.0], rows, matrix, top_k=0)
    with pytest.raises(ValueError, match="dimension"):
        mv.search([1.0, 0.0], rows, matrix)
    with pytest.raises(ValueError, match="mismatch"):
        mv.search([1.0, 0.0, 0.0], rows[:1], matrix)


def test_recency_weights_bounds(index: Path) -> None:
    rows, _ = mv.load_sidecar(index)
    weights = mv.recency_weights(rows, boost=0.5, half_life_days=30)
    assert weights[3] == 1.0  # missing file
    assert 1.0 < weights[0] < weights[1] <= 1.5
