"""Original, independently verified witnesses for explicit source contexts.

Canonical mutation evidence still comes from the unchanged native verifier. This
adapter never makes a combined receipt: it retains the native-verified originals
and requires a positive, complete witness separately for each declared source.
"""

from __future__ import annotations

import hashlib
import json
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any

from conductor.mutation_receipt_slim import ReceiptDetailError, expand_receipt
from conductor.mutation_value import admission_errors
from conductor.project_paths import conductor_table, registry_path


@dataclass(frozen=True, slots=True)
class ContextWitness:
    """One original attempt; validation authority belongs to the native reader."""

    receipt_path: str
    receipt_sha256: str
    receipt: Mapping[str, Any]
    native_validated: bool
    bindings_current: bool


def complete_pass_errors(receipt: Mapping[str, Any]) -> list[str]:
    """Reject nominal PASS outcomes that did not measure every reported kill."""
    errors = []
    baseline = receipt.get("baseline") or {}
    if receipt.get("status") != "PASS":
        errors.append("attempt is not PASS")
    if baseline.get("returncode") != 0 or baseline.get("timed_out") is not False:
        errors.append("baseline did not complete successfully")
    counts = receipt.get("outcome_counts") or {}
    for name in ("SURVIVED", "TIMED_OUT", "ERROR", "UNVIABLE"):
        if type(counts.get(name)) is not int or counts[name] != 0:
            errors.append(f"incomplete outcome count {name}")
    attribution = receipt.get("attribution") or {}
    killed = counts.get("KILLED")
    if (
        type(killed) is not int
        or killed <= 0
        or attribution.get("status") != "ATTRIBUTED"
        or attribution.get("attributed_mutants") != killed
        or attribution.get("killed_mutants") != killed
    ):
        errors.append("reported kills lack complete attribution")
    value = receipt.get("test_value")
    if not isinstance(value, Mapping) or value.get("status") != "PASS":
        errors.append("current PASS test-value evidence is missing")
    return errors


def declared_contexts(root: Path, nodeids: Sequence[str]) -> dict[str, tuple[str, ...]]:
    """Read exact node/source requirements; these declarations cannot waive gates."""
    from conductor.mutation_testing import CampaignError, _safe_relative_path

    raw = conductor_table(root).get("mutation_value_contexts", {})
    if not isinstance(raw, Mapping):
        raise CampaignError(
            "mutation_value_contexts must be a nodeid/source-path table"
        )
    result = {}
    for nodeid in nodeids:
        if nodeid not in raw:
            continue
        sources = raw[nodeid]
        if (
            not isinstance(sources, list)
            or not sources
            or any(not isinstance(source, str) for source in sources)
        ):
            raise CampaignError(f"{nodeid}: source contexts must be a nonempty list")
        result[nodeid] = tuple(
            sorted(
                {
                    _safe_relative_path(source, "required source context")
                    for source in sources
                }
            )
        )
    return result


def _stamp(witness: ContextWitness) -> datetime:
    value = witness.receipt.get("generated_at")
    if not isinstance(value, str):
        raise TypeError("attempt timestamp is missing")
    stamp = datetime.fromisoformat(value)
    if stamp.tzinfo is None:
        raise ValueError("attempt timestamp has no timezone")
    return stamp


def _context(witness: ContextWitness) -> str:
    fields = (
        "campaign_id",
        "manifest_sha256",
        "source_sha256",
        "test_sha256",
        "runner_sha256",
        "runner_components_sha256",
    )
    return json.dumps({key: witness.receipt.get(key) for key in fields}, sort_keys=True)


def _node_signature(witness: ContextWitness, nodeid: str) -> str:
    value = witness.receipt.get("test_value") or {}
    rows = [row for row in value.get("tests", []) if row.get("nodeid") == nodeid]
    return json.dumps(rows, sort_keys=True)


def _source_kill(receipt: Mapping[str, Any], nodeid: str, source: str) -> bool:
    value = receipt.get("test_value") or {}
    rows = [row for row in value.get("tests", []) if row.get("nodeid") == nodeid]
    killed = {mutation for row in rows for mutation in row.get("killed_mutants", [])}
    return any(
        row.get("id") in killed
        and row.get("path") == source
        and row.get("outcome") == "KILLED"
        for row in receipt.get("mutants", [])
    )


def _latest_context_attempts(
    attempts: Sequence[ContextWitness],
    nodeid: str,
) -> tuple[list[ContextWitness], list[str]]:
    try:
        latest_stamp = max(_stamp(attempt) for attempt in attempts)
        latest = [attempt for attempt in attempts if _stamp(attempt) == latest_stamp]
    except (TypeError, ValueError) as exc:
        return [], [str(exc)]
    signatures = {
        _node_signature(attempt, nodeid)
        for attempt in attempts
        if attempt.native_validated and not complete_pass_errors(attempt.receipt)
    }
    if len(signatures) > 1:
        return [], ["conflicting classifications in the same final context"]
    if any(
        not attempt.native_validated or complete_pass_errors(attempt.receipt)
        for attempt in latest
    ):
        return [], [
            "latest attempt in required context is incomplete or failed native verification"
        ]
    return latest, []


def _current_context_groups(
    witnesses: Sequence[ContextWitness],
) -> list[list[ContextWitness]]:
    groups: dict[str, list[ContextWitness]] = {}
    for witness in witnesses:
        if witness.bindings_current:
            groups.setdefault(_context(witness), []).append(witness)
    return list(groups.values())


def contextual_admission_errors(
    nodeid: str, required_sources: Sequence[str], witnesses: Sequence[ContextWitness]
) -> list[str]:
    """Require compatible positive originals, never a best-label global union."""
    if not required_sources:
        return [f"new test {nodeid!r} has no verified required source context"]
    accepted: set[str] = set()
    debt = []
    for attempts in _current_context_groups(witnesses):
        latest, errors = _latest_context_attempts(attempts, nodeid)
        debt.extend(errors)
        for attempt in latest:
            errors = admission_errors(attempt.receipt.get("test_value"), [nodeid])
            if errors:
                debt.extend(errors)
                continue
            accepted.update(
                source
                for source in required_sources
                if _source_kill(attempt.receipt, nodeid, source)
            )
    missing = set(required_sources) - accepted
    reason = "; ".join(sorted(set(debt))) or "no positive current complete witness"
    return [
        f"new test {nodeid!r} lacks admitted source context {source!r}: {reason}"
        for source in sorted(missing)
    ]


def prepare_context_admission(
    root: Path,
    anchor: Path,
    runtime: Path,
    new_nodeids: Mapping[str, Sequence[str]],
) -> tuple[dict[str, tuple[str, ...]], list[ContextWitness]]:
    """Retain original descriptors in review artifacts alongside canonical evidence."""
    from conductor.mutation_testing_support import atomic_json

    contexts = declared_contexts(
        root, [node for nodes in new_nodeids.values() for node in nodes]
    )
    witnesses = (
        collect_context_witnesses(root, anchor, list(new_nodeids)) if contexts else []
    )
    if contexts:
        atomic_json(
            runtime / "contextual_mutation_witnesses.json",
            {
                "required_sources": contexts,
                "originals": [
                    {
                        "receipt": witness.receipt_path,
                        "sha256": witness.receipt_sha256,
                        "native_validated": witness.native_validated,
                        "bindings_current": witness.bindings_current,
                        "campaign_id": witness.receipt.get("campaign_id"),
                        "manifest_sha256": witness.receipt.get("manifest_sha256"),
                        "source_sha256": witness.receipt.get("source_sha256"),
                        "test_sha256": witness.receipt.get("test_sha256"),
                        "runner_components_sha256": witness.receipt.get(
                            "runner_components_sha256"
                        ),
                        "test_value": witness.receipt.get("test_value"),
                    }
                    for witness in witnesses
                ],
            },
        )
    return contexts, witnesses


def _summary_current(
    receipt: Mapping[str, Any], campaign: Any, runner: Mapping[str, Any]
) -> bool:
    """Identify current failed attempts too, so older PASS cannot hide one."""
    return (
        receipt.get("campaign_id") == campaign.campaign_id
        and receipt.get("manifest_sha256") == campaign.manifest_sha256
        and receipt.get("source_sha256") == dict(campaign.source_sha256)
        and receipt.get("test_sha256") == dict(campaign.test_sha256)
        and receipt.get("runner_sha256") == runner["mutation_testing_sha256"]
        and receipt.get("runner_components_sha256") == runner["components"]
    )


def collect_context_witnesses(
    root: Path, anchor_repo: Path, test_paths: Sequence[str]
) -> list[ContextWitness]:
    """Keep actual registered, singleton-native-validated original attempts."""
    from conductor import mutation_testing as testing
    from conductor._native import (
        plan_mutation_evidence_native,
        verify_mutation_evidence_native,
    )

    registry = registry_path(root)
    relative = registry.relative_to(root).as_posix()
    plan = json.loads(
        plan_mutation_evidence_native(
            json.dumps({"repo_root": str(root), "registry_path": relative})
        )
    )
    request = testing._native_verification_request(
        relative, test_paths, repo_root=root, anchor_repo=anchor_repo, plan=plan
    )
    receipts, malformed = testing._support.load_receipts(
        root,
        request["receipt_directories"],
        safe_relative=testing._safe_relative_path,
        require_mapping=testing._require_mapping,
        error_type=testing.CampaignError,
    )
    if malformed:
        raise testing.CampaignError(
            "context receipts could not decode: " + "; ".join(malformed)
        )
    witnesses = []
    paths = set(test_paths)
    for campaign in testing._registry_campaigns(registry, repo_root=root, strict=False):
        bound = sorted(paths & set(campaign.test_sha256))
        if not bound:
            continue
        contract = testing._native_campaign_contract(campaign, repo_root=root)
        result = json.loads(
            verify_mutation_evidence_native(
                json.dumps(
                    {
                        **request,
                        "candidate_paths": bound,
                        "campaigns_override": [contract],
                    }
                )
            )
        )
        accepted = {row["receipt"] for row in result["evidence"]}
        for path, summary, raw in receipts:
            if not _summary_current(summary, campaign, request["runner"]):
                continue
            name = path.relative_to(root).as_posix()
            try:
                receipt = expand_receipt(summary)
            except ReceiptDetailError as exc:
                raise testing.CampaignError(
                    f"{name}: cannot expand original context: {exc}"
                ) from exc
            witnesses.append(
                ContextWitness(
                    name,
                    hashlib.sha256(raw).hexdigest(),
                    receipt,
                    bool(accepted)
                    and not testing._receipt_errors(
                        summary,
                        campaign,
                        root,
                        receipt_path=path,
                        receipt_bytes=raw,
                        anchor_repo=anchor_repo,
                    ),
                    True,
                )
            )
    return witnesses
