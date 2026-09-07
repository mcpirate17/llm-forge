"""Deterministic ROI oracle.

The autonomous loop must KNOW whether a fix round actually reduced bloat — it cannot
trust an LLM's self-report ("I split the god file"). This module computes a stable,
dependency-light violation vector from the same thresholds CLAUDE.md / GLOBAL_DEV_PROMPT.md
enforce, so the loop can compare before/after and stop when nothing improves.

Metrics (all whole-repo over configured targets, excluding tests and allowlisted debt):
  god_files      supported source files > 1250 lines
  god_functions  Python functions > 100 lines (via ast)
  lint           ruff F-codes (unused imports/vars/redefinition)  [if ruff present]
  dead_code      vulture high-confidence findings                 [if vulture present]
  duplicates     conductor/check_duplicate_function_bodies.py count
  reuse_*        high-confidence removable duplicate value
  silent_*       high-signal swallowed exception handlers

`total` is the weighted scalar the loop watches. Counts come from the same tools the
existing conductor/full_repo_audit gates use — this is the loop's measurement, not a
replacement for those detailed reports.
"""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict, dataclass, field
from pathlib import Path

from conductor.audit_root import resolve_audit_root
from conductor.duplicate_audit_config import DEFAULT_SOURCE_DIRS
from conductor.reuse import _support as files
from conductor.reuse import _support as process
from conductor.reuse import (
    consolidation,
    file_families,
    graph_index,
    repo_evidence,
    roi,
)
from conductor.reuse import core as slop_core

DEFAULT_OUT_REL = Path("tasks/audit/measure.json")
GOD_FILE_LINES = 1250
GOD_FUNC_LINES = 100
CODE_EXTENSIONS = {
    ".c",
    ".cc",
    ".cpp",
    ".cu",
    ".h",
    ".hpp",
    ".js",
    ".jsx",
    ".py",
    ".pxd",
    ".pyx",
    ".rs",
    ".ts",
    ".tsx",
}

# Weights make the scalar reflect impact, not raw count: a god file dwarfs one lint hit.
WEIGHTS = {
    "god_files": 10,
    "god_functions": 3,
    "reuse_clusters": 8,
    "reuse_value": 1,
    "file_families": 6,
    "file_family_value": 1,
    "duplicates": 4,
    "silent_fallbacks": 8,
    "dead_code": 2,
    "lint": 1,
}


@dataclass
class Metrics:
    god_files: int = 0
    god_functions: int = 0
    lint: int = 0
    dead_code: int = 0
    duplicates: int = 0
    reuse_clusters: int = 0
    reuse_value: int = 0
    file_families: int = 0
    file_family_value: int = 0
    silent_fallbacks: int = 0
    native_reuse: int = 0
    production_loc: int = 0
    test_loc: int = 0
    evidence_incomplete: int = 0
    roi_snapshot: dict = field(default_factory=dict)
    detail: dict = field(default_factory=dict)

    @property
    def total(self) -> int:
        return sum(WEIGHTS[k] * getattr(self, k) for k in WEIGHTS)

    def as_dict(self) -> dict:
        d = asdict(self)
        d["total"] = self.total
        d["legacy_total"] = self.total
        return d


def _iter_py(targets: list[Path], exclude: set[str]) -> list[Path]:
    skip = exclude | consolidation.HARD_SKIP_PARTS
    return files.iter_files(targets, suffixes={".py"}, skip_parts=skip)


def _iter_code(targets: list[Path], exclude: set[str]) -> list[Path]:
    skip = exclude | consolidation.HARD_SKIP_PARTS
    return files.iter_files(targets, suffixes=CODE_EXTENSIONS, skip_parts=skip)


def _guardrail_allowlist(repo: Path | None) -> tuple[set[str], set[str]]:
    if repo is None:
        return set(), set()
    path = repo / "conductor" / "guardrail_allowlist.json"
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return set(), set()
    return set(data.get("god_files", [])), set(data.get("god_functions", []))


def _god_counts(
    files: list[Path], repo: Path | None = None
) -> tuple[int, int, list[str], list[str]]:
    gf, gfn, gf_list, gfn_list, _ = _detector_scan(files, repo)
    return gf, gfn, gf_list, gfn_list


def _ruff_findings(repo: Path, rel_targets: list[str]) -> list[dict]:
    # --frozen: never let `uv run` rewrite uv.lock (it would dirty a tracked file every
    # time we measure, poisoning the clean-tree guard and fix commits).
    # --extra component-fab-dev: ruff/vulture are declared in this optional extra, not the
    # default dev group, so a plain `uv run` never installs them into .venv and only
    # resolves them via PATH -- which fails with "Failed to spawn: ruff" when the active
    # venv isn't on PATH. The extra is already resolved in uv.lock, so this syncs the
    # pinned tool into .venv with no lock rewrite and no network.
    cmd = [
        "uv",
        "run",
        "--frozen",
        "--extra",
        "component-fab-dev",
        "ruff",
        "check",
        "--select",
        "F",
        "--output-format",
        "json",
        *rel_targets,
    ]
    proc = process.run_tool_capture(
        cmd,
        repo,
        timeout=300,
        tool_name="ruff detector",
        ok_returncodes={0, 1},
        not_found_detail="uv was not found",
    )
    try:
        data = json.loads(proc.stdout or "[]")
    except json.JSONDecodeError as exc:
        raise RuntimeError("ruff detector returned invalid JSON") from exc
    return data if isinstance(data, list) else []


def _ruff_f_count(repo: Path, rel_targets: list[str]) -> int:
    return len(_ruff_findings(repo, rel_targets))


def _vulture_lines(repo: Path, rel_targets: list[str]) -> list[str]:
    # Same --extra component-fab-dev rationale as _ruff_findings: vulture is in that
    # optional extra, so a plain `uv run` won't install it into .venv.
    cmd = [
        "uv",
        "run",
        "--frozen",
        "--extra",
        "component-fab-dev",
        "vulture",
        "--min-confidence",
        "80",
        *rel_targets,
    ]
    # Vulture uses exit 3 when it successfully finds dead code.
    proc = process.run_tool_capture(
        cmd,
        repo,
        timeout=300,
        tool_name="vulture detector",
        ok_returncodes={0, 3},
        not_found_detail="uv was not found",
    )
    return [line for line in proc.stdout.splitlines() if line.strip()]


def _vulture_count(repo: Path, rel_targets: list[str]) -> int:
    return len(_vulture_lines(repo, rel_targets))


def _detector_scan(
    paths: list[Path], repo: Path | None
) -> tuple[int, int, list[str], list[str], list[dict]]:
    """God-file, god-function and silent-fallback findings for `paths`.

    Refuses on a file CPython cannot parse. The AST detectors saw nothing in it,
    and reporting that silence alongside the files they did read would hand the
    caller a clean scan of a repository this never finished reading -- the file
    most likely to be broken being exactly the one that would go unexamined.
    """
    allowed_files, allowed_functions = _guardrail_allowlist(repo)
    *findings, unparsable = slop_core.audit_detector_scan(
        paths=[str(path) for path in paths],
        repo=str(repo) if repo is not None else None,
        allowed_files=sorted(allowed_files),
        allowed_functions=sorted(allowed_functions),
        god_file_lines=GOD_FILE_LINES,
        god_func_lines=GOD_FUNC_LINES,
    )
    if unparsable:
        raise ValueError(
            "detector scan cannot parse "
            + ", ".join(unparsable)
            + " -- fix the file or drop it from the scan; the audit will not "
            "report a clean result for code it could not read"
        )
    god_file_count, god_func_count, god_files, god_functions, fallbacks = findings
    return god_file_count, god_func_count, god_files, god_functions, fallbacks


def _scan_file_families(
    repo: Path, targets: list[str], exclude: set[str], settings: dict | None
) -> tuple[list[file_families.FileFamily], dict]:
    settings = settings or {}
    if not settings.get("enabled", True):
        return [], {
            "files_profiled": 0,
            "files_unparsable": 0,
            "lsh_candidate_pairs": 0,
            "pairs_scored": 0,
            "families": 0,
            "estimated_net_deleted_loc": 0,
        }
    return file_families.scan_file_families(
        repo,
        targets,
        exclude,
        min_similarity=float(
            settings.get("min_similarity", file_families.DEFAULT_MIN_SIMILARITY)
        ),
        min_file_loc=int(
            settings.get("min_file_loc", file_families.DEFAULT_MIN_FILE_LOC)
        ),
        min_shared_features=int(
            settings.get(
                "min_shared_features", file_families.DEFAULT_MIN_SHARED_FEATURES
            )
        ),
        min_net_deleted_loc=int(
            settings.get(
                "min_net_deleted_loc", file_families.DEFAULT_MIN_NET_DELETED_LOC
            )
        ),
        max_family_size=int(
            settings.get("max_family_size", file_families.DEFAULT_MAX_FAMILY_SIZE)
        ),
        max_candidates=int(
            settings.get("max_candidates", file_families.DEFAULT_MAX_CANDIDATES)
        ),
        permutations=int(
            settings.get("minhash_permutations", file_families.DEFAULT_NUM_PERMUTATIONS)
        ),
        band_size=int(settings.get("lsh_band_size", file_families.DEFAULT_BAND_SIZE)),
    )


_INVENTORY_CATEGORIES = (
    "dead_code",
    "god_files",
    "god_functions",
    "duplication",
    "file_families",
    "silent_fallbacks",
    "perf_hotspots",
    "imports_deps",
    "compliance",
    "native_reuse",
    "test_contracts",
)


def _inventory_candidates(
    repo: Path,
    god_files: list[str],
    god_functions: list[str],
    clusters: list[consolidation.Cluster],
    families: list[file_families.FileFamily],
    vulture: list[str],
    ruff: list[dict],
    fallbacks: list[dict],
    token_clones: list[dict],
    native_reuse: list[dict],
    dependencies: list[dict],
    compliance: list[dict],
    contract_candidates: list[dict],
    limit: int,
    test_limit: int,
) -> dict[str, list[dict]]:
    by_category: dict[str, list[dict]] = {
        category: [] for category in _INVENTORY_CATEGORIES
    }
    native_categories = slop_core.audit_inventory_candidates(
        repo=str(repo),
        god_file_lines=GOD_FILE_LINES,
        god_func_lines=GOD_FUNC_LINES,
        god_files=god_files,
        god_functions=god_functions,
        clusters=clusters,
        families=families,
        vulture=vulture,
        ruff=ruff,
        fallbacks=fallbacks,
        token_clones=token_clones,
        native_reuse=native_reuse,
        dependencies=dependencies,
        limit=limit,
    )
    for category, candidates in native_categories.items():
        by_category[category] = candidates
    by_category["compliance"] = compliance
    by_category["test_contracts"] = contract_candidates
    for category in ("compliance", "test_contracts"):
        candidates = by_category[category]
        candidates.sort(
            key=lambda item: (item["value"], item["confidence"]), reverse=True
        )
        by_category[category] = candidates[
            : test_limit if category == "test_contracts" else limit
        ]
    return by_category


def _inventory_metrics(
    god_file_count: int,
    god_function_count: int,
    god_files: list[str],
    god_functions: list[str],
    clusters: list[consolidation.Cluster],
    families: list[file_families.FileFamily],
    ruff: list[dict],
    vulture: list[str],
    fallbacks: list[dict],
    duplicate_count: int,
    native_reuse_count: int,
    roi_snapshot: dict,
) -> Metrics:
    actionable = [cluster for cluster in clusters if cluster.disposition == "auto"]
    return Metrics(
        god_files=god_file_count,
        god_functions=god_function_count,
        lint=len(ruff),
        dead_code=len(vulture),
        duplicates=duplicate_count,
        reuse_clusters=len(actionable),
        reuse_value=sum(cluster.value_score for cluster in actionable),
        file_families=len(families),
        file_family_value=sum(family.estimated_net_deleted_loc for family in families),
        silent_fallbacks=sum(item["confidence"] >= 0.85 for item in fallbacks),
        native_reuse=native_reuse_count,
        production_loc=int(roi_snapshot.get("production_loc", 0)),
        test_loc=int(roi_snapshot.get("test_loc", 0)),
        evidence_incomplete=len(roi_snapshot.get("incomplete_reasons", [])),
        roi_snapshot=roi_snapshot,
        detail={
            "god_files": god_files[:50],
            "god_functions": god_functions[:80],
            "reuse_redundant_units": sum(cluster.est_bytes for cluster in actionable),
            "file_families": [
                file_families.family_to_dict(family) for family in families[:20]
            ],
        },
    )


def _incomplete_sources(graph_status, duplicate_scan: dict) -> list[str]:
    incomplete = []
    if not graph_status.complete:
        incomplete.append(f"graph:{graph_status.reason}")
    if duplicate_scan["status"] != "complete":
        incomplete.append(f"duplicates:{duplicate_scan['reason']}")
    return incomplete


def measure(
    repo: Path,
    targets: list[str],
    exclude: set[str],
    family_settings: dict | None = None,
) -> Metrics:
    target_paths = [repo / t for t in targets] if targets else [repo]
    index = graph_index.GraphIndex(repo)
    code_files = _iter_code(target_paths, exclude)
    gf, gfn, gf_list, gfn_list, fallbacks = _detector_scan(code_files, repo)
    rel = [t for t in targets] or ["."]
    clusters, _ = consolidation.scan_clusters(repo, targets, exclude)
    families, _ = _scan_file_families(repo, targets, exclude, family_settings)
    actionable = [cluster for cluster in clusters if cluster.disposition == "auto"]
    redundant = sum(c.est_bytes for c in actionable)
    strong_fallbacks = [item for item in fallbacks if item["confidence"] >= 0.85]
    token_clones, duplicate_scan = repo_evidence.jscpd_candidates(
        repo, "tasks/audit/duplication-jscpd/jscpd-report.json"
    )
    try:
        native = repo_evidence.native_reuse_candidates(index)
    except Exception:  # noqa: BLE001 - optional native reuse must fail soft
        native = []
    incomplete = []
    status = index.status(targets)
    if not status.complete:
        incomplete.append(f"graph:{status.reason}")
    if duplicate_scan["status"] != "complete":
        incomplete.append(f"duplicates:{duplicate_scan['reason']}")
    roi_snapshot = roi.snapshot(
        repo,
        targets,
        exclude,
        index,
        duplicate_lines=sum(item.get("duplicate_lines", 0) for item in token_clones),
        native_candidates=len(native),
        incomplete_sources=incomplete,
    )
    m = Metrics(
        god_files=gf,
        god_functions=gfn,
        lint=_ruff_f_count(repo, rel),
        dead_code=_vulture_count(repo, rel),
        duplicates=duplicate_scan["clusters"],
        reuse_clusters=len(actionable),
        reuse_value=sum(cluster.value_score for cluster in actionable),
        file_families=len(families),
        file_family_value=sum(family.estimated_net_deleted_loc for family in families),
        silent_fallbacks=len(strong_fallbacks),
        native_reuse=len(native),
        production_loc=roi_snapshot["production_loc"],
        test_loc=roi_snapshot["test_loc"],
        evidence_incomplete=len(roi_snapshot["incomplete_reasons"]),
        roi_snapshot=roi_snapshot,
        detail={
            "god_files": gf_list[:50],
            "god_functions": gfn_list[:80],
            "reuse_redundant_units": redundant,
            "file_families": [
                file_families.family_to_dict(family) for family in families[:20]
            ],
        },
    )
    return m


# --------------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Weighted repository health metrics over the source tree."
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
    parser.add_argument("--exclude", nargs="+", default=None)
    parser.add_argument("--out", default=None, help="Output JSON path.")
    return parser


def main(argv: list[str] | None = None) -> int:
    """Measure the tree and write the metrics JSON.

    Replaces ``audit/orchestrator/orchestrate.py cmd_measure``. The audit loop drove
    ``measure`` from a config file; the analyzer needs an entry point of its own now
    that the loop is gone, or nothing reaches the four native detectors behind it.
    """
    args = build_parser().parse_args(argv)
    repo = resolve_audit_root(args.repo)
    targets = args.targets if args.targets else list(DEFAULT_SOURCE_DIRS)
    exclude = set(args.exclude) if args.exclude else set()

    payload = measure(repo, targets, exclude).as_dict()
    out_path = Path(args.out) if args.out else repo / DEFAULT_OUT_REL
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    print(
        f"measure: total={payload['total']} god_files={payload['god_files']} "
        f"god_functions={payload['god_functions']} families={payload['file_families']} "
        f"silent_fallbacks={payload['silent_fallbacks']} -> {out_path}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
