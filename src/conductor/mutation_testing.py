"""Read-only inspection and evidence verification for mutation campaigns.

This module no longer executes anything. Mutation evidence is produced only by
the conductor-managed automatic engines -- ``conductor.mutation_campaign_generate``
to build a campaign and ``conductor.mutation_engine_generated run`` to run it
(fest for Python, cargo-mutants for Rust, Mull for C/C++). Hand-authored mutants,
manifests, patches, survivor baselines and receipt hashes are forbidden
(KB-MUT-02). The campaign executor and the ``repin`` driver that used to live
here were removed on 2026-09-08; what remains reads the frozen corpus and
verifies receipts.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import time
from collections import Counter
from collections.abc import Callable, Iterable, Mapping, Sequence
from dataclasses import asdict
from pathlib import Path
from typing import Any

from conductor import mutation_testing_support as _support
from conductor.mutation_receipt_build import (  # noqa: F401 - re-exported
    _BARE_INTERPRETERS,
    _default_receipt_path,
    _open_receipt,
    _pin_interpreter,
    _resolve_receipt_path,
    _score_receipt,
    _torch_version,
    _utc_stamp,
    killer_enforcement,
)
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
from conductor.project_paths import host_root, registry_relative
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
    ADAPTER,
    CARGO_ADAPTER,
    CTEST_ADAPTER,
    ValueEvidenceError,
    cargo_attribution_supported,
    collect_cargo_libtest_batch,
    collect_ctest_junit_batch,
    collect_pytest_junit_batch,
    ctest_attribution_supported,
    ctest_junit_path,
    pytest_attribution_supported,
    value_inspection_payload,
)

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


def _is_routine_kill(outcome: str, attribution: Mapping[str, Any]) -> bool:
    """A mutant that died exactly as its contract predicted.

    Across the published corpus 11597 of 11660 mutants land here, and for those the
    run's narrative -- the ranked pass/fail matrix and the pytest stdout tail --
    only restates what `killer_attribution` already proves, at roughly 13 KB a row.
    Survivors, timeouts and kills the contract did not predict keep every byte,
    because those are the rows anyone ever reads.
    """

    return outcome == "KILLED" and attribution.get("status") == "CONFIRMED"


def _attribution_summary(report: Mapping[str, Any]) -> dict[str, Any]:
    """A routine kill's attribution with the transcript folded to counts.

    Every field a reader can act on survives literally: the failing nodeids, the
    ranked tests the batch could not map, and the batch's own completeness. Only
    `tests` -- one row per ranked test per mutant, overwhelmingly the word PASSED
    beside a duration -- collapses, and it collapses to the tally that is all the
    full matrix was ever consulted for at rest.
    """

    counts: Counter[str] = Counter()
    tests = report.get("tests")
    if isinstance(tests, Mapping):
        for row in tests.values():
            if isinstance(row, Mapping):
                counts[str(row.get("outcome"))] += 1
    summary: dict[str, Any] = {
        key: list(value)
        for key in ("failed_nodeids", "missing_nodeids", "unmapped_cases")
        if isinstance(value := report.get(key), list)
    }
    summary["status"] = report.get("status")
    summary["ranked_outcome_counts"] = dict(sorted(counts.items()))
    return summary


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


def _require_declared_adapter(
    campaign: Campaign, ranked_nodeids: Sequence[str]
) -> None:
    """Refuse a value_analysis whose adapter names a harness it will not run under.

    The collector is chosen by nodeid shape, so a manifest is free to claim any
    adapter it likes and still get a matrix. That makes the label decorative, and a
    cargo campaign labelled `pytest-junit` reads as evidence from a harness that never
    ran. Bind the label to the collector instead.
    """

    if campaign.value_analysis is None:
        return
    if pytest_attribution_supported(campaign.test_argv, ranked_nodeids):
        expected = ADAPTER
    elif ctest_attribution_supported(ranked_nodeids):
        expected = CTEST_ADAPTER
    elif cargo_attribution_supported(ranked_nodeids):
        expected = CARGO_ADAPTER
    else:
        return
    if campaign.value_analysis.adapter != expected:
        raise CampaignError(
            f"value_analysis.adapter is {campaign.value_analysis.adapter!r} but this "
            f"batch is collected as {expected!r}"
        )


def _run_campaign_command(
    campaign: Campaign,
    *,
    snapshot_root: Path,
    report_name: str,
) -> tuple[CommandResult, Mapping[str, Any] | None]:
    """Run one batch, adding per-test evidence with no test-mutant Cartesian loop."""

    ranked_nodeids = [test.nodeid for test in campaign.ranked_tests]
    _require_declared_adapter(campaign, ranked_nodeids)
    if not pytest_attribution_supported(campaign.test_argv, ranked_nodeids):
        # Value analysis needs a per-test kill matrix, not a pytest one: it reads
        # `report["tests"][nodeid]["outcome"]`, which the ctest and cargo collectors
        # below produce in the same shape. Refusing everything but pytest made value
        # analysis unreachable for every Rust, C and C++ campaign in the repo.
        if campaign.value_analysis is not None and not (
            ctest_attribution_supported(ranked_nodeids)
            or cargo_attribution_supported(ranked_nodeids)
        ):
            raise CampaignError(
                "value analysis needs a batch that attributes failures per test: a "
                "pytest batch whose ranked tests are Python nodeids and whose argv "
                "does not already set --junitxml, a ctest batch, or a cargo libtest "
                "batch whose ranked tests are uniquely named `<path>.rs::<fn>`"
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


def main(argv: list[str] | None = None) -> int:
    """CLI entry point: inspection and evidence verification only.

    There is deliberately no ``run`` and no ``repin`` here. Producing mutation
    evidence goes through ``conductor.mutation_engine_generated``.
    """

    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    inspect_parser = subparsers.add_parser(
        "inspect", help="validate and inspect a campaign; runs nothing"
    )
    inspect_parser.add_argument("campaign", type=Path)
    verify_parser = subparsers.add_parser(
        "verify-evidence",
        help="report which changed tests lack a current full-campaign PASS receipt",
    )
    verify_parser.add_argument(
        "--registry",
        type=Path,
        default=Path(registry_relative(host_root())),
    )
    verify_parser.add_argument("paths", nargs="*")
    args = parser.parse_args(argv)
    try:
        if args.command == "verify-evidence":
            result = verify_evidence(args.registry, args.paths)
            _json_print(result)
            return 0 if result["status"] == "PASS" else 5
        campaign = load_campaign(args.campaign)
        result = inspect_campaign(campaign)
        _json_print(result)
        return 0 if result["status"] == "READY" else 3
    except CampaignError as exc:
        _json_print({"status": "REFUSED", "error": str(exc)})
        return 4


if __name__ == "__main__":
    raise SystemExit(main())
