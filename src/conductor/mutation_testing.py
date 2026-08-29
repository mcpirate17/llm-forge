"""Fail-closed, language-neutral orchestration for explicit mutation campaigns.

The framework deliberately does not generate mutants. A campaign names small,
reviewable patch files and the exact tests that must detect them. Every baseline
and mutant runs in a disposable snapshot of the current worktree, never in the
shared checkout.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass, field
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
from typing import Any, Iterable, Mapping, Sequence

from audit.orchestrator.snapshot_worktree import isolated_snapshot
from conductor import mutation_testing_support as _support
from conductor.mutation_scope import (
    CampaignError,
    TestFileScope,
    _load_test_scopes,
    _require_mapping,
    _require_string,
    _require_string_list,
    _safe_relative_path,
    _test_scope_errors,
    _test_scopes_payload,
)
from conductor.mutation_value import (
    ValueAnalysisSpec,
    ValueEvidenceError,
    analyze_test_value,
    collect_pytest_junit_batch,
    load_value_analysis,
    test_value_receipt_errors,
    value_inspection_payload,
)


SCHEMA_VERSION = 1
REGISTRY_SCHEMA_VERSION = 1
RECEIPT_SCHEMA = "llm.mutation-testing.receipt.v3"
LEGACY_RECEIPT_SCHEMA = "llm.mutation-testing.receipt.v2"
LEGACY_RECEIPT_ANCHOR_COMMIT = "61343f575215dd222a74fc2c060d0328692ded5e"
LEGACY_RECEIPT_ANCHOR_TREE = "b01877ba62c32445f7649450f9b395dd70de306a"
LEGACY_RECEIPT_PREFIX = "conductor/mutation_campaigns/receipts/"
CANONICAL_TEST_PATTERNS = _support.CANONICAL_TEST_PATTERNS
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
OUTPUT_TAIL_CHARS = 12_000
REPO_ROOT = Path(__file__).resolve().parents[1]
RUNNER_COMPONENT_PATHS = (
    "audit/orchestrator/snapshot_worktree.py",
    "conductor/mutation_scope.py",
    "conductor/mutation_testing.py",
    "conductor/mutation_testing_support.py",
    "conductor/mutation_value.py",
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


def _load_ranked_tests(value: object) -> tuple[RankedTest, ...]:
    if not isinstance(value, list) or not value:
        raise CampaignError("ranked_tests must be a non-empty list")
    tests: list[RankedTest] = []
    for index, raw in enumerate(value, start=1):
        row = _require_mapping(raw, f"ranked_tests[{index - 1}]")
        rank = row.get("rank")
        if not isinstance(rank, int):
            raise CampaignError(f"ranked_tests[{index - 1}].rank must be an integer")
        tests.append(
            RankedTest(
                rank=rank,
                nodeid=_require_string(row.get("nodeid"), "ranked test nodeid"),
                contract=_require_string(row.get("contract"), "ranked test contract"),
                rationale=_require_string(
                    row.get("rationale"), "ranked test rationale"
                ),
            )
        )
    ranks = [test.rank for test in tests]
    if ranks != list(range(1, len(tests) + 1)):
        raise CampaignError(
            f"ranked_tests must be ordered with contiguous ranks, got {ranks}"
        )
    nodeids = [test.nodeid for test in tests]
    if len(set(nodeids)) != len(nodeids):
        raise CampaignError("ranked_tests contains duplicate nodeids")
    return tuple(tests)


def _load_planned_mutations(value: object) -> tuple[PlannedMutation, ...]:
    if not isinstance(value, list):
        raise CampaignError("planned_mutations must be a list")
    planned: list[PlannedMutation] = []
    for index, raw in enumerate(value):
        row = _require_mapping(raw, f"planned_mutations[{index}]")
        planned.append(
            PlannedMutation(
                mutation_id=_require_string(
                    row.get("id"), f"planned_mutations[{index}].id"
                ),
                target_path=_safe_relative_path(
                    row.get("target_path"),
                    f"planned_mutations[{index}].target_path",
                ),
                contract=_require_string(
                    row.get("contract"), f"planned_mutations[{index}].contract"
                ),
                description=_require_string(
                    row.get("description"),
                    f"planned_mutations[{index}].description",
                ),
                expected_killers=_require_string_list(
                    row.get("expected_killers", []),
                    f"planned_mutations[{index}].expected_killers",
                ),
            )
        )
    ids = [mutation.mutation_id for mutation in planned]
    if len(set(ids)) != len(ids):
        raise CampaignError("planned_mutations contains duplicate ids")
    return tuple(planned)


def _patch_paths(patch_path: Path) -> tuple[str, ...]:
    """Extract and validate repository-relative paths from a unified diff."""

    paths: list[str] = []
    diff_paths: list[str] = []
    old_paths: list[str] = []
    try:
        lines = patch_path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError) as exc:
        raise CampaignError(f"cannot read mutation patch {patch_path}: {exc}") from exc
    for line in lines:
        if line.startswith(("rename from ", "rename to ", "copy from ", "copy to ")):
            raise CampaignError("mutation patches may not rename or copy files")
        if line.startswith("GIT binary patch") or line.startswith("Binary files "):
            raise CampaignError("mutation patches must be textual unified diffs")
        if line.startswith("diff --git "):
            fields = line.split()
            if (
                len(fields) != 4
                or not fields[2].startswith("a/")
                or not fields[3].startswith("b/")
            ):
                raise CampaignError(f"unsupported mutation diff header: {line!r}")
            old = _safe_relative_path(fields[2][2:], "mutation diff old path")
            new = _safe_relative_path(fields[3][2:], "mutation diff new path")
            if old != new:
                raise CampaignError("mutation patches may not rename files")
            diff_paths.append(new)
        elif line.startswith("--- "):
            raw = line[4:].split("\t", 1)[0]
            if raw == "/dev/null":
                raise CampaignError("mutation patches may not create or delete files")
            if not raw.startswith("a/"):
                raise CampaignError(f"unsupported mutation patch path: {raw!r}")
            old_paths.append(_safe_relative_path(raw[2:], "mutation patch path"))
        elif line.startswith("+++ "):
            raw = line[4:].split("\t", 1)[0]
            if raw == "/dev/null":
                raise CampaignError("mutation patches may not create or delete files")
            if not raw.startswith("b/"):
                raise CampaignError(f"unsupported mutation patch path: {raw!r}")
            paths.append(_safe_relative_path(raw[2:], "mutation patch path"))
    if not paths:
        raise CampaignError(f"mutation patch contains no modified paths: {patch_path}")
    if not diff_paths or sorted(diff_paths) != sorted(paths):
        raise CampaignError("mutation patch diff headers do not match modified paths")
    if sorted(old_paths) != sorted(paths):
        raise CampaignError("mutation patch old/new paths do not match")
    return tuple(sorted(set(paths)))


def _load_mutations(
    value: object, manifest_path: Path, repo_root: Path
) -> tuple[Mutation, ...]:
    if not isinstance(value, list):
        raise CampaignError("mutations must be a list")
    mutations: list[Mutation] = []
    for index, raw in enumerate(value):
        row = _require_mapping(raw, f"mutations[{index}]")
        patch_rel = _safe_relative_path(
            row.get("patch_file"), f"mutations[{index}].patch_file"
        )
        patch_path = (manifest_path.parent / patch_rel).resolve()
        try:
            patch_path.relative_to(repo_root.resolve())
        except ValueError as exc:
            raise CampaignError(
                f"mutations[{index}].patch_file escapes the repository"
            ) from exc
        allowed_paths = tuple(
            _safe_relative_path(item, f"mutations[{index}].allowed_paths")
            for item in _require_string_list(
                row.get("allowed_paths", []), f"mutations[{index}].allowed_paths"
            )
        )
        if not allowed_paths:
            raise CampaignError(f"mutations[{index}].allowed_paths may not be empty")
        patch_sha256 = _require_string(
            row.get("patch_sha256"), f"mutations[{index}].patch_sha256"
        )
        if not SHA256_RE.fullmatch(patch_sha256):
            raise CampaignError(
                f"mutations[{index}].patch_sha256 must be a lowercase SHA-256 digest"
            )
        actual_patch_sha256 = _sha256(patch_path) if patch_path.is_file() else None
        if actual_patch_sha256 != patch_sha256:
            raise CampaignError(
                f"mutation {row.get('id')!r} patch hash drifted: "
                f"expected {patch_sha256}, got {actual_patch_sha256}"
            )
        actual_paths = _patch_paths(patch_path)
        if actual_paths != tuple(sorted(set(allowed_paths))):
            raise CampaignError(
                f"mutation {row.get('id')!r} patch paths {actual_paths} do not match "
                f"allowed_paths {tuple(sorted(set(allowed_paths)))}"
            )
        mutations.append(
            Mutation(
                mutation_id=_require_string(row.get("id"), f"mutations[{index}].id"),
                patch_file=patch_path,
                patch_sha256=patch_sha256,
                allowed_paths=actual_paths,
                expected_killers=_require_string_list(
                    row.get("expected_killers", []),
                    f"mutations[{index}].expected_killers",
                ),
            )
        )
    ids = [mutation.mutation_id for mutation in mutations]
    if len(set(ids)) != len(ids):
        raise CampaignError("mutations contains duplicate ids")
    return tuple(mutations)


def load_campaign(path: Path, *, repo_root: Path = REPO_ROOT) -> Campaign:
    """Load and structurally validate a mutation campaign manifest."""

    manifest_path = path.resolve()
    try:
        manifest_path.relative_to(repo_root.resolve())
    except ValueError as exc:
        raise CampaignError("campaign manifest must be inside the repository") from exc
    try:
        raw = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise CampaignError(f"cannot load campaign {manifest_path}: {exc}") from exc
    payload = _require_mapping(raw, "campaign")
    if payload.get("schema_version") != SCHEMA_VERSION:
        raise CampaignError(
            f"unsupported schema_version={payload.get('schema_version')!r}; "
            f"expected {SCHEMA_VERSION}"
        )
    expected_mutations = payload.get("expected_mutations")
    if not isinstance(expected_mutations, int) or expected_mutations < 1:
        raise CampaignError("expected_mutations must be a positive integer")
    source_raw = _require_mapping(payload.get("source_sha256"), "source_sha256")
    source_sha256: dict[str, str] = {}
    for raw_path, raw_digest in source_raw.items():
        source_path = _safe_relative_path(raw_path, "source_sha256 path")
        digest = _require_string(raw_digest, f"source_sha256[{source_path}]")
        if not SHA256_RE.fullmatch(digest):
            raise CampaignError(
                f"source_sha256[{source_path}] must be a lowercase SHA-256 digest"
            )
        source_sha256[source_path] = digest
    ranked_tests = _load_ranked_tests(payload.get("ranked_tests"))
    expected_ranked_tests = payload.get("expected_ranked_tests", len(ranked_tests))
    if expected_ranked_tests != len(ranked_tests):
        raise CampaignError(
            f"expected_ranked_tests={expected_ranked_tests!r}, "
            f"but {len(ranked_tests)} tests are ranked"
        )
    planned = _load_planned_mutations(payload.get("planned_mutations", []))
    if len(planned) != expected_mutations:
        raise CampaignError(
            f"expected {expected_mutations} planned mutation slots, got {len(planned)}"
        )
    mutations = _load_mutations(payload.get("mutations", []), manifest_path, repo_root)
    planned_ids = {mutation.mutation_id for mutation in planned}
    unknown_ids = {
        mutation.mutation_id
        for mutation in mutations
        if mutation.mutation_id not in planned_ids
    }
    if unknown_ids:
        raise CampaignError(
            f"materialized mutations lack planned slots: {sorted(unknown_ids)}"
        )
    baseline = _require_mapping(payload.get("baseline"), "baseline")
    test_argv = _require_string_list(baseline.get("argv"), "baseline.argv")
    timeout_seconds = baseline.get("timeout_seconds")
    if not isinstance(timeout_seconds, int) or timeout_seconds < 1:
        raise CampaignError("baseline.timeout_seconds must be a positive integer")
    missing_tests = [
        test.nodeid
        for test in ranked_tests
        if test.nodeid not in test_argv
        and test.nodeid.split("::", 1)[0] not in test_argv
    ]
    if missing_tests:
        raise CampaignError(f"baseline.argv omits ranked tests: {missing_tests}")
    resource_gate = _require_mapping(payload.get("resource_gate", {}), "resource_gate")
    poll_seconds = resource_gate.get("poll_seconds", 30)
    if not isinstance(poll_seconds, int) or poll_seconds < 1 or poll_seconds > 300:
        raise CampaignError("resource_gate.poll_seconds must be in [1, 300]")
    environment_raw = _require_mapping(payload.get("environment", {}), "environment")
    environment: dict[str, str] = {}
    for raw_key, raw_value in environment_raw.items():
        key = _require_string(raw_key, "environment key")
        if not isinstance(raw_value, str):
            raise CampaignError(f"environment[{key!r}] must be a string")
        environment[key] = raw_value
    test_scopes = _load_test_scopes(
        payload.get("test_scopes", {}),
        source_sha256=source_sha256,
        ranked_tests=ranked_tests,
        repo_root=repo_root,
    )
    try:
        value_analysis = load_value_analysis(
            payload.get("value_analysis"),
            ranked_nodeids=[test.nodeid for test in ranked_tests],
            mutation_ids=[mutation.mutation_id for mutation in planned],
            source_paths=list(source_sha256),
        )
    except ValueEvidenceError as exc:
        raise CampaignError(f"invalid value_analysis: {exc}") from exc
    campaign = Campaign(
        manifest_path=manifest_path,
        manifest_sha256=_sha256(manifest_path),
        campaign_id=_require_string(payload.get("campaign_id"), "campaign_id"),
        title=_require_string(payload.get("title"), "title"),
        language=_require_string(payload.get("language"), "language"),
        mutation_engine=_require_string(
            payload.get("mutation_engine"), "mutation_engine"
        ),
        expected_mutations=expected_mutations,
        source_sha256=source_sha256,
        ranked_tests=ranked_tests,
        planned_mutations=planned,
        mutations=mutations,
        test_argv=test_argv,
        timeout_seconds=timeout_seconds,
        blocked_process_substrings=_require_string_list(
            resource_gate.get("blocked_process_substrings", []),
            "resource_gate.blocked_process_substrings",
        ),
        poll_seconds=poll_seconds,
        environment=environment,
        host_read_dependencies=tuple(
            _safe_relative_path(item, "host_read_dependencies")
            for item in _require_string_list(
                payload.get("host_read_dependencies", []),
                "host_read_dependencies",
            )
        ),
        test_scopes=test_scopes,
        value_analysis=value_analysis,
    )
    _validate_cross_references(campaign)
    return campaign


def _validate_cross_references(campaign: Campaign) -> None:
    ranked = {test.nodeid for test in campaign.ranked_tests}
    for planned in campaign.planned_mutations:
        missing = sorted(set(planned.expected_killers) - ranked)
        if missing:
            raise CampaignError(
                f"planned mutation {planned.mutation_id!r} has unranked killers: {missing}"
            )
        if planned.target_path not in campaign.source_sha256:
            raise CampaignError(
                f"planned mutation {planned.mutation_id!r} targets an unbound source: "
                f"{planned.target_path}"
            )
    planned_by_id = {
        mutation.mutation_id: mutation for mutation in campaign.planned_mutations
    }
    for mutation in campaign.mutations:
        planned = planned_by_id[mutation.mutation_id]
        if tuple(mutation.expected_killers) != tuple(planned.expected_killers):
            raise CampaignError(
                f"mutation {mutation.mutation_id!r} expected_killers drifted from its slot"
            )
        if planned.target_path not in mutation.allowed_paths:
            raise CampaignError(
                f"mutation {mutation.mutation_id!r} does not patch its planned target"
            )
        unbound = sorted(set(mutation.allowed_paths) - set(campaign.source_sha256))
        if unbound:
            raise CampaignError(
                f"mutation {mutation.mutation_id!r} patches unbound sources: {unbound}"
            )


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


def source_drift(campaign: Campaign, root: Path) -> list[dict[str, Any]]:
    """Return every absent or hash-drifted source bound by the campaign."""

    drift: list[dict[str, Any]] = []
    for relative, expected in campaign.source_sha256.items():
        path = root / relative
        actual = _sha256(path) if path.is_file() and not path.is_symlink() else None
        if actual != expected:
            drift.append(
                {
                    "path": relative,
                    "expected_sha256": expected,
                    "actual_sha256": actual,
                    "is_symlink": path.is_symlink(),
                }
            )
    return drift


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
    manifest_actual = (
        _sha256(campaign.manifest_path) if campaign.manifest_path.is_file() else None
    )
    manifest_drift = manifest_actual != campaign.manifest_sha256
    blockers = blocking_processes(campaign.blocked_process_substrings)
    readiness_reasons: list[str] = []
    if len(campaign.mutations) != campaign.expected_mutations:
        readiness_reasons.append(
            f"materialized_mutations={len(campaign.mutations)}; "
            f"expected={campaign.expected_mutations}"
        )
    if drift:
        readiness_reasons.append(f"source_hash_drift={len(drift)}")
    if manifest_drift:
        readiness_reasons.append("manifest_hash_drift=1")
    status = "READY" if not readiness_reasons else "NOT_READY"
    return {
        "schema_version": "llm.mutation-testing.inspect.v1",
        "campaign_id": campaign.campaign_id,
        "title": campaign.title,
        "status": status,
        "readiness_reasons": readiness_reasons,
        "resource_status": "BUSY" if blockers else "IDLE",
        "blocking_processes": blockers,
        "source_drift": drift,
        "manifest_sha256": campaign.manifest_sha256,
        "manifest_hash_drift": manifest_drift,
        "expected_mutations": campaign.expected_mutations,
        "materialized_mutations": len(campaign.mutations),
        "ranked_tests": [
            {
                "rank": test.rank,
                "nodeid": test.nodeid,
                "contract": test.contract,
                "rationale": test.rationale,
            }
            for test in campaign.ranked_tests
        ],
        "test_scopes": _test_scopes_payload(campaign),
        "value_analysis": value_inspection_payload(campaign.value_analysis),
        "planned_mutations": [
            {
                "id": mutation.mutation_id,
                "target_path": mutation.target_path,
                "contract": mutation.contract,
                "description": mutation.description,
                "expected_killers": list(mutation.expected_killers),
                "materialized": any(
                    ready.mutation_id == mutation.mutation_id
                    for ready in campaign.mutations
                ),
            }
            for mutation in campaign.planned_mutations
        ],
    }


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
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
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
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "status": "RUNNING",
        "source_sha256": dict(campaign.source_sha256),
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
    registry_path = path.resolve()
    try:
        registry_path.relative_to(repo_root.resolve())
    except ValueError as exc:
        raise CampaignError("mutation registry must be inside the repository") from exc
    try:
        payload = _require_mapping(
            json.loads(registry_path.read_text(encoding="utf-8")), "registry"
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise CampaignError(
            f"cannot load mutation registry {registry_path}: {exc}"
        ) from exc
    if payload.get("schema_version") != REGISTRY_SCHEMA_VERSION:
        raise CampaignError(
            f"unsupported registry schema_version={payload.get('schema_version')!r}; "
            f"expected {REGISTRY_SCHEMA_VERSION}"
        )
    if payload.get("enforcement") != "changed_tests":
        raise CampaignError("registry enforcement must be 'changed_tests'")
    patterns = _require_string_list(
        payload.get("test_patterns"), "registry.test_patterns"
    )
    if patterns != CANONICAL_TEST_PATTERNS:
        raise CampaignError("registry.test_patterns must match the canonical inventory")
    _require_string_list(
        payload.get("receipt_directories"), "registry.receipt_directories"
    )
    campaigns = payload.get("campaigns")
    if not isinstance(campaigns, list) or not campaigns:
        raise CampaignError("registry.campaigns must be a non-empty list")
    return payload


def _receipt_errors(
    receipt: Mapping[str, Any],
    campaign: Campaign,
    repo_root: Path,
    receipt_path: Path | None = None,
    receipt_bytes: bytes | None = None,
    anchor_repo: Path | None = None,
) -> list[str]:
    errors: list[str] = []
    expected_manifest = campaign.manifest_path.relative_to(repo_root).as_posix()
    schema = receipt.get("schema_version")
    anchored_legacy = schema == LEGACY_RECEIPT_SCHEMA
    if anchored_legacy:
        errors.extend(
            _support.legacy_receipt_anchor_errors(
                receipt_path,
                receipt_bytes,
                repo_root,
                anchor_repo or repo_root,
                anchor_commit=LEGACY_RECEIPT_ANCHOR_COMMIT,
                anchor_tree=LEGACY_RECEIPT_ANCHOR_TREE,
                receipt_prefix=LEGACY_RECEIPT_PREFIX,
                manifest_path=expected_manifest,
                manifest_sha256=campaign.manifest_sha256,
            )
        )
    elif schema != RECEIPT_SCHEMA:
        errors.append("receipt schema is not current")
    if receipt.get("status") != "PASS":
        errors.append(f"status={receipt.get('status')!r}")
    if receipt.get("campaign_id") != campaign.campaign_id:
        errors.append("campaign_id mismatch")
    if receipt.get("manifest") != expected_manifest:
        errors.append("manifest path mismatch")
    if receipt.get("manifest_sha256") != campaign.manifest_sha256:
        errors.append("manifest hash mismatch")
    if not anchored_legacy:
        try:
            runner_components = _runner_components_sha256()
        except CampaignError as exc:
            errors.append(str(exc))
        else:
            if (
                receipt.get("runner_sha256")
                != runner_components["conductor/mutation_testing.py"]
            ):
                errors.append("runner hash mismatch")
            if receipt.get("runner_components_sha256") != runner_components:
                errors.append("runner component hash map mismatch")
    if receipt.get("source_sha256") != dict(campaign.source_sha256):
        errors.append("source hash map mismatch")
    if receipt.get("test_scopes", {}) != _test_scopes_payload(campaign):
        errors.append("test scope map mismatch")
    if receipt.get("complete_campaign") is not True:
        errors.append("partial campaign receipt")
    expected_ids = [mutation.mutation_id for mutation in campaign.mutations]
    if receipt.get("selected_mutations") != expected_ids:
        errors.append("selected mutation ids mismatch")
    rows = receipt.get("mutants")
    if not isinstance(rows, list):
        errors.append("mutants must be a list")
    else:
        actual = {
            row.get("id"): row
            for row in rows
            if isinstance(row, dict) and isinstance(row.get("id"), str)
        }
        if list(actual) != expected_ids:
            errors.append("mutant result ids mismatch")
        for mutation in campaign.mutations:
            row = actual.get(mutation.mutation_id, {})
            if row.get("outcome") != "KILLED":
                errors.append(f"mutant {mutation.mutation_id} was not killed")
            if row.get("patch_sha256") != mutation.patch_sha256:
                errors.append(f"mutant {mutation.mutation_id} patch hash mismatch")
    if receipt.get("mutation_score") != 1.0:
        errors.append("mutation score is not 1.0")
    if campaign.value_analysis is not None:
        errors.extend(
            test_value_receipt_errors(
                receipt.get("test_value"),
                expected_nodeids=[test.nodeid for test in campaign.ranked_tests],
                expected_repetitions=campaign.value_analysis.baseline_repetitions,
            )
        )
    if source_drift(campaign, repo_root):
        errors.append("current source hashes drifted")
    return errors


def verify_evidence(
    registry_path: Path,
    paths: Sequence[str],
    *,
    repo_root: Path = REPO_ROOT,
    anchor_repo: Path | None = None,
) -> dict[str, Any]:
    """Require current full-campaign PASS receipts for every changed test path."""

    payload = _load_registry(registry_path, repo_root)
    normalized = tuple(_safe_relative_path(path, "candidate path") for path in paths)
    campaign_rows = payload.get("campaigns")
    assert isinstance(campaign_rows, list)
    campaigns: list[Campaign] = []
    for index, raw in enumerate(campaign_rows):
        row = _require_mapping(raw, f"registry.campaigns[{index}]")
        manifest = _safe_relative_path(
            row.get("manifest"), f"registry.campaigns[{index}].manifest"
        )
        campaigns.append(load_campaign(repo_root / manifest, repo_root=repo_root))

    receipts, malformed = _support.load_receipts(
        repo_root,
        _require_string_list(payload.get("receipt_directories"), "receipt_directories"),
        safe_relative=_safe_relative_path,
        require_mapping=_require_mapping,
        error_type=CampaignError,
    )

    evidence: list[dict[str, Any]] = []
    missing: list[dict[str, Any]] = []
    for test_path in sorted(set(normalized)):
        matching = [
            campaign
            for campaign in campaigns
            if test_path in campaign.source_sha256
            and any(
                ranked.nodeid.split("::", 1)[0] == test_path
                for ranked in campaign.ranked_tests
            )
        ]
        accepted: tuple[Campaign, Path] | None = None
        rejection_reasons: list[str] = []
        for campaign in matching:
            scope_errors = _test_scope_errors(campaign, test_path)
            if scope_errors:
                rejection_reasons.append(
                    f"{campaign.campaign_id}: {', '.join(scope_errors)}"
                )
                continue
            campaign_receipts = [
                (path, receipt, raw_bytes)
                for path, receipt, raw_bytes in receipts
                if receipt.get("campaign_id") == campaign.campaign_id
            ]
            for path, receipt, raw_bytes in reversed(campaign_receipts):
                errors = _receipt_errors(
                    receipt,
                    campaign,
                    repo_root,
                    path,
                    raw_bytes,
                    anchor_repo or repo_root,
                )
                if not errors:
                    accepted = (campaign, path)
                    break
                rejection_reasons.append(f"{path.name}: {', '.join(errors)}")
            if accepted is not None:
                break
        if accepted is None:
            missing.append(
                {
                    "path": test_path,
                    "reason": (
                        "no registered campaign ranks this test file"
                        if not matching
                        else "no current complete PASS receipt"
                    ),
                    "receipt_rejections": rejection_reasons,
                }
            )
        else:
            campaign, receipt_path = accepted
            evidence.append(
                {
                    "path": test_path,
                    "campaign_id": campaign.campaign_id,
                    "receipt": receipt_path.relative_to(repo_root).as_posix(),
                    "scope": _test_scopes_payload(campaign)[test_path],
                }
            )
    return _support.evidence_result(normalized, evidence, missing, malformed)


def _json_print(payload: Mapping[str, Any]) -> None:
    print(json.dumps(payload, indent=2, sort_keys=True))


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
