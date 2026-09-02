"""Fail-closed, language-neutral orchestration for explicit mutation campaigns.

The framework deliberately does not generate mutants. A campaign names small,
reviewable patch files and the exact tests that must detect them. Every baseline
and mutant runs in a disposable snapshot of the current worktree, never in the
shared checkout.
"""

from __future__ import annotations

import argparse
import ast
import hashlib
import json
import os
import re
import subprocess
import sys
import time
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import asdict, dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from audit.orchestrator.snapshot_worktree import isolated_snapshot
from conductor import mutation_testing_support as _support
from conductor.mutation_scope import (
    CampaignError,
    TestFileScope,
    _python_test_nodeids,
    _require_mapping,
    _require_string_list,
    _safe_relative_path,
    _test_scopes_payload,
)
from conductor.mutation_scope import (
    _load_test_scopes as _load_test_scopes,  # noqa: PLC0414
)
from conductor.mutation_scope import _require_string as _require_string  # noqa: PLC0414
from conductor.mutation_value import (
    ValueAnalysisSpec,
    ValueEvidenceError,
    analyze_test_value,
    collect_pytest_junit_batch,
    load_value_analysis,
    value_inspection_payload,
)

RECEIPT_SCHEMA = "llm.mutation-testing.receipt.v3"
LEGACY_RECEIPT_SCHEMA = "llm.mutation-testing.receipt.v2"
LEGACY_RECEIPT_ANCHOR_COMMIT = "61343f575215dd222a74fc2c060d0328692ded5e"
LEGACY_RECEIPT_ANCHOR_TREE = "b01877ba62c32445f7649450f9b395dd70de306a"
LEGACY_RECEIPT_PREFIX = "conductor/mutation_campaigns/receipts/"
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
CANONICAL_TEST_PATTERNS = _support.CANONICAL_TEST_PATTERNS
OUTPUT_TAIL_CHARS = 12_000
REPO_ROOT = Path(__file__).resolve().parents[1]
RUNNER_COMPONENT_PATHS = (
    "audit/orchestrator/snapshot_worktree.py",
    "conductor/mutation_scope.py",
    "conductor/mutation_testing.py",
    "conductor/mutation_testing_support.py",
    "conductor/mutation_value.py",
    "research/runtime/native/rust/research-runtime/src/mutation_evidence.rs",
    "research/runtime/native/rust/research-runtime/src/mutation_manifest.rs",
    "research/runtime/native/rust/research-runtime/src/mutation_receipt.rs",
)


@dataclass(frozen=True, slots=True)
class RankedTest:
    """One test selected for a campaign, ordered by contract importance."""

    rank: int
    nodeid: str
    contract: str
    rationale: str


@dataclass(frozen=True, slots=True)
class PlannedMutation:
    """A mutation design slot that contains no executable code change."""

    mutation_id: str
    target_path: str
    contract: str
    description: str
    expected_killers: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class Mutation:
    """One materialized, first-order mutation represented by a patch file."""

    mutation_id: str
    patch_file: Path
    patch_sha256: str
    allowed_paths: tuple[str, ...]
    expected_killers: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class Campaign:
    """Validated mutation campaign loaded from a machine-readable manifest."""

    manifest_path: Path
    manifest_sha256: str
    campaign_id: str
    title: str
    language: str
    mutation_engine: str
    expected_mutations: int
    source_sha256: Mapping[str, str]
    ranked_tests: tuple[RankedTest, ...]
    planned_mutations: tuple[PlannedMutation, ...]
    mutations: tuple[Mutation, ...]
    test_argv: tuple[str, ...]
    timeout_seconds: int
    blocked_process_substrings: tuple[str, ...]
    poll_seconds: int
    environment: Mapping[str, str]
    host_read_dependencies: tuple[str, ...]
    test_scopes: Mapping[str, TestFileScope] = field(default_factory=dict)
    # Optional per-symbol pins. A path here is checked symbol-by-symbol instead
    # of whole-file, so an edit outside the pinned functions does not drift the
    # campaign. Absent means the old whole-file behaviour, unchanged.
    source_symbols: Mapping[str, Mapping[str, str]] = field(default_factory=dict)
    value_analysis: ValueAnalysisSpec | None = None


@dataclass(frozen=True, slots=True)
class CommandResult:
    """Bounded subprocess evidence for a baseline or mutant test run."""

    returncode: int | None
    timed_out: bool
    duration_seconds: float
    stdout_tail: str
    stderr_tail: str

    def as_dict(self) -> dict[str, Any]:
        """Return a JSON-safe representation."""

        return {
            "returncode": self.returncode,
            "timed_out": self.timed_out,
            "duration_seconds": round(self.duration_seconds, 6),
            "stdout_tail": self.stdout_tail,
            "stderr_tail": self.stderr_tail,
        }


def _native_row(model: Any, row: Mapping[str, Any]) -> Any:
    """Materialize one normalized native row as its stable Python dataclass."""

    values = {
        name: row["id"] if name == "mutation_id" else row[name]
        for name in model.__dataclass_fields__
    }
    for name in ("allowed_paths", "expected_killers"):
        if name in values:
            values[name] = tuple(values[name])
    if "patch_file" in values:
        values["patch_file"] = Path(values["patch_file"])
    return model(**values)


def _native_json_call(
    function_name: str,
    payload: Mapping[str, Any],
) -> Any:
    """Call one native mutation primitive and decode its JSON result."""

    try:
        from conductor import _native as runtime

        operation = getattr(runtime, function_name)
        encoded = operation(
            json.dumps(payload, ensure_ascii=False, separators=(",", ":"))
        )
        return json.loads(encoded)
    except (ImportError, AttributeError, ValueError, json.JSONDecodeError) as exc:
        raise CampaignError(str(exc)) from exc


def _patch_paths(patch_path: Path) -> tuple[str, ...]:
    """Extract and validate repository-relative paths from a unified diff."""

    try:
        from conductor._native import mutation_patch_paths_native

        return tuple(mutation_patch_paths_native(str(patch_path)))
    except (ImportError, AttributeError, ValueError) as exc:
        raise CampaignError(str(exc)) from exc


def load_campaign(path: Path, *, repo_root: Path = REPO_ROOT) -> Campaign:
    """Load a mutation campaign through the native deterministic validator."""

    root = repo_root.resolve()
    manifest_path = path.resolve()
    try:
        relative_manifest = manifest_path.relative_to(root).as_posix()
    except ValueError as exc:
        raise CampaignError("campaign manifest must be inside the repository") from exc
    try:
        raw = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise CampaignError(f"cannot load campaign {manifest_path}: {exc}") from exc
    result = _native_json_call(
        "load_mutation_campaign_native",
        {"repo_root": str(root), "manifest_path": relative_manifest},
    )
    data = _require_mapping(result, "native mutation campaign")
    ranked_tests = tuple(_native_row(RankedTest, row) for row in data["ranked_tests"])
    planned_mutations = tuple(
        _native_row(PlannedMutation, row) for row in data["planned_mutations"]
    )
    mutations = tuple(_native_row(Mutation, row) for row in data["mutations"])
    test_scopes = {
        relative: TestFileScope(
            path=relative,
            mode=scope["mode"],
            inventory=scope["inventory"],
            nodeids=tuple(scope["nodeids"]),
        )
        for relative, scope in data["test_scopes"].items()
    }
    try:
        value_analysis = load_value_analysis(
            raw.get("value_analysis") if isinstance(raw, dict) else None,
            ranked_nodeids=[test.nodeid for test in ranked_tests],
            mutation_ids=[mutation.mutation_id for mutation in planned_mutations],
            source_paths=list(data["source_sha256"]),
        )
    except ValueEvidenceError as exc:
        raise CampaignError(f"invalid value_analysis: {exc}") from exc
    kwargs = {
        name: data[name]
        for name in Campaign.__dataclass_fields__
        if name in data and name not in {"value_analysis", "test_scopes"}
    }
    kwargs.update(
        manifest_path=root / data["manifest"],
        ranked_tests=ranked_tests,
        planned_mutations=planned_mutations,
        mutations=mutations,
        test_scopes=test_scopes,
        value_analysis=value_analysis,
    )
    for name in (
        "test_argv",
        "blocked_process_substrings",
        "host_read_dependencies",
    ):
        kwargs[name] = tuple(kwargs[name])
    return Campaign(**kwargs)


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _runner_components_sha256() -> dict[str, str]:
    """Bind every first-party module that can affect mutation execution."""

    root = Path(__file__).resolve().parents[1]
    components: dict[str, str] = {}
    for relative in RUNNER_COMPONENT_PATHS:
        path = root / relative
        if not path.is_file() or path.is_symlink():
            raise CampaignError(
                f"mutation runner component is missing or unsafe: {relative}"
            )
        components[relative] = _sha256(path)
    return components


RUNNER_LINEAGE_PATH = "conductor/mutation_runner_lineage.json"


def _lineage_accepts(recorded: object, repo_root: Path) -> bool:
    """Return whether a runner-component map is explicitly accepted."""

    try:
        from conductor._native import mutation_runner_lineage_accepts_native

        return bool(
            mutation_runner_lineage_accepts_native(
                json.dumps(
                    {"repo_root": str(repo_root.resolve()), "recorded": recorded},
                    ensure_ascii=False,
                    separators=(",", ":"),
                )
            )
        )
    except (ImportError, AttributeError, ValueError, TypeError):
        return False


def symbol_hashes(path: Path) -> dict[str, str]:
    """AST hash per top-level symbol, and per method, in a Python file.

    `ast.dump(..., include_attributes=False)` drops line and column numbers, so a
    comment, a docstring reflow, an import added above, or any edit to a NEIGHBOURING
    function leaves a symbol's hash untouched. Only a change to that symbol's own
    syntax tree moves it. That is the whole point: a campaign pins the functions its
    mutants actually touch, and edits elsewhere in the file stop voiding it.

    Raises rather than returning a partial map: a file that cannot be parsed must not
    silently produce "no drift".
    """
    try:
        tree = ast.parse(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, SyntaxError) as exc:
        raise CampaignError(f"cannot inventory symbols in {path}: {exc}") from exc

    hashes: dict[str, str] = {}

    def record(qualname: str, node: ast.AST) -> None:
        dumped = ast.dump(node, annotate_fields=True, include_attributes=False)
        hashes[qualname] = hashlib.sha256(dumped.encode("utf-8")).hexdigest()

    definition = (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)
    for node in tree.body:
        if not isinstance(node, definition):
            continue
        record(node.name, node)
        if isinstance(node, ast.ClassDef):
            for child in node.body:
                if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    record(f"{node.name}.{child.name}", child)
    return hashes


def source_drift(campaign: Campaign, root: Path) -> list[dict[str, Any]]:
    """Return native file drift plus CPython-AST symbol drift."""

    current = {
        relative: symbol_hashes(root / relative)
        for relative in campaign.source_symbols
        if (root / relative).is_file() and not (root / relative).is_symlink()
    }
    result = _native_json_call(
        "mutation_source_drift_native",
        {
            "repo_root": str(root.resolve()),
            "source_sha256": dict(campaign.source_sha256),
            "source_symbols": campaign.source_symbols,
            "symbol_hashes": current,
        },
    )
    if not isinstance(result, list) or not all(isinstance(row, dict) for row in result):
        raise CampaignError("native mutation source drift must be a list of objects")
    return result


def _ps_output() -> str:
    proc = subprocess.run(
        ["ps", "-eo", "pid=,args="],
        check=False,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise CampaignError(f"cannot inspect processes: {proc.stderr.strip()}")
    return proc.stdout


def blocking_processes(
    substrings: Iterable[str], *, process_output: str | None = None
) -> list[dict[str, Any]]:
    """Return live processes matching configured resource-blocking fragments."""

    fragments = tuple(substrings)
    if not fragments:
        return []
    rows: list[dict[str, Any]] = []
    output = process_output if process_output is not None else _ps_output()
    for raw in output.splitlines():
        stripped = raw.strip()
        if not stripped:
            continue
        pid_text, _, command = stripped.partition(" ")
        try:
            pid = int(pid_text)
        except ValueError:
            continue
        if pid == os.getpid():
            continue
        matched = [fragment for fragment in fragments if fragment in command]
        if matched:
            rows.append({"pid": pid, "command": command, "matched": matched})
    return rows


def inspect_campaign(
    campaign: Campaign, *, repo_root: Path = REPO_ROOT
) -> dict[str, Any]:
    """Return readiness, ranking, drift, and resource evidence without mutations."""

    drift = source_drift(campaign, repo_root)
    manifest_drift = (
        _sha256(campaign.manifest_path) if campaign.manifest_path.is_file() else None
    ) != campaign.manifest_sha256
    result = _native_json_call(
        "inspect_mutation_campaign_native",
        {
            "campaign": _native_campaign_contract(campaign, repo_root=repo_root),
            "source_drift": drift,
            "manifest_hash_drift": manifest_drift,
            "blocking_processes": blocking_processes(
                campaign.blocked_process_substrings
            ),
            "value_analysis": value_inspection_payload(campaign.value_analysis),
        },
    )
    return dict(_require_mapping(result, "native mutation campaign inspection"))


def _wait_for_idle(campaign: Campaign, wait_seconds: int) -> list[dict[str, Any]]:
    deadline = time.monotonic() + max(0, wait_seconds)
    while True:
        blockers = blocking_processes(campaign.blocked_process_substrings)
        if not blockers:
            return []
        if time.monotonic() >= deadline:
            return blockers
        remaining = max(0.0, deadline - time.monotonic())
        time.sleep(min(float(campaign.poll_seconds), remaining))


def _link_host_dependencies(
    campaign: Campaign, snapshot_root: Path, host_root: Path
) -> None:
    _support.link_host_dependencies(
        campaign,
        snapshot_root,
        host_root,
        materialize=_materialize,
        error_type=CampaignError,
    )


def _materialize(source: Path, destination: Path) -> None:
    """Place a host path inside the snapshot as real files, never a symlink.

    Receipt builders authenticate their inputs with repository-containment
    checks (``repo in target.resolve().parents``); a symlink resolves back to
    the host checkout and fails them even when the bytes are right. Hard links
    keep large read-only inputs (checkpoints, databases) free; a cross-device
    link error falls back to a byte copy.
    """
    _support.materialize(source, destination)


def _link_mutation_patches(
    campaign: Campaign, snapshot_root: Path, host_root: Path
) -> None:
    """Copy reviewed patch artifacts into snapshots without staging them."""

    _support.link_mutation_patches(
        campaign,
        snapshot_root,
        host_root,
        sha256=_sha256,
        error_type=CampaignError,
    )


_BARE_INTERPRETERS = frozenset({"python", "python3"})


def _pin_interpreter(argv: Sequence[str]) -> list[str]:
    """Resolve a bare ``python`` argv[0] to the runner's own interpreter.

    A bare name resolves through the invoking shell's PATH, so the same manifest
    ran under whichever venv the agent happened to have active (torch 2.12.1 in
    the project ``.venv`` vs 2.13.0 in ``~/venvs/llm``), and the receipt could not
    tell. Evidence must bind the interpreter the runner itself was started with.
    """
    return _support.pin_interpreter(
        argv, bare_interpreters=_BARE_INTERPRETERS, executable=sys.executable
    )


def _torch_version(interpreter: str) -> str | None:
    """Best-effort torch version of ``interpreter`` for receipt provenance."""

    return _support.torch_version(interpreter)


def _run_command(
    argv: Sequence[str],
    *,
    cwd: Path,
    timeout_seconds: int,
    environment: Mapping[str, str],
) -> CommandResult:
    return _support.run_command(
        argv,
        cwd=cwd,
        timeout_seconds=timeout_seconds,
        environment=environment,
        pin_argv=_pin_interpreter,
        result_factory=CommandResult,
        output_tail_chars=OUTPUT_TAIL_CHARS,
    )


def _run_campaign_command(
    campaign: Campaign,
    *,
    snapshot_root: Path,
    report_name: str,
) -> tuple[CommandResult, Mapping[str, Any] | None]:
    """Run one batch, adding per-test evidence with no test-mutant Cartesian loop."""

    if campaign.value_analysis is None:
        return (
            _run_command(
                campaign.test_argv,
                cwd=snapshot_root,
                timeout_seconds=campaign.timeout_seconds,
                environment=campaign.environment,
            ),
            None,
        )
    try:
        return collect_pytest_junit_batch(
            argv=campaign.test_argv,
            report_path=snapshot_root / ".mutation-value" / f"{report_name}.xml",
            ranked_nodeids=[test.nodeid for test in campaign.ranked_tests],
            run_command=lambda argv: _run_command(
                argv,
                cwd=snapshot_root,
                timeout_seconds=campaign.timeout_seconds,
                environment=campaign.environment,
            ),
        )
    except ValueEvidenceError as exc:
        raise CampaignError(f"cannot instrument value analysis: {exc}") from exc


def _apply_mutation(mutation: Mutation, snapshot_root: Path) -> None:
    _support.apply_mutation(
        mutation, snapshot_root, sha256=_sha256, error_type=CampaignError
    )


def _atomic_json(path: Path, payload: Mapping[str, Any]) -> None:
    _support.atomic_json(path, payload)


def _default_receipt_path(campaign: Campaign, repo_root: Path) -> Path:
    stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    return (
        repo_root
        / "research/reports/mutation_testing"
        / f"{campaign.campaign_id}_{stamp}.json"
    )


def run_campaign(
    campaign: Campaign,
    *,
    allow_mutations: bool,
    wait_seconds: int = 0,
    receipt_path: Path | None = None,
    mutation_ids: Sequence[str] | None = None,
    repo_root: Path = REPO_ROOT,
) -> dict[str, Any]:
    """Run a ready campaign in disposable snapshots and write a JSON receipt."""

    inspection = inspect_campaign(campaign, repo_root=repo_root)
    if inspection["status"] != "READY":
        raise CampaignError(
            "campaign is NOT_READY: " + "; ".join(inspection["readiness_reasons"])
        )
    if not allow_mutations:
        raise CampaignError("refusing mutation run without --allow-mutations")
    selected = _select_mutations(campaign, mutation_ids)
    blockers = _wait_for_idle(campaign, wait_seconds)
    if blockers:
        detail = ", ".join(f"pid={row['pid']}" for row in blockers)
        raise CampaignError(f"resource gate is BUSY after wait: {detail}")

    runner_components = _runner_components_sha256()
    receipt: dict[str, Any] = {
        "schema_version": RECEIPT_SCHEMA,
        "campaign_id": campaign.campaign_id,
        "manifest": campaign.manifest_path.relative_to(repo_root).as_posix(),
        "manifest_sha256": campaign.manifest_sha256,
        "runner_sha256": runner_components["conductor/mutation_testing.py"],
        "runner_components_sha256": runner_components,
        "language": campaign.language,
        "mutation_engine": campaign.mutation_engine,
        "generated_at": datetime.now(UTC).isoformat(),
        "status": "RUNNING",
        "source_sha256": dict(campaign.source_sha256),
        "source_symbols": {k: dict(v) for k, v in campaign.source_symbols.items()},
        "test_scopes": _test_scopes_payload(campaign),
        "test_argv": list(campaign.test_argv),
        "interpreter": _pin_interpreter(campaign.test_argv)[0],
        "torch_version": _torch_version(_pin_interpreter(campaign.test_argv)[0]),
        "expected_campaign_mutations": campaign.expected_mutations,
        "selected_mutations": [mutation.mutation_id for mutation in selected],
        "complete_campaign": len(selected) == campaign.expected_mutations,
        "baseline": None,
        "mutants": [],
        "mutation_score": None,
        "test_value": None,
    }
    if receipt_path is None:
        output_path = _default_receipt_path(campaign, repo_root)
    else:
        output_path = (
            receipt_path if receipt_path.is_absolute() else repo_root / receipt_path
        )
        output_path = output_path.resolve()
    try:
        receipt_relative = output_path.relative_to(repo_root.resolve()).as_posix()
    except ValueError as exc:
        raise CampaignError("receipt path must be inside the repository") from exc

    try:
        baseline_reports: list[Mapping[str, Any]] = []
        baseline_results: list[dict[str, Any]] = []
        repetitions = (
            campaign.value_analysis.baseline_repetitions
            if campaign.value_analysis is not None
            else 1
        )
        for repetition in range(1, repetitions + 1):
            with isolated_snapshot(repo_root) as snapshot:
                if drift := source_drift(campaign, snapshot.worktree):
                    raise CampaignError(f"snapshot source hashes drifted: {drift}")
                _link_mutation_patches(campaign, snapshot.worktree, repo_root)
                _link_host_dependencies(campaign, snapshot.worktree, repo_root)
                baseline, report = _run_campaign_command(
                    campaign,
                    snapshot_root=snapshot.worktree,
                    report_name=f"baseline-{repetition}",
                )
            baseline_results.append(baseline.as_dict())
            if report is not None:
                baseline_reports.append(report)
            if baseline.timed_out or baseline.returncode != 0:
                receipt["baseline"] = baseline_results[0]
                receipt["baseline_repetitions"] = baseline_results
                receipt["status"] = "BASELINE_FAILED"
                _atomic_json(output_path, receipt)
                raise CampaignError(f"unmutated baseline failed; receipt={output_path}")
        receipt["baseline"] = baseline_results[0]
        if campaign.value_analysis is not None:
            receipt["baseline_repetitions"] = baseline_results

        mutant_reports: dict[str, Mapping[str, Any]] = {}
        for mutation in selected:
            with isolated_snapshot(repo_root) as snapshot:
                if drift := source_drift(campaign, snapshot.worktree):
                    raise CampaignError(f"snapshot source hashes drifted: {drift}")
                _link_mutation_patches(campaign, snapshot.worktree, repo_root)
                _link_host_dependencies(campaign, snapshot.worktree, repo_root)
                _apply_mutation(mutation, snapshot.worktree)
                result, report = _run_campaign_command(
                    campaign,
                    snapshot_root=snapshot.worktree,
                    report_name=f"mutant-{len(receipt['mutants']) + 1}",
                )
            outcome = (
                "TIMED_OUT"
                if result.timed_out
                else "SURVIVED"
                if result.returncode == 0
                else "KILLED"
            )
            row: dict[str, Any] = {
                "id": mutation.mutation_id,
                "patch_sha256": mutation.patch_sha256,
                "allowed_paths": list(mutation.allowed_paths),
                "expected_killers": list(mutation.expected_killers),
                "outcome": outcome,
                "test_result": result.as_dict(),
            }
            if report is not None:
                row["test_attribution"] = report
                mutant_reports[mutation.mutation_id] = report
            receipt["mutants"].append(row)
            _atomic_json(output_path, receipt)
    except CampaignError as exc:
        if receipt["status"] == "RUNNING":
            receipt["status"] = "ERROR"
            receipt["error"] = str(exc)
            _atomic_json(output_path, receipt)
        raise
    except Exception as exc:
        receipt["status"] = "ERROR"
        receipt["error"] = f"{type(exc).__name__}: {exc}"
        _atomic_json(output_path, receipt)
        raise CampaignError(
            f"mutation campaign crashed; receipt={output_path}: {exc}"
        ) from exc

    killed = sum(row["outcome"] == "KILLED" for row in receipt["mutants"])
    survived = sum(row["outcome"] == "SURVIVED" for row in receipt["mutants"])
    timed_out = sum(row["outcome"] == "TIMED_OUT" for row in receipt["mutants"])
    denominator = killed + survived
    receipt["mutation_score"] = killed / denominator if denominator else None
    receipt["survivors"] = [
        row["id"] for row in receipt["mutants"] if row["outcome"] == "SURVIVED"
    ]
    receipt["classification_required"] = list(receipt["survivors"])
    mutation_status = (
        "PASS"
        if killed == len(selected) and not survived and not timed_out
        else "FAIL"
        if survived
        else "ERROR"
    )
    if campaign.value_analysis is not None:
        receipt["test_value"] = analyze_test_value(
            campaign.value_analysis,
            baseline_reports=baseline_reports,
            mutant_reports=mutant_reports,
            mutant_outcomes={row["id"]: row["outcome"] for row in receipt["mutants"]},
        )
    receipt["status"] = (
        mutation_status
        if receipt["test_value"] is None
        or receipt["test_value"].get("status") == "PASS"
        else "FAIL"
    )
    receipt["receipt_path"] = receipt_relative
    _atomic_json(output_path, receipt)
    return receipt


def _select_mutations(
    campaign: Campaign, mutation_ids: Sequence[str] | None
) -> tuple[Mutation, ...]:
    if mutation_ids is None:
        return campaign.mutations
    requested = tuple(mutation_ids)
    if not requested:
        raise CampaignError("at least one --mutation id is required")
    if len(set(requested)) != len(requested):
        raise CampaignError(f"duplicate --mutation ids: {requested}")
    by_id = {mutation.mutation_id: mutation for mutation in campaign.mutations}
    unknown = sorted(set(requested) - set(by_id))
    if unknown:
        raise CampaignError(f"unknown mutation ids: {unknown}")
    return tuple(by_id[mutation_id] for mutation_id in requested)


def _load_registry(path: Path, repo_root: Path) -> Mapping[str, Any]:
    root = repo_root.resolve()
    try:
        relative = path.resolve().relative_to(root).as_posix()
    except ValueError as exc:
        raise CampaignError("mutation registry must be inside the repository") from exc
    return _require_mapping(
        _native_json_call(
            "load_mutation_registry_native",
            {
                "repo_root": str(root),
                "registry_path": relative,
                "canonical_test_patterns": list(CANONICAL_TEST_PATTERNS),
            },
        ),
        "registry",
    )


def _native_campaign_contract(
    campaign: Campaign,
    *,
    repo_root: Path,
) -> dict[str, Any]:
    """Serialize the stable campaign contract consumed by native validation."""

    payload = asdict(campaign)
    payload["manifest"] = campaign.manifest_path.relative_to(
        repo_root.resolve()
    ).as_posix()
    del payload["manifest_path"]
    payload["test_scopes"] = _test_scopes_payload(campaign)
    payload["ranked_test_paths"] = [
        test.nodeid.split("::", 1)[0] for test in campaign.ranked_tests
    ]
    for row in payload["planned_mutations"]:
        row["id"] = row.pop("mutation_id")
    for row in payload["mutations"]:
        row["id"] = row.pop("mutation_id")
        row["patch_file"] = str(row["patch_file"])
    payload["value_analysis_payload"] = None
    payload["value_analysis"] = (
        None
        if campaign.value_analysis is None
        else {
            "expected_nodeids": [test.nodeid for test in campaign.ranked_tests],
            "expected_repetitions": campaign.value_analysis.baseline_repetitions,
        }
    )
    payload["source_drifted"] = bool(source_drift(campaign, repo_root))
    return payload


def _native_runner_payload() -> dict[str, Any]:
    try:
        components = _runner_components_sha256()
    except CampaignError as exc:
        return {"components": None, "error": str(exc), "mutation_testing_sha256": None}
    return {
        "components": components,
        "error": None,
        "mutation_testing_sha256": components["conductor/mutation_testing.py"],
    }


def _native_anchor_payload(repo_root: Path, anchor_repo: Path | None) -> dict[str, str]:
    return {
        "repo": str((anchor_repo or repo_root).resolve()),
        "commit": LEGACY_RECEIPT_ANCHOR_COMMIT,
        "tree": LEGACY_RECEIPT_ANCHOR_TREE,
        "receipt_prefix": LEGACY_RECEIPT_PREFIX,
    }


def _receipt_errors(
    receipt: Mapping[str, Any],
    campaign: Campaign,
    repo_root: Path,
    receipt_path: Path | None = None,
    receipt_bytes: bytes | None = None,
    anchor_repo: Path | None = None,
) -> list[str]:
    result = _native_json_call(
        "validate_mutation_receipt_native",
        {
            "repo_root": str(repo_root.resolve()),
            "anchor_repo": str((anchor_repo or repo_root).resolve()),
            "campaign": _native_campaign_contract(campaign, repo_root=repo_root),
            "receipt": dict(receipt),
            "receipt_path": None if receipt_path is None else str(receipt_path),
            "receipt_bytes": (None if receipt_bytes is None else list(receipt_bytes)),
            "runner": _native_runner_payload(),
            "anchor": _native_anchor_payload(repo_root, anchor_repo),
        },
    )
    if not isinstance(result, list) or not all(
        isinstance(error, str) for error in result
    ):
        raise CampaignError("native mutation receipt must be a list of strings")
    return result


_ORIGINAL_LOAD_CAMPAIGN = load_campaign


def _native_plan_paths(
    plan: Mapping[str, Any], key: str, label: str
) -> tuple[str, ...]:
    return tuple(
        _safe_relative_path(path, label)
        for path in _require_string_list(plan.get(key), f"native plan.{key}")
    )


def _native_verification_request(
    registry_relative: str,
    paths: Sequence[str],
    *,
    repo_root: Path,
    anchor_repo: Path | None,
    plan: Mapping[str, Any],
) -> dict[str, Any]:
    root = repo_root.resolve()
    normalized = tuple(_safe_relative_path(path, "candidate path") for path in paths)
    symbol_paths = _native_plan_paths(plan, "symbol_paths", "native plan symbol path")
    directories = _native_plan_paths(plan, "receipt_directories", "receipt directory")
    python_scope_paths = _native_plan_paths(
        {"python_scope_paths": plan.get("python_scope_paths", [])},
        "python_scope_paths",
        "native plan Python scope path",
    )
    return {
        "repo_root": str(root),
        "registry_path": registry_relative,
        "canonical_test_patterns": list(CANONICAL_TEST_PATTERNS),
        "receipt_directories": list(directories),
        "candidate_paths": list(normalized),
        "symbol_hashes": {path: symbol_hashes(root / path) for path in symbol_paths},
        "python_test_nodeids": {
            path: list(_python_test_nodeids(root / path, path))
            for path in python_scope_paths
        },
        "campaigns_override": None,
        "runner": _native_runner_payload(),
        "anchor": _native_anchor_payload(repo_root, anchor_repo),
    }


def verify_evidence(
    registry_path: Path,
    paths: Sequence[str],
    *,
    repo_root: Path = REPO_ROOT,
    anchor_repo: Path | None = None,
) -> dict[str, Any]:
    """Require current native full-campaign PASS evidence for changed tests."""

    try:
        from conductor._native import (
            plan_mutation_evidence_native,
            verify_mutation_evidence_native,
        )
    except (ImportError, AttributeError) as exc:
        raise CampaignError(str(exc)) from exc
    root = repo_root.resolve()
    try:
        registry_relative = registry_path.resolve().relative_to(root).as_posix()
    except ValueError as exc:
        raise CampaignError("mutation registry must be inside the repository") from exc
    campaigns_override: list[dict[str, Any]] | None = None
    if load_campaign is _ORIGINAL_LOAD_CAMPAIGN:
        try:
            plan_encoded = plan_mutation_evidence_native(
                json.dumps(
                    {"repo_root": str(root), "registry_path": registry_relative},
                    ensure_ascii=False,
                    separators=(",", ":"),
                )
            )
            plan = _require_mapping(
                json.loads(plan_encoded), "native mutation evidence plan"
            )
        except (ValueError, json.JSONDecodeError) as exc:
            raise CampaignError(str(exc)) from exc
    else:
        registry = _load_registry(registry_path, root)
        campaigns_override = [
            _native_campaign_contract(campaign, repo_root=root)
            for campaign in _registry_campaigns(
                registry_path, repo_root=root, strict=False
            )
        ]
        plan = {
            "symbol_paths": [],
            "receipt_directories": registry.get("receipt_directories"),
        }
    request = _native_verification_request(
        registry_relative,
        paths,
        repo_root=repo_root,
        anchor_repo=anchor_repo,
        plan=plan,
    )
    request["campaigns_override"] = campaigns_override
    try:
        encoded = verify_mutation_evidence_native(
            json.dumps(request, ensure_ascii=False, separators=(",", ":"))
        )
        result = json.loads(encoded)
    except (ValueError, json.JSONDecodeError) as exc:
        raise CampaignError(str(exc)) from exc
    return dict(_require_mapping(result, "native mutation evidence"))


def _json_print(payload: Mapping[str, Any]) -> None:
    print(json.dumps(payload, indent=2, sort_keys=True))


def _registry_campaigns(
    registry_path: Path, *, repo_root: Path, strict: bool = True
) -> list[Campaign]:
    """Every campaign the registry lists, loaded and validated.

    With ``strict=False`` a manifest that fails to load is skipped instead of aborting the
    whole load. That cannot weaken the evidence gate — a campaign that does not load also
    matches no candidate path, so the tests it claimed stay blocked — and it keeps one
    lane's half-written manifest from refusing evidence for every other lane. Re-pinning
    stays strict: silently skipping a campaign there would drop it from the repair pass.
    """
    payload = _load_registry(registry_path, repo_root)
    campaigns: list[Campaign] = []
    for index, row in enumerate(payload["campaigns"]):
        manifest = repo_root / _safe_relative_path(
            row["manifest"], f"registry.campaigns[{index}].manifest"
        )
        try:
            campaigns.append(load_campaign(manifest, repo_root=repo_root))
        except CampaignError:
            if strict:
                raise
    return campaigns


def repin_campaigns(
    registry_path: Path,
    *,
    campaign_ids: Sequence[str] | None = None,
    run: bool = False,
    allow_mutations: bool = False,
    repo_root: Path = REPO_ROOT,
) -> dict[str, Any]:
    """Re-pin every drifted campaign and, with `run`, regenerate its receipt.

    A re-pin without a re-run produces a manifest whose recorded digests describe the
    current tree while its receipt describes an older one -- a receipt that was never
    regenerated, which is not evidence. So `--run` is what makes this command finish
    the job, and it refuses without explicit mutation authorization.

    Without `--run` this is a REPORT and changes nothing on disk, so it is always safe
    to ask "what is drifted?".
    """
    if run and not allow_mutations:
        raise CampaignError(
            "repin --run executes mutants and requires --allow-mutations"
        )
    campaigns = _registry_campaigns(registry_path, repo_root=repo_root)
    wanted = set(campaign_ids or [])
    drifted: list[dict[str, Any]] = []
    for campaign in campaigns:
        if wanted and campaign.campaign_id not in wanted:
            continue
        drift = source_drift(campaign, repo_root)
        if drift:
            drifted.append({"campaign_id": campaign.campaign_id, "drift": drift})

    if not drifted:
        return {"status": "CLEAN", "drifted": [], "rerun": []}
    if not run:
        return {
            "status": "DRIFTED",
            "drifted": drifted,
            "rerun": [],
            "hint": "re-run with --run --allow-mutations to re-pin AND regenerate receipts",
        }

    selected = {
        campaign.campaign_id: campaign
        for campaign in campaigns
        if any(row["campaign_id"] == campaign.campaign_id for row in drifted)
    }
    symbol_paths = {
        relative
        for campaign in selected.values()
        for relative in campaign.source_symbols
    }
    source_paths = {
        relative
        for campaign in selected.values()
        for relative in campaign.source_sha256
        if relative not in campaign.source_symbols
    }
    try:
        from conductor._native import plan_mutation_repin_native

        plans = plan_mutation_repin_native(
            str(repo_root.resolve()),
            [
                (
                    campaign.manifest_path.relative_to(repo_root.resolve()).as_posix(),
                    dict(campaign.source_sha256),
                    {
                        path: dict(pins)
                        for path, pins in campaign.source_symbols.items()
                    },
                )
                for campaign in selected.values()
            ],
            {
                path: _sha256(repo_root / path)
                for path in source_paths
                if (repo_root / path).is_file()
            },
            {path: symbol_hashes(repo_root / path) for path in symbol_paths},
        )
    except (ImportError, AttributeError, ValueError) as exc:
        raise CampaignError(str(exc)) from exc
    updated = dict(plans)
    rerun: list[dict[str, Any]] = []
    for entry in drifted:
        campaign_id = str(entry["campaign_id"])
        manifest = selected[campaign_id].manifest_path
        relative = manifest.relative_to(repo_root.resolve()).as_posix()
        manifest.write_text(updated[relative], encoding="utf-8")
        refreshed = load_campaign(manifest, repo_root=repo_root)
        remaining = source_drift(refreshed, repo_root)
        if remaining:
            rerun.append(
                {
                    "campaign_id": campaign_id,
                    "status": "REFUSED",
                    "reason": "still drifted after re-pin",
                    "drift": remaining,
                }
            )
            continue
        result = run_campaign(refreshed, allow_mutations=True)
        rerun.append({"campaign_id": campaign_id, "status": result["status"]})
    failed = [item for item in rerun if item["status"] != "PASS"]
    return {
        "status": "REPINNED" if not failed else "FAILED",
        "drifted": drifted,
        "rerun": rerun,
    }


def main(argv: list[str] | None = None) -> int:
    """CLI entry point for inspection and explicitly authorized execution."""

    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    inspect_parser = subparsers.add_parser(
        "inspect", help="validate and inspect a campaign without mutations"
    )
    inspect_parser.add_argument("campaign", type=Path)
    run_parser = subparsers.add_parser(
        "run", help="run a ready campaign in isolated snapshots"
    )
    run_parser.add_argument("campaign", type=Path)
    run_parser.add_argument("--allow-mutations", action="store_true")
    run_parser.add_argument("--wait-seconds", type=int, default=0)
    run_parser.add_argument("--receipt", type=Path)
    run_parser.add_argument(
        "--mutation",
        action="append",
        dest="mutation_ids",
        help="run only this mutant id (repeatable); still runs the baseline first",
    )
    repin_parser = subparsers.add_parser(
        "repin",
        help="re-pin drifted campaigns and, with --run, regenerate their receipts",
    )
    repin_parser.add_argument(
        "--registry",
        type=Path,
        default=Path("conductor/mutation_campaigns/registry.json"),
    )
    repin_parser.add_argument(
        "--campaign",
        action="append",
        dest="campaign_ids",
        help="limit to this campaign id (repeatable); default is every drifted campaign",
    )
    repin_parser.add_argument(
        "--run",
        action="store_true",
        help=(
            "after re-pinning, RE-RUN each campaign so its receipt is regenerated. "
            "Without this the command reports what drifted and changes nothing: a "
            "re-pin alone produces a manifest whose receipt was never regenerated, "
            "which is not evidence."
        ),
    )
    repin_parser.add_argument(
        "--allow-mutations",
        action="store_true",
        help="required with --run; mutant execution is explicitly authorized, never implied",
    )
    verify_parser = subparsers.add_parser(
        "verify-evidence",
        help="require current full-campaign PASS receipts for changed tests",
    )
    verify_parser.add_argument(
        "--registry",
        type=Path,
        default=Path("conductor/mutation_campaigns/registry.json"),
    )
    verify_parser.add_argument("paths", nargs="*")
    args = parser.parse_args(argv)
    try:
        if args.command == "repin":
            result = repin_campaigns(
                args.registry,
                campaign_ids=args.campaign_ids,
                run=args.run,
                allow_mutations=args.allow_mutations,
            )
            _json_print(result)
            return 0 if result["status"] in ("CLEAN", "REPINNED") else 1
        if args.command == "verify-evidence":
            result = verify_evidence(args.registry, args.paths)
            _json_print(result)
            return 0 if result["status"] == "PASS" else 5
        campaign = load_campaign(args.campaign)
        if args.command == "inspect":
            result = inspect_campaign(campaign)
            _json_print(result)
            return 0 if result["status"] == "READY" else 3
        result = run_campaign(
            campaign,
            allow_mutations=args.allow_mutations,
            wait_seconds=args.wait_seconds,
            receipt_path=args.receipt,
            mutation_ids=args.mutation_ids,
        )
        _json_print(result)
        return 0 if result["status"] == "PASS" else 1
    except CampaignError as exc:
        _json_print({"status": "REFUSED", "error": str(exc)})
        return 4


if __name__ == "__main__":
    raise SystemExit(main())
