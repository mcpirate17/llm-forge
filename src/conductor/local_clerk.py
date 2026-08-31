#!/usr/bin/env python3
"""Bounded local clerical drafts for explicit workspace notes and task files.

The local model is a drafting aid only.  This command never edits or deletes
source files, never emits a gate verdict, and never treats model output as an
authorization.  Exact duplicate groups and source hashes are deterministic;
the model may only supply a bounded, reviewable draft.
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import os
import re
import sys
import tempfile
import urllib.parse
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Final

from conductor.context_envelope import fit_text
from conductor.local_ai_policy import CLERK_SYSTEM_PROMPT, require_clerical_task

ROOT: Final[Path] = Path(__file__).resolve().parents[1]
MAX_SOURCE_BYTES: Final[int] = 64_000
MAX_SOURCE_TEXT_CHARS: Final[int] = 2_400
MAX_SOURCE_COUNT: Final[int] = 3
MIN_PROMPT_SOURCE_CHARS: Final[int] = 128
MAX_PROMPT_CHARS: Final[int] = 4_000
MAX_RESPONSE_BYTES: Final[int] = 64_000
MAX_LIST_ITEMS: Final[int] = 3
MAX_ITEM_CHARS: Final[int] = 160
DEFAULT_MODEL: Final[str] = "qwen3.5:9b"
DEFAULT_OLLAMA_HOST: Final[str] = "http://127.0.0.1:11434"
ALLOWED_MODEL: Final[str] = "qwen3.5:9b"
ALLOWED_OLLAMA_HOSTS: Final[frozenset[str]] = frozenset(
    {"127.0.0.1", "localhost", "::1"}
)
OUTPUT_ROOT: Final[Path] = ROOT / "research" / "tmp" / "local_clerk"
APPLICATION_MARKER: Final[str] = "human review required; originals are preserved"
TITLE_RE: Final[re.Pattern[str]] = re.compile(r"^#{1,6}\s+(.+?)\s*#*\s*$")
FORBIDDEN_SOURCE_NAME: Final[str] = ".current_work.md"
ALLOWED_ROOTS: Final[tuple[Path, ...]] = (
    ROOT,
    Path("/home/tim/.claude/tasks"),
    Path("/home/tim/.codex/memories"),
    Path("/home/tim/Documents/CodexVault"),
)
MODEL_FIELDS: Final[frozenset[str]] = frozenset(
    {"summary", "source_decisions", "open_tasks", "todo_items", "duplicate_candidates"}
)
LIST_FIELDS: Final[frozenset[str]] = frozenset(
    {"source_decisions", "open_tasks", "todo_items", "duplicate_candidates"}
)
DOCUMENT_FIELDS: Final[frozenset[str]] = frozenset(
    {
        "schema_version",
        "authority",
        "task_class",
        "created_at",
        "model",
        "eval_count",
        "sources",
        "exact_duplicate_groups",
        "draft",
        "application",
    }
)
SOURCE_FIELDS: Final[frozenset[str]] = frozenset(
    {
        "path",
        "sha256",
        "size_bytes",
        "title",
        "truncated_for_prompt",
        "prompt_text_chars",
    }
)


class ClerkError(RuntimeError):
    """A local-clerk request or draft failed closed."""


@dataclass(frozen=True)
class Source:
    path: Path
    sha256: str
    size_bytes: int
    title: str
    text: str
    truncated: bool

    def metadata(self, *, prompt_text_chars: int | None = None) -> dict[str, Any]:
        metadata = {
            "path": str(self.path),
            "sha256": self.sha256,
            "size_bytes": self.size_bytes,
            "title": self.title,
            "truncated_for_prompt": self.truncated
            or self.size_bytes > len(self.text.encode("utf-8"))
            or (prompt_text_chars is not None and prompt_text_chars < len(self.text)),
        }
        if prompt_text_chars is not None:
            metadata["prompt_text_chars"] = prompt_text_chars
        return metadata


def _allowed_path(raw: Path) -> Path:
    path = raw.expanduser()
    if not path.is_absolute():
        path = ROOT / path
    path = path.resolve()
    if path.name == FORBIDDEN_SOURCE_NAME:
        raise ClerkError("direct .current_work.md ingestion is forbidden")
    if not path.is_file():
        raise ClerkError(f"source is not a file: {path}")
    if not any(path == base or base in path.parents for base in ALLOWED_ROOTS):
        raise ClerkError(f"source is outside the clerk catalog: {path}")
    return path


def _read_source(path: Path) -> Source:
    """Read and hash one stable file descriptor with bounded retained text."""

    digest = hashlib.sha256()
    with path.open("rb") as handle:
        before = os.fstat(handle.fileno())
        prefix = bytearray()
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
            if len(prefix) <= MAX_SOURCE_BYTES:
                remaining = MAX_SOURCE_BYTES + 1 - len(prefix)
                prefix.extend(chunk[:remaining])
        after = os.fstat(handle.fileno())
    identity_before = (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
    identity_after = (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
    if identity_before != identity_after:
        raise ClerkError(f"source changed while being read: {path}")
    data = bytes(prefix)
    truncated = before.st_size > MAX_SOURCE_BYTES
    text = data[:MAX_SOURCE_BYTES].decode("utf-8", errors="replace")
    return Source(
        path=path,
        sha256=digest.hexdigest(),
        size_bytes=before.st_size,
        title=_title(text, path),
        text=text[:MAX_SOURCE_TEXT_CHARS],
        truncated=truncated,
    )


def _title(text: str, path: Path) -> str:
    for line in text.splitlines():
        match = TITLE_RE.match(line.strip())
        if match:
            return match.group(1).strip()
    return path.name


def read_sources(paths: list[Path]) -> list[Source]:
    """Read explicit files with a hard per-file memory bound."""

    if not paths:
        raise ClerkError("at least one explicit source path is required")
    sources: list[Source] = []
    seen: set[Path] = set()
    for raw in paths:
        path = _allowed_path(raw)
        if path in seen:
            continue
        if len(seen) >= MAX_SOURCE_COUNT:
            raise ClerkError(
                f"local clerk accepts at most {MAX_SOURCE_COUNT} distinct sources"
            )
        seen.add(path)
        sources.append(_read_source(path))
    return sources


def exact_duplicate_groups(sources: list[Source]) -> list[list[str]]:
    by_hash: dict[str, list[str]] = {}
    for source in sources:
        by_hash.setdefault(source.sha256, []).append(str(source.path))
    return [paths for paths in by_hash.values() if len(paths) > 1]


def inventory(paths: list[Path]) -> dict[str, Any]:
    sources = read_sources(paths)
    return {
        "schema_version": 1,
        "authority": "deterministic-clerk-inventory",
        "sources": [source.metadata() for source in sources],
        "exact_duplicate_groups": exact_duplicate_groups(sources),
    }


def _prompt(sources: list[Source]) -> tuple[str, list[int]]:
    instruction = (
        "Task class: compaction. Produce a very compact clerical draft from the supplied "
        "workspace material. Return JSON with exactly these keys: summary "
        "(string <= 400 chars), source_decisions, open_tasks, todo_items, and "
        "duplicate_candidates (each list has at most 3 strings <= 160 chars). Preserve "
        "uncertainty, identify conflicts instead of resolving them, and never "
        "invent missing facts. This is a draft for human review; do not state "
        "that anything is approved, authorized, deleted, launched, or complete. "
        "The following source envelope is valid JSON.\n\n"
    )

    def render(text_limit: int) -> tuple[str, list[int]]:
        excerpts = [
            "" if text_limit < 1 else fit_text(source.text, text_limit)
            for source in sources
        ]
        material = [
            {
                **source.metadata(prompt_text_chars=len(excerpt)),
                "text": excerpt,
            }
            for source, excerpt in zip(sources, excerpts, strict=True)
        ]
        envelope = json.dumps(
            {"schema_version": 1, "sources": material},
            ensure_ascii=False,
            separators=(",", ":"),
        )
        return instruction + envelope, [len(excerpt) for excerpt in excerpts]

    low = 0
    high = MAX_SOURCE_TEXT_CHARS
    best: tuple[str, list[int]] | None = None
    while low <= high:
        midpoint = (low + high) // 2
        candidate = render(midpoint)
        if len(candidate[0]) <= MAX_PROMPT_CHARS:
            best = candidate
            low = midpoint + 1
        else:
            high = midpoint - 1
    if best is None or any(
        chars < min(MIN_PROMPT_SOURCE_CHARS, len(source.text))
        for source, chars in zip(sources, best[1], strict=True)
    ):
        raise ClerkError(
            "source metadata leaves too little prompt room; use fewer or shorter paths"
        )
    return best


def _validate_model_draft(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != MODEL_FIELDS:
        raise ClerkError("local clerk returned an unexpected JSON shape")
    summary = value.get("summary")
    if not isinstance(summary, str):
        raise ClerkError("local clerk draft lacks a summary string")
    result: dict[str, Any] = {"summary": summary[:400]}
    for field in LIST_FIELDS:
        raw = value.get(field, [])
        if not isinstance(raw, list):
            raise ClerkError(f"local clerk field {field!r} is not a list")
        items = [item[:MAX_ITEM_CHARS] for item in raw if isinstance(item, str)]
        result[field] = items[:MAX_LIST_ITEMS]
    return result


def _parse_model_json(raw: str) -> Any:
    """Extract one JSON object from a model response with optional wrappers."""

    candidate = raw.strip()
    if candidate.startswith("```"):
        candidate = candidate.split("\n", 1)[1] if "\n" in candidate else candidate
        if candidate.endswith("```"):
            candidate = candidate[:-3].rstrip()
    try:
        return json.loads(candidate)
    except json.JSONDecodeError:
        start = candidate.find("{")
        if start < 0:
            raise
        parsed, _ = json.JSONDecoder().raw_decode(candidate[start:])
        return parsed


def _ollama_endpoint() -> tuple[str, int, str]:
    raw = os.environ.get("OLLAMA_HOST", DEFAULT_OLLAMA_HOST).strip()
    parsed = urllib.parse.urlsplit(raw)
    if (
        parsed.scheme != "http"
        or parsed.hostname not in ALLOWED_OLLAMA_HOSTS
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or parsed.path not in ("", "/")
    ):
        raise ClerkError(
            "OLLAMA_HOST must be a plain loopback HTTP origin with no path or credentials"
        )
    try:
        port = parsed.port or 80
    except ValueError as exc:
        raise ClerkError(f"OLLAMA_HOST has an invalid port: {raw!r}") from exc
    return parsed.hostname, port, "/api/generate"


def _generate(prompt: str, *, model: str) -> tuple[dict[str, Any], int]:
    if model != ALLOWED_MODEL:
        raise ClerkError(f"local clerk model must be exactly {ALLOWED_MODEL!r}")
    task_class = os.environ.get("LOCAL_AI_TASK", "").strip()
    if not task_class:
        raise ClerkError("set LOCAL_AI_TASK=compaction before local inference")
    require_clerical_task(task_class, "make a bounded summary of source text")
    host, port, target = _ollama_endpoint()
    request_body = json.dumps(
        {
            "model": model,
            "system": CLERK_SYSTEM_PROMPT,
            "prompt": prompt,
            "stream": False,
            "format": "json",
            "think": False,
            "keep_alive": -1,
            "options": {
                "num_ctx": 2048,
                "num_predict": 384,
                "temperature": 0.1,
            },
        },
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode("utf-8")
    connection = http.client.HTTPConnection(host, port, timeout=120)
    try:
        connection.request(
            "POST",
            target,
            body=request_body,
            headers={"Content-Type": "application/json"},
        )
        response = connection.getresponse()
        response_body = response.read(MAX_RESPONSE_BYTES + 1)
        if len(response_body) > MAX_RESPONSE_BYTES:
            raise ClerkError(f"local clerk response exceeds {MAX_RESPONSE_BYTES} bytes")
        if response.status < 200 or response.status >= 300:
            raise ClerkError(
                f"local clerk HTTP error: status={response.status} reason={response.reason}"
            )
    finally:
        connection.close()
    try:
        payload = json.loads(response_body)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ClerkError("local clerk HTTP response was not JSON") from exc
    raw = payload.get("response") if isinstance(payload, dict) else None
    if not isinstance(raw, str):
        raise ClerkError("local clerk returned no response text")
    try:
        parsed = _parse_model_json(raw)
    except json.JSONDecodeError as exc:
        raise ClerkError("local clerk response was not JSON") from exc
    eval_count = payload.get("eval_count") or 0
    if (
        isinstance(eval_count, bool)
        or not isinstance(eval_count, int)
        or eval_count < 0
    ):
        raise ClerkError("local clerk returned an invalid eval_count")
    return _validate_model_draft(parsed), eval_count


def draft(paths: list[Path], *, model: str = DEFAULT_MODEL) -> dict[str, Any]:
    sources = read_sources(paths)
    prompt, prompt_text_chars = _prompt(sources)
    model_draft, eval_count = _generate(prompt, model=model)
    return {
        "schema_version": 1,
        "authority": "local-clerk-only",
        "task_class": "compaction",
        "created_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "model": model,
        "eval_count": eval_count,
        "sources": [
            source.metadata(prompt_text_chars=chars)
            for source, chars in zip(sources, prompt_text_chars, strict=True)
        ],
        "exact_duplicate_groups": exact_duplicate_groups(sources),
        "draft": model_draft,
        "application": APPLICATION_MARKER,
    }


def validate_document(path: Path) -> dict[str, Any]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ClerkError(f"cannot read clerk draft: {path}") from exc
    if not isinstance(payload, dict) or set(payload) != DOCUMENT_FIELDS:
        raise ClerkError("draft has an unexpected document shape")
    if payload.get("schema_version") != 1:
        raise ClerkError("draft schema version is not supported")
    if payload.get("authority") != "local-clerk-only":
        raise ClerkError("draft is not a local-clerk document")
    if payload.get("task_class") != "compaction":
        raise ClerkError("draft task class is not compaction")
    if payload.get("model") != ALLOWED_MODEL:
        raise ClerkError(f"draft model is not {ALLOWED_MODEL!r}")
    if payload.get("application") != APPLICATION_MARKER:
        raise ClerkError("draft application marker is invalid")
    created_at = payload.get("created_at")
    if not isinstance(created_at, str):
        raise ClerkError("draft created_at is invalid")
    try:
        if datetime.fromisoformat(created_at).tzinfo is None:
            raise ValueError("timezone missing")
    except ValueError as exc:
        raise ClerkError("draft created_at is invalid") from exc
    eval_count = payload.get("eval_count")
    if (
        isinstance(eval_count, bool)
        or not isinstance(eval_count, int)
        or eval_count < 0
    ):
        raise ClerkError("draft eval_count is invalid")
    sources = payload.get("sources")
    if not isinstance(sources, list) or not sources:
        raise ClerkError("draft has no sources")
    verified_sources: list[Source] = []
    for item in sources:
        if not isinstance(item, dict) or set(item) != SOURCE_FIELDS:
            raise ClerkError("draft source metadata is malformed")
        source = read_sources([Path(str(item.get("path", "")))])[0]
        if source.sha256 != item.get("sha256"):
            raise ClerkError(f"source changed since draft: {source.path}")
        if source.size_bytes != item.get("size_bytes") or source.title != item.get(
            "title"
        ):
            raise ClerkError(f"source metadata changed since draft: {source.path}")
        prompt_chars = item.get("prompt_text_chars")
        if (
            isinstance(prompt_chars, bool)
            or not isinstance(prompt_chars, int)
            or prompt_chars < min(MIN_PROMPT_SOURCE_CHARS, len(source.text))
            or prompt_chars > len(source.text)
        ):
            raise ClerkError(f"source prompt coverage is invalid: {source.path}")
        if source.metadata(prompt_text_chars=prompt_chars)[
            "truncated_for_prompt"
        ] != item.get("truncated_for_prompt"):
            raise ClerkError(f"source truncation metadata is invalid: {source.path}")
        verified_sources.append(source)
    if payload.get("exact_duplicate_groups") != exact_duplicate_groups(
        verified_sources
    ):
        raise ClerkError("draft duplicate groups do not match current sources")
    _validate_model_draft(payload.get("draft"))
    return {"valid": True, "sources": len(sources), "path": str(path)}


def _safe_output_path(path: Path, *, sources: list[dict[str, Any]]) -> Path:
    candidate = path.expanduser()
    if not candidate.is_absolute():
        candidate = OUTPUT_ROOT / candidate
    candidate = candidate.resolve()
    output_root = OUTPUT_ROOT.resolve()
    if output_root not in candidate.parents:
        raise ClerkError(f"draft output must be below {output_root}")
    if candidate.name == FORBIDDEN_SOURCE_NAME or candidate.suffix != ".json":
        raise ClerkError("draft output must be a JSON file and not .current_work.md")
    source_paths = {Path(str(item["path"])).resolve() for item in sources}
    if candidate in source_paths:
        raise ClerkError(f"draft output collides with a source: {candidate}")
    if candidate.exists():
        raise ClerkError(f"draft output already exists: {candidate}")
    return candidate


def _write_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        dir=path.parent,
        prefix=f".{path.name}.",
        delete=False,
    ) as handle:
        temporary = Path(handle.name)
        json.dump(payload, handle, ensure_ascii=False, indent=2)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    try:
        os.link(temporary, path)
        directory_fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    except FileExistsError as exc:
        raise ClerkError(f"draft output already exists: {path}") from exc
    finally:
        temporary.unlink(missing_ok=True)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    inv = sub.add_parser("inventory", help="hash and inventory explicit sources")
    inv.add_argument("paths", nargs="+", type=Path)
    make = sub.add_parser("draft", help="ask the local clerk for a bounded draft")
    make.add_argument("paths", nargs="+", type=Path)
    make.add_argument("--model", default=DEFAULT_MODEL)
    make.add_argument("--output", type=Path)
    check = sub.add_parser("validate", help="re-hash and validate a saved draft")
    check.add_argument("path", type=Path)
    args = parser.parse_args(argv)
    try:
        if args.command == "inventory":
            print(json.dumps(inventory(args.paths), ensure_ascii=False, indent=2))
        elif args.command == "draft":
            payload = draft(args.paths, model=args.model)
            if args.output is None:
                print(json.dumps(payload, ensure_ascii=False, indent=2))
            else:
                output = _safe_output_path(args.output, sources=payload["sources"])
                _write_json(output, payload)
                print(
                    json.dumps(
                        {
                            "written": str(output),
                            "sources": len(payload["sources"]),
                        }
                    )
                )
        else:
            print(json.dumps(validate_document(args.path), ensure_ascii=False))
        return 0
    except (
        ClerkError,
        http.client.HTTPException,
        OSError,
        ValueError,
    ) as exc:
        print(json.dumps({"error": str(exc)}), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
