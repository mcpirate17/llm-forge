from __future__ import annotations

import fcntl
import json
import hashlib
import random
from pathlib import Path

import pytest

from conductor import memory_index
from conductor.memory_index import HEADING_PREFIXES


def _embedding_meta(*, dimension: int = 2, num_gpu: int = 0) -> dict[str, object]:
    return {
        "fingerprint": "sha256:" + "a" * 64,
        "dimension": dimension,
        "paid": False,
        "num_gpu": num_gpu,
        "num_ctx": 2048,
    }


def test_chunk_text_splits_on_headings() -> None:
    text = "# One\n" + ("a" * 500) + "\n## Two\n" + ("b" * 500)
    chunks = memory_index.chunk_text(
        text, source_id="notes", path=Path("x.md"), mode="heading"
    )
    assert len(chunks) >= 2
    assert chunks[0]["source"] == "notes"
    assert "One" in chunks[0]["title"] or "a" in chunks[0]["text"]


def test_iter_source_files_respects_exclude(tmp_path: Path) -> None:
    notes = tmp_path / "notes"
    notes.mkdir()
    (notes / "kb_law.md").write_text("law", encoding="utf-8")
    (notes / "finding.md").write_text("evidence", encoding="utf-8")
    # Point root at tmp notes via absolute_root to avoid repo ROOT.
    entry = {
        "id": "notes",
        "kind": "index",
        "absolute_root": str(notes),
        "glob": "*.md",
        "exclude_globs": ["kb_*.md"],
    }
    names = {p.name for p in memory_index.iter_source_files(entry)}
    assert names == {"finding.md"}


def test_query_index_ranks_with_injected_embedder() -> None:
    def embed(text: str) -> list[float]:
        if text.startswith("Instruct:"):
            return [1.0, 0.0]
        if "eager" in text.lower():
            return [1.0, 0.0]
        return [0.0, 1.0]

    rows = [
        {
            "num_gpu": 0,
            "source": "notes",
            "path": "a.md",
            "title": "other",
            "text": "throughput",
            "vector": [0.0, 1.0],
        },
        {
            "num_gpu": 0,
            "source": "notes",
            "path": "b.md",
            "title": "eager",
            "text": "EAGER_REQUIRED stays",
            "vector": [1.0, 0.0],
        },
    ]
    hits = memory_index.query_index("what is eager", rows, top_k=1, embedder=embed)
    assert hits[0]["path"] == "b.md"


def test_load_index_allows_guest_gpu_rows(tmp_path: Path) -> None:
    path = tmp_path / "memory_index.jsonl"
    path.write_text(
        json.dumps(
            {
                "schema_version": memory_index.SCHEMA_VERSION,
                "embedding": _embedding_meta(num_gpu=99),
                "source_sha256": "a" * 64,
                "source": "notes",
                "path": "a.md",
                "title": "t",
                "text": "x",
                "vector": [1.0, 0.0],
            }
        )
        + "\n",
        encoding="utf-8",
    )
    rows = memory_index.load_index(path)
    assert rows[0]["embedding"]["num_gpu"] == 99


def test_reuse_rows_skips_unchanged_files(tmp_path: Path) -> None:
    src = tmp_path / "note.md"
    src.write_text("hello", encoding="utf-8")
    previous = {
        ("notes", str(src.resolve())): [
            {
                "source": "notes",
                "path": str(src.resolve()),
                "text": "hello",
                "vector": [1.0],
                "source_sha256": "expected",
            }
        ]
    }
    assert memory_index._reuse_rows("notes", src, previous, "expected") is not None
    assert memory_index._reuse_rows("notes", src, previous, "changed") is None


def test_catalog_does_not_index_live_current_work() -> None:
    catalog = memory_index.load_catalog()
    indexed_ids = {
        str(entry["id"]) for entry in catalog["source"] if entry.get("kind") == "index"
    }
    assert "current-work" not in indexed_ids


def test_save_index_replaces_atomically(tmp_path: Path) -> None:
    path = tmp_path / "memory.jsonl"
    path.write_text("old\n", encoding="utf-8")
    rows = [{"schema_version": memory_index.SCHEMA_VERSION, "value": "new"}]
    assert memory_index.save_index(rows, path) == path
    assert json.loads(path.read_text(encoding="utf-8")) == rows[0]
    assert list(tmp_path.glob(".memory.jsonl.*.tmp")) == []


def _index_row(path: Path, source: str, source_sha256: str) -> dict[str, object]:
    return {
        "schema_version": memory_index.SCHEMA_VERSION,
        "embedding": _embedding_meta(),
        "source_sha256": source_sha256,
        "source": source,
        "path": str(path.resolve()),
        "title": path.name,
        "text": path.read_text(encoding="utf-8"),
        "vector": [1.0, 0.0],
    }


def test_partial_refresh_preserves_unselected_sources(tmp_path: Path) -> None:
    notes = tmp_path / "notes"
    cards = tmp_path / "cards"
    notes.mkdir()
    cards.mkdir()
    note = notes / "finding.md"
    card = cards / "kb_rule.md"
    note.write_text("new finding", encoding="utf-8")
    card.write_text("stable rule", encoding="utf-8")
    catalog = tmp_path / "sources.toml"
    catalog.write_text(
        "\n".join(
            [
                "schema_version = 1",
                "[[source]]",
                'id = "notes"',
                'kind = "index"',
                f"absolute_root = {json.dumps(str(notes))}",
                'glob = "*.md"',
                'chunk = "heading"',
                "[[source]]",
                'id = "cards"',
                'kind = "index"',
                f"absolute_root = {json.dumps(str(cards))}",
                'glob = "*.md"',
                'chunk = "whole"',
            ]
        ),
        encoding="utf-8",
    )
    index = tmp_path / "index.jsonl"
    card_hash = hashlib.sha256(card.read_bytes()).hexdigest()
    memory_index.save_index(
        [
            _index_row(note, "notes", "0" * 64),
            _index_row(card, "cards", card_hash),
        ],
        index,
    )

    result = memory_index.build_index_result(
        source_ids={"notes"},
        catalog_path=catalog,
        index_path=index,
        embedder=lambda _text: [0.0, 1.0],
    )

    assert result.changed is True
    assert result.preserved_count == 1
    assert result.embedded_count == 1
    assert {row["source"] for row in result.materialize_rows()} == {"cards", "notes"}
    memory_index.save_index_result(result, index)

    no_op = memory_index.build_index_result(
        source_ids={"notes"},
        catalog_path=catalog,
        index_path=index,
        embedder=lambda _text: pytest.fail("unchanged note should not re-embed"),
    )
    assert no_op.changed is False
    assert no_op.preserved_count == 1
    assert no_op.reused_count == 1


def test_partial_refresh_requires_existing_full_index(tmp_path: Path) -> None:
    notes = tmp_path / "notes"
    cards = tmp_path / "cards"
    notes.mkdir()
    cards.mkdir()
    (notes / "finding.md").write_text("finding", encoding="utf-8")
    (cards / "kb_rule.md").write_text("rule", encoding="utf-8")
    catalog = tmp_path / "sources.toml"
    catalog.write_text(
        "\n".join(
            [
                "schema_version = 1",
                "[[source]]",
                'id = "notes"',
                'kind = "index"',
                f"absolute_root = {json.dumps(str(notes))}",
                "[[source]]",
                'id = "cards"',
                'kind = "index"',
                f"absolute_root = {json.dumps(str(cards))}",
            ]
        ),
        encoding="utf-8",
    )
    with pytest.raises(memory_index.RetrieveError, match="existing full index"):
        memory_index.build_index_result(
            source_ids={"notes"},
            catalog_path=catalog,
            index_path=tmp_path / "missing.jsonl",
            embedder=lambda _text: [1.0],
        )


def _query_row(**overrides: object) -> dict[str, object]:
    row: dict[str, object] = {
        "schema_version": memory_index.SCHEMA_VERSION,
        "embedding": _embedding_meta(),
        "source_sha256": "a" * 64,
        "source": "notes",
        "path": "a.md",
        "title": "t",
        "text": "x",
        "vector": [1.0, 0.0],
    }
    row.update(overrides)
    return row


def test_index_build_result_total_rows_sums_new_and_preserved() -> None:
    result = memory_index.IndexBuildResult(
        rows=[{"source": "notes"}],
        changed=True,
        selected_sources=("notes",),
        reused_count=0,
        embedded_count=1,
        preserved_count=3,
        removed_count=0,
    )
    assert result.total_rows == 4


def test_load_catalog_requires_matching_schema_version(tmp_path: Path) -> None:
    catalog = tmp_path / "sources.toml"
    catalog.write_text('schema_version = 999\n[[source]]\nid = "x"\n', encoding="utf-8")
    with pytest.raises(memory_index.RetrieveError, match="unsupported catalog schema"):
        memory_index.load_catalog(catalog)


def test_load_catalog_requires_at_least_one_source(tmp_path: Path) -> None:
    catalog = tmp_path / "sources.toml"
    catalog.write_text(
        "schema_version = 1\nsource = []\nsources = []\n", encoding="utf-8"
    )
    with pytest.raises(
        memory_index.RetrieveError, match="no \\[\\[source\\]\\] entries"
    ):
        memory_index.load_catalog(catalog)


def test_path_matches_source_honors_exclude_dir_names(tmp_path: Path) -> None:
    excluded = tmp_path / "node_modules"
    excluded.mkdir()
    target = excluded / "a.md"
    target.write_text("x", encoding="utf-8")
    entry = {
        "kind": "index",
        "absolute_root": str(tmp_path),
        "glob": "*.md",
        "exclude_dir_names": ["node_modules"],
    }
    assert memory_index.path_matches_source(entry, target) is False


def test_path_matches_source_honors_exclude_globs(tmp_path: Path) -> None:
    target = tmp_path / "kb_secret.md"
    target.write_text("x", encoding="utf-8")
    entry = {
        "kind": "index",
        "absolute_root": str(tmp_path),
        "glob": "*.md",
        "exclude_globs": ["kb_*.md"],
    }
    assert memory_index.path_matches_source(entry, target) is False


def test_path_matches_source_defaults_glob_to_markdown(tmp_path: Path) -> None:
    target = tmp_path / "a.md"
    target.write_text("x", encoding="utf-8")
    entry = {"kind": "index", "absolute_root": str(tmp_path)}
    assert memory_index.path_matches_source(entry, target) is True


def test_chunk_text_returns_empty_for_blank_input() -> None:
    assert (
        memory_index.chunk_text(
            "   ", source_id="notes", path=Path("x.md"), mode="heading"
        )
        == []
    )


def test_rows_by_path_groups_by_source_and_path() -> None:
    rows = [
        {"source": "notes", "path": "a.md", "n": 1},
        {"source": "notes", "path": "a.md", "n": 2},
        {"source": "notes", "path": "b.md", "n": 3},
    ]
    grouped = memory_index._rows_by_path(rows)
    assert grouped[("notes", "a.md")] == [rows[0], rows[1]]
    assert grouped[("notes", "b.md")] == [rows[2]]


def test_decode_index_row_rejects_wrong_schema_version() -> None:
    row = _query_row(schema_version=999)
    with pytest.raises(
        memory_index.RetrieveError, match="unsupported memory index schema"
    ):
        memory_index._decode_index_row(json.dumps(row))


def test_decode_index_row_requires_dimension_match() -> None:
    row = _query_row(vector=[1.0, 2.0, 3.0])
    with pytest.raises(memory_index.RetrieveError, match="dimension disagrees"):
        memory_index._decode_index_row(json.dumps(row))


def test_load_index_partition_splits_selected_preserved_and_dropped(
    tmp_path: Path,
) -> None:
    path = tmp_path / "memory_index.jsonl"
    rows = [
        _query_row(source="notes", path="a.md"),
        _query_row(source="cards", path="b.md"),
        _query_row(source="stale", path="c.md"),
    ]
    path.write_text("\n".join(json.dumps(row) for row in rows) + "\n", encoding="utf-8")
    selected, preserved_count, dropped_count = memory_index.load_index_partition(
        path, selected_sources={"notes"}, available_sources={"notes", "cards"}
    )
    assert [row["source"] for row in selected] == ["notes"]
    assert preserved_count == 1
    assert dropped_count == 1


@pytest.mark.parametrize("top_k", [0, -1])
def test_query_index_rejects_non_positive_top_k(top_k: int) -> None:
    with pytest.raises(memory_index.RetrieveError, match="top_k must be"):
        memory_index.query_index("q", [], top_k=top_k, embedder=lambda text: [1.0])


def test_query_index_with_injected_embedder_ranks_and_truncates_text() -> None:
    rows = [
        _query_row(source="notes", path="low.md", title="low", vector=[0.0, 1.0]),
        _query_row(source="notes", path="high.md", title="high", vector=[1.0, 0.0]),
    ]
    hits = memory_index.query_index(
        "q", rows, top_k=1, embedder=lambda text: [1.0, 0.0]
    )
    assert len(hits) == 1
    assert hits[0]["path"] == "high.md"


def test_index_write_lock_excludes_concurrent_holder_and_releases_on_exit(
    tmp_path: Path,
) -> None:
    index_path = tmp_path / "cache" / "memory_index.jsonl"
    index_path.parent.mkdir(parents=True)
    lock_path = index_path.parent / f".{index_path.name}.lock"

    holder = lock_path.open("a+", encoding="utf-8")
    fcntl.flock(holder.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    try:
        with pytest.raises(memory_index.RetrieveError, match="timed out"):
            with memory_index.index_write_lock(index_path, timeout=0.2):
                pass
    finally:
        fcntl.flock(holder.fileno(), fcntl.LOCK_UN)
        holder.close()

    entered = False
    with memory_index.index_write_lock(index_path, timeout=1.0):
        entered = True
    assert entered

    with pytest.raises(RuntimeError, match="boom"):
        with memory_index.index_write_lock(index_path, timeout=1.0):
            raise RuntimeError("boom")

    reacquired = False
    with memory_index.index_write_lock(index_path, timeout=1.0):
        reacquired = True
    assert reacquired


def test_main_index_builds_saves_and_prints_when_changed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    fake_index_path = tmp_path / "cache" / "memory_index.jsonl"
    monkeypatch.setattr(memory_index, "INDEX_PATH", fake_index_path)

    calls: dict[str, object] = {}

    from contextlib import contextmanager

    @contextmanager
    def fake_lock(*_a: object, **_k: object):
        calls["locked"] = True
        yield

    monkeypatch.setattr(memory_index, "index_write_lock", fake_lock)

    result = memory_index.IndexBuildResult(
        rows=[{"a": 1}, {"a": 2}],
        changed=True,
        selected_sources=("notes",),
        reused_count=1,
        embedded_count=1,
        preserved_count=0,
        removed_count=0,
    )

    def fake_build(
        *, source_ids: object, incremental: bool
    ) -> memory_index.IndexBuildResult:
        calls["source_ids"] = source_ids
        calls["incremental"] = incremental
        return result

    def fake_save(built_result: memory_index.IndexBuildResult) -> Path:
        calls["saved"] = built_result
        fake_index_path.parent.mkdir(parents=True, exist_ok=True)
        fake_index_path.write_text("saved\n", encoding="utf-8")
        return fake_index_path

    monkeypatch.setattr(memory_index, "build_index_result", fake_build)
    monkeypatch.setattr(memory_index, "save_index_result", fake_save)

    exit_code = memory_index.main(["index", "--sources", "notes, other", "--full"])

    assert exit_code == 0
    assert calls["locked"] is True
    assert calls["source_ids"] == {"notes", "other"}
    assert calls["incremental"] is False
    assert calls["saved"] is result

    payload = json.loads(capsys.readouterr().out)
    assert payload == {
        "index": str(fake_index_path),
        "chunks": 2,
        "sources": ["notes"],
        "updated": True,
        "reused": 1,
        "embedded": 1,
        "preserved": 0,
        "removed": 0,
    }


def test_main_index_skips_save_when_unchanged_and_index_exists(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake_index_path = tmp_path / "cache" / "memory_index.jsonl"
    fake_index_path.parent.mkdir(parents=True)
    fake_index_path.write_text("existing\n", encoding="utf-8")
    monkeypatch.setattr(memory_index, "INDEX_PATH", fake_index_path)

    from contextlib import contextmanager

    @contextmanager
    def fake_lock(*_a: object, **_k: object):
        yield

    monkeypatch.setattr(memory_index, "index_write_lock", fake_lock)

    result = memory_index.IndexBuildResult(
        rows=[],
        changed=False,
        selected_sources=(),
        reused_count=0,
        embedded_count=0,
        preserved_count=0,
        removed_count=0,
    )
    monkeypatch.setattr(memory_index, "build_index_result", lambda **_k: result)

    def fail_save(_result: memory_index.IndexBuildResult) -> Path:
        raise AssertionError("save_index_result must not be called when unchanged")

    monkeypatch.setattr(memory_index, "save_index_result", fail_save)

    assert memory_index.main(["index"]) == 0


def test_main_query_prints_hits_from_the_given_index(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    calls: dict[str, object] = {}
    canned_rows = [{"path": "a.md"}]
    canned_hits = [{"name": "card", "text": "hit"}]

    def fake_load_index(path: Path) -> list[dict[str, object]]:
        calls["load_path"] = path
        return canned_rows

    def fake_query_index(
        query: str, rows: list[dict[str, object]], *, top_k: int
    ) -> list[dict]:
        calls["query"] = query
        calls["rows"] = rows
        calls["top_k"] = top_k
        return canned_hits

    monkeypatch.setattr(memory_index, "load_index", fake_load_index)
    monkeypatch.setattr(memory_index, "query_index", fake_query_index)

    index_path = tmp_path / "custom_index.jsonl"
    exit_code = memory_index.main(
        ["query", "how do I authenticate", "--top-k", "3", "--index", str(index_path)]
    )

    assert exit_code == 0
    assert calls == {
        "load_path": index_path,
        "query": "how do I authenticate",
        "rows": canned_rows,
        "top_k": 3,
    }
    assert json.loads(capsys.readouterr().out) == canned_hits


def test_main_reports_retrieve_error_and_exits_2(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    def boom(*_a: object, **_k: object) -> list[dict[str, object]]:
        raise memory_index.RetrieveError("index produced zero chunks")

    monkeypatch.setattr(memory_index, "load_index", boom)

    exit_code = memory_index.main(["query", "anything"])

    assert exit_code == 2
    err = json.loads(capsys.readouterr().err)
    assert err == {"error": "index produced zero chunks"}


# --- native chunking differential (memory_chunking.rs) -----------------------
# ``chunk_text`` delegates to Rust; the reference below is the retired Python
# chunker kept as the byte-parity contract. Randomized documents are compared
# chunk-for-chunk (source, path, title, text) across both modes.


def _reference_chunk_text(
    text: str, source_id: str, path_name: str, full_path: str, mode: str
) -> list[dict[str, str]]:
    text = text.strip()
    if not text:
        return []
    if mode == "whole":
        return [
            {
                "source": source_id,
                "path": full_path,
                "title": path_name,
                "text": text[:3000],
            }
        ]
    chunks: list[dict[str, str]] = []
    buf: list[str] = []
    title = path_name
    size = 0

    def flush() -> None:
        nonlocal buf, size
        body = "\n".join(buf).strip()
        if body:
            chunks.append(
                {
                    "source": source_id,
                    "path": full_path,
                    "title": title,
                    "text": body[:3000],
                }
            )
        buf, size = [], 0

    for line in text.splitlines():
        if line.startswith(HEADING_PREFIXES) and size >= 400:
            flush()
            title = line.lstrip("#").strip() or path_name
        buf.append(line)
        size += len(line) + 1
        if size >= 1500:
            flush()
    flush()
    return chunks or [
        {
            "source": source_id,
            "path": full_path,
            "title": path_name,
            "text": text[:1500],
        }
    ]


def _reference_chunk(
    text: str, source_id: str, path: Path, mode: str
) -> list[dict[str, str]]:
    return _reference_chunk_text(text, source_id, path.name, str(path), mode)


def test_chunk_text_randomized_matches_python_reference() -> None:
    # fixed seed: identical across machines, pins parity round-for-round
    rng = random.Random(20260906)
    topics = ["alpha", "beta", "gamma", "delta"]
    for round_index in range(60):
        lines: list[str] = []
        for _ in range(rng.choice([1, 4, 40, 120])):
            kind = rng.choice(["text", "heading", "blank", "space"])
            if kind == "text":
                lines.append(rng.choice(topics) * rng.choice([1, 20, 80]))
            elif kind == "heading":
                level = rng.choice(["", "#", "##", "###", "#### ", "##### "])
                lines.append(f"{level} {rng.choice(topics)}")
            elif kind == "blank":
                lines.append("")
            else:
                lines.append("   ")
        document = "\n".join(lines)
        mode = rng.choice(["chunk", "whole"])
        for mode_try in {mode, "chunk"}:
            got = memory_index.chunk_text(
                document, source_id="s", path=Path("docs/n.md"), mode=mode_try
            )
            want = _reference_chunk(document, "s", Path("docs/n.md"), mode_try)
            assert got == want, f"round {round_index} mode {mode_try} diverged"


def test_chunk_text_exotic_boundaries_parity() -> None:
    for sample in ["\x1cA\x1dB\x1eC", "a\r\nb\rc\vd\x0cf", "a b c", " x "]:
        for mode in ("chunk", "whole"):
            got = memory_index.chunk_text(
                sample, source_id="s", path=Path("docs/n.md"), mode=mode
            )
            want = _reference_chunk(sample, "s", Path("docs/n.md"), mode)
            assert got == want


def test_chunk_text_heading_only_falls_back_to_file_name_title() -> None:
    chunks = memory_index.chunk_text(
        "# ###", source_id="s", path=Path("docs/n.md"), mode="chunk"
    )
    assert len(chunks) == 1
    assert chunks[0]["title"] == "n.md"
    assert chunks[0]["path"] == str(Path("docs/n.md"))


def test_chunk_text_whole_mode_caps_at_3000() -> None:
    document = "x" * 9000
    chunks = memory_index.chunk_text(
        document, source_id="s", path=Path("docs/n.md"), mode="whole"
    )
    assert len(chunks) == 1
    assert chunks[0]["text"] == "x" * 3000
    assert chunks[0]["title"] == "n.md"


def test_chunk_text_maps_native_tuple_fields_into_dict_fields() -> None:
    chunks = memory_index.chunk_text(
        "body", source_id="src", path=Path("docs/n.md"), mode="whole"
    )
    assert chunks == [
        {
            "source": "src",
            "path": str(Path("docs/n.md")),
            "title": "n.md",
            "text": "body",
        }
    ]


def test_expand_root_resolves_relative_roots_against_the_workspace(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (tmp_path / "pyproject.toml").write_text(
        '[tool.conductor]\nnotes_root = "cards"\n', encoding="utf-8"
    )
    monkeypatch.chdir(tmp_path)
    # The catalog's "research/notes" spelling means the notes root, wherever
    # this workspace configured it; other relative roots resolve literally.
    assert memory_index._expand_root({"id": "notes", "root": "research/notes"}) == (
        tmp_path / "cards"
    )
    assert memory_index._expand_root({"id": "tasks", "root": "tasks"}) == (
        tmp_path / "tasks"
    ).resolve()


def test_expand_root_keeps_the_monorepo_default_for_a_host_so_configured(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (tmp_path / "pyproject.toml").write_text(
        '[tool.conductor]\nnotes_root = "research/notes"\n', encoding="utf-8"
    )
    monkeypatch.chdir(tmp_path)
    assert memory_index._expand_root({"id": "notes", "root": "research/notes"}) == (
        tmp_path / "research/notes"
    )
