#!/usr/bin/env python3
"""Knowledge-card retriever over the canonical embedding broker.

The index binds to the exact vector-space fingerprint returned by port 7317.
Queries must use that fingerprint, so a local-to-paid route change selects or
builds a compatible index instead of silently comparing unlike vectors.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import sys
import tempfile
import urllib.error
import urllib.request
from dataclasses import dataclass
from itertools import chain
from pathlib import Path
from typing import Any, Final

from conductor.embedding_contract import EMBED_TIMEOUT_SECONDS
from conductor.http_transport import open_http
from conductor.project_paths import host_root, notes_root

BROKER_URL: Final[str] = "http://127.0.0.1:7317/v1/embeddings"
# Two card spellings one tree may mix: the monorepo's lowercase ``kb_*.md`` and
# the KB-id pages this repository's docs use. Both name knowledge cards.
CARD_GLOBS: Final[tuple[str, ...]] = ("kb_*.md", "KB-*.md")
EMBED_MODEL: Final[str] = "qwen3-embed-cpu"
QUERY_INSTRUCT: Final[str] = (
    "Instruct: Given a search query about the Aria LLM workspace, "
    "retrieve the knowledge card or note that answers it.\nQuery: "
)
SCHEMA_VERSION: Final[int] = 2
ALLOWED_NUM_GPU: Final[frozenset[int]] = frozenset({0, 99})
PINNED_NUM_CTX: Final[int] = 2048


class RetrieveError(RuntimeError):
    """Fail-closed retrieval or embedding error."""


@dataclass(frozen=True)
class ScoredCard:
    name: str
    path: str
    score: float
    text: str


@dataclass(frozen=True)
class EmbeddingBatch:
    vectors: list[list[float]]
    metadata: dict[str, Any]


def _l2_normalize(vec: list[float]) -> list[float]:
    from conductor._native import kb_retrieve_l2_normalize_native

    try:
        return kb_retrieve_l2_normalize_native(vec)
    except ValueError as exc:
        raise RetrieveError(str(exc)) from exc


def assert_embedding_meta(payload: dict[str, Any]) -> dict[str, Any]:
    metadata = payload.get("embedding")
    if not isinstance(metadata, dict):
        raise RetrieveError("index embedding metadata is missing")
    fingerprint = metadata.get("fingerprint")
    if not isinstance(fingerprint, str) or not fingerprint.startswith("sha256:"):
        raise RetrieveError("index embedding fingerprint is invalid")
    dimension = metadata.get("dimension")
    if isinstance(dimension, bool) or not isinstance(dimension, int) or dimension < 1:
        raise RetrieveError(f"index embedding dimension is invalid: {dimension!r}")
    paid = metadata.get("paid")
    if not isinstance(paid, bool):
        raise RetrieveError("index embedding paid flag is invalid")
    if not paid:
        gpu = metadata.get("num_gpu")
        if type(gpu) is not int or gpu not in ALLOWED_NUM_GPU:
            raise RetrieveError(
                f"index num_gpu={gpu!r} not in {sorted(ALLOWED_NUM_GPU)} "
                "(0=CPU, 99=GPU guest)"
            )
        ctx = metadata.get("num_ctx")
        if ctx != PINNED_NUM_CTX:
            raise RetrieveError(f"index num_ctx={ctx!r} must be {PINNED_NUM_CTX}")
    return metadata


def assert_guest_embed_meta(payload: dict[str, Any]) -> None:
    """Compatibility alias for older callers while schemas migrate."""

    assert_embedding_meta(payload)


def embed_batch(
    texts: list[str],
    *,
    purpose: str,
    required_fingerprint: str = "",
    url: str | None = None,
    timeout_s: float = EMBED_TIMEOUT_SECONDS,
) -> EmbeddingBatch:
    if not texts:
        return EmbeddingBatch(vectors=[], metadata={})
    endpoint = url or os.environ.get("WORKSPACE_EMBED_BROKER_URL", BROKER_URL)
    payload: dict[str, Any] = {
        "input": texts,
        "workspace_purpose": purpose,
    }
    if required_fingerprint:
        payload["workspace_required_fingerprint"] = required_fingerprint
    request = urllib.request.Request(
        endpoint,
        data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with open_http(
            urllib.request.urlopen,
            request,
            timeout=timeout_s,
            error_type=RetrieveError,
        ) as response:
            body = json.loads(response.read().decode("utf-8"))
    except urllib.error.URLError:
        from conductor.cpu_embed import CpuEmbedError, ensure_service

        try:
            ensure_service()
            with open_http(
                urllib.request.urlopen,
                request,
                timeout=timeout_s,
                error_type=RetrieveError,
            ) as response:
                body = json.loads(response.read().decode("utf-8"))
        except (
            CpuEmbedError,
            urllib.error.URLError,
            TimeoutError,
            json.JSONDecodeError,
        ) as exc:
            raise RetrieveError(
                f"embedding broker failed after auto-start: {exc}"
            ) from exc
    except TimeoutError as exc:
        raise RetrieveError(
            f"embedding broker did not answer within {timeout_s:.0f}s for "
            f"{len(texts)} input(s). The backend holds nothing resident, so the "
            "first call after a quiet period pays a full model load (117 s "
            "measured cold, 0.6 s warm). Load it first with "
            "`python -m conductor.cpu_embed warm`, and do not reindex while a "
            "run is competing for the CPU."
        ) from exc
    except json.JSONDecodeError as exc:
        raise RetrieveError(f"embedding broker returned malformed JSON: {exc}") from exc
    if not isinstance(body, dict):
        raise RetrieveError("embedding broker returned a non-object response")
    error = body.get("error")
    if error is not None:
        raise RetrieveError(f"embedding broker rejected request: {error}")
    rows = body.get("data")
    metadata = body.get("workspace_embedding")
    if not isinstance(rows, list) or not isinstance(metadata, dict):
        raise RetrieveError("embedding broker response lacks data or metadata")
    vectors: list[list[float]] = []
    for index, row in enumerate(rows):
        if not isinstance(row, dict) or not isinstance(row.get("embedding"), list):
            raise RetrieveError(f"embedding broker row {index} is invalid")
        vector = row["embedding"]
        if not vector or not all(
            isinstance(value, (int, float))
            and not isinstance(value, bool)
            and math.isfinite(float(value))
            for value in vector
        ):
            raise RetrieveError(f"embedding broker row {index} is not finite numeric")
        vectors.append(_l2_normalize([float(value) for value in vector]))
    if len(vectors) != len(texts):
        raise RetrieveError(
            f"embedding broker returned {len(vectors)} vectors for {len(texts)} inputs"
        )
    dimension = metadata.get("dimension")
    if any(len(vector) != dimension for vector in vectors):
        raise RetrieveError("embedding broker vector dimension disagrees with metadata")
    return EmbeddingBatch(vectors=vectors, metadata=dict(metadata))


def embed_texts(
    texts: list[str],
    *,
    url: str | None = None,
    required_fingerprint: str = "",
    purpose: str = "document",
    timeout_s: float = EMBED_TIMEOUT_SECONDS,
) -> list[list[float]]:
    """Embed many strings through the canonical route."""

    return embed_batch(
        texts,
        purpose=purpose,
        required_fingerprint=required_fingerprint,
        url=url,
        timeout_s=timeout_s,
    ).vectors


def embed_text(
    text: str,
    *,
    url: str | None = None,
    required_fingerprint: str = "",
    purpose: str = "document",
    timeout_s: float = EMBED_TIMEOUT_SECONDS,
) -> list[float]:
    """Embed one string through the canonical route."""

    return embed_texts(
        [text],
        url=url,
        required_fingerprint=required_fingerprint,
        purpose=purpose,
        timeout_s=timeout_s,
    )[0]


def default_notes_dir() -> Path:
    """The knowledge tree of the workspace this runs in, per its own config."""
    return notes_root(host_root())


def default_index_path() -> Path:
    """The card-index cache, beside the notes tree's parent as the monorepo laid out."""
    return host_root() / "research" / "cache" / "kb_card_index.json"


def load_cards(notes_dir: Path | None = None) -> list[dict[str, str]]:
    notes = default_notes_dir() if notes_dir is None else notes_dir
    if not notes.is_dir():
        raise RetrieveError(f"notes dir missing: {notes}")
    cards: list[dict[str, str]] = []
    for path in sorted(chain.from_iterable(notes.glob(glob) for glob in CARD_GLOBS)):
        cards.append(
            {
                "name": path.name,
                "path": str(path),
                "text": path.read_text(encoding="utf-8"),
            }
        )
    if not cards:
        raise RetrieveError(f"no card files ({', '.join(CARD_GLOBS)}) in {notes}")
    return cards


def build_index(
    cards: list[dict[str, str]],
    *,
    embedder: Any = embed_text,
) -> dict[str, Any]:
    metadata: dict[str, Any]
    if embedder is embed_text:
        batch = embed_batch([card["text"] for card in cards], purpose="document")
        vectors = batch.vectors
        metadata = batch.metadata
    else:
        vectors = [embedder(card["text"]) for card in cards]
        dimension = len(vectors[0]) if vectors else 0
        metadata = {
            "fingerprint": "sha256:" + "a" * 64,
            "dimension": dimension,
            "paid": False,
            "num_gpu": 0,
            "num_ctx": PINNED_NUM_CTX,
        }
    entries: list[dict[str, Any]] = []
    for card, vector in zip(cards, vectors, strict=True):
        entries.append(
            {
                "name": card["name"],
                "path": card["path"],
                "text": card["text"],
                "vector": vector,
            }
        )
    return {
        "schema_version": SCHEMA_VERSION,
        "embedding": metadata,
        "cards": entries,
    }


def save_index(index: dict[str, Any], path: Path | None = None) -> Path:
    destination = default_index_path() if path is None else path
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            "w",
            encoding="utf-8",
            dir=destination.parent,
            prefix=f".{destination.name}.",
            suffix=".tmp",
            delete=False,
        ) as handle:
            temporary = Path(handle.name)
            json.dump(index, handle)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        temporary.replace(destination)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)
    return destination


def load_index(path: Path | None = None) -> dict[str, Any]:
    source = default_index_path() if path is None else path
    if not source.is_file():
        raise RetrieveError(
            f"index missing: {source}; run: python -m conductor.kb_retrieve index"
        )
    try:
        payload = json.loads(source.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise RetrieveError(f"unreadable index {source}: {exc}") from exc
    if not isinstance(payload, dict):
        raise RetrieveError("index root must be a JSON object")
    if payload.get("schema_version") != SCHEMA_VERSION:
        raise RetrieveError(
            f"unsupported index schema {payload.get('schema_version')!r}"
        )
    metadata = assert_embedding_meta(payload)
    cards = payload.get("cards")
    if not isinstance(cards, list) or not cards:
        raise RetrieveError("index contains no cards")
    for card in cards:
        if not isinstance(card, dict):
            raise RetrieveError("index card must be a JSON object")
        if not isinstance(card.get("vector"), list) or not card["vector"]:
            raise RetrieveError(f"index card {card.get('name')!r} has no vector")
        if len(card["vector"]) != metadata["dimension"]:
            raise RetrieveError(
                f"index card {card.get('name')!r} dimension does not match metadata"
            )
    return payload


def query_index(
    query: str,
    index: dict[str, Any],
    *,
    top_k: int = 5,
    embedder: Any = embed_text,
) -> list[ScoredCard]:
    if not query.strip():
        raise RetrieveError("query is empty")
    if top_k < 1:
        raise RetrieveError("top_k must be >= 1")
    metadata = index.get("embedding")
    if embedder is embed_text:
        if not isinstance(metadata, dict) or not isinstance(
            metadata.get("fingerprint"), str
        ):
            raise RetrieveError("index embedding fingerprint is missing")
        qvec = embed_text(
            QUERY_INSTRUCT + query.strip(),
            purpose="query",
            required_fingerprint=metadata["fingerprint"],
        )
    else:
        qvec = embedder(QUERY_INSTRUCT + query.strip())
    from conductor._native import kb_retrieve_score_cards_native

    try:
        hits = kb_retrieve_score_cards_native(qvec, index["cards"], top_k)
    except ValueError as exc:
        raise RetrieveError(str(exc)) from exc
    scored = [
        ScoredCard(name=name, path=path, score=score, text=text)
        for name, path, score, text in hits
    ]
    return scored


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Provider-neutral knowledge-card retriever"
    )
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("index", help="embed kb_*.md and write the card index")
    qparser = sub.add_parser("query", help="retrieve top cards for a query")
    qparser.add_argument("query", type=str)
    qparser.add_argument("--top-k", type=int, default=5)
    qparser.add_argument("--index", type=Path, default=None)
    args = parser.parse_args(argv)
    try:
        if args.command == "index":
            path = save_index(build_index(load_cards()))
            print(
                json.dumps(
                    {
                        "index": str(path),
                        "cards": len(json.loads(path.read_text())["cards"]),
                    }
                )
            )
            return 0
        hits = query_index(args.query, load_index(args.index), top_k=args.top_k)
        print(
            json.dumps(
                [
                    {"name": h.name, "score": round(h.score, 4), "path": h.path}
                    for h in hits
                ],
                indent=2,
            )
        )
        return 0
    except RetrieveError as exc:
        print(json.dumps({"error": str(exc)}), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
