"""Deterministic code-reuse/consolidation cluster aggregator.

Scans the repo's Python source with stdlib `ast`, then uses the shipped native runtime
to find functions that are exact-or-near duplicates (renamed clones),
and groups them into ranked "consolidation clusters". Emits one compact JSON plus a
skimmable markdown table so cheap agents can plan dedup fixes from a small slice
without reading the whole repo.

Core algorithm: normalize bound local identifiers to per-function positional
placeholders, preserve global names/callees/literals/signatures, drop docstrings, and hash
the normalized AST dump. Functions that differ only in local spelling collapse together;
semantic constants and called operations remain distinct.

Optional `--with-token-clones` best-effort merges jscpd token-level clone output via
`conductor/run_duplicate_audit.py`; missing/broken tooling only prints a warning and
never fails the core run.
"""

from __future__ import annotations

import argparse
import ast
import json
import sys
from dataclasses import dataclass
from pathlib import Path

from conductor.audit_root import resolve_audit_root
from conductor.duplicate_audit_config import DEFAULT_SOURCE_DIRS
from conductor.reuse import _support
from conductor.reuse import core as slop_core

DEFAULT_MIN_LINES = 6
DEFAULT_BATCH_SIZE = 8
DEFAULT_OUT_REL = Path("tasks/audit/consolidation.json")
DEFAULT_MD_REL = Path("tasks/audit/consolidation.md")
JSCPD_REPORT_REL = Path("tasks/audit/duplication-jscpd/jscpd-report.json")

# Always-skip path components, in addition to whatever --exclude names.
HARD_SKIP_PARTS = {
    "tests",
    "test",
    "__pycache__",
    "node_modules",
    "migrations",
    ".venv",
    "build",
    "dist",
}


@dataclass
class FuncRecord:
    """One qualifying function occurrence (a candidate clone site)."""

    file: str  # repo-relative posix path
    line_start: int
    line_end: int
    name: str
    node_hash: str
    tokens: int
    source: str


@dataclass
class Cluster:
    kind: str  # "exact" | "near" | "token"
    tokens: int
    sites: list[FuncRecord]
    id: str = ""
    batch: int = -1
    confidence: float = 0.0
    value_score: int = 0
    risk: str = "high"
    disposition: str = "ignore"  # "auto" | "validate" | "ignore"
    rationale: str = ""

    @property
    def n_sites(self) -> int:
        return len(self.sites)

    @property
    def est_bytes(self) -> int:
        return self.tokens * (self.n_sites - 1)


# --------------------------------------------------------------------------------
# AST normalization
# --------------------------------------------------------------------------------


def _normalize_hash(node: ast.FunctionDef | ast.AsyncFunctionDef) -> tuple[str, int]:
    """Hash a function without erasing literals, callees, or signature semantics."""
    return slop_core.audit_consolidation_normalize(node)


# --------------------------------------------------------------------------------
# File collection + function extraction
# --------------------------------------------------------------------------------


def iter_python_files(repo: Path, targets: list[str], exclude: set[str]) -> list[Path]:
    skip = exclude | HARD_SKIP_PARTS
    roots = [repo / t for t in targets] if targets else [repo]
    return _support.iter_files(
        roots,
        suffixes={".py"},
        skip_parts=skip,
        sort_paths=True,
    )


def collect_functions(
    files: list[Path], repo: Path, min_lines: int
) -> tuple[list[FuncRecord], int]:
    """Return (qualifying function records, count of files that failed to parse)."""
    records, files_unparsable = slop_core.audit_consolidation_collect(
        paths=[str(path) for path in files], repo=str(repo), min_lines=min_lines
    )
    return [FuncRecord(**record) for record in records], files_unparsable


# --------------------------------------------------------------------------------
# Clustering
# --------------------------------------------------------------------------------


def build_clusters(records: list[FuncRecord]) -> list[Cluster]:
    return [
        Cluster(
            kind=raw["kind"],
            tokens=raw["tokens"],
            sites=[FuncRecord(**site) for site in raw["sites"]],
            confidence=raw["confidence"],
            value_score=raw["value_score"],
            risk=raw["risk"],
            disposition=raw["disposition"],
            rationale=raw["rationale"],
        )
        for raw in slop_core.audit_consolidation_build(records)
    ]


def _cluster_evidence(cluster: Cluster) -> None:
    confidence, value_score, risk, disposition, rationale = (
        slop_core.audit_consolidation_evidence(
            cluster.kind, cluster.tokens, cluster.sites
        )
    )
    cluster.confidence = confidence
    cluster.value_score = value_score
    cluster.risk = risk
    cluster.disposition = disposition
    cluster.rationale = rationale


def _assign_ids(clusters: list[Cluster], batch_size: int) -> None:
    for cluster, (cluster_id, batch) in zip(
        clusters, slop_core.audit_consolidation_assign(clusters, batch_size)
    ):
        cluster.id = cluster_id
        cluster.batch = batch


def scan_clusters(
    repo: Path,
    targets: list[str],
    exclude: set[str],
    min_lines: int = DEFAULT_MIN_LINES,
    batch_size: int = DEFAULT_BATCH_SIZE,
) -> tuple[list[Cluster], dict]:
    """Deterministic core scan reused by both the ROI oracle and the `consolidate`
    subcommand. Returns (ranked+id'd clusters, stats)."""
    files = iter_python_files(repo, targets, exclude)
    records, files_unparsable = collect_functions(files, repo, min_lines)
    clusters = build_clusters(records)
    _assign_ids(clusters, batch_size)
    stats = {
        "files_scanned": len(files),
        "files_unparsable": files_unparsable,
        "functions_considered": len(records),
    }
    return clusters, stats


def _suggested_home(sites: list[FuncRecord]) -> str | None:
    """Purpose-named shared home for multi-file clone clusters.

    Mirrors ``file_families.purpose_named_home``: propose a concrete module named
    for what the sites share (never a generic ``_shared.py`` dumping ground).
    Returns None only when members span repo roots (no single common directory).
    """
    return slop_core.audit_consolidation_suggested_home(sites)


# --------------------------------------------------------------------------------
# Optional external ingest: jscpd token-level clones (best-effort, OFF by default)
# --------------------------------------------------------------------------------


def ingest_token_clones(repo: Path) -> list[Cluster]:
    """Best-effort merge of jscpd token-clone clusters via the existing conductor
    entrypoint. Never raises: any failure just warns on stderr and returns [].
    """
    script = repo / "conductor" / "run_duplicate_audit.py"
    if not script.exists():
        print(
            "WARNING: --with-token-clones requested but "
            "conductor/run_duplicate_audit.py is missing; continuing core-only",
            file=sys.stderr,
        )
        return []
    try:
        _support.run_tool_capture(
            [sys.executable, str(script), "--tool", "jscpd"],
            repo,
            timeout=600,
            tool_name="jscpd token-clone ingest",
            ok_returncodes={0},
            not_found_detail=f"{sys.executable} was not found",
        )
    except RuntimeError as exc:
        print(
            f"WARNING: jscpd token-clone ingest failed ({exc}); continuing core-only",
            file=sys.stderr,
        )
        return []

    report = repo / JSCPD_REPORT_REL
    if not report.exists():
        print(
            "WARNING: jscpd ran but produced no report; continuing core-only",
            file=sys.stderr,
        )
        return []
    try:
        data = json.loads(report.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        print(
            f"WARNING: could not read jscpd report ({exc}); continuing core-only",
            file=sys.stderr,
        )
        return []
    return _clusters_from_jscpd(data)


def _clusters_from_jscpd(data: dict) -> list[Cluster]:
    clusters: list[Cluster] = []
    for dup in data.get("duplicates", []):
        first = dup.get("firstFile") or {}
        second = dup.get("secondFile") or {}
        if not first or not second:
            continue
        tokens = dup.get("tokens") or dup.get("lines") or 0
        sites = [
            FuncRecord(
                file=first["name"],
                line_start=first["start"],
                line_end=first["end"],
                name="<clone>",
                node_hash="",
                tokens=tokens,
                source="",
            ),
            FuncRecord(
                file=second["name"],
                line_start=second["start"],
                line_end=second["end"],
                name="<clone>",
                node_hash="",
                tokens=tokens,
                source="",
            ),
        ]
        cluster = Cluster(kind="token", tokens=tokens, sites=sites)
        _cluster_evidence(cluster)
        clusters.append(cluster)
    return clusters


# --------------------------------------------------------------------------------
# Output
# --------------------------------------------------------------------------------


def cluster_to_dict(cluster: Cluster) -> dict:
    return {
        "id": cluster.id,
        "kind": cluster.kind,
        "tokens": cluster.tokens,
        "n_sites": cluster.n_sites,
        "est_bytes": cluster.est_bytes,
        "confidence": cluster.confidence,
        "value_score": cluster.value_score,
        "risk": cluster.risk,
        "disposition": cluster.disposition,
        "rationale": cluster.rationale,
        "batch": cluster.batch,
        "suggested_home": _suggested_home(cluster.sites),
        "sites": [
            {
                "file": s.file,
                "line_start": s.line_start,
                "line_end": s.line_end,
                "name": s.name,
            }
            for s in cluster.sites
        ],
    }


def build_output(
    clusters: list[Cluster],
    targets: list[str],
    min_lines: int,
    batch_size: int,
    files_scanned: int,
    files_unparsable: int,
    functions_considered: int,
) -> dict:
    actionable = [cluster for cluster in clusters if cluster.disposition == "auto"]
    n_batches = len({cluster.batch for cluster in actionable})
    summary = {
        "n_clusters": len(clusters),
        "n_actionable": len(actionable),
        "n_validate": sum(c.disposition == "validate" for c in clusters),
        "n_ignored": sum(c.disposition == "ignore" for c in clusters),
        "n_batches": n_batches,
        "total_redundant_bytes": sum(c.est_bytes for c in clusters),
        "actionable_redundant_bytes": sum(c.est_bytes for c in actionable),
        "actionable_value_score": sum(c.value_score for c in actionable),
        "files_scanned": files_scanned,
        "files_unparsable": files_unparsable,
        "functions_considered": functions_considered,
    }
    return _support.generated_output(
        base={
            "targets": targets,
            "min_lines": min_lines,
            "batch_size": batch_size,
            "summary": summary,
        },
        items_key="clusters",
        items=clusters,
        serialize=cluster_to_dict,
    )


def write_markdown(path: Path, data: dict) -> None:
    s = data["summary"]
    rows = [_markdown_row(cluster) for cluster in data["clusters"]]
    _support.write_markdown_table(
        path,
        title=f"# Consolidation report — generated {data['generated_at']}",
        summary=(
            f"{s['n_clusters']} clusters across {s['n_batches']} batches; "
            f"{s['n_actionable']} auto-actionable, {s['n_validate']} require validation; "
            f"~{s['total_redundant_bytes']} redundant AST-node-units; "
            f"{s['files_scanned']} files scanned ({s['files_unparsable']} unparsable), "
            f"{s['functions_considered']} functions considered."
        ),
        columns=[
            "id",
            "batch",
            "kind",
            "disposition",
            "confidence",
            "value",
            "n_sites",
            "est_bytes",
            "suggested_home",
            "sites (file:line, ...)",
        ],
        rows=rows,
    )


def _markdown_row(cluster: dict) -> str:
    site_strs = [f"{s['file']}:{s['line_start']}" for s in cluster["sites"]]
    site_field = _support.plus_more_list(site_strs, show=6)
    return (
        f"| {cluster['id']} | {cluster['batch']} | {cluster['kind']} | "
        f"{cluster['disposition']} | {cluster['confidence']:.2f} | "
        f"{cluster['value_score']} | "
        f"{cluster['n_sites']} | {cluster['est_bytes']} | "
        f"{cluster['suggested_home'] or 'review required'} | {site_field} |"
    )


# --------------------------------------------------------------------------------
# Config + CLI
# --------------------------------------------------------------------------------


def resolve_settings(args: argparse.Namespace) -> tuple[Path, list[str], list[str]]:
    """Scan roots for this run: explicit flags, else conductor's own scan config.

    The root comes from ``resolve_audit_root``, which requires the Git worktree the
    operator is standing in -- never ``__file__``, which would name whichever checkout
    supplied the imported module.
    """
    repo = resolve_audit_root(args.repo)
    targets = args.targets if args.targets else list(DEFAULT_SOURCE_DIRS)
    exclude = args.exclude if args.exclude else []
    return repo, targets, exclude


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Deterministic, stdlib-only code-reuse/consolidation cluster aggregator."
        )
    )
    parser.add_argument(
        "--repo", default=None, help="Repo root (default: the Git worktree of the cwd)."
    )
    parser.add_argument(
        "--targets",
        nargs="+",
        default=None,
        help="Subdirs to scan (default: conductor DEFAULT_SOURCE_DIRS).",
    )
    parser.add_argument(
        "--exclude",
        nargs="+",
        default=None,
        help="Path parts to skip (overrides config).",
    )
    parser.add_argument("--min-lines", type=int, default=DEFAULT_MIN_LINES)
    parser.add_argument("--batch-size", type=int, default=DEFAULT_BATCH_SIZE)
    parser.add_argument("--out", default=None, help="Output JSON path.")
    parser.add_argument("--md", default=None, help="Output markdown path.")
    parser.add_argument(
        "--with-token-clones",
        action="store_true",
        help="Best-effort merge jscpd token-level clone clusters (off by default).",
    )
    parser.add_argument(
        "--selftest",
        action="store_true",
        help="Run the normalizer self-test and exit.",
    )
    return parser


# --------------------------------------------------------------------------------
# Self-test
# --------------------------------------------------------------------------------


def _parse_func(src: str) -> ast.FunctionDef:
    node = ast.parse(src).body[0]
    assert isinstance(node, ast.FunctionDef)
    return node


def _selftest() -> None:
    same_a = _parse_func("def f(a, b):\n    return a + b\n")
    same_b = _parse_func("def g(x, y):\n    return x + y\n")
    different = _parse_func("def h(a, b):\n    return a - b\n")

    hash_a, _ = _normalize_hash(same_a)
    hash_b, _ = _normalize_hash(same_b)
    hash_c, _ = _normalize_hash(different)

    assert hash_a == hash_b, "renamed-identifier clones must hash identically"
    assert hash_a != hash_c, "structurally different bodies must hash differently"
    print("SELFTEST OK")


# --------------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------------


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)

    if args.selftest:
        _selftest()
        return 0

    repo, targets, exclude = resolve_settings(args)
    exclude_set = set(exclude)

    clusters, stats = scan_clusters(
        repo, targets, exclude_set, args.min_lines, args.batch_size
    )

    if args.with_token_clones:
        clusters.extend(ingest_token_clones(repo))
        clusters.sort(key=lambda c: c.est_bytes, reverse=True)
        _assign_ids(clusters, args.batch_size)

    out_path = Path(args.out) if args.out else repo / DEFAULT_OUT_REL
    md_path = Path(args.md) if args.md else repo / DEFAULT_MD_REL
    out_path.parent.mkdir(parents=True, exist_ok=True)
    md_path.parent.mkdir(parents=True, exist_ok=True)

    data = build_output(
        clusters=clusters,
        targets=targets,
        min_lines=args.min_lines,
        batch_size=args.batch_size,
        files_scanned=stats["files_scanned"],
        files_unparsable=stats["files_unparsable"],
        functions_considered=stats["functions_considered"],
    )
    out_path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    write_markdown(md_path, data)

    print(
        f"consolidation: {len(clusters)} clusters, "
        f"{data['summary']['total_redundant_bytes']} redundant bytes -> "
        f"{out_path} / {md_path}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
