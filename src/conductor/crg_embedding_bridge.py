#!/usr/bin/env python3
"""Version-gated code-review-graph provider for workspace embeddings.

The installed code-review-graph release has no external provider plugin seam.
This module replaces only its ``get_provider`` factory before the MCP server is
imported.  The source/version assertions make dependency drift visible instead
of silently restoring keyword fallback or a cloud provider.
"""

from __future__ import annotations

import hashlib
import importlib.metadata
import json
import os
import time
from pathlib import Path

from conductor.atomic_json import write_json_atomic
from typing import Any, Final

from conductor.cpu_embed import ensure_service
from conductor.embedding_contract import (
    load_quality_receipt,
    load_routing_config,
    route_attempts,
)
from conductor.kb_retrieve import EmbeddingBatch, embed_batch

EXPECTED_CRG_VERSION: Final[str] = "2.3.1"
EXPECTED_EMBEDDINGS_SHA256: Final[str] = (
    "aa86b01dacae7832548d788be9b2ca5123500da06f0fa65c81cb0bf04eea1b29"
)
EXPECTED_MAIN_SHA256: Final[str] = (
    "1d01605d2c97d4e2753b7a89e4e46e9f0113fbe8f20d17b3a08a5c08bced6f70"
)
BRIDGE_CONTRACT_VERSION: Final[int] = 1
QUERY_INSTRUCT: Final[str] = (
    "Instruct: Given a natural-language request about a Python/ML workspace, "
    "retrieve the code entity whose behavior best answers it.\nQuery: "
)
EMBED_BATCH_SIZE: Final[int] = 128
TRACE_PATH_ENV: Final[str] = "WORKSPACE_CRG_EMBED_TRACE_PATH"


class CrgBridgeError(RuntimeError):
    """The pinned CRG seam or embedding contract is unavailable."""


def _source_sha256(module: Any) -> str:
    source = getattr(module, "__file__", None)
    if not isinstance(source, str):
        raise CrgBridgeError(f"module {module!r} has no source path")
    return hashlib.sha256(Path(source).read_bytes()).hexdigest()


def assert_supported_crg() -> None:
    """Fail closed if the private injection seam changed."""

    version = importlib.metadata.version("code-review-graph")
    if version != EXPECTED_CRG_VERSION:
        raise CrgBridgeError(
            f"code-review-graph version {version!r} != {EXPECTED_CRG_VERSION!r}"
        )
    import code_review_graph.embeddings as embeddings
    import code_review_graph.main as crg_main

    hashes = {
        "embeddings": _source_sha256(embeddings),
        "main": _source_sha256(crg_main),
    }
    expected = {
        "embeddings": EXPECTED_EMBEDDINGS_SHA256,
        "main": EXPECTED_MAIN_SHA256,
    }
    if hashes != expected:
        raise CrgBridgeError(
            f"code-review-graph source hash drift: actual={hashes}, expected={expected}"
        )
    for symbol in ("get_provider", "EmbeddingStore", "EmbeddingProvider"):
        if not hasattr(embeddings, symbol):
            raise CrgBridgeError(f"code-review-graph embeddings lacks {symbol!r}")


class WorkspaceEmbeddingProvider:
    """CRG provider whose vectors all come from one broker fingerprint."""

    def __init__(self, requested_model: str | None = None) -> None:
        ensure_service()
        attempts = route_attempts(load_routing_config(), quality=load_quality_receipt())
        route = attempts[0].route
        if requested_model not in {None, "", route.model}:
            raise CrgBridgeError(
                f"requested CRG model {requested_model!r} does not match selected "
                f"route model {route.model!r}"
            )
        self._route = route
        self._query_contract_sha256 = hashlib.sha256(
            QUERY_INSTRUCT.encode()
        ).hexdigest()
        provider_payload = {
            "bridge_contract_version": BRIDGE_CONTRACT_VERSION,
            "backend_fingerprint": route.fingerprint,
            "query_contract_sha256": self._query_contract_sha256,
            "document_transform": "none",
        }
        canonical = json.dumps(provider_payload, sort_keys=True, separators=(",", ":"))
        self._provider_fingerprint = hashlib.sha256(canonical.encode()).hexdigest()

    @property
    def dimension(self) -> int:
        return self._route.dimension

    @property
    def name(self) -> str:
        return f"workspace:{self._provider_fingerprint}"

    def _embed(self, texts: list[str], *, purpose: str) -> list[list[float]]:
        if not texts:
            return []
        started = time.monotonic()
        vectors: list[list[float]] = []
        calls = 0
        last: EmbeddingBatch | None = None
        for start in range(0, len(texts), EMBED_BATCH_SIZE):
            batch = texts[start : start + EMBED_BATCH_SIZE]
            last = embed_batch(
                batch,
                purpose=purpose,
                required_fingerprint=self._route.fingerprint,
            )
            if last.metadata.get("fingerprint") != self._route.fingerprint:
                raise CrgBridgeError(
                    "broker changed vector spaces during CRG embedding"
                )
            if last.metadata.get("dimension") != self.dimension:
                raise CrgBridgeError("broker dimension changed during CRG embedding")
            vectors.extend(last.vectors)
            calls += 1
        self._write_trace(
            {
                "schema_version": 1,
                "bridge_contract_version": BRIDGE_CONTRACT_VERSION,
                "provider_name": self.name,
                "backend_fingerprint": self._route.fingerprint,
                "query_contract_sha256": self._query_contract_sha256,
                "route_id": self._route.route_id,
                "model": self._route.model,
                "model_revision": self._route.model_revision,
                "dimension": self.dimension,
                "paid": self._route.paid,
                "purpose": purpose,
                "vector_count": len(vectors),
                "broker_calls": calls,
                "selection_reason": (
                    last.metadata.get("selection_reason") if last is not None else None
                ),
                "elapsed_seconds": round(time.monotonic() - started, 6),
            }
        )
        return vectors

    def embed(self, texts: list[str]) -> list[list[float]]:
        return self._embed(texts, purpose="document")

    def embed_query(self, text: str) -> list[float]:
        return self._embed([QUERY_INSTRUCT + text], purpose="query")[0]

    @staticmethod
    def _write_trace(payload: dict[str, Any]) -> None:
        raw_path = os.environ.get(TRACE_PATH_ENV, "").strip()
        if not raw_path:
            return
        write_json_atomic(Path(raw_path), payload)


def install_bridge() -> None:
    """Install the workspace factory before CRG constructs an EmbeddingStore."""

    assert_supported_crg()
    import code_review_graph.embeddings as embeddings

    def workspace_get_provider(
        provider: str | None = None,
        model: str | None = None,
    ) -> WorkspaceEmbeddingProvider:
        if provider not in {None, "", "workspace"}:
            raise CrgBridgeError(
                f"CRG provider {provider!r} is disabled; use the workspace route"
            )
        return WorkspaceEmbeddingProvider(requested_model=model)

    embeddings.get_provider = workspace_get_provider
