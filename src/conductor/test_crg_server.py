from __future__ import annotations

import hashlib
import json
import sys
import types
from pathlib import Path

import pytest

from conductor import crg_embedding_bridge as bridge


def test_source_sha256_hashes_or_rejects_missing_file(tmp_path: Path) -> None:
    with pytest.raises(bridge.CrgBridgeError, match="no source path"):
        bridge._source_sha256(object())
    (tmp_path / "m.py").write_bytes(b"X = 1\n")
    mod = types.SimpleNamespace(__file__=str(tmp_path / "m.py"))
    assert bridge._source_sha256(mod) == hashlib.sha256(b"X = 1\n").hexdigest()


def _install_fake_crg_modules(
    monkeypatch: pytest.MonkeyPatch,
) -> tuple[types.ModuleType, types.ModuleType]:
    fake_embeddings = types.ModuleType("code_review_graph.embeddings")
    fake_main = types.ModuleType("code_review_graph.main")
    monkeypatch.setitem(
        sys.modules, "code_review_graph", types.ModuleType("code_review_graph")
    )
    monkeypatch.setitem(sys.modules, "code_review_graph.embeddings", fake_embeddings)
    monkeypatch.setitem(sys.modules, "code_review_graph.main", fake_main)
    monkeypatch.setattr(
        bridge.importlib.metadata, "version", lambda name: bridge.EXPECTED_CRG_VERSION
    )
    return fake_embeddings, fake_main


def _matching_hash(fake_embeddings: types.ModuleType, fake_main: types.ModuleType):
    def fake_hash(module: object) -> str:
        return (
            bridge.EXPECTED_EMBEDDINGS_SHA256
            if module is fake_embeddings
            else bridge.EXPECTED_MAIN_SHA256
        )

    return fake_hash


def test_assert_supported_crg_validates_version_hashes_and_symbols(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(bridge.importlib.metadata, "version", lambda name: "9.9.9")
    with pytest.raises(bridge.CrgBridgeError, match="version"):
        bridge.assert_supported_crg()

    fake_embeddings, fake_main = _install_fake_crg_modules(monkeypatch)
    monkeypatch.setattr(bridge, "_source_sha256", lambda module: "deadbeef" * 8)
    with pytest.raises(bridge.CrgBridgeError, match="source hash drift"):
        bridge.assert_supported_crg()

    monkeypatch.setattr(
        bridge, "_source_sha256", _matching_hash(fake_embeddings, fake_main)
    )
    with pytest.raises(bridge.CrgBridgeError, match="lacks"):
        bridge.assert_supported_crg()

    fake_embeddings.get_provider = None
    fake_embeddings.EmbeddingStore = object
    fake_embeddings.EmbeddingProvider = object
    bridge.assert_supported_crg()


def test_install_bridge_replaces_get_provider(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(bridge, "assert_supported_crg", lambda: None)
    fake_embeddings = types.ModuleType("code_review_graph.embeddings")
    monkeypatch.setitem(
        sys.modules, "code_review_graph", types.ModuleType("code_review_graph")
    )
    monkeypatch.setitem(sys.modules, "code_review_graph.embeddings", fake_embeddings)

    bridge.install_bridge()

    assert fake_embeddings.get_provider is not None
    with pytest.raises(bridge.CrgBridgeError, match="disabled"):
        fake_embeddings.get_provider(provider="cloud")
    monkeypatch.setattr(bridge, "ensure_service", lambda: False)
    provider = fake_embeddings.get_provider()
    assert isinstance(provider, bridge.WorkspaceEmbeddingProvider)


def test_init_raises_when_requested_model_does_not_match_route(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(bridge, "ensure_service", lambda: False)
    with pytest.raises(bridge.CrgBridgeError, match="does not match selected"):
        bridge.WorkspaceEmbeddingProvider(requested_model="totally-different-model")


def test_embed_validates_batch_size_and_response_integrity(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(bridge, "ensure_service", lambda: False)
    provider = bridge.WorkspaceEmbeddingProvider()
    assert provider.embed([]) == []

    monkeypatch.setattr(
        bridge,
        "embed_batch",
        lambda texts, **kwargs: bridge.EmbeddingBatch(
            vectors=[[0.0] * provider.dimension for _ in texts],
            metadata={
                "fingerprint": "sha256:" + "9" * 64,
                "dimension": provider.dimension,
            },
        ),
    )
    with pytest.raises(bridge.CrgBridgeError, match="changed vector spaces"):
        provider.embed(["a"])

    monkeypatch.setattr(
        bridge,
        "embed_batch",
        lambda texts, **kwargs: bridge.EmbeddingBatch(
            vectors=[[0.0] * (provider.dimension + 1) for _ in texts],
            metadata={
                "fingerprint": provider._route.fingerprint,
                "dimension": provider.dimension + 1,
            },
        ),
    )
    with pytest.raises(bridge.CrgBridgeError, match="dimension changed"):
        provider.embed(["a"])


def test_provider_name_binds_backend_and_query_contract(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(bridge, "ensure_service", lambda: False)
    provider = bridge.WorkspaceEmbeddingProvider()

    assert provider.dimension == 1024
    assert provider.name.startswith("workspace:")
    assert "qwen" not in provider.name


def test_provider_pins_broker_fingerprint(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(bridge, "ensure_service", lambda: False)
    provider = bridge.WorkspaceEmbeddingProvider()
    calls: list[dict[str, object]] = []

    def fake_embed_batch(texts: list[str], **kwargs: object) -> bridge.EmbeddingBatch:
        calls.append({"texts": texts, **kwargs})
        vector = [0.0] * provider.dimension
        vector[0] = 1.0
        return bridge.EmbeddingBatch(
            vectors=[vector for _ in texts],
            metadata={
                "fingerprint": provider._route.fingerprint,
                "dimension": provider.dimension,
                "selection_reason": "pinned-index",
            },
        )

    monkeypatch.setattr(bridge, "embed_batch", fake_embed_batch)

    assert len(provider.embed(["a", "b"])) == 2
    assert len(provider.embed_query("find stale authorization")) == 1024
    assert calls[0]["required_fingerprint"] == provider._route.fingerprint
    assert calls[0]["purpose"] == "document"
    assert str(calls[1]["texts"][0]).startswith(bridge.QUERY_INSTRUCT)
    assert calls[1]["purpose"] == "query"


def test_trace_contains_no_raw_text(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    path = tmp_path / "trace.json"
    monkeypatch.setenv(bridge.TRACE_PATH_ENV, str(path))
    monkeypatch.setattr(bridge, "ensure_service", lambda: False)
    provider = bridge.WorkspaceEmbeddingProvider()
    # Assembled from fragments so the fake redaction sentinel is not itself a
    # secret-like literal that trips this suite's own generic-api-key scan.
    secret = "".join(["RAW_", "SECRET_", "MUST_NOT_APPEAR"])
    vector = [0.0] * provider.dimension
    vector[0] = 1.0
    monkeypatch.setattr(
        bridge,
        "embed_batch",
        lambda texts, **_kwargs: bridge.EmbeddingBatch(
            vectors=[vector for _ in texts],
            metadata={
                "fingerprint": provider._route.fingerprint,
                "dimension": provider.dimension,
                "selection_reason": "pinned-index",
            },
        ),
    )

    provider.embed_query(secret)

    payload = json.loads(path.read_text(encoding="utf-8"))
    assert payload["purpose"] == "query"
    assert payload["vector_count"] == 1
    assert secret not in path.read_text(encoding="utf-8")
