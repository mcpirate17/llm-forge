"""Bounded, on-demand AST and code-graph context for A2A message batches.

The durable A2A journal remains lossless.  This module scans only bounded message
prefixes, accepts only concrete Python files inside the selected repository, and
emits a deterministic context envelope with a hard serialized-character budget.
It never calls a model and never mutates the graph or message store.
"""

from __future__ import annotations

import json
import re
import sqlite3
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Final

from conductor.graph_context import GraphContextError, get_file_context

AUTHORITY: Final[str] = "bounded-a2a-code-context"
DEFAULT_CODE_CONTEXT_CHARS: Final[int] = 600
DEFAULT_CONTEXT_SCAN_CHARS: Final[int] = 4_096
DEFAULT_MAX_REFS: Final[int] = 2
MAX_CODE_CONTEXT_CHARS: Final[int] = 2_000
MAX_CONTEXT_SCAN_CHARS: Final[int] = 16_384
MAX_REFS: Final[int] = 4
MAX_SOURCE_BYTES: Final[int] = 1 << 18
MAX_MESSAGE_IDS: Final[int] = 8
MAX_RELATIONSHIPS: Final[int] = 3
MIN_AST_CHARS: Final[int] = 48
DEFAULT_AST_CHARS: Final[int] = 320

_PYTHON_REF_RE: Final[re.Pattern[str]] = re.compile(
    r"(?<![A-Za-z0-9_.-])"
    r"(?P<path>/?(?:[A-Za-z0-9_.-]+/)+[A-Za-z0-9_.-]+\.py)"
    r"(?:::(?P<symbol>[A-Za-z_][A-Za-z0-9_]*))?"
)


class A2aGraphContextError(ValueError):
    """A graph-context request violated a hard operational bound."""


@dataclass(frozen=True, slots=True)
class ContextRef:
    """One repository-contained Python file and optional symbol reference."""

    path: str
    symbol: str | None
    message_ids: tuple[str, ...]


def _compact_json(value: Any) -> str:
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    )


def _fit_text(value: str, max_chars: int) -> str:
    normalized = " ".join(value.split())
    if len(normalized) <= max_chars:
        return normalized
    if max_chars < 2:
        return "…"[:max_chars]
    return normalized[: max_chars - 1].rstrip() + "…"


def _safe_ref(
    repo: Path, raw_path: str, symbol: str | None
) -> tuple[str, str | None] | None:
    repo_root = repo.resolve()
    candidate = Path(raw_path)
    if not candidate.is_absolute():
        candidate = repo_root / candidate
    try:
        resolved = candidate.resolve(strict=True)
        relative = resolved.relative_to(repo_root)
    except (OSError, ValueError):
        return None
    if (
        resolved.suffix != ".py"
        or not resolved.is_file()
        or any(part.startswith(".") for part in relative.parts)
    ):
        return None
    try:
        if resolved.stat().st_size > MAX_SOURCE_BYTES:
            return None
    except OSError:
        return None
    return relative.as_posix(), symbol


def extract_context_refs(
    repo: Path,
    fragments: Iterable[Mapping[str, Any]],
    *,
    max_refs: int = DEFAULT_MAX_REFS,
) -> list[ContextRef]:
    """Extract deduplicated, repository-contained references from bounded fragments."""

    if not 1 <= max_refs <= MAX_REFS:
        raise A2aGraphContextError(f"max_refs must be between 1 and {MAX_REFS}")
    refs: dict[tuple[str, str | None], list[str]] = {}
    for fragment in fragments:
        message_id = fragment.get("message_id")
        if not isinstance(message_id, str) or not message_id:
            continue
        text = "\n".join(
            value
            for key in ("body", "data_json")
            if isinstance((value := fragment.get(key)), str)
        )
        for match in _PYTHON_REF_RE.finditer(text):
            safe = _safe_ref(repo, match.group("path"), match.group("symbol"))
            if safe is None:
                continue
            if safe not in refs:
                if len(refs) >= max_refs:
                    continue
                refs[safe] = []
            if message_id not in refs[safe]:
                refs[safe].append(message_id)
    return [
        ContextRef(path=path, symbol=symbol, message_ids=tuple(message_ids))
        for (path, symbol), message_ids in refs.items()
    ]


def read_context_fragments(
    store_path: Path,
    message_ids: Sequence[str],
    *,
    scan_chars: int = DEFAULT_CONTEXT_SCAN_CHARS,
) -> list[dict[str, str]]:
    """Project bounded inbound body/data prefixes from an existing A2A store."""

    unique_ids = list(dict.fromkeys(message_ids))
    if len(unique_ids) > MAX_MESSAGE_IDS:
        raise A2aGraphContextError(
            f"context discovery accepts at most {MAX_MESSAGE_IDS} messages"
        )
    if not 256 <= scan_chars <= MAX_CONTEXT_SCAN_CHARS:
        raise A2aGraphContextError(
            f"scan_chars must be between 256 and {MAX_CONTEXT_SCAN_CHARS}"
        )
    if not unique_ids:
        return []
    placeholders = ",".join("?" for _ in unique_ids)
    query = f"""
        SELECT message_id,
               substr(body, 1, ?) AS body,
               substr(COALESCE(data_json, ''), 1, ?) AS data_json
        FROM messages
        WHERE direction='inbound' AND message_id IN ({placeholders})
    """
    database_uri = f"{store_path.resolve().as_uri()}?mode=ro"
    with sqlite3.connect(database_uri, uri=True, timeout=5.0) as connection:
        connection.row_factory = sqlite3.Row
        connection.execute("PRAGMA query_only=ON")
        rows = connection.execute(
            query, (scan_chars, scan_chars, *unique_ids)
        ).fetchall()
    by_id = {
        str(row["message_id"]): {
            "message_id": str(row["message_id"]),
            "body": str(row["body"]),
            "data_json": str(row["data_json"]),
        }
        for row in rows
    }
    return [by_id[message_id] for message_id in unique_ids if message_id in by_id]


def _relationship_names(rows: Iterable[Any]) -> list[str]:
    names: list[str] = []
    for row in rows:
        name = getattr(row, "qualified_name", None)
        if isinstance(name, str) and name and name not in names:
            names.append(_fit_text(name, 120))
        if len(names) >= MAX_RELATIONSHIPS:
            break
    return names


def _record(repo: Path, ref: ContextRef) -> dict[str, Any]:
    summary = get_file_context(
        repo,
        ref.path,
        target_symbol=ref.symbol,
        with_graph=True,
    )
    return {
        "messages": list(ref.message_ids),
        "path": ref.path,
        "symbol": ref.symbol,
        "ast": _fit_text(summary.skeleton, DEFAULT_AST_CHARS),
        "callers": _relationship_names(summary.callers),
        "callees": _relationship_names(summary.callees),
        "graph_status": _fit_text(summary.graph_status, 80),
    }


def _fit_record(
    envelope: dict[str, Any], record: dict[str, Any], max_chars: int
) -> dict[str, Any] | None:
    fitted = dict(record)
    while True:
        candidate = {**envelope, "contexts": [*envelope["contexts"], fitted]}
        if len(_compact_json(candidate)) <= max_chars:
            return fitted
        ast_text = str(fitted["ast"])
        if len(ast_text) > MIN_AST_CHARS:
            fitted["ast"] = _fit_text(ast_text, max(MIN_AST_CHARS, len(ast_text) // 2))
            continue
        if fitted["callees"]:
            fitted["callees"] = fitted["callees"][:-1]
            continue
        if fitted["callers"]:
            fitted["callers"] = fitted["callers"][:-1]
            continue
        return None


def build_bounded_code_context(
    repo: Path,
    fragments: Iterable[Mapping[str, Any]],
    *,
    max_chars: int = DEFAULT_CODE_CONTEXT_CHARS,
    max_refs: int = DEFAULT_MAX_REFS,
) -> dict[str, Any]:
    """Return a deterministic AST/graph envelope within ``max_chars`` characters."""

    if not 256 <= max_chars <= MAX_CODE_CONTEXT_CHARS:
        raise A2aGraphContextError(
            f"max_chars must be between 256 and {MAX_CODE_CONTEXT_CHARS}"
        )
    refs = extract_context_refs(repo, fragments, max_refs=max_refs)
    envelope: dict[str, Any] = {
        "schema_version": 1,
        "authority": AUTHORITY,
        "contexts": [],
        "omitted_refs": 0,
    }
    for ref in refs:
        try:
            record = _record(repo, ref)
        except (GraphContextError, OSError, UnicodeError):
            envelope["omitted_refs"] += 1
            continue
        fitted = _fit_record(envelope, record, max_chars)
        if fitted is None:
            envelope["omitted_refs"] += 1
            continue
        envelope["contexts"].append(fitted)
    if len(_compact_json(envelope)) > max_chars:
        raise A2aGraphContextError("internal code-context budget invariant failed")
    return envelope
