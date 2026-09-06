from __future__ import annotations

import json
import math
import random
import urllib.error
from pathlib import Path

import pytest

from conductor import kb_retrieve


def _unit(vec: list[float]) -> list[float]:
    return kb_retrieve._l2_normalize(vec)


def _index_payload(
    *, num_gpu: int = 0, fingerprint: str = "sha256:" + "a" * 64
) -> dict[str, object]:
    return {
        "schema_version": kb_retrieve.SCHEMA_VERSION,
        "embedding": {
            "fingerprint": fingerprint,
            "dimension": 1,
            "paid": False,
            "num_gpu": num_gpu,
            "num_ctx": 2048,
        },
        "cards": [{"name": "x", "path": "x", "text": "x", "vector": [1.0]}],
    }


def test_query_ranks_instruct_query_against_document_vectors(tmp_path: Path) -> None:
    calls: list[str] = []

    def fake_embed(text: str, **_: object) -> list[float]:
        calls.append(text)
        if text.startswith("Instruct:"):
            return _unit([1.0, 0.0, 0.0])
        if "EAGER" in text:
            return _unit([0.9, 0.1, 0.0])
        return _unit([0.0, 1.0, 0.0])

    index = {
        "schema_version": 1,
        "model": kb_retrieve.EMBED_MODEL,
        "num_gpu": 0,
        "cards": [
            {
                "name": "kb_hw.md",
                "path": "hw",
                "text": "throughput rules",
                "vector": _unit([0.0, 1.0, 0.0]),
            },
            {
                "name": "kb_comp.md",
                "path": "comp",
                "text": "EAGER_REQUIRED stays",
                "vector": _unit([1.0, 0.0, 0.0]),
            },
        ],
    }
    hits = kb_retrieve.query_index(
        "What is EAGER_REQUIRED?", index, top_k=1, embedder=fake_embed
    )
    assert hits[0].name == "kb_comp.md"
    assert calls and calls[0].startswith(kb_retrieve.QUERY_INSTRUCT)


def test_load_index_rejects_unbounded_gpu(tmp_path: Path) -> None:
    path = tmp_path / "idx.json"
    path.write_text(
        json.dumps(_index_payload(num_gpu=8)),
        encoding="utf-8",
    )
    with pytest.raises(kb_retrieve.RetrieveError, match="num_gpu"):
        kb_retrieve.load_index(path)


def test_load_index_allows_guest_gpu(tmp_path: Path) -> None:
    path = tmp_path / "idx.json"
    path.write_text(
        json.dumps(_index_payload(num_gpu=99)),
        encoding="utf-8",
    )
    payload = kb_retrieve.load_index(path)
    assert payload["embedding"]["num_gpu"] == 99


def test_embed_payload_pins_ctx_and_guest_gpu(monkeypatch: pytest.MonkeyPatch) -> None:
    captured: dict[str, object] = {}

    class _Resp:
        def read(self) -> bytes:
            return json.dumps(
                {
                    "data": [{"index": 0, "embedding": [3.0, 4.0]}],
                    "workspace_embedding": {
                        "fingerprint": "sha256:" + "b" * 64,
                        "dimension": 2,
                        "paid": False,
                        "num_gpu": 99,
                        "num_ctx": 2048,
                    },
                }
            ).encode()

        def __enter__(self) -> _Resp:
            return self

        def __exit__(self, *args: object) -> None:
            return None

    def fake_urlopen(request: object, timeout: float = 0.0) -> _Resp:
        captured["data"] = json.loads(request.data.decode())  # type: ignore[attr-defined]
        captured["timeout"] = timeout
        return _Resp()

    monkeypatch.setattr(kb_retrieve.urllib.request, "urlopen", fake_urlopen)
    vec = kb_retrieve.embed_text("hello")
    assert captured["data"]["input"] == ["hello"]
    assert captured["data"]["workspace_purpose"] == "document"
    assert vec == pytest.approx([0.6, 0.8])


def test_broker_client_auto_starts_once_after_connection_refused(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls = 0

    class _Resp:
        def read(self) -> bytes:
            return json.dumps(
                {
                    "data": [{"index": 0, "embedding": [1.0, 0.0]}],
                    "workspace_embedding": {
                        "fingerprint": "sha256:" + "c" * 64,
                        "dimension": 2,
                        "paid": False,
                        "num_gpu": 0,
                        "num_ctx": 2048,
                    },
                }
            ).encode()

        def __enter__(self) -> _Resp:
            return self

        def __exit__(self, *args: object) -> None:
            return None

    def flaky_urlopen(*_args: object, **_kwargs: object) -> _Resp:
        nonlocal calls
        calls += 1
        if calls == 1:
            raise urllib.error.URLError("connection refused")
        return _Resp()

    from conductor import cpu_embed

    starts: list[bool] = []
    monkeypatch.setattr(kb_retrieve.urllib.request, "urlopen", flaky_urlopen)
    monkeypatch.setattr(
        cpu_embed, "ensure_service", lambda: starts.append(True) or True
    )

    assert kb_retrieve.embed_text("hello") == [1.0, 0.0]
    assert starts == [True]
    assert calls == 2


def test_load_cards_requires_kb_glob(tmp_path: Path) -> None:
    (tmp_path / "readme.md").write_text("nope", encoding="utf-8")
    with pytest.raises(kb_retrieve.RetrieveError, match="no kb_"):
        kb_retrieve.load_cards(tmp_path)


def test_load_index_requires_valid_fingerprint(tmp_path: Path) -> None:
    path = tmp_path / "idx.json"
    path.write_text(json.dumps(_index_payload(fingerprint="wrong")), encoding="utf-8")
    with pytest.raises(kb_retrieve.RetrieveError, match="fingerprint"):
        kb_retrieve.load_index(path)


def test_l2_normalize_rejects_zero_vector() -> None:
    with pytest.raises(kb_retrieve.RetrieveError, match="zero vector"):
        kb_retrieve._l2_normalize([0.0, 0.0, 0.0])


def test_assert_embedding_meta_requires_sha256_fingerprint() -> None:
    with pytest.raises(kb_retrieve.RetrieveError, match="fingerprint is invalid"):
        kb_retrieve.assert_embedding_meta({"embedding": {"fingerprint": "not-sha256"}})


def test_assert_embedding_meta_requires_positive_dimension() -> None:
    payload = {"embedding": {"fingerprint": "sha256:" + "a" * 64, "dimension": 0}}
    with pytest.raises(kb_retrieve.RetrieveError, match="dimension is invalid"):
        kb_retrieve.assert_embedding_meta(payload)


def test_assert_embedding_meta_requires_paid_flag() -> None:
    payload = {
        "embedding": {
            "fingerprint": "sha256:" + "a" * 64,
            "dimension": 1,
        }
    }
    with pytest.raises(kb_retrieve.RetrieveError, match="paid flag is invalid"):
        kb_retrieve.assert_embedding_meta(payload)


def test_assert_embedding_meta_requires_pinned_num_ctx_when_unpaid() -> None:
    payload = {
        "embedding": {
            "fingerprint": "sha256:" + "a" * 64,
            "dimension": 1,
            "paid": False,
            "num_gpu": 0,
            "num_ctx": 4096,
        }
    }
    with pytest.raises(kb_retrieve.RetrieveError, match="num_ctx"):
        kb_retrieve.assert_embedding_meta(payload)


def test_load_cards_reads_all_kb_files_sorted(tmp_path: Path) -> None:
    (tmp_path / "kb_b.md").write_text("second", encoding="utf-8")
    (tmp_path / "kb_a.md").write_text("first", encoding="utf-8")
    (tmp_path / "readme.md").write_text("ignored", encoding="utf-8")
    cards = kb_retrieve.load_cards(tmp_path)
    assert [card["name"] for card in cards] == ["kb_a.md", "kb_b.md"]
    assert cards[0]["text"] == "first"


def test_load_cards_requires_existing_directory(tmp_path: Path) -> None:
    with pytest.raises(kb_retrieve.RetrieveError, match="notes dir missing"):
        kb_retrieve.load_cards(tmp_path / "does-not-exist")


def test_build_index_with_injected_embedder_is_deterministic() -> None:
    cards = [
        {"name": "kb_a.md", "path": "a", "text": "alpha"},
        {"name": "kb_b.md", "path": "b", "text": "beta"},
    ]

    def fake_embed(text: str) -> list[float]:
        return [float(len(text)), 0.0]

    index = kb_retrieve.build_index(cards, embedder=fake_embed)
    assert index["schema_version"] == kb_retrieve.SCHEMA_VERSION
    assert [entry["vector"] for entry in index["cards"]] == [[5.0, 0.0], [4.0, 0.0]]
    assert index["embedding"]["dimension"] == 2
    assert index["embedding"]["paid"] is False


def test_save_index_then_load_index_round_trips(tmp_path: Path) -> None:
    path = tmp_path / "nested" / "idx.json"
    index = _index_payload()
    saved = kb_retrieve.save_index(index, path)
    assert saved == path
    loaded = kb_retrieve.load_index(path)
    assert loaded == index


def test_load_index_rejects_unsupported_schema_version(tmp_path: Path) -> None:
    path = tmp_path / "idx.json"
    payload = _index_payload()
    payload["schema_version"] = 999
    path.write_text(json.dumps(payload), encoding="utf-8")
    with pytest.raises(kb_retrieve.RetrieveError, match="unsupported index schema"):
        kb_retrieve.load_index(path)


def test_load_index_rejects_card_vector_dimension_mismatch(tmp_path: Path) -> None:
    path = tmp_path / "idx.json"
    payload = _index_payload()
    payload["cards"][0]["vector"] = [1.0, 2.0]
    path.write_text(json.dumps(payload), encoding="utf-8")
    with pytest.raises(kb_retrieve.RetrieveError, match="dimension does not match"):
        kb_retrieve.load_index(path)


@pytest.mark.parametrize("top_k", [0, -1])
def test_query_index_rejects_non_positive_top_k(top_k: int) -> None:
    index = {"cards": [], "embedding": {"fingerprint": "sha256:" + "a" * 64}}
    with pytest.raises(kb_retrieve.RetrieveError, match="top_k must be"):
        kb_retrieve.query_index("q", index, top_k=top_k, embedder=lambda text: [1.0])


def test_query_index_with_injected_embedder_ranks_by_dot_product() -> None:
    index = {
        "cards": [
            {"name": "kb_low.md", "path": "low", "text": "low", "vector": [0.0, 1.0]},
            {
                "name": "kb_high.md",
                "path": "high",
                "text": "high",
                "vector": [1.0, 0.0],
            },
        ],
        "embedding": {"fingerprint": "sha256:" + "a" * 64},
    }
    hits = kb_retrieve.query_index(
        "q", index, top_k=1, embedder=lambda text: [1.0, 0.0]
    )
    assert len(hits) == 1
    assert hits[0].name == "kb_high.md"


def test_main_index_builds_saves_and_reports_card_count(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    calls: dict[str, object] = {}
    canned_cards = [{"name": "a"}, {"name": "b"}]
    canned_index = {"cards": canned_cards}
    saved_path = tmp_path / "kb_card_index.json"

    def fake_load_cards() -> list[dict[str, str]]:
        calls["load_cards"] = True
        return canned_cards

    def fake_build_index(cards: list[dict[str, str]]) -> dict[str, object]:
        calls["build_cards"] = cards
        return canned_index

    def fake_save_index(index: dict[str, object]) -> Path:
        calls["saved_index"] = index
        saved_path.write_text(json.dumps(index), encoding="utf-8")
        return saved_path

    monkeypatch.setattr(kb_retrieve, "load_cards", fake_load_cards)
    monkeypatch.setattr(kb_retrieve, "build_index", fake_build_index)
    monkeypatch.setattr(kb_retrieve, "save_index", fake_save_index)

    exit_code = kb_retrieve.main(["index"])

    assert exit_code == 0
    assert calls["load_cards"] is True
    assert calls["build_cards"] == canned_cards
    assert calls["saved_index"] == canned_index
    payload = json.loads(capsys.readouterr().out)
    assert payload == {"index": str(saved_path), "cards": 2}


def test_main_query_prints_ranked_hits_from_the_given_index(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    calls: dict[str, object] = {}
    canned_index = {"cards": []}
    canned_hits = [
        kb_retrieve.ScoredCard(
            name="alpha", path="notes/alpha.md", score=0.98765, text="x"
        ),
        kb_retrieve.ScoredCard(name="beta", path="notes/beta.md", score=0.5, text="y"),
    ]

    def fake_load_index(path: Path) -> dict[str, object]:
        calls["load_path"] = path
        return canned_index

    def fake_query_index(query: str, index: dict[str, object], *, top_k: int) -> list:
        calls["query"] = query
        calls["index"] = index
        calls["top_k"] = top_k
        return canned_hits

    monkeypatch.setattr(kb_retrieve, "load_index", fake_load_index)
    monkeypatch.setattr(kb_retrieve, "query_index", fake_query_index)

    index_path = tmp_path / "custom_kb_index.json"
    exit_code = kb_retrieve.main(
        ["query", "how do claims work", "--top-k", "2", "--index", str(index_path)]
    )

    assert exit_code == 0
    assert calls == {
        "load_path": index_path,
        "query": "how do claims work",
        "index": canned_index,
        "top_k": 2,
    }
    assert json.loads(capsys.readouterr().out) == [
        {"name": "alpha", "score": 0.9877, "path": "notes/alpha.md"},
        {"name": "beta", "score": 0.5, "path": "notes/beta.md"},
    ]


def test_main_reports_retrieve_error_and_exits_2(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    def boom(*_a: object, **_k: object) -> dict[str, object]:
        raise kb_retrieve.RetrieveError("broker unreachable")

    monkeypatch.setattr(kb_retrieve, "load_index", boom)

    exit_code = kb_retrieve.main(["query", "anything"])

    assert exit_code == 2
    assert json.loads(capsys.readouterr().err) == {"error": "broker unreachable"}


def test_save_index_writes_atomically_and_cleans_up_temp_file(tmp_path: Path) -> None:
    path = tmp_path / "kb_card_index.json"
    index = {"cards": [{"name": "a"}]}

    result = kb_retrieve.save_index(index, path)

    assert result == path
    assert json.loads(path.read_text(encoding="utf-8")) == index
    assert list(tmp_path.glob(f".{path.name}.*.tmp")) == []

    unserializable_index = {"cards": {1, 2, 3}}  # a set is not JSON-serializable
    with pytest.raises(TypeError):
        kb_retrieve.save_index(unserializable_index, path)
    assert list(tmp_path.glob(f".{path.name}.*.tmp")) == []


# --- native scoring boundary differential (kb_retrieve.rs) -------------------
# The dot product, sort and normalization run in Rust; these tests drive them
# through the production ``query_index``/``_l2_normalize`` paths and hold them
# to the exact Python arithmetic the port replaced (CPython 3.12 sum() is
# Neumaier-compensated, so bit parity is the contract).


def _reference_dot(left: list[float], right: list[float]) -> float:
    """The expression the native dot replaced: builtin sum() over zip()."""
    return sum(a * b for a, b in zip(left, right, strict=True))


def _score_via_query_index(
    query: list[float],
    cards: list[dict[str, object]],
    top_k: int,
) -> list[kb_retrieve.ScoredCard]:
    """Run the production query_index path with the embedding stubbed out."""

    def embed(text: str) -> list[float]:
        return query

    index: dict[str, object] = {"embedding": {}, "cards": cards}
    return kb_retrieve.query_index("find", index, top_k=top_k, embedder=embed)


def test_native_scoring_matches_builtin_sum_bit_for_bit() -> None:
    # fixed seed: identical across machines, pins parity round-for-round
    rng = random.Random(20260905)
    for _ in range(32):
        dim = rng.choice([8, 64, 1024])
        query = [rng.uniform(-1.0, 1.0) for _ in range(dim)]
        cards = [
            {
                "name": "c0",
                "path": "n/c0.md",
                "text": "t0",
                "vector": [rng.uniform(-1000.0, 1000.0) for _ in range(dim)],
            },
        ]
        hits = _score_via_query_index(query, cards, 1)
        assert hits[0].score == _reference_dot(query, cards[0]["vector"])
        assert (hits[0].name, hits[0].path, hits[0].text) == ("c0", "n/c0.md", "t0")


def test_native_scoring_keeps_index_order_on_ties() -> None:
    cards = [
        {"name": f"c{i}", "path": f"n/c{i}.md", "text": "t", "vector": [1.0, 0.0]}
        for i in range(4)
    ]
    hits = _score_via_query_index([1.0, 0.0], cards, 4)
    assert [hit.name for hit in hits] == ["c0", "c1", "c2", "c3"]


def test_native_scoring_truncates_to_top_k() -> None:
    cards = [
        {"name": f"c{i}", "path": "n", "text": "t", "vector": [float(i + 1)]}
        for i in range(10)
    ]
    hits = _score_via_query_index([1.0], cards, 3)
    assert [hit.name for hit in hits] == ["c9", "c8", "c7"]


def test_native_scoring_dim_mismatch_maps_to_retrieve_error() -> None:
    cards = [{"name": "c3", "path": "p", "text": "t", "vector": [1.0, 2.0]}]
    with pytest.raises(
        kb_retrieve.RetrieveError, match=r"vector dim mismatch for c3: 2 != 3"
    ):
        _score_via_query_index([1.0, 2.0, 3.0], cards, 1)


def test_native_l2_normalize_matches_builtin_sum_reference() -> None:
    rng = random.Random(20260907)
    for _ in range(32):
        vector = [rng.uniform(-1e6, 1e6) for _ in range(rng.choice([4, 64, 512]))]
        norm = math.sqrt(sum(x * x for x in vector))
        expected = [x / norm for x in vector]
        assert kb_retrieve._l2_normalize(vector) == expected
