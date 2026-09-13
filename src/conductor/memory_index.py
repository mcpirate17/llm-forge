"""Chunk index over the workspace memory catalog.

Reads ``conductor/memory_sources.toml`` (kind=index), chunks markdown, and
embeds through the canonical provider-neutral broker.  Every row carries the
vector-space fingerprint required for a compatible query.  Corpora,
checkpoints, and worktrees are never indexed.
"""

from __future__ import annotations

import argparse
import fcntl
import fnmatch
import hashlib
import json
import sys
import time
import tomllib
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from dataclasses import dataclass
from itertools import chain
from pathlib import Path, PurePosixPath
from typing import Any, Final

from conductor._native import (
    memory_index_chunk_text_native,
    memory_index_metadata_native,
    memory_index_query_file_native,
    memory_index_score_rows_native,
)
from conductor.atomic_json import write_lines_atomic
from conductor.kb_retrieve import (
    QUERY_INSTRUCT,
    RetrieveError,
    assert_embedding_meta,
    embed_batch,
    embed_text,
)
from conductor.project_paths import DEFAULT_NOTES_ROOT, host_root, notes_root

ROOT: Final[Path] = host_root()
# Ships inside the package next to this module -- not host data, unlike INDEX_PATH.
SOURCES_PATH: Final[Path] = Path(__file__).resolve().parent / "memory_sources.toml"
INDEX_PATH: Final[Path] = ROOT / "research" / "cache" / "memory_index.jsonl"
CATALOG_SCHEMA_VERSION: Final[int] = 1
SCHEMA_VERSION: Final[int] = 3
MAX_CHUNK_CHARS: Final[int] = 1500
HEADING_PREFIXES: Final[tuple[str, ...]] = ("# ", "## ", "### ", "#### ")


@dataclass(frozen=True)
class IndexBuildResult:
    """Rows and change accounting for one index build."""

    rows: list[dict[str, Any]]
    changed: bool
    selected_sources: tuple[str, ...]
    reused_count: int
    embedded_count: int
    preserved_count: int
    removed_count: int
    preserve_from: Path | None = None
    preserved_sources: tuple[str, ...] = ()

    @property
    def total_rows(self) -> int:
        return self.preserved_count + len(self.rows)

    def materialize_rows(self) -> list[dict[str, Any]]:
        """Materialize preserved rows for compatibility-only callers."""

        preserved: list[dict[str, Any]] = []
        if self.preserve_from is not None:
            for row in load_index(self.preserve_from):
                if str(row["source"]) in self.preserved_sources:
                    preserved.append(row)
        return [*preserved, *self.rows]


def load_catalog(path: Path = SOURCES_PATH) -> dict[str, Any]:
    payload = tomllib.loads(path.read_text(encoding="utf-8"))
    if payload.get("schema_version") != CATALOG_SCHEMA_VERSION:
        raise RetrieveError(
            f"unsupported catalog schema {payload.get('schema_version')!r}"
        )
    sources = payload.get("source") or payload.get("sources")
    if not isinstance(sources, list) or not sources:
        raise RetrieveError("catalog has no [[source]] entries")
    return payload


def _expand_relative(text: str) -> Path:
    """Resolve a catalog's repo-relative root against the workspace.

    Calls ``host_root()`` directly rather than reusing the module-level ``ROOT``
    so this keeps working if ``ROOT`` is ever narrowed to package-only data. The
    notes spelling is the DEFAULT_NOTES_ROOT literal: a host that repointed
    ``notes_root`` means that tree, so it resolves through the configured path
    instead of the default location.
    """
    workspace = host_root()
    if PurePosixPath(text) == DEFAULT_NOTES_ROOT:
        return notes_root(workspace)
    return (workspace / text).resolve()


def _expand_root(entry: dict[str, Any]) -> Path:
    if "absolute_root" in entry:
        return Path(entry["absolute_root"]).expanduser()
    if "root" in entry:
        return _expand_relative(str(entry["root"]))
    raise RetrieveError(f"source {entry.get('id')!r} missing root")


def _iter_roots(entry: dict[str, Any]) -> list[Path]:
    if "absolute_root" in entry or "root" in entry:
        roots = [_expand_root(entry)]
    else:
        roots = [_expand_relative(str(r)) for r in entry.get("roots", [])]
        if "include_dirs" in entry:
            base = (
                Path(entry["absolute_root"]).expanduser()
                if "absolute_root" in entry
                else host_root()
            )
            roots = [base / d for d in entry["include_dirs"]]
    return [p for p in roots if p.exists()]


def _source_roots(entry: dict[str, Any]) -> list[Path]:
    include_dirs = entry.get("include_dirs")
    if include_dirs and "absolute_root" in entry:
        base = Path(entry["absolute_root"]).expanduser()
        return [base / str(directory) for directory in include_dirs]
    return _iter_roots(entry)


def path_matches_source(entry: dict[str, Any], path: Path) -> bool:
    """Return whether an existing file belongs to one indexed catalog source."""

    if entry.get("kind") != "index" or not path.is_file():
        return False
    resolved = path.resolve()
    relative: Path | None = None
    for root in _source_roots(entry):
        try:
            relative = resolved.relative_to(root.resolve())
            break
        except ValueError:
            continue
    if relative is None:
        return False
    if any(part in set(entry.get("exclude_dir_names", [])) for part in relative.parts):
        return False
    globs = [str(entry["glob"])] if "glob" in entry else []
    globs.extend(str(pattern) for pattern in entry.get("globs", []))
    if not globs:
        globs = ["*.md"]
    if not any(fnmatch.fnmatch(path.name, pattern) for pattern in globs):
        return False
    return not any(
        fnmatch.fnmatch(path.name, str(pattern))
        for pattern in entry.get("exclude_globs", [])
    )


def iter_source_files(entry: dict[str, Any]) -> Iterator[Path]:
    if entry.get("kind") != "index":
        return
    globs = []
    if "glob" in entry:
        globs.append(str(entry["glob"]))
    globs.extend(str(g) for g in entry.get("globs", []))
    if not globs:
        globs = ["*.md"]
    roots = _source_roots(entry)
    seen: set[Path] = set()
    for root in roots:
        if not root.exists():
            continue
        for pattern in globs:
            walker = root.rglob(pattern) if root.is_dir() else iter([root])
            for path in walker:
                if not path_matches_source(entry, path):
                    continue
                resolved = path.resolve()
                if resolved in seen:
                    continue
                seen.add(resolved)
                yield resolved


def chunk_text(
    text: str, *, source_id: str, path: Path, mode: str
) -> list[dict[str, str]]:
    """Split indexed text into bounded chunks (native: see ``memory_chunking.rs``)."""

    return [
        {"source": source, "path": chunk_path, "title": title, "text": body}
        for source, chunk_path, title, body in memory_index_chunk_text_native(
            text, source_id, str(path), path.name, mode
        )
    ]


def _rows_by_path(
    rows: list[dict[str, Any]],
) -> dict[tuple[str, str], list[dict[str, Any]]]:
    grouped: dict[tuple[str, str], list[dict[str, Any]]] = {}
    for row in rows:
        key = (str(row["source"]), str(row["path"]))
        grouped.setdefault(key, []).append(row)
    return grouped


def _reuse_rows(
    source_id: str,
    path: Path,
    previous: dict[tuple[str, str], list[dict[str, Any]]],
    source_sha256: str,
) -> list[dict[str, Any]] | None:
    key = (source_id, str(path.resolve()))
    if key not in previous:
        return None
    rows = previous[key]
    if not rows or any(row.get("source_sha256") != source_sha256 for row in rows):
        return None
    return rows


def _load_previous_rows(
    index_path: Path,
    *,
    incremental: bool,
    partial: bool,
    selected_ids: set[str],
    available_ids: set[str],
) -> tuple[bool, list[dict[str, Any]], int, int]:
    """Load reusable rows while preserving the partial-index fail-closed rule."""
    index_exists = index_path.is_file()
    previous_rows: list[dict[str, Any]] = []
    preserved_count = 0
    dropped_catalog_rows = 0
    if index_exists and (incremental or partial):
        try:
            if partial:
                previous_rows, preserved_count, dropped_catalog_rows = (
                    load_index_partition(
                        index_path,
                        selected_sources=selected_ids,
                        available_sources=available_ids,
                    )
                )
            else:
                previous_rows = load_index(index_path)
        except (RetrieveError, OSError, json.JSONDecodeError) as exc:
            if partial:
                raise RetrieveError(
                    "partial indexing requires a valid existing full index"
                ) from exc
    elif partial:
        raise RetrieveError("partial indexing requires an existing full index")
    return index_exists, previous_rows, preserved_count, dropped_catalog_rows


def _collect_index_chunks(
    entries: list[dict[str, Any]],
    selected_ids: set[str],
    previous: dict[tuple[str, str], list[dict[str, Any]]],
) -> tuple[list[dict[str, str]], list[dict[str, Any]], set[tuple[str, str]]]:
    """Collect changed chunks and exact reusable rows from selected sources."""
    fresh: list[dict[str, str]] = []
    reused: list[dict[str, Any]] = []
    emitted_paths: set[tuple[str, str]] = set()
    for entry in entries:
        sid = str(entry["id"])
        if sid not in selected_ids:
            continue
        mode = str(entry.get("chunk", "heading"))
        for path in iter_source_files(entry):
            try:
                raw = path.read_bytes()
            except OSError:
                continue
            source_sha256 = hashlib.sha256(raw).hexdigest()
            kept = _reuse_rows(sid, path, previous, source_sha256)
            if kept is not None:
                reused.extend(kept)
                emitted_paths.add((sid, str(path.resolve())))
                continue
            chunks = chunk_text(
                raw.decode("utf-8", errors="replace"),
                source_id=sid,
                path=path,
                mode=mode,
            )
            for chunk in chunks:
                chunk["source_sha256"] = source_sha256
            fresh.extend(chunks)
            if chunks:
                emitted_paths.add((sid, str(path.resolve())))
    return fresh, reused, emitted_paths


def _embed_fresh_chunks(
    fresh: list[dict[str, str]],
    embedder: Callable[[str], list[float]],
) -> tuple[list[list[float]], dict[str, Any]]:
    """Embed fresh chunks while pinning one broker route for the whole build."""
    texts = [row["text"] for row in fresh]
    vectors: list[list[float]] = []
    embedding_meta: dict[str, Any] | None = None
    if embedder is embed_text:
        batch_size = 128
        for start in range(0, len(texts), batch_size):
            result = embed_batch(texts[start : start + batch_size], purpose="document")
            if embedding_meta is None:
                embedding_meta = result.metadata
            elif result.metadata.get("fingerprint") != embedding_meta.get(
                "fingerprint"
            ):
                raise RetrieveError("embedding route changed during memory indexing")
            vectors.extend(result.vectors)
            print(
                f"[memory_index] embedded {min(start + batch_size, len(texts))}/{len(texts)}",
                file=sys.stderr,
            )
    else:
        vectors = [embedder(text) for text in texts]
        embedding_meta = {
            "fingerprint": "sha256:" + "a" * 64,
            "dimension": len(vectors[0]) if vectors else 0,
            "paid": False,
            "num_gpu": 0,
            "num_ctx": 2048,
        }
    if embedding_meta is None:
        raise RetrieveError("embedding broker returned no route metadata")
    return vectors, embedding_meta


def build_index_result(
    *,
    source_ids: set[str] | None = None,
    catalog_path: Path = SOURCES_PATH,
    embedder: Callable[[str], list[float]] = embed_text,
    incremental: bool = True,
    index_path: Path = INDEX_PATH,
) -> IndexBuildResult:
    """Build selected sources while preserving valid, unselected cache rows."""

    catalog = load_catalog(catalog_path)
    entries = [entry for entry in catalog["source"] if entry.get("kind") == "index"]
    available_ids = {str(entry["id"]) for entry in entries}
    selected_ids = available_ids if source_ids is None else set(source_ids)
    if not selected_ids:
        raise RetrieveError("at least one index source must be selected")
    unknown_ids = selected_ids - available_ids
    if unknown_ids:
        raise RetrieveError(f"unknown index sources: {sorted(unknown_ids)}")

    partial = selected_ids != available_ids
    index_exists, previous_rows, preserved_count, dropped_catalog_rows = (
        _load_previous_rows(
            index_path,
            incremental=incremental,
            partial=partial,
            selected_ids=selected_ids,
            available_ids=available_ids,
        )
    )
    previous = _rows_by_path(previous_rows) if incremental else {}
    fresh, reused, emitted_paths = _collect_index_chunks(
        entries, selected_ids, previous
    )
    previous_selected_paths = {
        (str(row["source"]), str(row["path"]))
        for row in previous_rows
        if str(row["source"]) in selected_ids
    }
    removed_count = len(previous_selected_paths - emitted_paths) + dropped_catalog_rows
    if not fresh and not reused and preserved_count == 0:
        raise RetrieveError("index produced zero chunks")
    print(
        f"[memory_index] preserve={preserved_count} reuse={len(reused)} "
        f"reembed={len(fresh)} removed={removed_count}",
        file=sys.stderr,
    )
    changed = not index_exists or not incremental or bool(fresh) or removed_count > 0
    if not fresh:
        return IndexBuildResult(
            rows=list(reused),
            changed=changed,
            selected_sources=tuple(sorted(selected_ids)),
            reused_count=len(reused),
            embedded_count=0,
            preserved_count=preserved_count,
            removed_count=removed_count,
            preserve_from=index_path if partial else None,
            preserved_sources=tuple(sorted(available_ids - selected_ids)),
        )
    vectors, embedding_meta = _embed_fresh_chunks(fresh, embedder)
    existing_fingerprints = {
        str(row.get("embedding", {}).get("fingerprint"))
        for row in reused
        if isinstance(row.get("embedding"), dict)
    }
    if existing_fingerprints and existing_fingerprints != {
        embedding_meta["fingerprint"]
    }:
        raise RetrieveError("embedding route changed; full memory reindex required")
    out: list[dict[str, Any]] = list(reused)
    for chunk, vector in zip(fresh, vectors, strict=True):
        out.append(
            {
                "schema_version": SCHEMA_VERSION,
                "embedding": embedding_meta,
                "source_sha256": chunk["source_sha256"],
                "source": chunk["source"],
                "path": chunk["path"],
                "title": chunk["title"],
                "text": chunk["text"],
                "vector": vector,
            }
        )
    return IndexBuildResult(
        rows=out,
        changed=changed,
        selected_sources=tuple(sorted(selected_ids)),
        reused_count=len(reused),
        embedded_count=len(fresh),
        preserved_count=preserved_count,
        removed_count=removed_count,
        preserve_from=index_path if partial else None,
        preserved_sources=tuple(sorted(available_ids - selected_ids)),
    )


def build_index(**kwargs: Any) -> list[dict[str, Any]]:
    """Compatibility wrapper returning only the built rows.

    Takes the same keyword arguments as :func:`build_index_result`, spelled
    out there once.
    """

    return build_index_result(**kwargs).materialize_rows()


def _save_index_lines(lines: Iterator[str], path: Path) -> Path:
    # The atomic-write scaffold is atomic_json's, not a local copy: one
    # implementation of "no partial file behind", every writer sharing it.
    return write_lines_atomic(path, lines)


def save_index(rows: list[dict[str, Any]], path: Path = INDEX_PATH) -> Path:
    lines = (json.dumps(row, ensure_ascii=False) for row in rows)
    return _save_index_lines(lines, path)


def save_index_result(result: IndexBuildResult, path: Path = INDEX_PATH) -> Path:
    """Atomically save selected rows plus streamed, unselected prior rows."""

    def preserved_lines() -> Iterator[str]:
        if result.preserve_from is None:
            return
        with result.preserve_from.open(encoding="utf-8") as handle:
            for line in handle:
                if not line.strip():
                    continue
                row = _decode_index_row(line)
                if str(row["source"]) in result.preserved_sources:
                    yield line

    new_lines = (json.dumps(row, ensure_ascii=False) for row in result.rows)
    return _save_index_lines(chain(preserved_lines(), new_lines), path)


def _decode_index_row(line: str) -> dict[str, Any]:
    row = json.loads(line)
    if not isinstance(row, dict):
        raise RetrieveError("memory index row must be a JSON object")
    if row.get("schema_version") != SCHEMA_VERSION:
        raise RetrieveError(
            f"unsupported memory index schema {row.get('schema_version')!r}; reindex required"
        )
    metadata = assert_embedding_meta(row)
    source_sha256 = row.get("source_sha256")
    if not isinstance(source_sha256, str) or len(source_sha256) != 64:
        raise RetrieveError(f"memory index row {row.get('path')!r} lacks source_sha256")
    if not isinstance(row.get("vector"), list) or not row["vector"]:
        raise RetrieveError(f"memory index row {row.get('path')!r} has no vector")
    if len(row["vector"]) != metadata["dimension"]:
        raise RetrieveError(
            f"memory index row {row.get('path')!r} dimension disagrees with metadata"
        )
    return row


def _iter_index_rows(path: Path) -> Iterator[dict[str, Any]]:
    if not path.is_file():
        raise RetrieveError(f"memory index missing: {path}")
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            if line.strip():
                yield _decode_index_row(line)


def load_index(path: Path = INDEX_PATH) -> list[dict[str, Any]]:
    rows = list(_iter_index_rows(path))
    if not rows:
        raise RetrieveError("memory index is empty")
    return rows


def load_index_partition(
    path: Path,
    *,
    selected_sources: set[str],
    available_sources: set[str],
) -> tuple[list[dict[str, Any]], int, int]:
    """Load selected rows only while validating and counting the full index."""

    selected: list[dict[str, Any]] = []
    preserved_count = 0
    dropped_count = 0
    total = 0
    for row in _iter_index_rows(path):
        total += 1
        source = str(row["source"])
        if source in selected_sources:
            selected.append(row)
        elif source in available_sources:
            preserved_count += 1
        else:
            dropped_count += 1
    if total == 0:
        raise RetrieveError("memory index is empty")
    return selected, preserved_count, dropped_count


def query_index(
    query: str,
    rows: list[dict[str, Any]],
    *,
    top_k: int = 8,
    embedder: Callable[[str], list[float]] = embed_text,
) -> list[dict[str, Any]]:
    if not query.strip():
        raise RetrieveError("query is empty")
    if top_k < 1:
        raise RetrieveError("top_k must be >= 1")
    if embedder is embed_text:
        fingerprints = {
            str(row.get("embedding", {}).get("fingerprint"))
            for row in rows
            if isinstance(row.get("embedding"), dict)
        }
        if len(fingerprints) != 1 or "" in fingerprints:
            raise RetrieveError("memory index contains mixed embedding fingerprints")
        qvec = embed_text(
            QUERY_INSTRUCT + query.strip(),
            purpose="query",
            required_fingerprint=next(iter(fingerprints)),
        )
    else:
        qvec = embedder(QUERY_INSTRUCT + query.strip())
    try:
        native_hits = memory_index_score_rows_native(qvec, rows, top_k)
    except ValueError as exc:
        raise RetrieveError(str(exc)) from exc
    return _native_hits(native_hits)


def _native_hits(
    rows: list[tuple[float, str, str, str, str]],
) -> list[dict[str, Any]]:
    return [
        {
            "score": score,
            "source": source,
            "path": path,
            "title": title,
            "text": text,
        }
        for score, source, path, title, text in rows
    ]


def query_index_file(
    query: str,
    path: Path = INDEX_PATH,
    *,
    top_k: int = 8,
    embedder: Callable[[str], list[float]] = embed_text,
) -> list[dict[str, Any]]:
    """Query a validated JSONL index without materializing vectors in Python."""

    if not query.strip():
        raise RetrieveError("query is empty")
    if top_k < 1:
        raise RetrieveError("top_k must be >= 1")
    try:
        fingerprint, dimension, _count = memory_index_metadata_native(str(path))
    except ValueError as exc:
        raise RetrieveError(str(exc)) from exc
    if embedder is embed_text:
        qvec = embed_text(
            QUERY_INSTRUCT + query.strip(),
            purpose="query",
            required_fingerprint=fingerprint,
        )
    else:
        qvec = embedder(QUERY_INSTRUCT + query.strip())
    if len(qvec) != dimension:
        raise RetrieveError(
            f"query dimension {len(qvec)} != index dimension {dimension}"
        )
    try:
        native_hits = memory_index_query_file_native(
            str(path), qvec, top_k, fingerprint
        )
    except ValueError as exc:
        raise RetrieveError(str(exc)) from exc
    return _native_hits(native_hits)


@contextmanager
def index_write_lock(
    index_path: Path = INDEX_PATH,
    *,
    timeout: float = 300.0,
) -> Iterator[None]:
    """Serialize index builders without blocking concurrent atomic readers."""

    index_path.parent.mkdir(parents=True, exist_ok=True)
    lock_path = index_path.parent / f".{index_path.name}.lock"
    deadline = time.monotonic() + timeout
    with lock_path.open("a+", encoding="utf-8") as lock:
        while True:
            try:
                fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError as exc:
                if time.monotonic() >= deadline:
                    raise RetrieveError(
                        f"timed out waiting for memory index lock: {lock_path}"
                    ) from exc
                time.sleep(0.05)
        try:
            yield
        finally:
            fcntl.flock(lock.fileno(), fcntl.LOCK_UN)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="CPU workspace memory index")
    sub = parser.add_subparsers(dest="command", required=True)
    idx = sub.add_parser("index")
    idx.add_argument(
        "--sources",
        type=str,
        default=None,
        help="Comma-separated sources to refresh; untouched sources are preserved",
    )
    idx.add_argument(
        "--full",
        action="store_true",
        help="Re-embed selected chunks (default is content-hash incremental)",
    )
    qparser = sub.add_parser("query")
    qparser.add_argument("query")
    qparser.add_argument("--top-k", type=int, default=8)
    qparser.add_argument("--index", type=Path, default=INDEX_PATH)
    args = parser.parse_args(argv)
    try:
        if args.command == "index":
            wanted = (
                None
                if args.sources is None
                else {part.strip() for part in args.sources.split(",") if part.strip()}
            )
            with index_write_lock():
                result = build_index_result(
                    source_ids=wanted,
                    incremental=not args.full,
                )
                path = INDEX_PATH
                if result.changed or not path.is_file():
                    path = save_index_result(result)
            print(
                json.dumps(
                    {
                        "index": str(path),
                        "chunks": result.total_rows,
                        "sources": list(result.selected_sources),
                        "updated": result.changed,
                        "reused": result.reused_count,
                        "embedded": result.embedded_count,
                        "preserved": result.preserved_count,
                        "removed": result.removed_count,
                    }
                )
            )
            return 0
        if args.index.is_file():
            hits = query_index_file(args.query, args.index, top_k=args.top_k)
        else:
            # Preserve the public load/query seam for callers that provide a
            # virtual path; real files use the bounded-memory native path.
            hits = query_index(args.query, load_index(args.index), top_k=args.top_k)
        print(json.dumps(hits, indent=2, ensure_ascii=False))
        return 0
    except RetrieveError as exc:
        print(json.dumps({"error": str(exc)}), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
