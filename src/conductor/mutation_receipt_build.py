"""Receipt construction and scoring for mutation campaigns.

Split out of the runner so the orchestration in `mutation_testing` stays about
running things, and because these two ends are what a receipt's integrity rests
on: `_open_receipt` captures every hash binding from the tree as it is before
anything runs, and `_score_receipt` decides the status those bindings vouch for.
Both are declared runner components -- a change in here changes what a receipt
means, so it must invalidate the receipts pinned to the old behaviour.
"""

from __future__ import annotations

import sys
from collections.abc import Mapping, Sequence
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from conductor.mutation_campaign_model import (
    RECEIPT_SCHEMA,
    Campaign,
    Mutation,
)
from conductor import mutation_testing_support as _support
from conductor.mutation_campaign_model import _runner_components_sha256
from conductor.mutation_scope import CampaignError, _test_scopes_payload
from conductor.mutation_value import analyze_test_value
from conductor.project_paths import mutation_receipt_root_relative


_BARE_INTERPRETERS = frozenset({"python", "python3"})


def _utc_stamp() -> str:
    return datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")


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


def _default_receipt_path(campaign: Campaign, repo_root: Path) -> Path:
    """Where a receipt lands absent an explicit ``--receipt``.

    The directory is host-configurable (``[tool.conductor].mutation_receipt_root``)
    and created on demand -- a host with no scratch-output tree of its own must not
    have to create one by hand just to run a campaign without an explicit path.
    """

    directory = repo_root / mutation_receipt_root_relative(repo_root).as_posix()
    try:
        directory.mkdir(parents=True, exist_ok=True)
    except OSError as exc:
        raise CampaignError(
            f"cannot create mutation receipt directory {directory}: {exc}"
        ) from exc
    return directory / f"{campaign.campaign_id}_{_utc_stamp()}.json"


def _open_receipt(
    campaign: Campaign, selected: Sequence[Mutation], repo_root: Path
) -> dict[str, Any]:
    """The receipt as it stands before anything has run.

    Written in the RUNNING state so a crash leaves a receipt that names what was
    attempted; the hash bindings (manifest, runner components, sources, symbols)
    are all captured here, from the tree as it is now, so nothing later in the
    run can quietly rebind them.
    """

    runner_components = _runner_components_sha256()
    interpreter = _pin_interpreter(campaign.test_argv)[0]
    return {
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
        "interpreter": interpreter,
        "torch_version": _torch_version(interpreter),
        "expected_campaign_mutations": campaign.expected_mutations,
        "selected_mutations": [mutation.mutation_id for mutation in selected],
        "complete_campaign": len(selected) == campaign.expected_mutations,
        "baseline": None,
        "mutants": [],
        "mutation_score": None,
        "test_value": None,
        "killer_enforcement": None,
    }


def _resolve_receipt_path(
    campaign: Campaign, receipt_path: Path | None, repo_root: Path
) -> tuple[Path, str]:
    """Absolute output path and its repo-relative name, refusing anything outside."""

    if receipt_path is None:
        output_path = _default_receipt_path(campaign, repo_root)
    else:
        output_path = (
            receipt_path if receipt_path.is_absolute() else repo_root / receipt_path
        )
        output_path = output_path.resolve()
    try:
        return output_path, output_path.relative_to(repo_root.resolve()).as_posix()
    except ValueError as exc:
        raise CampaignError("receipt path must be inside the repository") from exc


def _score_receipt(
    campaign: Campaign,
    receipt: dict[str, Any],
    selected: Sequence[Mutation],
    baseline_reports: Sequence[Mapping[str, Any]],
    mutant_reports: Mapping[str, Mapping[str, Any]],
) -> None:
    """Turn completed mutant rows into a score and a final status, in place.

    PASS demands that every selected mutant was killed, so a timeout is never
    scored as a kill: a mutant the tests merely outran was not detected. A
    REFUSED killer enforcement or a failed value analysis overrides a numeric
    PASS, because a perfect score attributed to nothing is not evidence.
    """

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
