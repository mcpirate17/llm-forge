"""Dependency-graph test selection, targeted execution, and changed-line coverage."""

from __future__ import annotations

import ast
import json
import re
import sqlite3
import subprocess
import sys
import time
from collections.abc import Mapping, Sequence
from datetime import date
from pathlib import Path, PurePosixPath

from conductor.candidate_review import external_invariants
from conductor.candidate_review.checks import (
    ContractPlan,
    ReviewContext,
    TestSelection,
    _result,
)
from conductor.candidate_review.command_runner import _tail
from conductor.candidate_review.contract_runtime import prepare_contract_runtime
from conductor.candidate_review.coverage_eval import (  # noqa: F401
    # `_coverage_counts` and `_risk_buckets` are re-exported, not used here:
    # callers and tests reach them through this module's namespace.
    _coverage_counts,
    _evaluate_changed_coverage,
    _risk_buckets,
)
from conductor.candidate_review.git_source import (
    GitSourceError,
    _batch_blobs,
    list_tree,
    run_git,
)
from conductor.candidate_review.graph_selection import (
    _convention_tests,
    _graph_test_paths,
    _rust_crate_tests,
)
from conductor.candidate_review.model import (
    Change,
    CheckResult,
    CheckStatus,
    Finding,
    Severity,
    TreeEntry,
    sha256_bytes,
    sha256_file,
)
from conductor.candidate_review.policy import (
    MUTATION_WAIVER_SOURCE_ANCHOR,
    W7_TRIDENT_LINEAR_INTEGRATION_MILESTONE,
    CheckPolicy,
)
from conductor.candidate_review.sharding import (
    combine_coverage,
    execute_shards,
    shard_data_file,
    shard_outcome_findings,
    shard_tests,
    shard_timeout_findings,
)
from conductor.candidate_review.value_waivers import (
    WAIVED_RULE,
    apply_value_waivers,
)
from conductor.candidate_review.value_waivers import (
    waiver_states as value_waiver_states,
)
from conductor.mutation_receipt_slim import expand_receipt_field
from conductor.project_paths import registry_path, registry_relative

GRANDFATHER_SCHEMA_VERSION = 1
GRANDFATHER_INVENTORY_RELPATH = (
    "conductor/candidate_review/grandfathered_test_nodeids_61343f57.json"
)
GRANDFATHER_INVENTORY_SHA256 = (
    "aa0a89e12c7a43e9dc3f509f5540ed67df112462cb9fd6cafe060c596f3414d8"
)
GRANDFATHER_ANCHOR_COMMIT_OID = "61343f575215dd222a74fc2c060d0328692ded5e"
GRANDFATHER_ANCHOR_TREE_OID = "b01877ba62c32445f7649450f9b395dd70de306a"
GRANDFATHER_DEAD_TEST_PATHS: frozenset[str] = frozenset(
    {
        "research/tests/test_nm_f6_avo_fused_ce_battery.py",
        "research/tests/test_nm_f6_partitioned_head_ce.py",
        "research/tests/test_nm_f6_phase22_20k_continuation.py",
        "research/tests/test_nm_f6_phase22_chinchilla.py",
        "research/tests/test_nm_f6_phase22_chinchilla_minimal_battery_sweep.py",
        "research/tests/test_nm_f6_phase22_chinchilla_ordered_path_knockout.py",
        "research/tests/test_nm_f6_phase22_chinchilla_read_write_trajectory.py",
        "research/tests/test_nm_f6_phase22_chinchilla_terminal_stage1.py",
        "research/tests/test_nm_f6_phase22_minimal_battery.py",
        "research/tests/test_nm_f6_phase22_ordered_path_knockout.py",
        "research/tests/test_nm_f6_phase22_partitioned_head_training.py",
        "research/tests/test_nm_f6_phase22_postrun_validation.py",
    }
)

TEST_PROPERTY_TEXT = re.compile(
    r"hypothesis|@(?:pytest\.)?mark\.parametrize|@pytest\.mark\.(?:property|invariant)|"
    r"def test_.*(?:property|invariant|boundary|roundtrip|adversarial)"
)
ZERO_OID = "0" * 40


def _native_decide(operation: str, payload: object) -> object:
    from conductor._native import candidate_verification_native

    return json.loads(candidate_verification_native(operation, json.dumps(payload)))


def _native_findings(rows: Sequence[Mapping[str, object]]) -> list[Finding]:
    findings: list[Finding] = []
    for row in rows:
        fields = dict(row)
        fields["severity"] = Severity(str(fields["severity"]))
        findings.append(Finding(**fields))
    return findings


def _python_test_definitions(source: str, path: str) -> dict[str, str]:
    """Collect test definitions and their decorator-inclusive source text in Rust."""
    from conductor._native import candidate_verification_ast_native

    try:
        return json.loads(candidate_verification_ast_native(source, path))
    except ValueError as exc:
        try:
            ast.parse(source)
        except SyntaxError as syntax:
            raise RuntimeError(
                f"cannot parse test definitions in {path}: {syntax}"
            ) from syntax
        raise RuntimeError(str(exc)) from exc


def _python_test_labels(source: str, path: str) -> set[str]:
    return set(_python_test_definitions(source, path))


class _GrandfatherError(RuntimeError):
    """The grandfather inventory is absent from the snapshot or fails validation."""


def _prove_anchor_commit(store: Path) -> None:
    """Prove the anchor commit exists and its tree matches the bound OID."""

    kind = (
        run_git(store, ["cat-file", "-t", GRANDFATHER_ANCHOR_COMMIT_OID])
        .stdout.decode()
        .strip()
    )
    if kind != "commit":
        raise _GrandfatherError(
            "grandfather anchor object "
            f"{GRANDFATHER_ANCHOR_COMMIT_OID} is a {kind!r}, not a commit"
        )
    tree = (
        run_git(store, ["rev-parse", f"{GRANDFATHER_ANCHOR_COMMIT_OID}^{{tree}}"])
        .stdout.decode()
        .strip()
    )
    if tree != GRANDFATHER_ANCHOR_TREE_OID:
        raise _GrandfatherError(
            "grandfather anchor commit tree drifted: expected "
            f"{GRANDFATHER_ANCHOR_TREE_OID}, got {tree}"
        )


def _tree_inventory(store: Path) -> dict[str, frozenset[str]]:
    """Label inventory for every test_*.py blob in the anchored tree.

    Files that declare no module-level or class-level test function carry no
    grandfatherable nodeids and are omitted, exactly as the original
    inventory generator did.
    """

    entries = list_tree(store, GRANDFATHER_ANCHOR_TREE_OID)
    test_entries = [
        entry
        for entry in entries
        if entry.path.endswith(".py")
        and PurePosixPath(entry.path).name.startswith("test_")
    ]
    blobs = _batch_blobs(store, test_entries)
    inventory: dict[str, frozenset[str]] = {}
    for entry in test_entries:
        try:
            source = blobs[entry.oid].decode("utf-8")
        except UnicodeDecodeError as exc:
            raise _GrandfatherError(
                f"grandfather anchor blob for {entry.path} is not UTF-8 text"
            ) from exc
        labels = frozenset(_python_test_labels(source, entry.path))
        if labels:
            inventory[entry.path] = labels
    return inventory


def _derive_inventory_from_tree(
    snapshot: Path, repo: Path
) -> dict[str, frozenset[str]]:
    """Derive the inventory from the anchored tree, proving both bound OIDs.

    The candidate snapshot is object-less (a materialized tree), so git
    resolution falls back to the host repository object store; only when both
    stores fail is the anchor unprovable and the review fails closed.
    """

    failures: list[str] = []
    for store, label in ((snapshot, "candidate snapshot"), (repo, "host repository")):
        try:
            _prove_anchor_commit(store)
            return _tree_inventory(store)
        except (GitSourceError, RuntimeError, OSError) as exc:
            failures.append(f"{label}: {exc}")
    raise _GrandfatherError(
        "grandfather anchor commit "
        f"{GRANDFATHER_ANCHOR_COMMIT_OID} cannot be proven from git: "
        + "; ".join(failures)
    )


def _inventory_divergence(
    parsed: Mapping[str, frozenset[str]], derived: Mapping[str, frozenset[str]]
) -> str:
    return str(
        _native_decide(
            "inventory_divergence",
            {
                "parsed": {path: sorted(labels) for path, labels in parsed.items()},
                "derived": {path: sorted(labels) for path, labels in derived.items()},
            },
        )
    )


def _load_grandfathered_nodeids(ctx: ReviewContext) -> dict[str, frozenset[str]]:
    """Prove the SHA-bound inventory and validate its effective exemptions in Rust."""
    path = ctx.snapshot / GRANDFATHER_INVENTORY_RELPATH
    if not path.is_file():
        raise _GrandfatherError(
            "grandfather inventory is absent from the candidate snapshot; "
            f"expected {GRANDFATHER_INVENTORY_RELPATH}"
        )
    raw = path.read_bytes()
    if sha256_bytes(raw) != GRANDFATHER_INVENTORY_SHA256:
        raise _GrandfatherError(
            "grandfather inventory bytes do not match the SHA-256 bound at "
            f"{MUTATION_WAIVER_SOURCE_ANCHOR}; refusing to honor it"
        )
    derived = _derive_inventory_from_tree(ctx.snapshot, ctx.repo)
    try:
        payload = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise _GrandfatherError(
            f"grandfather inventory is not valid JSON: {exc}"
        ) from exc
    tests = payload.get("tests") if isinstance(payload, dict) else None
    request = {
        "payload": payload,
        "expected_schema": (
            "conductor.candidate_review.grandfather_inventory/"
            f"v{GRANDFATHER_SCHEMA_VERSION}"
        ),
        "anchor_commit": MUTATION_WAIVER_SOURCE_ANCHOR,
        "milestone": W7_TRIDENT_LINEAR_INTEGRATION_MILESTONE,
        "derived": {name: sorted(labels) for name, labels in derived.items()},
        "present_paths": list(tests) if isinstance(tests, dict) else [],
        "dead_paths": sorted(GRANDFATHER_DEAD_TEST_PATHS),
    }
    try:
        effective = _native_decide("inventory_validate", request)
    except ValueError as exc:
        raise _GrandfatherError(str(exc)) from exc
    return {
        rel_path: frozenset(labels)
        for rel_path, labels in effective.items()
        if (ctx.repo / rel_path).is_file()
    }


def _inventory_path_unsafe(rel_path: object) -> bool:
    return bool(_native_decide("inventory_path_unsafe", rel_path))


def _base_test_definitions(ctx: ReviewContext, change: Change) -> dict[str, str] | None:
    """The candidate base's definitions for `change.path`, or None if unavailable.

    None means "assume nothing was there", which gates every definition in the
    file -- the pre-2026-09-03 behaviour, kept as the fail-closed answer for an
    added file, an unreadable base blob, or a base that does not parse.
    """

    if change.old_mode == "000000" or change.old_oid == ZERO_OID:
        return None
    entry = TreeEntry(
        path=change.old_path or change.path,
        mode=change.old_mode,
        object_type="blob",
        oid=change.old_oid,
    )
    try:
        blob = _batch_blobs(ctx.repo, [entry]).get(change.old_oid)
        if blob is None:
            return None
        return _python_test_definitions(blob.decode("utf-8"), entry.path)
    except (GitSourceError, UnicodeError, RuntimeError):
        return None


def _value_gated_nodeids(
    ctx: ReviewContext, grandfathered: Mapping[str, frozenset[str]]
) -> dict[str, tuple[str, ...]]:
    """Compare candidate definitions with the base and anchored exemptions."""
    entries: list[dict[str, object]] = []
    for change in ctx.live_changes:
        row: dict[str, object] = {
            "path": change.path,
            "classes": change.classes,
            "old_mode": change.old_mode,
            "old_oid": change.old_oid,
        }
        if "test" in change.classes and change.path.endswith(".py"):
            try:
                source = (ctx.snapshot / change.path).read_text(encoding="utf-8")
            except (OSError, UnicodeError) as exc:
                raise RuntimeError(
                    f"cannot read candidate test {change.path}: {exc}"
                ) from exc
            row["definitions"] = _python_test_definitions(source, change.path)
            row["base"] = _base_test_definitions(ctx, change)
        entries.append(row)
    result = _native_decide(
        "gated_nodeids",
        {
            "entries": entries,
            "grandfathered": {
                path: sorted(labels) for path, labels in grandfathered.items()
            },
        },
    )
    return {path: tuple(nodeids) for path, nodeids in result.items()}


def _waiver_states(ctx: ReviewContext) -> list[dict[str, object]]:
    """Read pinned bytes, then let Rust decide each waiver's activation state."""

    def pinned(path: str, digest: str) -> bool:
        try:
            return sha256_file(ctx.snapshot / path) == digest
        except OSError:
            return False

    waivers = []
    for waiver in ctx.policy.mutation_waivers:
        # Preserve the policy's short circuit: unrelated bases and drifted test
        # bytes never require reading the pinned source files.
        bound = ctx.candidate.waiver_base == waiver.integration_base
        file_ok = bound and pinned(waiver.path, waiver.sha256)
        sources = []
        if file_ok:
            for source in waiver.sources:
                source_ok = pinned(source.path, source.sha256)
                sources.append({"path": source.path, "ok": source_ok})
                if not source_ok:
                    break
        waivers.append(
            {
                "id": waiver.waiver_id,
                "path": waiver.path,
                "integration_base": waiver.integration_base,
                "file_ok": file_ok,
                "sources": sources,
            }
        )
    return _native_decide(
        "waiver_states", {"base": ctx.candidate.waiver_base, "waivers": waivers}
    )


def select_tests(ctx: ReviewContext) -> TestSelection:
    from conductor._native import contract_test_plan_native

    changes = [
        {
            "path": change.path,
            "classes": change.classes,
            "deleted": change.deleted,
            "risk": change.risk,
        }
        for change in ctx.live_changes
    ]
    plan = _native_decide("selection_plan", {"changes": changes})
    sources = plan["sources"]
    graph_tests: set[str] = set()
    graph_error: str | None = None
    if sources:
        try:
            graph_tests, graph = _graph_test_paths(ctx, sources)
        except (RuntimeError, sqlite3.Error) as exc:
            graph_error = str(exc)
            graph = {}
    else:
        graph = {"status": "not-required", "selected_edges": 0}
    convention_tests = _convention_tests(ctx, sources)
    native_tests = _rust_crate_tests(ctx, sources)
    contract_plan = json.loads(
        contract_test_plan_native(
            str(ctx.snapshot),
            [
                path
                for change in ctx.candidate.changes
                for path in (change.path, change.old_path)
                if path
            ],
        )
    )
    evidence_tests = (
        graph_tests
        | convention_tests
        | set(plan["changed_tests"])
        | {test for files in native_tests.values() for test in files}
        | set(contract_plan["test_paths"])
    )
    property_evidence = (
        _has_property_evidence(ctx, evidence_tests)
        if plan["high_risk"] and evidence_tests
        else False
    )
    result = _native_decide(
        "selection_decide",
        {
            **plan,
            "graph_tests": sorted(graph_tests),
            "convention_tests": sorted(convention_tests),
            "native_tests": native_tests,
            "contract_sources": contract_plan["source_paths"],
            "contract_targets": contract_plan["targets"],
            "contract_test_paths": contract_plan["test_paths"],
            "graph": graph,
            "graph_error": graph_error,
            "property_evidence": property_evidence,
        },
    )
    findings = _native_findings(result["findings"])
    return TestSelection(
        tuple(result["tests"]),
        result["graph"],
        tuple(finding.finalize() for finding in findings),
        contract_plan,
    )


def _has_property_evidence(ctx: ReviewContext, tests: set[str]) -> bool:
    for test in tests:
        try:
            if TEST_PROPERTY_TEXT.search(
                (ctx.snapshot / test).read_text(encoding="utf-8")
            ):
                return True
        except (OSError, UnicodeDecodeError):
            continue
    return _has_mutation_evidence(ctx, tests)


def _has_mutation_evidence(ctx: ReviewContext, tests: set[str]) -> bool:
    """True when a selected test is ranked by a campaign with a current PASS receipt.

    The rule this backs is ``missing-property-or-mutation-evidence``, but until
    this branch existed only the name-and-decorator regex above could satisfy it:
    a registered campaign at mutation score 1.0 — strictly stronger evidence than
    a ``parametrize`` decorator — counted for nothing, and satisfying the gate
    meant editing a test whose bytes a campaign pins.
    """
    registry = registry_path(ctx.snapshot)
    if not registry.is_file():
        return False
    from conductor.mutation_testing import CampaignError, verify_evidence

    try:
        payload = verify_evidence(registry, sorted(tests), repo_root=ctx.snapshot)
    except CampaignError:
        # A broken registry is the mutation-evidence check's finding to report,
        # not a reason to claim property evidence here.
        return False
    covered = {
        row.get("path")
        for row in payload.get("evidence", [])
        if isinstance(row, Mapping)
    }
    return bool(covered & tests)


def _missing_evidence_findings(
    payload: Mapping[str, object], *, waived: set[str]
) -> list[Finding]:
    return _native_findings(
        _native_decide(
            "receipt_findings",
            {
                "payload": {
                    "missing_evidence": payload.get("missing_evidence", []),
                    "malformed_receipts": [],
                },
                "waived": sorted(waived),
            },
        )
    )


def _mutation_receipt_findings(
    payload: Mapping[str, object], *, waived: set[str]
) -> list[Finding]:
    return _native_findings(
        _native_decide(
            "receipt_findings", {"payload": payload, "waived": sorted(waived)}
        )
    )


def _evidence_index(
    evidence_rows: list[object],
) -> tuple[dict[str, dict[str, object]], list[Finding]]:
    result = _native_decide("evidence_index", {"evidence_rows": evidence_rows})
    return result["index"], _native_findings(result["findings"])


def _manifest_for_campaign(snapshot: Path, campaign_id: object) -> dict[str, object]:
    """The manifest declaring ``campaign_id``, or an empty mapping if unresolvable."""

    if not isinstance(campaign_id, str):
        return {}
    registry = registry_path(snapshot)
    try:
        rows = json.loads(registry.read_text("utf-8")).get("campaigns", [])
    except (OSError, UnicodeError, json.JSONDecodeError):
        return {}
    for row in rows:
        if not isinstance(row, dict) or not isinstance(row.get("manifest"), str):
            continue
        try:
            payload = json.loads((snapshot / row["manifest"]).read_text("utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError):
            continue
        if payload.get("campaign_id") == campaign_id:
            return payload
    return {}


def _external_invariant_admissions(
    ctx: ReviewContext,
    path: str,
    nodeids: Sequence[str],
    evidence: Mapping[str, object],
) -> tuple[frozenset[str], list[Finding]]:
    """Nodeids admitted as verified external invariants, plus findings for the rest.

    Fails closed in every direction: an unresolvable manifest, an undeclared nodeid, a
    stale torch pin or an unmeasurable coverage run all leave the nodeid gated, and a
    waiver that was declared but did not verify says which condition failed rather than
    falling through to a generic "has no value classification".
    """

    manifest = _manifest_for_campaign(ctx.snapshot, evidence.get("campaign_id"))
    declarations = manifest.get("external_invariants")
    if not isinstance(declarations, list) or not declarations:
        return frozenset(), []
    outcomes = external_invariants.evaluate(
        ctx.snapshot,
        ctx.runtime_dir,
        [row for row in declarations if isinstance(row, Mapping)],
        nodeids,
    )
    admitted = {outcome.nodeid for outcome in outcomes if outcome.admitted}
    findings = [
        Finding(
            check_id="mutation-evidence",
            rule_id="external-invariant-not-verified",
            severity=Severity.CRITICAL,
            message=(
                f"{path}: {outcome.nodeid} declares an external-invariant waiver "
                f"that did not verify: {outcome.reason}"
            ),
            path=path,
            help=(
                "A waiver is verified, not asserted: the nodeid must execute no "
                "repository source outside test files, the pinned torch version must "
                "equal the installed one, and the justification must be non-empty."
            ),
        )
        for outcome in outcomes
        if not outcome.admitted
    ]
    return frozenset(admitted), findings


def _new_test_value_findings(
    ctx: ReviewContext,
    payload: Mapping[str, object],
    new_nodeids: Mapping[str, Sequence[str]],
) -> list[Finding]:
    """Load receipts and external proofs for Rust-planned admission decisions."""
    plan = _native_decide(
        "value_admission_plan",
        {"payload": payload, "new_nodeids": new_nodeids},
    )
    findings = _native_findings(plan["prefix_findings"])
    from conductor.mutation_value import admission_errors

    for step in plan["steps"]:
        if "finding" in step:
            findings.extend(_native_findings([step["finding"]]))
            continue
        task = step["task"]
        path = task["path"]
        nodeids = task["nodeids"]
        try:
            receipt = json.loads(
                (ctx.snapshot / task["receipt"]).read_text(encoding="utf-8")
            )
        except (OSError, UnicodeError, json.JSONDecodeError) as exc:
            findings.append(
                Finding(
                    check_id="mutation-evidence",
                    rule_id="test-value-receipt-unavailable",
                    severity=Severity.CRITICAL,
                    message=f"{path}: cannot load test-value receipt: {exc}",
                    path=path,
                )
            )
            continue
        waived, waiver_findings = _external_invariant_admissions(
            ctx, path, nodeids, task["evidence"]
        )
        findings.extend(waiver_findings)
        value = (
            expand_receipt_field(receipt, "test_value")
            if isinstance(receipt, dict)
            else None
        )
        errors = admission_errors(
            value, [nodeid for nodeid in nodeids if nodeid not in waived]
        )
        findings.extend(
            _native_findings(
                _native_decide(
                    "value_admission_findings",
                    {"path": path, "nodeids": nodeids, "errors": errors},
                )
            )
        )
    return findings


def _receipt_required_paths(
    test_paths: Sequence[str], gated_nodeids: Mapping[str, Sequence[str]] | None
) -> list[str]:
    return _native_decide(
        "receipt_required_paths",
        {"test_paths": test_paths, "gated_nodeids": gated_nodeids},
    )


def check_mutation_evidence(ctx: ReviewContext) -> CheckResult:
    """Require mutation PASS receipts and value admission for new tests."""

    started = time.perf_counter()
    test_paths = [
        change.path
        for change in ctx.live_changes
        if "test" in change.classes and change.path.endswith(".py")
    ]
    if not test_paths:
        return CheckResult(
            check_id="mutation-evidence",
            status=CheckStatus.SKIPPED,
            duration_ms=0,
            skipped_reason="no matching candidate test changes",
        )
    scope_findings: list[Finding] = []
    gated_nodeids: dict[str, tuple[str, ...]] = {}
    try:
        grandfathered = _load_grandfathered_nodeids(ctx)
    except _GrandfatherError as exc:
        scope_findings.append(
            Finding(
                check_id="mutation-evidence",
                rule_id="grandfather-inventory-invalid",
                severity=Severity.CRITICAL,
                message=f"grandfather inventory fails closed: {exc}",
                help=(
                    "Restore the inventory bytes bound at commit "
                    f"{MUTATION_WAIVER_SOURCE_ANCHOR}; the value gate cannot be "
                    "evaluated without an anchored inventory."
                ),
            )
        )
    else:
        try:
            gated_nodeids = _value_gated_nodeids(ctx, grandfathered)
        except RuntimeError as exc:
            scope_findings.append(
                Finding(
                    check_id="mutation-evidence",
                    rule_id="new-test-definition-unavailable",
                    severity=Severity.CRITICAL,
                    message=f"new test definitions could not be verified: {exc}",
                )
            )
    receipt_paths = _receipt_required_paths(
        test_paths, None if scope_findings else gated_nodeids
    )
    waiver_states = _waiver_states(ctx)
    exempt = [path for path in test_paths if path not in set(receipt_paths)]
    registry = registry_path(ctx.snapshot)
    if not registry.is_file():
        finding = Finding(
            check_id="mutation-evidence",
            rule_id="mutation-registry-missing",
            severity=Severity.CRITICAL,
            message=(
                f"candidate snapshot lacks {registry_relative(ctx.snapshot)}; "
                "changed tests cannot prove mutation evidence"
            ),
            help=(
                "Include the mutation registry, campaign, and PASS receipt in the "
                "same candidate as the test file."
            ),
        )
        return _result("mutation-evidence", started, [finding], files=test_paths)
    from conductor.mutation_testing import CampaignError, verify_evidence

    try:
        payload = verify_evidence(
            registry, receipt_paths, repo_root=ctx.snapshot, anchor_repo=ctx.repo
        )
    except CampaignError as exc:
        finding = Finding(
            check_id="mutation-evidence",
            rule_id="mutation-evidence-unavailable",
            severity=Severity.CRITICAL,
            message=f"mutation evidence could not be verified: {exc}",
        )
        return _result("mutation-evidence", started, [finding], files=test_paths)
    active_paths = {str(s["path"]) for s in waiver_states if s["active"]}
    findings = _mutation_receipt_findings(payload, waived=active_paths)
    findings.extend(scope_findings)
    value_findings = _new_test_value_findings(ctx, payload, gated_nodeids)
    metrics = _mutation_evidence_metrics(payload, gated_nodeids, waiver_states)
    metrics["receipt_exempt_tests"] = exempt
    return _mutation_evidence_result(
        ctx, started, [*findings, *value_findings], test_paths, metrics
    )


def _mutation_evidence_result(
    ctx: ReviewContext,
    started: float,
    findings: list[Finding],
    test_paths: list[str],
    metrics: dict[str, object],
) -> CheckResult:
    """Apply the policy's value waivers; a result made only of WAIVED lines passes."""

    today = date.today()  # noqa: DTZ011 - preserve the policy's local-date contract
    # Waivers bind to the integration base, never to HEAD: an index candidate reviewed
    # at pre-commit must honour exactly the waivers CI honours for the same branch.
    base = ctx.candidate.waiver_base
    findings = apply_value_waivers(
        findings, ctx.policy.value_waivers, base=base, today=today
    )
    metrics["value_waiver_base"] = {
        "commit": base,
        "resolved_by": ctx.candidate.integration_base_detail,
    }
    metrics["value_waiver_states"] = value_waiver_states(
        ctx.policy.value_waivers, base=base, today=today
    )
    result = _result(
        "mutation-evidence", started, findings, files=test_paths, metrics=metrics
    )
    if result.findings and all(f.rule_id == WAIVED_RULE for f in result.findings):
        result.status = CheckStatus.PASSED
    return result


def _mutation_evidence_metrics(
    payload: Mapping[str, object],
    gated_nodeids: Mapping[str, Sequence[str]],
    waiver_states: list[dict[str, object]],
) -> dict[str, object]:
    return _native_decide(
        "mutation_metrics",
        {
            "payload": payload,
            "new_nodeids": gated_nodeids,
            "waiver_states": waiver_states,
        },
    )


def check_test_evidence(ctx: ReviewContext) -> tuple[CheckResult, TestSelection]:
    started = time.perf_counter()
    selection = select_tests(ctx)
    result = _result(
        "test-evidence",
        started,
        selection.findings,
        files=[
            *selection.tests,
            *(selection.contract_plan or {}).get("test_paths", []),
        ],
        metrics={"selected_tests": len(selection.tests), **selection.graph},
    )
    return result, selection


def run_targeted_tests(
    ctx: ReviewContext,
    selection: TestSelection,
    check: CheckPolicy,
    *,
    coverage: bool,
) -> CheckResult:
    started = time.perf_counter()
    contract_plan = selection.contract_plan
    contract_paths = contract_plan["test_paths"] if contract_plan else []
    if not selection.tests and not contract_paths:
        return CheckResult(
            check_id=check.check_id,
            status=CheckStatus.SKIPPED,
            duration_ms=0,
            skipped_reason="no selected tests",
        )
    coverage_file = ctx.runtime_dir / ".coverage-targeted"
    shards = (
        shard_tests(selection.tests, check.shard_max_files) if selection.tests else []
    )
    pytest_shard_count = len(shards)
    contract_commands = contract_plan["commands"] if contract_plan else []
    shards.extend(command["test_paths"] for command in contract_commands)
    total = len(shards)
    shard_files = [
        shard_data_file(coverage_file, index, pytest_shard_count)
        for index in range(pytest_shard_count)
    ]
    commands = [
        _pytest_command(tests, shard_files[index] if coverage else None)
        for index, tests in enumerate(shards[:pytest_shard_count])
    ]
    commands.extend(command["argv"] for command in contract_commands)
    selected_files = [*selection.tests, *contract_paths]
    try:
        completed_raw, timed_out = _run_selected_shards(
            ctx, check, contract_plan, commands
        )
    except (OSError, RuntimeError, ValueError) as exc:
        finding = Finding(
            check_id=check.check_id,
            rule_id="targeted-test-crash",
            severity=Severity.CRITICAL,
            message=f"targeted test execution did not complete: {type(exc).__name__}: {exc}",
        )
        return _result(check.check_id, started, [finding], files=selected_files)

    timeout_findings = shard_timeout_findings(check, shards, timed_out, total)
    finished = [index for index in range(total) if completed_raw[index] is not None]
    if not finished:
        return _result(check.check_id, started, timeout_findings, files=selected_files)
    completed_all = [completed for completed in completed_raw if completed is not None]
    shards = [shards[index] for index in finished]
    shard_files = [
        shard_files[index] for index in finished if index < pytest_shard_count
    ]

    findings, exit_codes, failed = shard_outcome_findings(
        check, shards, completed_all, len(selected_files)
    )
    # Timeouts first: they explain any missing coverage the other shards cannot.
    findings = timeout_findings + findings
    metrics: dict[str, object] = {
        "selected_tests": len(selection.tests),
        "selected_contract_targets": contract_plan["targets"] if contract_plan else [],
    }
    if total > 1:
        metrics["shard_count"] = total
        metrics["shard_workers"] = min(check.shard_workers, total)
        metrics["shard_exit_codes"] = exit_codes
    if not findings and coverage and pytest_shard_count:
        if pytest_shard_count > 1:
            combine_finding = combine_coverage(ctx, coverage_file, shard_files, check)
            if combine_finding is not None:
                findings.append(combine_finding)
        if not findings:
            coverage_findings, coverage_metrics = _evaluate_changed_coverage(
                ctx, coverage_file
            )
            findings.extend(coverage_findings)
            metrics.update(coverage_metrics)
    if coverage and contract_plan and contract_paths:
        metrics["python_coverage_scope"] = "pytest-only"
        findings.append(_contract_coverage_finding(check, contract_plan))
    result = _result(
        check.check_id, started, findings, files=selected_files, metrics=metrics
    )
    representative = failed[0] if failed else 0
    result.command = commands[finished[representative]]
    result.exit_code = exit_codes[representative]
    result.stdout_tail = _tail(
        "\n".join(completed.stdout for completed in completed_all),
        check.max_output_chars,
    )
    result.stderr_tail = _tail(
        "\n".join(completed.stderr for completed in completed_all),
        check.max_output_chars,
    )
    return result


def _run_selected_shards(
    ctx: ReviewContext,
    check: CheckPolicy,
    contract_plan: ContractPlan | None,
    commands: list[list[str]],
) -> tuple[list[subprocess.CompletedProcess[str] | None], list[int]]:
    environment = (
        prepare_contract_runtime(ctx, check, contract_plan)
        if contract_plan and contract_plan["targets"]
        else None
    )
    return execute_shards(ctx, commands, check, extra_env=environment)


def _contract_coverage_finding(check: CheckPolicy, plan: ContractPlan) -> Finding:
    return Finding(
        check_id=check.check_id,
        rule_id="rust-contract-coverage-unsupported",
        severity=Severity.HIGH,
        message=(
            "targeted Rust contracts ran, but Python coverage cannot measure "
            "their embedded PyO3 calls; changed-line coverage is incomplete"
        ),
        evidence={
            "contract_targets": plan["targets"],
            "python_coverage_scope": "pytest-only",
        },
    )


def _pytest_command(tests: Sequence[str], coverage_file: Path | None) -> list[str]:
    prefix = [sys.executable]
    if coverage_file is not None:
        prefix.extend(
            [
                "-m",
                "coverage",
                "run",
                "--branch",
                f"--data-file={coverage_file}",
            ]
        )
    prefix.extend(
        [
            "-m",
            "pytest",
            "-q",
            "-o",
            "addopts=",
            "-p",
            "no:cacheprovider",
            *tests,
        ]
    )
    return prefix
