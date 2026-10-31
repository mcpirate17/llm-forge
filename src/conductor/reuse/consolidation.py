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

Optional `--with-token-clones` merges jscpd token-level clone output through the
packaged duplicate-audit collector. Requested detector failures retain the AST
report, mark token evidence incomplete, and return a nonzero status.
"""

from __future__ import annotations

import argparse
import ast
import json
import sys
from collections import Counter
from dataclasses import asdict, dataclass
from pathlib import Path

from conductor.atomic_json import atomic_write, write_json_atomic
from conductor.duplicate_audit_config import (
    JSCPD_FULL_FILE_SUFFIXES,
    JSCPD_SOURCE_SUFFIXES,
    add_scan_root_arguments,
    scan_roots,
    source_dirs,
)
from conductor.reuse import _support
from conductor.reuse import core as slop_core
from conductor.run_duplicate_audit import (
    DuplicateAuditError,
    _live_jscpd_sources,
    collect_jscpd_report,
)


def _native_decide(operation: str, payload: object) -> object:
    from conductor._native import reuse_consolidation_native

    return json.loads(reuse_consolidation_native(operation, json.dumps(payload)))


DEFAULT_MIN_LINES = 6
DEFAULT_BATCH_SIZE = 8
DEFAULT_OUT_REL = Path("tasks/audit/consolidation.json")
DEFAULT_MD_REL = Path("tasks/audit/consolidation.md")

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
    files: list[Path], repo: Path, min_lines: int, *, profiles: list[dict] | None = None
) -> tuple[list[FuncRecord], int]:
    """Return (qualifying function records, count of files that failed to parse)."""
    settings = dict(
        paths=[str(path) for path in files], repo=str(repo), min_lines=min_lines
    )
    if profiles is None:
        records, files_unparsable = slop_core.audit_consolidation_collect(**settings)
    else:
        records, collected, files_unparsable = (
            slop_core.audit_consolidation_collect_profiles(**settings)
        )
        profiles.extend(collected)
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
    *,
    profiles: list[dict] | None = None,
) -> tuple[list[Cluster], dict]:
    """Deterministic core scan reused by both the ROI oracle and the `consolidate`
    subcommand. Returns (ranked+id'd clusters, stats)."""
    files = iter_python_files(repo, targets, exclude)
    records, files_unparsable = collect_functions(
        files, repo, min_lines, profiles=profiles
    )
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
# Optional external ingest: jscpd token-level clones (OFF by default)
# --------------------------------------------------------------------------------


def _generated_by_cython(path: Path) -> bool:
    """Recognize generated Cython outputs without reading an entire native file."""
    if path.suffix.lower() not in {".c", ".cpp", ".cxx", ".h", ".hpp"}:
        return False
    try:
        with path.open("rb") as source:
            return b"Generated by Cython" in source.read(512)
    except OSError as exc:
        raise DuplicateAuditError(
            f"cannot inspect native source {path}: {exc}"
        ) from exc


def _token_source_paths(
    repo: Path,
    targets: list[str] | None = None,
    *,
    exclude: set[str] | None = None,
    full_files: bool = False,
    metadata: dict | None = None,
) -> list[str]:
    """Deduplicate owned source inputs before expanding the coverage audit."""
    roots = [
        repo / target for target in (source_dirs(repo) if targets is None else targets)
    ]
    for root in roots:
        if not root.resolve().is_relative_to(repo):
            raise DuplicateAuditError(
                f"token-clone scan target is outside repository: {root}"
            )
    files = _support.iter_files(
        roots,
        suffixes=set(JSCPD_SOURCE_SUFFIXES | JSCPD_FULL_FILE_SUFFIXES),
        skip_parts=HARD_SKIP_PARTS | (exclude or set()),
        sort_paths=True,
    )
    selected = {path.resolve() for path in files if not path.is_symlink()}
    visible = (
        _live_jscpd_sources(
            root=repo,
            paths=tuple(str(root.relative_to(repo)) for root in roots),
            suffixes=JSCPD_SOURCE_SUFFIXES | JSCPD_FULL_FILE_SUFFIXES,
        )
        if full_files
        else None
    )
    before_ownership = len(selected)
    if visible is not None:
        owned = {repo / path for path in visible}
        selected.intersection_update(owned)
    non_code = Counter()
    generated = []
    paths = []
    for path in sorted(selected):
        relative = path.relative_to(repo).as_posix()
        if full_files and path.suffix.lower() not in JSCPD_FULL_FILE_SUFFIXES:
            non_code[path.suffix.lower()] += 1
            continue
        if full_files and _generated_by_cython(path):
            generated.append(relative)
            continue
        paths.append(relative)
    if metadata is not None:
        metadata.update(
            selected_files=len(paths),
            excluded_non_code_files=sum(non_code.values()),
            excluded_non_code_suffixes=dict(non_code),
            excluded_generated_files=len(generated),
            excluded_generated_paths=generated,
            ownership="git-tracked-and-nonignored-untracked"
            if visible is not None
            else "filesystem-export",
            excluded_unowned_files=before_ownership - len(selected),
        )
    return paths


def _token_coverage(repo: Path, paths: list[str], data: dict) -> dict:
    """Explain source omissions using the actual report and detector thresholds."""
    reported = set()
    for format_stats in data.get("statistics", {}).get("formats", {}).values():
        sources = format_stats.get("sources", {})
        if not isinstance(sources, dict):
            raise DuplicateAuditError("jscpd report has malformed source statistics")
        for name in sources:
            path = Path(name)
            absolute = path if path.is_absolute() else repo / path
            try:
                reported.add(absolute.resolve().relative_to(repo).as_posix())
            except ValueError as exc:
                raise DuplicateAuditError(
                    f"jscpd statistics name an external source: {name}"
                ) from exc
    missing = sorted(set(paths) - reported)
    minimum = data.get("audit_config", {}).get("min_lines")
    below_lines = []
    if isinstance(minimum, int):
        for relative in missing:
            try:
                lines = (repo / relative).read_bytes().count(b"\n") + 1
            except OSError as exc:
                raise DuplicateAuditError(
                    f"cannot verify omitted source {relative}: {exc}"
                ) from exc
            if lines < minimum:
                below_lines.append(relative)
    unexplained = sorted(set(missing) - set(below_lines))
    return {
        "reported_files": len(reported),
        "min_lines": minimum,
        "min_tokens": data.get("audit_config", {}).get("min_tokens"),
        "below_min_lines_count": len(below_lines),
        "below_min_lines_paths": below_lines,
        "unexplained_missing_count": len(unexplained),
        "unexplained_missing_paths": unexplained,
    }


def ingest_token_clones(
    repo: Path,
    targets: list[str] | None = None,
    *,
    exclude: set[str] | None = None,
    full_files: bool = False,
    metadata: dict | None = None,
) -> list[Cluster]:
    """Use the packaged strict collector; no checkout-local script is required."""
    repo = repo.resolve()
    paths = _token_source_paths(
        repo, targets, exclude=exclude, full_files=full_files, metadata=metadata
    )
    data = collect_jscpd_report(
        paths,
        cwd=repo,
        full_files=full_files,
    )
    if metadata is not None or full_files:
        statistics = data.get("statistics", {})
        formats = (
            statistics.get("formats", {}) if isinstance(statistics, dict) else None
        )
        if not isinstance(formats, dict) or not all(
            isinstance(value, dict) for value in formats.values()
        ):
            raise DuplicateAuditError("jscpd report has malformed statistics")
        if metadata is not None:
            metadata["statistics"] = {
                "total": statistics.get("total", {}),
                "formats": {
                    name: value.get("total", {}) for name, value in formats.items()
                },
            }
    if full_files:
        coverage = _token_coverage(repo, paths, data)
        if metadata is not None:
            metadata["coverage"] = coverage
        if missing := coverage["unexplained_missing_count"]:
            error = f"jscpd omitted {missing} selected source files without explained thresholds"
            if metadata is None:
                raise DuplicateAuditError(error)
            metadata.update(status="incomplete", error=error)
    for duplicate in data["duplicates"]:
        for side in ("firstFile", "secondFile"):
            file = duplicate[side]
            reported = Path(file["name"])
            absolute = reported if reported.is_absolute() else repo / reported
            try:
                file["name"] = absolute.resolve().relative_to(repo).as_posix()
            except ValueError as exc:
                raise DuplicateAuditError(
                    f"jscpd reported a source outside the audited repository: {reported}"
                ) from exc
    return _clusters_from_jscpd(data)


def _clusters_from_jscpd(data: dict) -> list[Cluster]:
    clusters = []
    for row in _native_decide("token_clones", data):
        cluster = Cluster(
            kind=row["kind"],
            tokens=row["tokens"],
            sites=[FuncRecord(**site) for site in row["sites"]],
        )
        _cluster_evidence(cluster)
        clusters.append(cluster)
    return clusters


# --------------------------------------------------------------------------------
# Output
# --------------------------------------------------------------------------------


def cluster_to_dict(cluster: Cluster) -> dict:
    return _native_decide(
        "cluster_dicts",
        {"clusters": [asdict(cluster)], "homes": [_suggested_home(cluster.sites)]},
    )[0]


def build_output(
    clusters: list[Cluster],
    targets: list[str],
    min_lines: int,
    batch_size: int,
    files_scanned: int,
    files_unparsable: int,
    functions_considered: int,
) -> dict:
    rows = [asdict(cluster) for cluster in clusters]
    payload = {
        "clusters": rows,
        "homes": [_suggested_home(cluster.sites) for cluster in clusters],
        "files_scanned": files_scanned,
        "files_unparsable": files_unparsable,
        "functions_considered": functions_considered,
    }
    summary = _native_decide("report_summary", payload)
    cluster_rows = _native_decide("cluster_dicts", payload)
    return _support.generated_output(
        base={
            "targets": targets,
            "min_lines": min_lines,
            "batch_size": batch_size,
            "summary": summary,
        },
        items_key="clusters",
        items=cluster_rows,
        serialize=lambda row: row,
    )


def write_markdown(path: Path, data: dict) -> None:
    markdown = _native_decide("markdown", data)
    token = data.get("detectors", {}).get("token_clones")
    if token:
        markdown += f"\nToken-clone detector: **{token['status']}**.\n"
        if "error" in token:
            markdown += f"\n{token['error']}\n"
    abstraction = data.get("abstractions")
    if abstraction:
        coverage = abstraction["coverage"]
        markdown += (
            f"\nStructural abstraction candidates: **{len(abstraction['edges'])}** "
            f"review-only pairs; coverage complete: **{coverage['complete']}**. "
            "These pairs do not establish semantic equivalence or file retirement.\n"
        )
    with atomic_write(path) as handle:
        handle.write(markdown)


def _markdown_row(cluster: dict) -> str:
    return _native_decide("markdown_row", cluster)


# --------------------------------------------------------------------------------
# Config + CLI
# --------------------------------------------------------------------------------


def resolve_settings(args: argparse.Namespace) -> tuple[Path, list[str], list[str]]:
    """Scan roots for this run (see ``scan_roots``) and the path parts to skip."""
    repo, targets = scan_roots(args)
    exclude = args.exclude if args.exclude else []
    return repo, targets, exclude


def add_abstraction_arguments(parser: argparse.ArgumentParser) -> None:
    """Shared bounded structural-comparison controls for standalone and sweep CLIs."""
    parser.add_argument("--abstraction-min-similarity", type=float, default=0.80)
    parser.add_argument("--abstraction-min-shared-features", type=int, default=8)
    parser.add_argument("--abstraction-max-pairs", type=int, default=2_000_000)
    parser.add_argument("--abstraction-max-bucket-size", type=int, default=80)
    parser.add_argument(
        "--abstraction-exhaustive",
        action="store_true",
        help="Compare all function pairs within --abstraction-max-pairs; report incomplete at the cap.",
    )
    parser.add_argument(
        "--abstraction-include-features",
        action="store_true",
        help="Keep raw profile feature arrays in the report (potentially very large).",
    )


def abstraction_settings(args: argparse.Namespace) -> dict:
    """Validate CLI controls before running detectors or publishing reports."""
    settings = {
        name: getattr(args, f"abstraction_{name}")
        for name in (
            "min_similarity",
            "min_shared_features",
            "max_pairs",
            "max_bucket_size",
            "exhaustive",
            "include_features",
        )
    }
    if not 0 < settings["min_similarity"] <= 1:
        raise ValueError("--abstraction-min-similarity must be in (0, 1]")
    for name, minimum in (
        ("min_shared_features", 1),
        ("max_pairs", 1),
        ("max_bucket_size", 2),
    ):
        if settings[name] < minimum:
            raise ValueError(
                f"--abstraction-{name.replace('_', '-')} must be at least {minimum}"
            )
    return settings


def abstraction_cli_options(options: dict) -> list[str]:
    """Serialize the shared controls for subprocess and reproduction commands."""
    arguments = []
    for name, value in options.items():
        flag = f"--abstraction-{name.replace('_', '-')}"
        if isinstance(value, bool):
            arguments.extend([flag] if value else [])
        else:
            arguments.extend([flag, str(value)])
    return arguments


def add_abstraction_report(
    data: dict, profiles: list[dict] | None, options: dict, repo: Path
) -> None:
    """Keep retrieval completion distinct from unknown approximate recall."""
    status = {"status": "not_requested"}
    if profiles is not None:
        native_options = {
            key: value for key, value in options.items() if key != "include_features"
        }
        evidence = slop_core.audit_abstraction_pairs(profiles, **native_options)
        include_features = options["include_features"]
        feature_keys = (
            "structure",
            "api",
            "fields",
            "calls",
            "control",
            "imports",
            "schemas",
        )
        for profile in profiles:
            profile["feature_counts"] = {key: len(profile[key]) for key in feature_keys}
            if not include_features:
                for key in feature_keys:
                    del profile[key]
        data["abstractions"] = {
            "profiles": profiles,
            **evidence,
            "features_omitted_from_report": not include_features,
            "feature_report_reason": "Raw retrieval features are omitted by default to bound report size; all identities, semantic values, counts and pair evidence are retained.",
            "feature_reproduction_cli": [
                sys.executable,
                "-m",
                "conductor.reuse.consolidation",
                "--repo",
                str(repo),
                *(["--targets", *data["targets"]] if data["targets"] else []),
                *(["--exclude", *data["exclude"]] if data["exclude"] else []),
                "--min-lines",
                str(data["min_lines"]),
                "--with-abstractions",
                *abstraction_cli_options({**options, "include_features": True}),
            ],
        }
        coverage = evidence["coverage"]
        complete = coverage["candidate_generation_complete"]
        status = {
            "status": "complete" if complete else "incomplete",
            "coverage": coverage,
        }
        data["summary"]["abstraction_candidates"] = len(evidence["edges"])
        data["summary"]["abstraction_candidate_generation_complete"] = complete
        data["complete"] &= complete
    data["detectors"]["abstractions"] = status


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Deterministic, stdlib-only code-reuse/consolidation cluster aggregator."
        )
    )
    add_scan_root_arguments(parser)
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
        "--with-abstractions",
        action="store_true",
        help="Include review-only structural pairs separately from normalized clones.",
    )
    add_abstraction_arguments(parser)
    parser.add_argument(
        "--with-token-clones",
        action="store_true",
        help="Merge jscpd token-level clone clusters; failure returns status 2.",
    )
    parser.add_argument(
        "--token-clone-full-files",
        action="store_true",
        help="Scan selected files without inherited masks or max-size limits; include CUDA.",
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
    if args.token_clone_full_files and not args.with_token_clones:
        parser.error("--token-clone-full-files requires --with-token-clones")
    try:
        abstraction_options = abstraction_settings(args)
    except ValueError as error:
        parser.error(str(error))

    if args.selftest:
        _selftest()
        return 0

    repo, targets, exclude = resolve_settings(args)
    exclude_set = set(exclude)

    profiles = [] if args.with_abstractions else None
    clusters, stats = scan_clusters(
        repo, targets, exclude_set, args.min_lines, args.batch_size, profiles=profiles
    )

    token_status = {"status": "not_requested"}
    if args.with_token_clones:
        token_status = {"status": "complete", "full_files": args.token_clone_full_files}
        try:
            clusters.extend(
                ingest_token_clones(
                    repo,
                    targets,
                    exclude=exclude_set,
                    full_files=args.token_clone_full_files,
                    metadata=token_status,
                )
            )
        except DuplicateAuditError as exc:
            token_status.update(status="failed", error=str(exc))
            print(f"ERROR: token-clone detector incomplete: {exc}", file=sys.stderr)
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
    data["detectors"] = {"token_clones": token_status}
    data["exclude"] = exclude
    data["complete"] = not stats["files_unparsable"] and token_status["status"] in {
        "complete",
        "not_requested",
    }
    add_abstraction_report(data, profiles, abstraction_options, repo)
    write_json_atomic(out_path, data)
    write_markdown(md_path, data)

    print(
        f"consolidation: {len(clusters)} clusters, "
        f"{data['summary']['total_redundant_bytes']} redundant bytes -> "
        f"{out_path} / {md_path}"
    )
    return 0 if data["complete"] else 2


if __name__ == "__main__":
    raise SystemExit(main())
