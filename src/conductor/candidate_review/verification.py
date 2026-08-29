"""Dependency-graph test selection, targeted execution, and changed-line coverage."""

from __future__ import annotations

import ast
import json
import re
import sqlite3
import sys
import time
from pathlib import Path, PurePosixPath
from typing import Mapping, Sequence

from conductor.candidate_review.checks import (
    ReviewContext,
    TestSelection,
    _result,
)
from conductor.candidate_review.command_runner import _run_process, _tail
from conductor.candidate_review.git_source import (
    GitSourceError,
    _batch_blobs,
    changed_line_numbers,
    list_tree,
    run_git,
)
from conductor.candidate_review.model import (
    CheckResult,
    CheckStatus,
    Finding,
    Severity,
    sha256_bytes,
    sha256_file,
    sha256_json,
)
from conductor.candidate_review.sharding import (
    combine_coverage,
    execute_shards,
    shard_outcome_findings,
    shard_data_file,
    shard_tests,
    shard_timeout_findings,
)
from conductor.candidate_review.policy import (
    W7_TRIDENT_LINEAR_INTEGRATION_MILESTONE,
    MUTATION_WAIVER_SOURCE_ANCHOR,
    CheckPolicy,
)

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


def _graph_database(repo: Path) -> Path:
    return repo / ".code-review-graph" / "graph.db"


def _python_test_labels(source: str, path: str) -> set[str]:
    try:
        tree = ast.parse(source)
    except SyntaxError as exc:
        raise RuntimeError(f"cannot parse test definitions in {path}: {exc}") from exc
    labels: set[str] = set()
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            if node.name.startswith("test_"):
                labels.add(node.name)
            continue
        if not isinstance(node, ast.ClassDef) or not node.name.startswith("Test"):
            continue
        for child in node.body:
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef)) and (
                child.name.startswith("test_")
            ):
                labels.add(f"{node.name}::{child.name}")
    return labels


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
    """Summarize the first drift between a parsed and a tree-derived inventory."""

    missing = sorted(set(derived) - set(parsed))
    extra = sorted(set(parsed) - set(derived))
    drifted = sorted(
        path for path in set(parsed) & set(derived) if parsed[path] != derived[path]
    )
    return (
        f"missing-from-inventory={missing[:3]} ({len(missing)}), "
        f"not-in-anchor-tree={extra[:3]} ({len(extra)}), "
        f"label-drift={drifted[:3]} ({len(drifted)})"
    )


def _exclude_tombstoned_entries(
    grandfathered: Mapping[str, frozenset[str]],
) -> dict[str, frozenset[str]]:
    """Drop dead-lane tombstones unconditionally from the effective exemptions.

    A recreated dead lane must never resurrect its anchored exemptions, so a
    tombstoned path is excluded whether or not the file exists again; its test
    definitions return to the value gate as new tests.
    """

    return {
        rel_path: labels
        for rel_path, labels in grandfathered.items()
        if rel_path not in GRANDFATHER_DEAD_TEST_PATHS
    }


def _prune_dead_entries(
    grandfathered: Mapping[str, frozenset[str]], repo: Path
) -> dict[str, frozenset[str]]:
    """Drop inventory entries whose test file no longer exists in the repo.

    The anchored artifact stays byte-identical: pruning happens at evaluation
    time only, after the derived==shipped proof, so it can only shrink the
    exemption set, never widen it.
    """

    return {
        rel_path: labels
        for rel_path, labels in grandfathered.items()
        if (repo / rel_path).is_file()
    }


def _load_grandfathered_nodeids(ctx: ReviewContext) -> dict[str, frozenset[str]]:
    """Load the SHA-bound nodeid inventory frozen at MUTATION_WAIVER_SOURCE_ANCHOR."""

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
    expected_schema = (
        f"conductor.candidate_review.grandfather_inventory/v"
        f"{GRANDFATHER_SCHEMA_VERSION}"
    )
    tests = payload.get("tests") if isinstance(payload, dict) else None
    if (
        not isinstance(tests, dict)
        or payload.get("schema") != expected_schema
        or payload.get("anchor_commit") != MUTATION_WAIVER_SOURCE_ANCHOR
        or payload.get("milestone") != W7_TRIDENT_LINEAR_INTEGRATION_MILESTONE
        or not tests
    ):
        raise _GrandfatherError("grandfather inventory fails schema validation")
    grandfathered: dict[str, frozenset[str]] = {}
    seen_nodeids: set[str] = set()
    for rel_path, labels in tests.items():
        if _inventory_path_unsafe(rel_path):
            raise _GrandfatherError(
                f"grandfather inventory entry path is unsafe: {rel_path!r}"
            )
        if (
            not isinstance(rel_path, str)
            or not isinstance(labels, list)
            or not labels
            or any(not isinstance(label, str) or not label for label in labels)
        ):
            raise _GrandfatherError(
                f"grandfather inventory entry is malformed: {rel_path!r}"
            )
        unique = frozenset(labels)
        if len(unique) != len(labels):
            raise _GrandfatherError(
                f"grandfather inventory has duplicate nodeids in {rel_path}"
            )
        full = {f"{rel_path}::{label}" for label in unique}
        clash = seen_nodeids & full
        if clash:
            raise _GrandfatherError(
                "grandfather inventory has duplicate nodeids across paths: "
                + sorted(clash)[0]
            )
        seen_nodeids |= full
        grandfathered[rel_path] = unique
    if grandfathered != derived:
        raise _GrandfatherError(
            "grandfather inventory does not match the inventory derived from the "
            f"anchored tree: {_inventory_divergence(grandfathered, derived)}"
        )
    return _prune_dead_entries(_exclude_tombstoned_entries(grandfathered), ctx.repo)


def _inventory_path_unsafe(rel_path: object) -> bool:
    """True when an inventory key is not a normalized repo-relative .py path."""

    if not isinstance(rel_path, str):
        return False  # non-str keys are reported by the malformed-entry check
    parts = PurePosixPath(rel_path).parts
    return bool(
        not rel_path.endswith(".py")
        or rel_path.startswith("/")
        or "\\" in rel_path
        or any(ord(ch) < 32 or ord(ch) == 127 for ch in rel_path)
        or set(rel_path) & set("*?[")
        or list(parts) != rel_path.split("/")
        or any(part in {".", ".."} for part in parts)
    )


def _value_gated_nodeids(
    ctx: ReviewContext, grandfathered: Mapping[str, frozenset[str]]
) -> dict[str, tuple[str, ...]]:
    """Nodeids lacking anchored evidence: every current test def not in the inventory."""

    gated: dict[str, tuple[str, ...]] = {}
    for change in ctx.live_changes:
        if "test" not in change.classes:
            continue
        path = change.path
        if path.endswith(".patch"):
            # A reviewed mutant patch fragment, not a test definition; its name
            # starting with "test_" (describing the mutant) is what tripped
            # the coarse "test" classifier, not anything pytest will collect.
            continue
        if not path.endswith(".py"):
            if (
                change.old_mode == "000000" or change.old_oid == ZERO_OID
            ) and path not in grandfathered:
                gated[path] = (path,)
            continue
        try:
            source = (ctx.snapshot / path).read_text(encoding="utf-8")
        except (OSError, UnicodeError) as exc:
            raise RuntimeError(f"cannot read candidate test {path}: {exc}") from exc
        excluded = grandfathered.get(path, frozenset())
        labels = sorted(set(_python_test_labels(source, path)) - excluded)
        if labels:
            gated[path] = tuple(f"{path}::{label}" for label in labels)
    return gated


def _waiver_states(ctx: ReviewContext) -> list[dict[str, object]]:
    """Evaluate each configured waiver once into an auditable activation state."""

    states: list[dict[str, object]] = []
    base = ctx.candidate.base_commit_oid
    for waiver in ctx.policy.mutation_waivers:
        state: dict[str, object] = {
            "id": waiver.waiver_id,
            "path": waiver.path,
            "active": False,
            "reason": "",
        }
        active = base == waiver.integration_base
        if not active:
            state["reason"] = "candidate base commit is not the pinned integration base"
        if active:
            try:
                file_bytes_ok = sha256_file(ctx.snapshot / waiver.path) == waiver.sha256
            except OSError:
                file_bytes_ok = False
            active = file_bytes_ok
            if not active:
                state["reason"] = "test file missing or drifted from pinned sha256"
        if active:
            for binding in waiver.sources:
                try:
                    source_ok = (
                        sha256_file(ctx.snapshot / binding.path) == binding.sha256
                    )
                except OSError:
                    source_ok = False
                if not source_ok:
                    active = False
                    state["reason"] = (
                        f"pinned source {binding.path} missing or drifted from "
                        "pinned sha256"
                    )
                    break
        state["active"] = active
        states.append(state)
    return states


def _graph_test_paths(
    ctx: ReviewContext, source_paths: Sequence[str]
) -> tuple[set[str], dict[str, object]]:
    database = _graph_database(ctx.repo)
    if not database.is_file():
        raise RuntimeError("code-review graph database is missing")
    uri = f"file:{database.as_posix()}?mode=ro&immutable=1"
    connection = sqlite3.connect(uri, uri=True, timeout=2.0)
    try:
        metadata = dict(connection.execute("SELECT key, value FROM metadata"))
        expected = (
            ctx.candidate.base_commit_oid
            if ctx.candidate.kind == "index"
            else ctx.candidate.commit_oid
        )
        if not expected or metadata.get("git_head_sha") != expected:
            raise RuntimeError(
                "stale code-review graph: "
                f"expected {expected}, found {metadata.get('git_head_sha')}"
            )
        absolute = [str((ctx.repo / path).resolve()) for path in source_paths]
        if not absolute:
            return set(), {
                "head_sha": expected,
                "schema_version": metadata.get("schema_version"),
            }
        placeholders = ",".join("?" for _ in absolute)
        rows = connection.execute(
            f"""
            SELECT DISTINCT source.file_path, edge.kind, target.qualified_name
            FROM nodes AS target
            JOIN edges AS edge ON edge.target_qualified = target.qualified_name
            JOIN nodes AS source ON source.qualified_name = edge.source_qualified
            WHERE target.file_path IN ({placeholders}) AND source.is_test = 1
            ORDER BY source.file_path, edge.kind, target.qualified_name
            """,
            absolute,
        ).fetchall()
        tests: set[str] = set()
        evidence_rows: list[tuple[str, str, str]] = []
        for file_path, edge_kind, target in rows:
            try:
                relative = Path(file_path).resolve().relative_to(ctx.repo).as_posix()
            except ValueError:
                continue
            if (ctx.snapshot / relative).is_file():
                tests.add(relative)
                evidence_rows.append((relative, edge_kind, target))
        graph = {
            "head_sha": expected,
            "schema_version": metadata.get("schema_version"),
            "last_updated": metadata.get("last_updated"),
            "selected_edges": len(evidence_rows),
            "evidence_sha256": sha256_json(evidence_rows),
        }
        return tests, graph
    finally:
        connection.close()


def _convention_tests(ctx: ReviewContext, source_paths: Sequence[str]) -> set[str]:
    names = {f"test_{PurePosixPath(path).stem}.py" for path in source_paths}
    tests: set[str] = set()
    for base in (
        "conductor",
        "research/tests",
        "component_fab/tests",
        "aria_core/tests",
        "aria_designer/tests",
    ):
        root = ctx.snapshot / base
        if not root.is_dir():
            continue
        for path in root.rglob("test*.py"):
            rel = path.relative_to(ctx.snapshot).as_posix()
            if path.name in names:
                tests.add(rel)
                continue
            try:
                text = path.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                continue
            for source in source_paths:
                module = source.removesuffix(".py").replace("/", ".")
                if module in text:
                    tests.add(rel)
                    break
    return tests


def select_tests(ctx: ReviewContext) -> TestSelection:
    sources = [
        change.path
        for change in ctx.live_changes
        if ({"python", "native"} & set(change.classes)) and "test" not in change.classes
    ]
    changed_tests = {
        change.path
        for change in ctx.live_changes
        if "test" in change.classes and change.path.endswith(".py")
    }
    findings: list[Finding] = []
    graph: dict[str, object] = {}
    graph_tests: set[str] = set()
    if sources:
        try:
            graph_tests, graph = _graph_test_paths(ctx, sources)
        except (RuntimeError, sqlite3.Error) as exc:
            findings.append(
                Finding(
                    check_id="test-evidence",
                    rule_id="graph-evidence-incomplete",
                    severity=Severity.CRITICAL,
                    message=f"dependency/call-graph test selection failed closed: {exc}",
                )
            )
    else:
        graph = {"status": "not-required", "selected_edges": 0}
    tests = graph_tests | _convention_tests(ctx, sources) | changed_tests
    if sources and not tests:
        findings.append(
            Finding(
                check_id="test-evidence",
                rule_id="no-targeted-tests",
                severity=Severity.HIGH,
                message="changed production code has no graph-selected or convention-matched tests",
                evidence={"source_paths": sources},
            )
        )
    high_risk = any(
        change.risk == "high" and "test" not in change.classes
        for change in ctx.live_changes
    )
    if high_risk and tests and not _has_property_evidence(ctx, tests):
        findings.append(
            Finding(
                check_id="test-evidence",
                rule_id="missing-property-or-mutation-evidence",
                severity=Severity.HIGH,
                message="high-risk logic lacks property/parameterized/mutation-style test evidence",
            )
        )
    return TestSelection(
        tuple(sorted(tests)), graph, tuple(finding.finalize() for finding in findings)
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
    registry = ctx.snapshot / "conductor/mutation_campaigns/registry.json"
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


def _malformed_container_finding(field: str) -> Finding:
    """CRITICAL for an evidence container whose rows cannot be evaluated."""

    return Finding(
        check_id="mutation-evidence",
        rule_id="malformed-evidence-container",
        severity=Severity.CRITICAL,
        message=(
            f"{field}: evidence container is malformed (not a list); its rows "
            "cannot be evaluated and admission cannot be proven"
        ),
    )


def _malformed_receipt_row_finding(row: object) -> Finding:
    """CRITICAL for a non-object row inside the missing-evidence container."""

    return Finding(
        check_id="mutation-evidence",
        rule_id="malformed-mutation-receipt",
        severity=Severity.CRITICAL,
        message=f"malformed mutation receipt row (not an object): {row!r}",
    )


def _missing_evidence_findings(
    payload: Mapping[str, object], *, waived: set[str]
) -> list[Finding]:
    rows = payload.get("missing_evidence", [])
    if not isinstance(rows, list):
        return [_malformed_container_finding("missing_evidence")]
    findings: list[Finding] = []
    for missing in rows:
        if not isinstance(missing, dict):
            findings.append(_malformed_receipt_row_finding(missing))
            continue
        path_name = str(missing.get("path", ""))
        if path_name in waived:
            continue
        findings.append(
            Finding(
                check_id="mutation-evidence",
                rule_id="missing-mutation-receipt",
                severity=Severity.CRITICAL,
                message=(
                    f"{path_name}: {missing.get('reason', 'missing mutation evidence')}"
                ),
                path=path_name or None,
                help=(
                    "Scaffold with `python -m conductor.mutation_coverage scaffold "
                    "PATH --source SRC`, register the campaign, obtain Tim's "
                    "authority, then `make mutation-run` and keep the PASS receipt."
                ),
                evidence={
                    "receipt_rejections": missing.get("receipt_rejections", []),
                },
            )
        )
    return findings


def _malformed_receipt_findings(payload: Mapping[str, object]) -> list[Finding]:
    rows = payload.get("malformed_receipts", [])
    if not isinstance(rows, list):
        return [_malformed_container_finding("malformed_receipts")]
    return [
        Finding(
            check_id="mutation-evidence",
            rule_id="malformed-mutation-receipt",
            severity=Severity.CRITICAL,
            message=f"malformed mutation receipt: {malformed}",
        )
        for malformed in rows
    ]


def _mutation_receipt_findings(
    payload: Mapping[str, object], *, waived: set[str]
) -> list[Finding]:
    return [
        *_missing_evidence_findings(payload, waived=waived),
        *_malformed_receipt_findings(payload),
    ]


def _receipt_unavailable_finding(
    path: str, nodeids: Sequence[str], detail: str
) -> Finding:
    """CRITICAL for gated paths whose value admission cannot be evaluated."""

    return Finding(
        check_id="mutation-evidence",
        rule_id="test-value-receipt-unavailable",
        severity=Severity.CRITICAL,
        message=(
            f"{path}: {detail}; "
            f"value admission cannot be evaluated for {', '.join(nodeids)}"
        ),
        path=path,
    )


def _evidence_index(
    evidence_rows: list[object],
) -> tuple[dict[str, dict[str, object]], list[Finding]]:
    """Index evidence rows by path, failing closed on malformed duplicates.

    A path claimed by more than one row is rejected outright: neither row may
    admit it, because the evidence identity for a gated path must be unique.
    """

    index: dict[str, dict[str, object]] = {}
    positions: dict[str, int] = {}
    findings: list[Finding] = []
    for position, row in enumerate(evidence_rows, start=1):
        if not isinstance(row, dict) or not isinstance(row.get("path"), str):
            findings.append(
                Finding(
                    check_id="mutation-evidence",
                    rule_id="malformed-evidence-row",
                    severity=Severity.CRITICAL,
                    message=(
                        f"evidence row {position} is malformed (needs an object with "
                        f"a string 'path'); the row cannot be evaluated: {row!r}"
                    ),
                )
            )
            continue
        path_name = row["path"]
        if path_name in index:
            findings.append(
                Finding(
                    check_id="mutation-evidence",
                    rule_id="duplicate-evidence-row",
                    severity=Severity.CRITICAL,
                    message=(
                        f"{path_name}: evidence rows {positions[path_name]} and "
                        f"{position} claim the same path; evidence identities must "
                        "be unique and every row for this path is rejected"
                    ),
                )
            )
            continue
        index[path_name] = row
        positions[path_name] = position
    return index, findings


def _new_test_value_findings(
    ctx: ReviewContext,
    payload: Mapping[str, object],
    new_nodeids: Mapping[str, Sequence[str]],
) -> list[Finding]:
    findings: list[Finding] = []
    evidence_rows = payload.get("evidence", [])
    if not isinstance(evidence_rows, list):
        for gated_path, gated_ids in new_nodeids.items():
            findings.append(
                _receipt_unavailable_finding(
                    gated_path,
                    gated_ids,
                    "payload evidence envelope is malformed (not a list)",
                )
            )
        return findings
    evidence_by_path, malformed = _evidence_index(evidence_rows)
    findings.extend(malformed)
    from conductor.mutation_value import admission_errors

    for path, nodeids in new_nodeids.items():
        evidence = evidence_by_path.get(path)
        if not isinstance(evidence, dict):
            findings.append(
                Finding(
                    check_id="mutation-evidence",
                    rule_id="new-test-value-not-admitted",
                    severity=Severity.CRITICAL,
                    message=(
                        f"{path}: new test definition(s) lack admitted value "
                        f"evidence: {', '.join(nodeids)}"
                    ),
                    path=path,
                    help=(
                        "Mutation waivers exempt only legacy receipt debt; new test "
                        "definitions still need a value-classified PASS receipt."
                    ),
                )
            )
            continue
        receipt_name = evidence.get("receipt")
        if not isinstance(receipt_name, str):
            findings.append(
                _receipt_unavailable_finding(
                    path,
                    nodeids,
                    "evidence row lacks a string receipt name",
                )
            )
            continue
        try:
            receipt = json.loads((ctx.snapshot / receipt_name).read_text("utf-8"))
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
        for error in admission_errors(
            receipt.get("test_value") if isinstance(receipt, dict) else None,
            nodeids,
        ):
            findings.append(
                Finding(
                    check_id="mutation-evidence",
                    rule_id="new-test-value-not-admitted",
                    severity=Severity.CRITICAL,
                    message=f"{path}: {error}",
                    path=path,
                    help=(
                        "Bind the new test to a critical/high active-source contract, "
                        "record batch-level per-test attribution, and retain it as "
                        "CORE or explicitly justified INTENTIONAL_REDUNDANCY."
                    ),
                )
            )
    return findings


def _container_len(payload: Mapping[str, object], key: str) -> int:
    """Count payload rows without crashing on a malformed container."""

    rows = payload.get(key, [])
    return len(rows) if isinstance(rows, list) else 0


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
    registry = ctx.snapshot / "conductor/mutation_campaigns/registry.json"
    if not registry.is_file():
        finding = Finding(
            check_id="mutation-evidence",
            rule_id="mutation-registry-missing",
            severity=Severity.CRITICAL,
            message=(
                "candidate snapshot lacks conductor/mutation_campaigns/registry.json; "
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
            registry, test_paths, repo_root=ctx.snapshot, anchor_repo=ctx.repo
        )
    except CampaignError as exc:
        finding = Finding(
            check_id="mutation-evidence",
            rule_id="mutation-evidence-unavailable",
            severity=Severity.CRITICAL,
            message=f"mutation evidence could not be verified: {exc}",
        )
        return _result("mutation-evidence", started, [finding], files=test_paths)
    waiver_states = _waiver_states(ctx)
    active_paths = {str(s["path"]) for s in waiver_states if s["active"]}
    findings = _mutation_receipt_findings(payload, waived=active_paths)
    gated_nodeids: dict[str, tuple[str, ...]] = {}
    try:
        grandfathered = _load_grandfathered_nodeids(ctx)
    except _GrandfatherError as exc:
        findings.append(
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
            findings.append(
                Finding(
                    check_id="mutation-evidence",
                    rule_id="new-test-definition-unavailable",
                    severity=Severity.CRITICAL,
                    message=f"new test definitions could not be verified: {exc}",
                )
            )
    findings.extend(_new_test_value_findings(ctx, payload, gated_nodeids))
    return _result(
        "mutation-evidence",
        started,
        findings,
        files=test_paths,
        metrics={
            "checked_test_paths": payload.get("checked_test_paths", []),
            "covered_tests": _container_len(payload, "evidence"),
            "missing_tests": _container_len(payload, "missing_evidence"),
            "value_gated_nodeids": [
                nodeid for nodeids in gated_nodeids.values() for nodeid in nodeids
            ],
            "mutation_waiver_applied": sorted(active_paths),
            "mutation_waiver_states": sorted(
                waiver_states, key=lambda s: str(s["path"])
            ),
        },
    )


def check_test_evidence(ctx: ReviewContext) -> tuple[CheckResult, TestSelection]:
    started = time.perf_counter()
    selection = select_tests(ctx)
    result = _result(
        "test-evidence",
        started,
        selection.findings,
        files=selection.tests,
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
    if not selection.tests:
        return CheckResult(
            check_id=check.check_id,
            status=CheckStatus.SKIPPED,
            duration_ms=0,
            skipped_reason="no selected tests",
        )
    coverage_file = ctx.runtime_dir / ".coverage-targeted"
    shards = shard_tests(selection.tests, check.shard_max_files)
    total = len(shards)
    shard_files = [
        shard_data_file(coverage_file, index, total) for index in range(total)
    ]
    commands = [
        _pytest_command(tests, shard_files[index] if coverage else None)
        for index, tests in enumerate(shards)
    ]

    try:
        completed_raw, timed_out = execute_shards(ctx, commands, check)
    except OSError as exc:
        finding = Finding(
            check_id=check.check_id,
            rule_id="targeted-test-crash",
            severity=Severity.CRITICAL,
            message=f"targeted test execution did not complete: {type(exc).__name__}: {exc}",
        )
        return _result(check.check_id, started, [finding], files=selection.tests)

    timeout_findings = shard_timeout_findings(check, shards, timed_out, total)
    finished = [index for index in range(total) if completed_raw[index] is not None]
    if not finished:
        return _result(check.check_id, started, timeout_findings, files=selection.tests)
    completed_all = [completed for completed in completed_raw if completed is not None]
    shards = [shards[index] for index in finished]
    shard_files = [shard_files[index] for index in finished]

    findings, exit_codes, failed = shard_outcome_findings(
        check, shards, completed_all, len(selection.tests)
    )
    # Timeouts first: they explain any missing coverage the other shards cannot.
    findings = timeout_findings + findings
    metrics: dict[str, object] = {"selected_tests": len(selection.tests)}
    if total > 1:
        metrics["shard_count"] = total
        metrics["shard_workers"] = min(check.shard_workers, total)
        metrics["shard_exit_codes"] = exit_codes
    if not findings and coverage:
        if total > 1:
            combine_finding = combine_coverage(ctx, coverage_file, shard_files, check)
            if combine_finding is not None:
                findings.append(combine_finding)
        if not findings:
            coverage_findings, coverage_metrics = _evaluate_changed_coverage(
                ctx, coverage_file
            )
            findings.extend(coverage_findings)
            metrics.update(coverage_metrics)
    result = _result(
        check.check_id, started, findings, files=selection.tests, metrics=metrics
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


def _evaluate_changed_coverage(
    ctx: ReviewContext, coverage_file: Path
) -> tuple[list[Finding], dict[str, object]]:
    output = ctx.runtime_dir / "coverage.json"
    command = [
        sys.executable,
        "-m",
        "coverage",
        "json",
        f"--data-file={coverage_file}",
        "-o",
        str(output),
        "--quiet",
    ]
    completed = _run_process(
        command,
        ctx=ctx,
        timeout_seconds=60,
        memory_mb=2048,
        limit_resources=False,
        include_git_metadata=False,
    )
    if completed.returncode or not output.is_file():
        return [
            Finding(
                check_id="targeted-tests-full",
                rule_id="coverage-incomplete",
                severity=Severity.CRITICAL,
                message=(
                    completed.stderr
                    or completed.stdout
                    or "coverage JSON was not produced"
                ).strip(),
            )
        ], {}
    payload = json.loads(output.read_text(encoding="utf-8"))
    source_paths = [
        change.path
        for change in ctx.live_changes
        if "python" in change.classes and "test" not in change.classes
    ]
    changed = changed_line_numbers(ctx.repo, ctx.candidate, source_paths)
    covered, measurable, per_file = _coverage_counts(ctx, payload, changed)
    percent = 100.0 if measurable == 0 else covered * 100.0 / measurable
    threshold = (
        ctx.policy.high_risk_coverage_threshold
        if any(change.risk == "high" for change in ctx.live_changes)
        else ctx.policy.coverage_threshold
    )
    findings: list[Finding] = []
    if percent < threshold:
        findings.append(
            Finding(
                check_id="targeted-tests-full",
                rule_id="changed-code-coverage",
                severity=Severity.HIGH,
                message=f"changed-code coverage is {percent:.1f}%, below {threshold:.1f}%",
                evidence={
                    "covered": covered,
                    "measurable": measurable,
                    "per_file": per_file,
                },
            )
        )
    return findings, {
        "changed_coverage_percent": round(percent, 2),
        "changed_coverage_threshold": threshold,
        "changed_lines_measurable": measurable,
        "changed_lines_covered": covered,
    }


def _coverage_counts(
    ctx: ReviewContext,
    payload: object,
    changed: dict[str, set[int]],
) -> tuple[int, int, dict[str, dict[str, int]]]:
    if not isinstance(payload, dict) or not isinstance(payload.get("files"), dict):
        raise ValueError("coverage JSON has no files object")
    coverage_files = payload["files"]
    covered = 0
    measurable = 0
    per_file: dict[str, dict[str, int]] = {}
    for rel, changed_lines in changed.items():
        record = coverage_files.get(rel) or coverage_files.get(str(ctx.snapshot / rel))
        if not isinstance(record, dict):
            continue
        executed = set(record.get("executed_lines", []))
        missing = set(record.get("missing_lines", []))
        relevant = changed_lines & (executed | missing)
        hits = relevant & executed
        measurable += len(relevant)
        covered += len(hits)
        per_file[rel] = {"measurable": len(relevant), "covered": len(hits)}
    return covered, measurable, per_file
