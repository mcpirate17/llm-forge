#!/usr/bin/env python3
"""Memory-mapped vector sidecar for the workspace memory index.

``memory_index.jsonl`` stores every chunk vector as a JSON list (240 MB,
~1.4 s to parse per query). This module keeps a ``.vectors.npy`` matrix and a
small ``.meta.json`` next to it, rebuilt only when the JSONL's mtime/size
changes, and searches with one matrix product. Ranking adds a mild recency
boost (a note touched this week outranks a 2026-06 archive at equal
similarity) and collapses near-duplicate chunks (the same handoff text lives
in several sources) so the top-k is diverse. Nothing here writes to the index.
"""

from __future__ import annotations

import json
import math
import os
import tempfile
import time
from pathlib import Path
from typing import Any, Final

import numpy as np

from conductor import memory_index

INDEX_PATH: Final[Path] = memory_index.INDEX_PATH
TEXT_CHARS: Final[int] = 500
DEFAULT_HALF_LIFE_DAYS: Final[float] = 30.0
DEFAULT_RECENCY_BOOST: Final[float] = 0.15
DEFAULT_DEDUP_COSINE: Final[float] = 0.97
SCHEMA_VERSION: Final[int] = 1


def sidecar_paths(index_path: Path = INDEX_PATH) -> tuple[Path, Path]:
    return index_path.with_suffix(".vectors.npy"), index_path.with_suffix(".meta.json")


def _stamp(path: Path) -> dict[str, int]:
    stat = path.stat()
    return {"mtime_ns": stat.st_mtime_ns, "size": stat.st_size}


def _atomic_write_bytes(path: Path, data: bytes) -> None:
    with tempfile.NamedTemporaryFile(
        dir=path.parent, prefix=f".{path.name}.", delete=False
    ) as handle:
        handle.write(data)
        temporary = Path(handle.name)
    temporary.replace(path)


def build_sidecar(
    index_path: Path = INDEX_PATH,
) -> tuple[list[dict[str, Any]], np.ndarray]:
    """Parse the JSONL once and persist matrix + metadata atomically."""
    rows = memory_index.load_index(index_path)
    matrix = np.asarray([row["vector"] for row in rows], dtype=np.float32)
    meta_rows = [
        {
            "source": row["source"],
            "path": row["path"],
            "title": row["title"],
            "text": row["text"][:TEXT_CHARS],
        }
        for row in rows
    ]
    vectors_path, meta_path = sidecar_paths(index_path)
    with tempfile.NamedTemporaryFile(
        dir=vectors_path.parent,
        prefix=f".{vectors_path.name}.",
        suffix=".npy",
        delete=False,
    ) as handle:
        np.save(handle, matrix)
        temporary = Path(handle.name)
    temporary.replace(vectors_path)
    payload = {
        "schema_version": SCHEMA_VERSION,
        "stamp": _stamp(index_path),
        "rows": meta_rows,
    }
    _atomic_write_bytes(
        meta_path, json.dumps(payload, ensure_ascii=False).encode("utf-8")
    )
    return meta_rows, matrix


def load_sidecar(
    index_path: Path = INDEX_PATH,
) -> tuple[list[dict[str, Any]], np.ndarray]:
    """Return (rows, matrix), rebuilding when the JSONL changed or the sidecar is missing."""
    vectors_path, meta_path = sidecar_paths(index_path)
    if vectors_path.is_file() and meta_path.is_file():
        try:
            payload = json.loads(meta_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            payload = None
        if (
            isinstance(payload, dict)
            and payload.get("schema_version") == SCHEMA_VERSION
            and payload.get("stamp") == _stamp(index_path)
        ):
            matrix = np.load(vectors_path, mmap_mode="r")
            rows = payload["rows"]
            if matrix.shape[0] == len(rows):
                return rows, matrix
    return build_sidecar(index_path)


def recency_weights(
    rows: list[dict[str, Any]],
    *,
    now: float | None = None,
    half_life_days: float = DEFAULT_HALF_LIFE_DAYS,
    boost: float = DEFAULT_RECENCY_BOOST,
) -> np.ndarray:
    """1 + boost * 2^(-age/half_life) per row, from the source file's mtime (1.0 if gone)."""
    now = time.time() if now is None else now
    cache: dict[str, float] = {}
    weights = np.ones(len(rows), dtype=np.float32)
    for i, row in enumerate(rows):
        path = row["path"]
        if path not in cache:
            try:
                age_days = max(0.0, now - os.stat(path).st_mtime) / 86400.0
                cache[path] = 1.0 + boost * math.pow(2.0, -age_days / half_life_days)
            except OSError:
                cache[path] = 1.0
        weights[i] = cache[path]
    return weights


def search(
    query_vector: list[float],
    rows: list[dict[str, Any]],
    matrix: np.ndarray,
    *,
    top_k: int = 8,
    now: float | None = None,
    half_life_days: float = DEFAULT_HALF_LIFE_DAYS,
    boost: float = DEFAULT_RECENCY_BOOST,
    dedup_cosine: float = DEFAULT_DEDUP_COSINE,
) -> list[dict[str, Any]]:
    """Top-k rows by recency-weighted dot product, skipping near-duplicate chunks."""
    if top_k < 1:
        raise ValueError("top_k must be >= 1")
    if matrix.shape[0] != len(rows):
        raise ValueError("rows/matrix length mismatch")
    query = np.asarray(query_vector, dtype=np.float32)
    if query.shape != (matrix.shape[1],):
        raise ValueError(
            f"query dimension {query.shape} != index dimension {matrix.shape[1:]}"
        )
    scores = matrix @ query
    weighted = scores * recency_weights(
        rows, now=now, half_life_days=half_life_days, boost=boost
    )
    order = np.argsort(-weighted, kind="stable")
    norms = np.linalg.norm(matrix, axis=1)
    selected: list[int] = []
    for idx in order:
        idx = int(idx)
        vec = matrix[idx]
        duplicate = any(
            float(vec @ matrix[j]) / max(float(norms[idx] * norms[j]), 1e-12)
            > dedup_cosine
            for j in selected
        )
        if duplicate:
            continue
        selected.append(idx)
        if len(selected) == top_k:
            break
    return [
        {
            "score": float(scores[i]),
            "weighted_score": float(weighted[i]),
            "source": rows[i]["source"],
            "path": rows[i]["path"],
            "title": rows[i]["title"],
            "text": rows[i]["text"],
        }
        for i in selected
    ]
