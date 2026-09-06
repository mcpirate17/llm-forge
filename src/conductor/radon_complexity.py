"""Radon complexity report and ratchet for production Python paths."""

from __future__ import annotations

import argparse
import fnmatch
import json
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_PATHS = ("conductor", "research", "aria_core", "aria_designer")
DEFAULT_EXCLUDES = (
    "*/rust/*",
    "*/tests/*",
    "*/tmp/*",
    "*/research/tmp/*",
    "*__pycache__*",
)
DEFAULT_BASELINE = REPO_ROOT / "conductor" / "radon_complexity_baseline.json"
RANKS = ("A", "B", "C", "D", "E", "F")


def _rank_at_or_above(rank: str, minimum: str) -> bool:
    return RANKS.index(rank) >= RANKS.index(minimum)


def _iter_python_files(paths: list[str], excludes: list[str]) -> list[Path]:
    files: list[Path] = []
    for raw_path in paths:
        base = (REPO_ROOT / raw_path).resolve()
        candidates = [base] if base.is_file() else sorted(base.rglob("*.py"))
        for path in candidates:
            rel = (
                path.relative_to(REPO_ROOT).as_posix()
                if path.is_relative_to(REPO_ROOT)
                else path.as_posix()
            )
            if any(fnmatch.fnmatch(rel, pattern) for pattern in excludes):
                continue
            files.append(path)
    return files


def _block_key(path: str, block: Any) -> str:
    class_name = getattr(block, "classname", None)
    name = f"{class_name}.{block.name}" if class_name else block.name
    return f"{path}::{name}"


def _scan(
    paths: list[str], excludes: list[str]
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    from radon.complexity import cc_rank, cc_visit

    findings: list[dict[str, Any]] = []
    parse_errors: list[dict[str, Any]] = []
    for path in _iter_python_files(paths, excludes):
        rel = (
            path.relative_to(REPO_ROOT).as_posix()
            if path.is_relative_to(REPO_ROOT)
            else path.as_posix()
        )
        try:
            source = path.read_text(encoding="utf-8")
            blocks = cc_visit(source)
        except (OSError, SyntaxError, ValueError, UnicodeDecodeError) as exc:
            parse_errors.append({"path": rel, "error": str(exc)})
            continue
        for block in blocks:
            rank = cc_rank(block.complexity)
            findings.append(
                {
                    "key": _block_key(rel, block),
                    "path": rel,
                    "name": block.name,
                    "type": getattr(block, "letter", "?"),
                    "line": int(block.lineno),
                    "complexity": int(block.complexity),
                    "rank": rank,
                }
            )
    findings.sort(key=lambda item: (-item["complexity"], item["path"], item["line"]))
    return findings, parse_errors


def _counts(findings: list[dict[str, Any]]) -> dict[str, int]:
    counts = {rank: 0 for rank in RANKS}
    for item in findings:
        counts[item["rank"]] += 1
    return counts


def _load_baseline(path: Path) -> dict[str, int]:
    """Map each grandfathered key to the worst score it is allowed to hold."""

    raw = json.loads(path.read_text(encoding="utf-8"))
    entries = raw.get("findings", raw if isinstance(raw, list) else [])
    baseline: dict[str, int] = {}
    for item in entries:
        key = str(item["key"])
        complexity = int(item["complexity"])
        # Two blocks can share path::name. Keeping the larger score would let
        # the smaller one grow up to it unnoticed, so the lowest one binds.
        baseline[key] = min(complexity, baseline.get(key, complexity))
    return baseline


def _write_baseline(
    path: Path,
    findings: list[dict[str, Any]],
    minimum_rank: str,
    paths: list[str],
    excludes: list[str],
) -> None:
    baseline_findings = [
        item for item in findings if _rank_at_or_above(item["rank"], minimum_rank)
    ]
    payload = {
        "minimum_rank": minimum_rank,
        "paths": paths,
        "excludes": excludes,
        "findings": baseline_findings,
    }
    path.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def _print_report(findings: list[dict[str, Any]], minimum_rank: str) -> None:
    counts = _counts(findings)
    total = sum(counts.values())
    flagged = [
        item for item in findings if _rank_at_or_above(item["rank"], minimum_rank)
    ]
    print(f"Total blocks: {total}")
    print("Rank counts: " + ", ".join(f"{rank}={counts[rank]}" for rank in RANKS))
    print(f"{minimum_rank}-F blocks: {len(flagged)}")
    for item in flagged[:50]:
        print(
            f"{item['complexity']:3d} {item['rank']} "
            f"{item['path']}:{item['line']} {item['type']} {item['name']}"
        )


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("report", "check", "refresh-baseline"))
    parser.add_argument("--baseline", default=str(DEFAULT_BASELINE))
    parser.add_argument("--min-rank", default="D", choices=RANKS)
    parser.add_argument("--path", dest="paths", action="append", default=[])
    parser.add_argument("--exclude", dest="excludes", action="append", default=[])
    return parser.parse_args()


def _describe(item: dict[str, Any]) -> str:
    return (
        f"{item['complexity']:3d} {item['rank']} "
        f"{item['path']}:{item['line']} {item['type']} {item['name']}"
    )


def _run_check(
    baseline_path: Path,
    findings: list[dict[str, Any]],
    minimum_rank: str,
) -> int:
    baseline = _load_baseline(baseline_path)
    current = [
        item for item in findings if _rank_at_or_above(item["rank"], minimum_rank)
    ]
    new_findings = [item for item in current if item["key"] not in baseline]
    worsened = [
        item
        for item in current
        if item["key"] in baseline and item["complexity"] > baseline[item["key"]]
    ]
    # A grandfathered block that improved -- including one that fell below the
    # minimum rank and left the flagged set -- is reported so the baseline can
    # be tightened. Left unsaid, the recorded debt only ever rots upward.
    best: dict[str, int] = {}
    for item in findings:
        key = item["key"]
        if key in baseline:
            best[key] = min(item["complexity"], best.get(key, item["complexity"]))
    improved = sorted(
        (key, baseline[key], score)
        for key, score in best.items()
        if score < baseline[key]
    )

    if improved:
        print(f"{len(improved)} grandfathered blocks improved; run refresh-baseline:")
        for key, was, now in improved[:50]:
            print(f"  {was} -> {now}  {key}")

    if not new_findings and not worsened:
        print(f"Complexity ratchet passed: no new or worsened {minimum_rank}-F blocks.")
        return 0

    if new_findings:
        print(
            f"Complexity ratchet failed: {len(new_findings)} "
            f"new {minimum_rank}-F blocks."
        )
        for item in new_findings[:50]:
            print(_describe(item))
    if worsened:
        print(
            f"Complexity ratchet failed: {len(worsened)} grandfathered blocks worsened."
        )
        for item in worsened[:50]:
            print(
                f"{_describe(item)}  ({baseline[item['key']]} -> {item['complexity']})"
            )
    return 1


def main() -> int:
    args = _parse_args()
    paths = args.paths or list(DEFAULT_PATHS)
    excludes = args.excludes or list(DEFAULT_EXCLUDES)
    baseline_path = (REPO_ROOT / args.baseline).resolve()
    findings, parse_errors = _scan(paths, excludes)
    if parse_errors:
        for err in parse_errors:
            print(f"Parse error: {err['path']}: {err['error']}")
        return 2

    if args.command == "report":
        _print_report(findings, args.min_rank)
        return 0

    if args.command == "refresh-baseline":
        _write_baseline(baseline_path, findings, args.min_rank, paths, excludes)
        _print_report(findings, args.min_rank)
        print(f"Wrote baseline: {baseline_path.relative_to(REPO_ROOT)}")
        return 0

    return _run_check(baseline_path, findings, args.min_rank)


if __name__ == "__main__":
    raise SystemExit(main())
