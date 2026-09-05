"""Fail-closed, language-neutral orchestration for explicit mutation campaigns.

The framework deliberately does not generate mutants. A campaign names small,
reviewable patch files and the exact tests that must detect them. Every baseline
and mutant runs in a disposable snapshot of the current worktree, never in the
shared checkout.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from collections.abc import Callable, Iterable, Mapping, Sequence
from dataclasses import asdict
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from conductor import mutation_testing_support as _support
from conductor.mutation_campaign_model import (  # noqa: F401
    CANONICAL_TEST_PATTERNS,
    LEGACY_RECEIPT_ANCHOR_COMMIT,
    LEGACY_RECEIPT_ANCHOR_TREE,
    LEGACY_RECEIPT_PREFIX,
    LEGACY_RECEIPT_SCHEMA,
    OUTPUT_TAIL_CHARS,
    RECEIPT_SCHEMA,
    REPO_ROOT,
    RUNNER_COMPONENT_PATHS,
    RUNNER_LINEAGE_PATH,
    SHA256_RE,
    Campaign,
    CommandResult,
    Mutation,
    PlannedMutation,
    RankedTest,
    _campaign_receipts,
    _lineage_accepts,
    _load_registry,
    _native_json_call,
    _native_row,
    _patch_paths,
    _runner_components_sha256,
    _sha256,
    load_campaign,
    source_drift,
    symbol_hashes,
)
from conductor.mutation_scope import (
    CampaignError,
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
    ValueEvidenceError,
    analyze_test_value,
    cargo_attribution_supported,
    collect_cargo_libtest_batch,
    collect_ctest_junit_batch,
    collect_pytest_junit_batch,
    ctest_attribution_supported,
    ctest_junit_path,
    pytest_attribution_supported,
    value_inspection_payload,
)
from conductor.snapshot_worktree import isolated_snapshot

# Re-exported because callers and tests reach through this module as the framework's
# single entry point; the definitions live in mutation_scope / mutation_value.
from conductor.mutation_scope import TestFileScope as TestFileScope  # noqa: E402,PLC0414
from conductor.mutation_value import (  # noqa: E402,PLC0414
    ValueAnalysisSpec as ValueAnalysisSpec,
)
from conductor.mutation_value import (  # noqa: E402,PLC0414
    load_value_analysis as load_value_analysis,
)


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
    stdout_sink: Callable[[str], None] | None = None,
) -> CommandResult:
    return _support.run_command(
        argv,
        cwd=cwd,
        timeout_seconds=timeout_seconds,
        environment=environment,
        pin_argv=_pin_interpreter,
        result_factory=CommandResult,
        output_tail_chars=OUTPUT_TAIL_CHARS,
        stdout_sink=stdout_sink,
    )


KILLER_FAILURE_OUTCOMES = frozenset({"FAILED", "ERROR"})


def killer_verdict(
    mutation: Mutation,
    report: Mapping[str, Any] | None,
    outcome: str,
) -> dict[str, Any]:
    """Adjudicate whether a killed mutant died to the tests its contract named.

    A non-zero exit only says *something* failed. `expected_killers` is the
    campaign's prediction about *which* test catches the defect, and a mutant
    killed by anything else is evidence the contract does not hold.
    """

    declared = list(mutation.expected_killers)
    if outcome != "KILLED":
        return {"status": "NOT_APPLICABLE", "declared": declared}
    if report is None:
        return {
            "status": "UNAVAILABLE",
            "declared": declared,
            "reason": "campaign batch carries no per-test attribution",
        }
    if report.get("status") != "COMPLETE":
        # The batch *can* be attributed -- it produced a report -- and this run
        # still did not say which test killed the mutant. That is not the same
        # as a harness with no attribution at all: the usual cause is a mutant
        # that broke the build or the collection, so the non-zero exit that
        # reads as KILLED came from the toolchain rather than from a test. A
        # kill nobody can attribute is not evidence, so it is a refusal.
        return {
            "status": "UNATTRIBUTED",
            "declared": declared,
            "reason": f"attribution is {report.get('status')}",
            "missing_nodeids": list(report.get("missing_nodeids") or ()),
            "error": report.get("error"),
        }
    tests = report.get("tests", {})
    observed = sorted(
        nodeid
        for nodeid, row in tests.items()
        if row.get("outcome") in KILLER_FAILURE_OUTCOMES
    )
    matched = sorted(set(declared) & set(observed))
    verdict = {
        "status": "CONFIRMED" if matched else "MISATTRIBUTED",
        "declared": declared,
        "observed_failures": observed,
        "matched": matched,
        "unobservable": sorted(set(declared) - set(tests)),
        "collateral": sorted(set(observed) - set(declared)),
    }
    # A batch that runs a whole test binary -- every cargo campaign -- also fails
    # tests outside the ranked set. Those cannot be adjudicated (they have no
    # contract), but their COUNT is what separates a precise mutant from a blunt
    # one that breaks the build and would be "killed" by any test at all. Carry
    # them so the receipt records the bluntness instead of discarding it.
    unranked = report.get("unranked_failures") or []
    if unranked:
        verdict["unranked_failures"] = list(unranked)
    return verdict


def killer_enforcement(mutants: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
    """Fold per-mutant verdicts into the campaign's contract-binding verdict.

    Three outcomes, and the middle one is the point. `ENFORCED` means every kill
    was traced to a test the mutant's contract named. `UNAVAILABLE` means the
    harness produces no per-test evidence at all -- a property of the batch that
    the campaign cannot fix, so it is reported and not punished. `REFUSED` means
    the campaign's own claims did not hold: a kill landed on the wrong test, or
    a batch that *can* attribute did not attribute this one, which is how a
    mutant that breaks the build or the collection reads as a kill.
    """

    def ids(status: str) -> list[str]:
        return [
            row["id"]
            for row in mutants
            if row["killer_attribution"]["status"] == status
        ]

    misattributed = ids("MISATTRIBUTED")
    unattributed = ids("UNAVAILABLE")
    unattributed_runs = ids("UNATTRIBUTED")
    return {
        "status": "REFUSED"
        if misattributed or unattributed_runs
        else "UNAVAILABLE"
        if unattributed
        else "ENFORCED",
        "misattributed": misattributed,
        "unattributed": unattributed,
        "unattributed_runs": unattributed_runs,
    }


def _run_campaign_command(
    campaign: Campaign,
    *,
    snapshot_root: Path,
    report_name: str,
) -> tuple[CommandResult, Mapping[str, Any] | None]:
    """Run one batch, adding per-test evidence with no test-mutant Cartesian loop."""

    ranked_nodeids = [test.nodeid for test in campaign.ranked_tests]
    if not pytest_attribution_supported(campaign.test_argv, ranked_nodeids):
        if campaign.value_analysis is not None:
            raise CampaignError(
                "value analysis needs a pytest batch whose ranked tests are "
                "Python nodeids and whose argv does not already set --junitxml"
            )
        if ctest_attribution_supported(ranked_nodeids):
            return collect_ctest_junit_batch(
                argv=campaign.test_argv,
                report_path=ctest_junit_path(snapshot_root),
                ranked_nodeids=ranked_nodeids,
                run_command=lambda argv: _run_command(
                    argv,
                    cwd=snapshot_root,
                    timeout_seconds=campaign.timeout_seconds,
                    environment=campaign.environment,
                ),
            )
        if cargo_attribution_supported(ranked_nodeids):
            return collect_cargo_libtest_batch(
                argv=campaign.test_argv,
                ranked_nodeids=ranked_nodeids,
                run_command=lambda argv, sink: _run_command(
                    argv,
                    cwd=snapshot_root,
                    timeout_seconds=campaign.timeout_seconds,
                    environment=campaign.environment,
                    stdout_sink=sink,
                ),
            )
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
            ranked_nodeids=ranked_nodeids,
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


def _utc_stamp() -> str:
    return datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")


def _default_receipt_path(campaign: Campaign, repo_root: Path) -> Path:
    return (
        repo_root
        / "research/reports/mutation_testing"
        / f"{campaign.campaign_id}_{_utc_stamp()}.json"
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
        "killer_enforcement": None,
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
            row["killer_attribution"] = killer_verdict(mutation, report, outcome)
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
    receipt["killer_enforcement"] = killer_enforcement(receipt["mutants"])
    mutation_status = (
        "PASS"
        if killed == len(selected) and not survived and not timed_out
        else "FAIL"
        if survived
        else "ERROR"
    )
    if receipt["killer_enforcement"]["status"] == "REFUSED":
        mutation_status = "FAIL"
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


def receipt_drift(
    campaigns: Sequence[Campaign],
    repo_root: Path,
    registry_path: Path,
    *,
    anchor_repo: Path | None = None,
) -> list[dict[str, Any]]:
    """Return the campaigns that hold no receipt the evidence gate would accept.

    `source_drift` compares a manifest's pins against the tree, so it is blind to
    everything that ages a RECEIPT rather than a manifest: a runner component moved
    (a receipt records the component hashes that produced it), or the manifest itself
    was repaired (a receipt pins `manifest_sha256`). `repin` reported CLEAN over
    exactly that evidence and left the gate to discover it later, under the misleading
    name "missing mutation receipt".

    The predicate is the gate's own -- `validate_mutation_receipt_native`, the same
    call `verify_evidence` makes -- so repin and the gate cannot drift into two
    different definitions of "acceptable". A campaign is stale only when NO receipt of
    its own is clean; one good receipt is evidence however many superseded ones sit
    beside it, and the errors reported are the closest miss, because that is the one
    worth reading.
    """

    index = _campaign_receipts(repo_root, registry_path)
    stale: list[dict[str, Any]] = []
    for campaign in campaigns:
        receipts = index.get(campaign.campaign_id, [])
        if not receipts:
            # No receipt at all is a missing-evidence failure the gate already names by
            # that name; re-running it here would hide the hole behind a repin.
            continue
        closest: tuple[Path, list[str]] | None = None
        for path, payload in receipts:
            errors = _receipt_errors(
                payload,
                campaign,
                repo_root,
                receipt_path=path,
                receipt_bytes=path.read_bytes(),
                anchor_repo=anchor_repo,
            )
            if not errors:
                closest = None
                break
            if closest is None or len(errors) < len(closest[1]):
                closest = (path, errors)
        if closest is None:
            continue
        stale.append(
            {
                "campaign_id": campaign.campaign_id,
                "receipts": len(receipts),
                "closest_receipt": closest[0].relative_to(repo_root).as_posix(),
                "errors": closest[1],
            }
        )
    return stale


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


def _declared_campaign_id(manifest: Path) -> str | None:
    """The id a manifest claims, read without validating the rest of it.

    Selecting by id needs the id/path map before anything is validated, so this reads
    the one field and judges nothing else. A manifest too broken to parse claims no id,
    which is the honest answer: it can still be selected by name through the strict
    load, which will then say exactly what is wrong with it.
    """

    try:
        payload = json.loads(manifest.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        return None
    if not isinstance(payload, Mapping):
        return None
    campaign_id = payload.get("campaign_id")
    return campaign_id if isinstance(campaign_id, str) else None


def _registry_campaigns(
    registry_path: Path,
    *,
    repo_root: Path,
    strict: bool = True,
    campaign_ids: Sequence[str] | None = None,
) -> list[Campaign]:
    """Every campaign the registry lists, loaded and validated.

    With ``strict=False`` a manifest that fails to load is skipped instead of aborting the
    whole load. That cannot weaken the evidence gate — a campaign that does not load also
    matches no candidate path, so the tests it claimed stay blocked — and it keeps one
    lane's half-written manifest from refusing evidence for every other lane. Re-pinning
    stays strict: silently skipping a campaign there would drop it from the repair pass.

    ``campaign_ids`` narrows the strict load to the manifests a caller actually named.
    Loading all 400-odd of them to then discard all but one made every targeted repair
    hostage to an unrelated lane's half-written manifest: `repin --campaign mine` aborted
    on someone else's file, which is a refusal that teaches the wrong lesson. Selection
    still refuses an id that names nothing — that check runs against the ids declared on
    disk, so it keeps its meaning without validating every manifest.
    """
    payload = _load_registry(registry_path, repo_root)
    manifests = [
        repo_root
        / _safe_relative_path(row["manifest"], f"registry.campaigns[{index}].manifest")
        for index, row in enumerate(payload["campaigns"])
    ]
    if campaign_ids:
        wanted = set(campaign_ids)
        declared = {manifest: _declared_campaign_id(manifest) for manifest in manifests}
        if wanted <= {name for name in declared.values() if name}:
            # Narrow only when the on-disk ids provably cover everything asked for.
            # If they do not, fall through to the full strict load so the refusal for an
            # unknown id -- and any manifest whose id this cheap read could not see --
            # is still decided by the real loader rather than by this shortcut.
            manifests = [
                manifest for manifest in manifests if declared[manifest] in wanted
            ]
    campaigns: list[Campaign] = []
    for manifest in manifests:
        try:
            campaigns.append(load_campaign(manifest, repo_root=repo_root))
        except CampaignError:
            if strict:
                raise
    return campaigns


def _plan_repin(selected: Mapping[str, Campaign], repo_root: Path) -> dict[str, str]:
    return _support.plan_repin(
        selected, repo_root, _sha256, symbol_hashes, CampaignError
    )


def _repin_one_and_rerun(
    campaign_id: str,
    *,
    selected: dict[str, Campaign],
    by_id: dict[str, Campaign],
    updated: dict[str, str],
    receipts: Path,
    repo_root: Path,
) -> dict[str, Any]:
    """Write one campaign's re-pinned manifest, then regenerate its receipt.

    A campaign in `selected` had drifted source pins and gets the rewritten manifest;
    one that is not was pinned correctly all along and is here only because its receipt
    went stale under a runner change, so its manifest is left exactly as it is.
    """
    if campaign_id in selected:
        manifest = selected[campaign_id].manifest_path
        relative = manifest.relative_to(repo_root.resolve()).as_posix()
        manifest.write_text(updated[relative], encoding="utf-8")
        refreshed = load_campaign(manifest, repo_root=repo_root)
        remaining = source_drift(refreshed, repo_root)
        if remaining:
            return {
                "campaign_id": campaign_id,
                "status": "REFUSED",
                "reason": "still drifted after re-pin",
                "drift": remaining,
            }
    else:
        refreshed = by_id[campaign_id]
    receipt_path = receipts / f"{campaign_id}_{_utc_stamp()}.json"
    result = run_campaign(
        refreshed, allow_mutations=True, receipt_path=receipt_path, repo_root=repo_root
    )
    if not receipt_path.is_file():
        raise CampaignError(
            f"re-run of {campaign_id!r} reported {result['status']} but published "
            f"no receipt at {receipt_path}"
        )
    return {
        "campaign_id": campaign_id,
        "status": result["status"],
        "receipt": result["receipt_path"],
    }


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

    Two independent things go stale. A manifest's source pins drift when the code under
    test moves; a campaign's RECEIPT goes stale when the runner itself moves, because
    every receipt records the hashes of the runner components that produced it. Only
    the first used to be checked here, so a framework edit left the evidence set
    unusable while this command printed CLEAN.

    Without `--run` this is a REPORT and changes nothing on disk, so it is always safe
    to ask "what is drifted?".
    """
    if run and not allow_mutations:
        raise CampaignError(
            "repin --run executes mutants and requires --allow-mutations"
        )
    campaigns = _support.select_campaigns(
        _registry_campaigns(
            registry_path, repo_root=repo_root, campaign_ids=campaign_ids
        ),
        campaign_ids,
        CampaignError,
    )
    drifted: list[dict[str, Any]] = []
    for campaign in campaigns:
        drift = source_drift(campaign, repo_root)
        if drift:
            drifted.append({"campaign_id": campaign.campaign_id, "drift": drift})

    stale_receipts = receipt_drift(campaigns, repo_root, registry_path)

    if not drifted and not stale_receipts:
        return {"status": "CLEAN", "drifted": [], "stale_receipts": [], "rerun": []}
    if not run:
        return {
            "status": "DRIFTED",
            "drifted": drifted,
            "stale_receipts": stale_receipts,
            "rerun": [],
            "hint": "re-run with --run --allow-mutations to re-pin AND regenerate receipts",
        }

    by_id = {campaign.campaign_id: campaign for campaign in campaigns}
    repin_ids = [str(row["campaign_id"]) for row in drifted]
    # A stale receipt needs a re-run, not a re-pin: the manifest already describes the
    # tree correctly, so rewriting it would be a no-op that hides why the receipt died.
    rerun_only = [
        str(row["campaign_id"])
        for row in stale_receipts
        if str(row["campaign_id"]) not in set(repin_ids)
    ]
    selected = {campaign_id: by_id[campaign_id] for campaign_id in repin_ids}
    updated = _plan_repin(selected, repo_root) if selected else {}
    # A receipt published outside the registry's receipt directory is not evidence:
    # `research/reports/` is gitignored, so the default path this used to fall back on
    # dropped every regenerated receipt while reporting PASS.
    receipts = repo_root / _support.receipt_directory(
        _load_registry(registry_path, repo_root), CampaignError
    )
    rerun = [
        _repin_one_and_rerun(
            campaign_id,
            selected=selected,
            by_id=by_id,
            updated=updated,
            receipts=receipts,
            repo_root=repo_root,
        )
        for campaign_id in (*repin_ids, *rerun_only)
    ]
    failed = [item for item in rerun if item["status"] != "PASS"]
    return {
        "status": "REPINNED" if not failed else "FAILED",
        "drifted": drifted,
        "stale_receipts": stale_receipts,
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
