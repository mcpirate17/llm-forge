"""Repeatable, broad reuse inventory using the existing native-backed detectors."""

from __future__ import annotations

import argparse
import json
import os
import signal
import sqlite3
import subprocess
import sys
import threading
import time
from dataclasses import asdict
from datetime import datetime, timezone
from pathlib import Path
from typing import TextIO

from conductor.atomic_json import atomic_write, write_json_atomic
from conductor.reuse.consolidation import (
    _native_decide,
    abstraction_cli_options,
    abstraction_settings,
    add_abstraction_arguments,
)
from conductor.reuse.graph_index import GraphIndex
from conductor.reuse.file_families import (
    RetirementPlan,
    load_retirement_plans,
    retirement_report,
)
from conductor.reuse.repo_evidence import native_reuse_candidates

EXCLUDE = (
    ".git",
    ".venv",
    ".run",
    ".forge",
    ".code-review-graph",
    "__pycache__",
    "node_modules",
    "build",
    "dist",
    "target",
    "reports",
    "tmp",
    "cache",
    "caches",
    "catalog",
    "data",
    "corpus",
    "checkpoints",
    "perf_artifacts",
    "third_party",
    "vendor",
    "external",
    "snapshots",
)
SOURCE_SUFFIXES = {
    ".py",
    ".rs",
    ".c",
    ".cpp",
    ".cc",
    ".cxx",
    ".h",
    ".hpp",
    ".cu",
    ".cuh",
    ".js",
    ".jsx",
    ".ts",
    ".tsx",
}
OUTPUT_NAMES = (
    "scope.json",
    "summary.json",
    "native-reuse.json",
    "retirement-plan-input.json",
    "retirement-plans.json",
    "opportunities.json",
    "opportunities.md",
    *(
        f"{stage}.{suffix}"
        for stage in ("functions", "families")
        for suffix in ("json", "md", "log")
    ),
)


def validate_inputs(
    repo: Path, out: Path, targets: list[str] | None, plans: Path | None
) -> None:
    """Reject invalid explicit scopes and input clobbering before any output write."""
    if not repo.is_dir():
        raise ValueError(f"repository root is not a directory: {repo}")
    for target in targets or ():
        resolved = (repo / target).resolve(strict=True)
        if not resolved.is_relative_to(repo) or not resolved.is_dir():
            raise ValueError(
                f"scan target must be a directory inside the repository: {target}"
            )
    if plans is not None:
        source = plans.resolve(strict=True)
        for name in OUTPUT_NAMES:
            output = (out / name).resolve()
            if source == output or (output.exists() and source.samefile(output)):
                raise ValueError(
                    f"retirement plan input collides with report output: {name}"
                )


def discover_scope(repo: Path, exclude: set[str]) -> dict:
    """Find owned source roots and explicitly recover nested test directories."""
    result = subprocess.run(
        [
            "git",
            "-C",
            str(repo),
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
        capture_output=True,
        check=True,
        timeout=60,
    )
    paths = sorted({Path(path) for path in result.stdout.decode().split("\0") if path})
    source = [path for path in paths if path.suffix.lower() in SOURCE_SUFFIXES]
    selected = [path for path in source if not (set(path.parts) & exclude)]
    selected_set = set(selected)
    targets: set[str] = set()
    for path in selected:
        targets.add(path.parts[0] if len(path.parts) > 1 else ".")
        for index, part in enumerate(path.parts[:-1]):
            if part in {"test", "tests"}:
                targets.add(Path(*path.parts[: index + 1]).as_posix())
    if not targets:
        raise ValueError(
            "no owned source files found; supply --targets for a non-Git source tree"
        )
    return {
        "repo": str(repo),
        "targets": sorted(targets),
        "exclude": sorted(exclude),
        "source_files_discovered": len(source),
        "eligible_source_files": len(selected),
        "eligible_python_files": sum(path.suffix == ".py" for path in selected),
        "excluded_source_files": [
            path.as_posix() for path in source if path not in selected_set
        ],
        "selection": "Git tracked and non-ignored untracked source roots; recursive live scan, including explicitly rooted nested tests",
    }


def detector_commands(
    repo: Path,
    out: Path,
    targets: list[str],
    exclude: list[str],
    quick: bool,
    retirement_plans: Path | None = None,
    abstraction_options: dict | None = None,
) -> list[tuple[str, list[str]]]:
    common = ["--repo", str(repo), "--targets", *targets, "--exclude", *exclude]
    abstraction_args = abstraction_cli_options(abstraction_options or {})
    functions = [
        sys.executable,
        "-m",
        "conductor.reuse.consolidation",
        *common,
        "--min-lines",
        "4",
        *(
            []
            if quick
            else [
                "--with-token-clones",
                "--token-clone-full-files",
                "--with-abstractions",
                *abstraction_args,
            ]
        ),
        "--out",
        str(out / "functions.json"),
        "--md",
        str(out / "functions.md"),
    ]
    families = [
        sys.executable,
        "-m",
        "conductor.reuse.file_families",
        *common,
        "--min-similarity",
        "0.70",
        "--min-file-loc",
        "30",
        "--min-shared-features",
        "8",
        "--min-net-deleted-loc",
        "20",
        "--max-family-size",
        "32",
        "--max-candidates",
        "10000",
        "--out",
        str(out / "families.json"),
        "--md",
        str(out / "families.md"),
        *(["--retirement-plans", str(retirement_plans)] if retirement_plans else []),
    ]
    return (
        [("functions", functions)]
        if quick
        else [("functions", functions), ("families", families)]
    )


def write_retirement_report(
    plans: list[RetirementPlan], stages: dict, out: Path
) -> dict:
    """Link only fresh source-scan evidence; keep plans independent of scan caps."""
    evidence = []
    for name, key in (("functions", "clusters"), ("families", "families")):
        stage = stages.get(name, {})
        if not stage.get("fresh_report") or stage.get("status") not in {
            "ok",
            "incomplete",
        }:
            continue
        payload = json.loads((out / f"{name}.json").read_text(encoding="utf-8"))
        for row in payload.get(key, []):
            files = row.get("files") or [site["file"] for site in row.get("sites", [])]
            evidence.append({"id": f"{name}:{row['id']}", "files": files})
    report = retirement_report(plans, evidence)
    write_json_atomic(out / "retirement-plans.json", report)
    return report["summary"]


def candidate_sites(functions: dict) -> list[dict]:
    """Deduplicate only callable sites participating in review evidence."""
    sites = [
        site
        for row in functions.get("clusters", [])
        if row.get("kind") in {"exact", "near"}
        for site in row.get("sites", [])
    ]
    abstractions = functions.get("abstractions", {})
    profiles = abstractions.get("profiles", [])
    for edge in abstractions.get("edges", []):
        sites.extend(profiles[edge[side]] for side in ("left", "right"))
    identities = {
        (site["file"], site["name"], site["line_start"])
        for site in sites
        if site.get("file") and site.get("name") and site.get("line_start", 0) > 0
    }
    return [
        {"file": file, "name": name, "line_start": start, "caller_count": None}
        for file, name, start in sorted(identities)
    ]


def caller_hints(repo: Path, functions: dict) -> dict:
    """Expose observed caller counts only across a stable, wholly current graph."""
    hints = {
        "complete": False,
        "sites": candidate_sites(functions),
        "graph_status_before": {},
        "graph_status_after": {},
    }
    if not hints["sites"]:
        return {**hints, "reason": "no callable candidate identities"}
    try:
        index = GraphIndex(repo)
        before = index.status(["."]).as_dict()
        hints["graph_status_before"] = before
        if not before.get("complete"):
            return {**hints, "reason": "whole-repository graph is incomplete"}
        rows = index.native_reuse_rows(caller_limit=None)
        after = index.status(["."]).as_dict()
        hints["graph_status_after"] = after
        if not after.get("complete") or before != after:
            return {**hints, "reason": "graph freshness changed during caller query"}
        counts: dict[tuple, list[int]] = {}
        for row in rows:
            key = (row["file"], row["name"], row["line_start"])
            counts.setdefault(key, []).append(row["caller_count"])
        for site in hints["sites"]:
            matches = counts.get((site["file"], site["name"], site["line_start"]), [])
            if len(matches) == 1:
                site["caller_count"] = matches[0]
            else:
                site["reason"] = (
                    "ambiguous graph identity" if matches else "no exact graph identity"
                )
        hints.update(
            complete=True, reason="observed distinct graph callers; review hints only"
        )
    except (OSError, ValueError, RuntimeError, sqlite3.Error) as error:
        hints["reason"] = f"caller query failed: {error}"
    return hints


def write_opportunity_report(
    stages: dict, summary: dict, out: Path, repo: Path
) -> dict:
    """Unify fresh evidence while retaining per-detector limits and failure status."""
    payload = {
        name: {} for name in ("functions", "families", "native_reuse", "retirement")
    }
    for name, filename in (
        ("functions", "functions.json"),
        ("families", "families.json"),
        ("native_reuse", "native-reuse.json"),
    ):
        stage = stages.get(name, {})
        usable = name == "native_reuse" or stage.get("status") in {"ok", "incomplete"}
        if stage.get("fresh_report") and usable:
            payload[name] = json.loads((out / filename).read_text(encoding="utf-8"))
    if "retirement_summary" in summary:
        payload["retirement"] = json.loads(
            (out / "retirement-plans.json").read_text(encoding="utf-8")
        )
    payload["coverage"] = {
        key: summary[key]
        for key in (
            "source_scans_complete",
            "graph_evidence_complete",
            "execution_complete",
        )
    }
    payload["coverage"]["detectors"] = stages
    payload["coverage"]["abstractions"] = (
        payload["functions"]
        .get("abstractions", {})
        .get(
            "coverage",
            {"status": "not_requested" if payload["functions"] else "unavailable"},
        )
    )
    payload["caller_hints"] = caller_hints(repo, payload["functions"])
    report = _native_decide("opportunities", payload)
    markdown = _native_decide("opportunity_markdown", report)
    write_json_atomic(out / "opportunities.json", report)
    with atomic_write(out / "opportunities.md") as handle:
        handle.write(markdown)
    return report["summary"]


def execute_detector(
    argv: list[str], log: TextIO, timeout: int, *, environment: dict[str, str] | None
) -> int:
    """Own the detector process group so timeout/cancellation stops Node too."""

    def terminate(signum: int, _frame: object) -> None:
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        raise SystemExit(128 + signum)

    # The CLI runs on the main thread. Library callers on worker threads retain
    # their process-wide signal policy; timeout cleanup still applies there.
    own_handler = threading.current_thread() is threading.main_thread()
    previous = signal.signal(signal.SIGTERM, terminate) if own_handler else None
    try:
        with subprocess.Popen(
            argv,
            stdout=log,
            stderr=subprocess.STDOUT,
            env=environment,
            start_new_session=os.name == "posix",
        ) as process:
            try:
                return process.wait(timeout=timeout)
            except (subprocess.TimeoutExpired, KeyboardInterrupt, SystemExit):
                if os.name == "posix":
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass  # The owned process group already exited.
                else:
                    process.kill()
                process.wait()
                raise
    finally:
        if own_handler:
            signal.signal(signal.SIGTERM, previous)


def run_detector(
    name: str, argv: list[str], out: Path, timeout: int, node_heap_mb: int = 8192
) -> dict:
    started = time.monotonic()
    report = out / f"{name}.json"
    # Preserve previous evidence but never count it as a result of this invocation.
    previous_mtime = report.stat().st_mtime_ns if report.exists() else None
    status = {"argv": argv, "log": str(out / f"{name}.log")}
    environment = None
    if "--with-token-clones" in argv:
        environment = dict(os.environ)
        environment.setdefault("NODE_OPTIONS", f"--max-old-space-size={node_heap_mb}")
        status["node_options"] = environment["NODE_OPTIONS"]
    with (out / f"{name}.log").open("w", encoding="utf-8") as log:
        try:
            returncode = execute_detector(argv, log, timeout, environment=environment)
            status.update(
                returncode=returncode,
                status="ok" if returncode == 0 else "failed",
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            print(f"{name}: {error}", file=log)
            status.update(status="failed", error=str(error))
    status["elapsed_seconds"] = time.monotonic() - started
    fresh = report.exists() and report.stat().st_mtime_ns != previous_mtime
    status["fresh_report"] = fresh
    if fresh:
        try:
            payload = json.loads(report.read_text(encoding="utf-8"))
            status["summary"] = payload["summary"]
            if payload.get("complete") is False or payload["summary"].get(
                "files_unparsable", 0
            ):
                status["status"] = "incomplete"
        except (OSError, ValueError, KeyError, TypeError) as error:
            status.update(status="failed", error=f"invalid report: {error}")
    elif status["status"] == "ok":
        status.update(status="failed", error="detector did not publish a new report")
    print(f"{name}: {status['status']} ({status['elapsed_seconds']:.2f}s)", flush=True)
    return status


def native_inventory(repo: Path, targets: list[str], out: Path) -> dict:
    started = time.monotonic()
    try:
        index = GraphIndex(repo)
        # Reuse may point to a native implementation outside the cloned source
        # roots. The API queries the whole graph, so freshness must cover it too.
        graph_status = index.status(["."]).as_dict()
        if not graph_status.get("available"):
            raise ValueError(
                f"native graph unavailable: {graph_status.get('reason', 'missing graph')}"
            )
        rows = native_reuse_candidates(index, limit=100000)
        report = {
            "graph_status": graph_status,
            "scope": "whole-repository graph",
            "requested_clone_targets": targets,
            "candidates": rows,
            "evidence_kind": "normalized-name matches; requires source review and profiling, not proof of Rust benefit",
            "candidate_limit": 100000,
            "limit_reached": len(rows) == 100000,
        }
        write_json_atomic(out / "native-reuse.json", report)
        freshness = "complete" if graph_status.get("complete") else "incomplete"
        print(
            f"native reuse: {len(rows)} name-match candidates; graph evidence {freshness}",
            flush=True,
        )
        return {
            "status": "ok",
            "fresh_report": True,
            "candidates": len(rows),
            "graph_status": graph_status,
            "limit_reached": len(rows) == 100000,
            "elapsed_seconds": time.monotonic() - started,
        }
    except (OSError, ValueError, RuntimeError, sqlite3.Error) as error:
        failure = {
            "status": "failed",
            "error": str(error),
            "elapsed_seconds": time.monotonic() - started,
        }
        write_json_atomic(out / "native-reuse.json", failure)
        failure["fresh_report"] = True
        print(f"native reuse: {error}", file=sys.stderr)
        return failure


def build_parser() -> argparse.ArgumentParser:
    """Describe the existing detector sweep and optional explicit retirement plans."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--out-dir", type=Path)
    parser.add_argument("--targets", nargs="+")
    parser.add_argument("--exclude", nargs="+", default=list(EXCLUDE))
    parser.add_argument(
        "--retirement-plans",
        type=Path,
        help=(
            "Optional explicit file-retirement JSON; report reviewed/planned net file "
            "reduction without treating detector similarity as deletion evidence"
        ),
    )
    parser.add_argument(
        "--quick",
        action="store_true",
        help="Run normalized function clones and native-name inventory; skip token blocks and whole-file pair analysis",
    )
    add_abstraction_arguments(parser)
    parser.add_argument(
        "--timeout",
        type=int,
        default=900,
        help="Maximum seconds per clone detector (default: 900)",
    )
    parser.add_argument(
        "--node-heap-mb",
        type=int,
        default=8192,
        help="Broad token-stage Node heap limit; existing NODE_OPTIONS takes precedence (default: 8192)",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    if args.node_heap_mb < 128:
        parser.error("--node-heap-mb must be at least 128")
    try:
        abstraction_options = abstraction_settings(args)
        plans = (
            load_retirement_plans(args.retirement_plans)
            if args.retirement_plans
            else None
        )
    except (OSError, ValueError) as error:
        parser.error(str(error))
    try:
        repo = args.repo.resolve(strict=True)
        out = (args.out_dir or repo / ".forge/reports/reuse").resolve()
        validate_inputs(repo, out, args.targets, args.retirement_plans)
    except (OSError, ValueError) as error:
        parser.error(str(error))
    out.mkdir(parents=True, exist_ok=True)
    plan_input = out / "retirement-plan-input.json" if plans is not None else None
    if plan_input is not None:
        write_json_atomic(
            plan_input, {"schema_version": 1, "plans": [asdict(plan) for plan in plans]}
        )
    scope = (
        discover_scope(repo, set(args.exclude))
        if args.targets is None
        else {
            "repo": str(repo),
            "targets": args.targets,
            "exclude": args.exclude,
            "selection": "explicit roots",
        }
    )
    write_json_atomic(out / "scope.json", scope)
    commands = detector_commands(
        repo,
        out,
        scope["targets"],
        scope["exclude"],
        args.quick,
        plan_input,
        abstraction_options,
    )
    stages = {
        name: run_detector(name, command, out, args.timeout, args.node_heap_mb)
        for name, command in commands
    }
    stages["native_reuse"] = native_inventory(repo, scope["targets"], out)
    execution_complete = all(stage["status"] == "ok" for stage in stages.values())
    graph_complete = bool(
        stages["native_reuse"].get("graph_status", {}).get("complete", False)
    )
    summary = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "repo": str(repo),
        "mode": "quick" if args.quick else "full",
        "detectors": stages,
        "complete": execution_complete and graph_complete,
        "execution_complete": execution_complete,
        "source_scans_complete": all(
            stages[name]["status"] == "ok" for name, _ in commands
        ),
        "graph_evidence_complete": graph_complete,
        "review_only": True,
        "limits": [
            "Normalized clone matches omit decorator/import context; preserve scientific mechanisms and controls.",
            "Structural pairs are review-only; uncapped approximate retrieval still has unknown recall.",
            "est_bytes/tokens are AST structural units, not measured bytes saved; family LOC savings are estimates.",
            "Native-name evidence has separate graph freshness; it does not prove runtime equivalence or speedup.",
            "Inputs are live, not an atomic source snapshot. Full candidates are retained; inspect explicit cap fields.",
        ],
    }
    if plans is not None:
        summary["retirement_plans"] = str(args.retirement_plans.resolve())
        summary["retirement_summary"] = write_retirement_report(plans, stages, out)
    summary["opportunity_summary"] = write_opportunity_report(
        stages, summary, out, repo
    )
    write_json_atomic(out / "summary.json", summary)
    print(f"Reports: {out}")
    return 0 if execution_complete else 2


if __name__ == "__main__":
    raise SystemExit(main())
