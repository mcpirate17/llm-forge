"""Repository-wide duplicate, native-reuse, dependency, and compliance evidence."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

from conductor._native import native_reuse_candidates_native
from conductor.reuse import graph_index


def _fingerprint(kind: str, identities: list[str]) -> str:
    payload = kind + "\0" + "\0".join(sorted(identities))
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()[:20]


def native_reuse_candidates(
    index: graph_index.GraphIndex, limit: int = 80
) -> list[dict]:
    if hasattr(index, "native_reuse_rows"):
        rows = index.native_reuse_rows()
    else:
        symbols = index.symbols(languages={"python", "c", "cpp", "rust"})
        rows = [
            {
                "name": symbol.name,
                "file": symbol.file,
                "line_start": symbol.line_start,
                "language": symbol.language,
                "params": symbol.params,
                "caller_count": len(index.callers(symbol.qualified_name)),
            }
            for symbol in symbols
            if symbol.kind in {"Function", "Class"} and not symbol.is_test
        ]
    return json.loads(native_reuse_candidates_native(json.dumps(rows), limit))


def jscpd_candidates(
    repo: Path, report_rel: str, limit: int = 80
) -> tuple[list[dict], dict]:
    report = repo / report_rel
    if not report.exists():
        return [], {
            "status": "incomplete",
            "reason": f"missing {report_rel}",
            "clusters": 0,
        }
    try:
        data = json.loads(report.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        return [], {"status": "incomplete", "reason": str(exc), "clusters": 0}
    duplicates = data.get("duplicates", []) if isinstance(data, dict) else []
    candidates: list[dict] = []
    for duplicate in duplicates:
        first = duplicate.get("firstFile") or {}
        second = duplicate.get("secondFile") or {}
        if not first.get("name") or not second.get("name"):
            continue
        identities = [
            f"{first['name']}:{first.get('start', 0)}-{first.get('end', 0)}",
            f"{second['name']}:{second.get('start', 0)}-{second.get('end', 0)}",
        ]
        lines = int(duplicate.get("lines") or 0)
        candidates.append(
            {
                "id": f"token-clone:{_fingerprint('token-clone', identities)}",
                "category": "duplication",
                "severity": "medium",
                "confidence": 0.9,
                "value": max(20, lines),
                "files": sorted({first["name"], second["name"]}),
                "symbols": [],
                "tests": [],
                "location": ", ".join(identities),
                "evidence": f"jscpd {duplicate.get('format', 'unknown')} clone spanning {lines} lines",
                "evidence_complete": True,
                "disposition": "validate",
                "duplicate_lines": lines,
            }
        )
    candidates.sort(key=lambda item: item["value"], reverse=True)
    return candidates[:limit], {
        "status": "complete",
        "reason": "parsed repo-wide jscpd report",
        "clusters": len(duplicates),
        "reported_candidates": len(candidates),
        "report": report_rel,
    }


def compliance_candidates(repo: Path) -> list[dict]:
    candidates: list[dict] = []
    forbidden_root = {".log", ".jsonl", ".sqlite", ".db"}
    for path in sorted(repo.iterdir()):
        if path.is_file() and path.suffix.lower() in forbidden_root:
            rel = path.relative_to(repo).as_posix()
            candidates.append(
                {
                    "id": f"compliance:{_fingerprint('root-file', [rel])}",
                    "category": "compliance",
                    "severity": "medium",
                    "confidence": 1.0,
                    "value": 30,
                    "files": [rel],
                    "location": rel,
                    "evidence": "forbidden generated/data artifact at repository root",
                    "evidence_complete": True,
                }
            )
    notes = repo / "research" / "notes"
    if notes.exists():
        for path in sorted(notes.rglob("*")):
            if path.is_file() and path.suffix.lower() in {".json", ".jsonl", ".csv"}:
                rel = path.relative_to(repo).as_posix()
                candidates.append(
                    {
                        "id": f"compliance:{_fingerprint('notes-data', [rel])}",
                        "category": "compliance",
                        "severity": "medium",
                        "confidence": 1.0,
                        "value": 30,
                        "files": [rel],
                        "location": rel,
                        "evidence": "structured data inside markdown-only research/notes",
                        "evidence_complete": True,
                    }
                )
    return candidates


def dependency_candidates(
    repo: Path, index: graph_index.GraphIndex, limit: int = 80
) -> list[dict]:
    """Emit deterministic direct-cycle and heavyweight-import candidates."""
    edges = index.import_edges()
    pairs = {(source, target) for source, target, _, _ in edges}
    candidates: list[dict] = []
    seen_cycles: set[tuple[str, str]] = set()
    for source, target, file_path, line in edges:
        if (target, source) in pairs:
            cycle = tuple(sorted((source, target)))
            if cycle in seen_cycles:
                continue
            seen_cycles.add(cycle)
            path = Path(file_path)
            try:
                rel = path.relative_to(repo).as_posix()
            except ValueError:
                rel = path.as_posix()
            candidates.append(
                {
                    "id": f"dependency-cycle:{_fingerprint('dependency-cycle', list(cycle))}",
                    "category": "imports_deps",
                    "severity": "medium",
                    "confidence": 0.9,
                    "value": 45,
                    "files": [rel],
                    "location": f"{rel}:{line}",
                    "evidence": f"direct import cycle between {cycle[0]} and {cycle[1]}",
                    "evidence_complete": True,
                }
            )
        lowered = target.lower()
        if any(token in lowered for token in ("torch", "cuda", "triton")):
            path = Path(file_path)
            try:
                rel = path.relative_to(repo).as_posix()
            except ValueError:
                continue
            candidates.append(
                {
                    "id": f"heavy-import:{_fingerprint('heavy-import', [source, target])}",
                    "category": "imports_deps",
                    "severity": "low",
                    "confidence": 0.72,
                    "value": 15,
                    "files": [rel],
                    "location": f"{rel}:{line}",
                    "evidence": f"heavy top-level import candidate: {target}",
                    "evidence_complete": False,
                    "disposition": "validate",
                }
            )
    candidates.sort(key=lambda item: (item["value"], item["confidence"]), reverse=True)
    return candidates[:limit]
