"""Ask whether the registered mutant corpus can still be applied to this tree.

``repin`` re-pins source digests and, with ``--run``, regenerates receipts. What
neither it nor the evidence gate ever asks is whether a campaign's *patches* still
land. A mutant patch is anchored to three lines of context; any edit that moves
that anchor rots it silently. The campaign keeps its PASS receipt, keeps matching
candidate paths, and keeps unblocking the tests it claims to protect -- while not
one of its mutants can be applied. The receipt describes a tree that no longer
exists and cannot be reproduced.

Nothing detects this today because the rot is only visible at run time, and a
campaign whose sources never drifted is never re-run. This audit applies every
registered patch against the working tree and reports the ones that fail, so
corpus rot is a number instead of a surprise mid-campaign REFUSAL.

"Can it be applied" is one of three ways a campaign stops being reproducible, and
this module now asks all three.

The second is whether it can still be *run*. ``_pin_interpreter`` resolves a bare
``python`` in a campaign's argv to the runner's own interpreter, because evidence
must bind the interpreter that produced it. An absolute path defeats that and
binds the campaign to one host's filesystem instead -- and when that path stops
existing, or stops being able to import the runner, the campaign dies at
collection with ``unmutated baseline failed`` while its tests pass standalone.

The third is whether its receipt is still *acceptable*. Every receipt pins the
runner components that produced it; ``mutation_runner_lineage.json`` narrows that
whole-file pin by declaring which older runners are semantically equivalent. A
receipt matching neither the current runner nor any lineage entry is not evidence,
and it says ``PASS`` while not being evidence. Nothing surfaced that count before.

The fourth is whether its tests are worth running at all. A PASS says the
mutants died; it says nothing about which tests killed them. `value_analysis`
answers that, and 298 of 441 registered campaigns do not carry it -- they have
never been asked. Of the ones that have, 228 tests came back DELETE_CANDIDATE:
measured, in a run where every mutant died, to kill none of them. Both are the
check-box this corpus is supposed to be the opposite of, and neither was a
number before. The repair for a DELETE_CANDIDATE is the mutant it uniquely
kills, never deleting the test -- a test that kills nothing is first evidence
that the corpus is missing a mutant.

The four share one expensive step -- loading every registered manifest -- and
differ only in what they then ask, so they are one walk.

The patch work is one ``git apply --check`` process per mutant -- fork/exec bound,
not compute -- so it is spread across a thread pool rather than ported native: the
interpreter is idle in ``waitpid`` either way. A mutant git refuses is retried
in-process against ``mutation_patch_apply``, mirroring what the runner does, so a
verdict here means what a run would mean. The three added dimensions are dict
comparisons over the manifests and receipts already in memory and cost nothing
measurable.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
from collections.abc import Mapping, Sequence
from pathlib import Path
import subprocess
from typing import Any

from conductor.mutation_patch_apply import PatchApplyError, check_patch_text
from conductor.project_paths import campaigns_relative, host_root, registry_relative
from conductor.mutation_scope import CampaignError, _safe_relative_path
from conductor.mutation_testing import (
    REPO_ROOT,
    Campaign,
    _lineage_accepts,
    _load_registry,
    _runner_components_sha256,
    _sha256,
    load_campaign,
)


def _patch_verdict(
    campaign: Campaign, mutation: Any, repo_root: Path
) -> dict[str, str] | None:
    """Why this mutant cannot be applied, or ``None`` when it applies cleanly."""

    patch = mutation.patch_file
    row = {"campaign_id": campaign.campaign_id, "mutation_id": mutation.mutation_id}
    if not patch.is_file():
        return row | {"reason": "MISSING", "detail": f"no patch file at {patch}"}
    actual = _sha256(patch)
    if actual != mutation.patch_sha256:
        return row | {
            "reason": "HASH_DRIFT",
            "detail": f"expected {mutation.patch_sha256}, got {actual}",
        }
    # Mirror the runner exactly: `git apply --check` first, the anchored applier
    # only where git refused. A verdict that disagreed with the runner would be
    # worse than no verdict at all.
    check = subprocess.run(
        ["git", "apply", "--check", str(patch)],
        cwd=repo_root,
        check=False,
        capture_output=True,
        text=True,
    )
    if check.returncode != 0:
        try:
            check_patch_text(patch.read_text(), repo_root)
        except PatchApplyError as exc:
            return row | {
                "reason": "DOES_NOT_APPLY",
                "detail": f"{(check.stderr or check.stdout).strip()[:200]}; "
                f"anchored retry: {str(exc)[:200]}",
            }


def _interpreter_verdict(campaign: Campaign, repo_root: Path) -> dict[str, str] | None:
    """Why this campaign cannot be reproduced off this host, or ``None``.

    A bare ``python`` is correct: ``_pin_interpreter`` rewrites it to whichever
    interpreter the runner was started with, so the campaign runs wherever it is
    checked out. An absolute path is never rewritten, so it pins the evidence to
    one machine -- and the failure it produces later names the baseline, not the
    interpreter, which is why this is worth reporting before it fires.
    """

    if not campaign.test_argv:
        return None
    argv0 = campaign.test_argv[0]
    if argv0.startswith("/"):
        row = {"campaign_id": campaign.campaign_id, "interpreter": argv0}
        if not Path(argv0).exists():
            return row | {
                "reason": "INTERPRETER_ABSENT",
                "detail": "argv[0] is an absolute path that does not exist on this host",
            }
        try:
            inside = Path(argv0).resolve().is_relative_to(repo_root.resolve())
        except (OSError, ValueError):
            inside = False
        return row | {
            "reason": "INTERPRETER_HOST_PINNED",
            "detail": (
                "argv[0] is an absolute path, so the campaign binds to this host's"
                + (" checkout" if inside else " filesystem")
                + " instead of the runner's own interpreter; use a bare `python`"
            ),
        }


def _receipts_by_campaign(
    repo_root: Path, directories: Sequence[str]
) -> dict[str, list[Mapping[str, Any]]]:
    """Index every receipt by the ``campaign_id`` it declares.

    Keyed on the field rather than the filename because that is what the evidence
    gate does: a campaign may carry many receipts and is covered if any one of them
    holds up. Reading a receipt by its ``<id>.json`` path instead overstates the
    gap badly -- it reported 209 uncovered campaigns here where the true number
    was 71.
    """

    index: dict[str, list[Mapping[str, Any]]] = {}
    for directory in directories:
        root = repo_root / _safe_relative_path(
            directory, "registry.receipt_directories"
        )
        if not root.is_dir():
            continue
        for path in sorted(root.glob("*.json")):
            try:
                payload = json.loads(path.read_text(encoding="utf-8"))
            except (OSError, ValueError):
                continue
            if not isinstance(payload, dict):
                continue
            campaign_id = payload.get("campaign_id")
            if isinstance(campaign_id, str):
                index.setdefault(campaign_id, []).append(payload)
    return index


# The two statuses a passing campaign publishes. A patch campaign passes with
# ``PASS``; a generated one passes with ``RATCHET_HELD``, because its verdict is
# the survivor set rather than the score -- no mutant survived that did not
# survive the recorded baseline. ``mutation_engine_generated`` already exits 0 on
# either, so accepting only ``PASS`` here made every generated campaign
# permanently ``NO_ACCEPTABLE_RECEIPT``: the corpus audit could not see a
# generated campaign as covered however well it ran. Restated rather than
# imported because every receipt in the repository pins that module's hash, so an
# export added there would invalidate all of them.
PASSING_RECEIPT_STATUSES = frozenset({"PASS", "RATCHET_HELD"})


def _receipt_rejection(
    receipt: Mapping[str, Any], current: Mapping[str, str], repo_root: Path
) -> str | None:
    """Why this receipt is not usable evidence, or ``None`` when it is."""

    status = receipt.get("status")
    if status not in PASSING_RECEIPT_STATUSES:
        return f"status={status}"
    recorded = receipt.get("runner_components_sha256")
    if not isinstance(recorded, dict):
        return "no runner component map"
    if not (recorded == dict(current) or _lineage_accepts(recorded, repo_root)):
        return "runner components match neither this runner nor any lineage entry"


def _evidence_verdict(
    campaign: Campaign,
    receipts: Mapping[str, list[Mapping[str, Any]]],
    current: Mapping[str, str],
    repo_root: Path,
) -> dict[str, Any] | None:
    """Why no receipt vouches for this campaign, or ``None`` when one does."""

    rows = receipts.get(campaign.campaign_id, [])
    if not rows:
        return {
            "campaign_id": campaign.campaign_id,
            "reason": "NO_RECEIPT",
            "receipts": 0,
            "detail": "no receipt declares this campaign_id",
        }
    rejections = [_receipt_rejection(row, current, repo_root) for row in rows]
    if not any(reason is None for reason in rejections):
        return {
            "campaign_id": campaign.campaign_id,
            "reason": "NO_ACCEPTABLE_RECEIPT",
            "receipts": len(rows),
            "detail": "; ".join(sorted({str(reason) for reason in rejections})),
        }


def _acceptable_receipt(
    campaign: Campaign,
    receipts: Mapping[str, list[Mapping[str, Any]]],
    current: Mapping[str, str],
    repo_root: Path,
) -> Mapping[str, Any] | None:
    """The newest receipt that vouches for this campaign, or ``None``.

    Newest by ``generated_at``, not by filename: a campaign accumulates a receipt
    per run and only the latest describes the tests it ships today. Reading an
    older one would report classifications that a later run already repaired.
    """

    rows = [
        row
        for row in receipts.get(campaign.campaign_id, [])
        if _receipt_rejection(row, current, repo_root) is None
    ]
    if rows:
        return max(rows, key=lambda row: str(row.get("generated_at") or ""))


def _value_verdicts(
    campaigns: Sequence[Campaign],
    receipts: Mapping[str, list[Mapping[str, Any]]],
    current: Mapping[str, str],
    repo_root: Path,
) -> tuple[list[dict[str, str]], list[dict[str, str]]]:
    """Campaigns that never measured their tests, and tests measured to kill nothing.

    Two different failures. A campaign with no ``value_analysis`` has never been
    asked whether any of its tests detects anything -- its PASS says the mutants
    died, not that the suite earns the machine time. A campaign that HAS asked and
    got ``DELETE_CANDIDATE`` back has a measured answer: in a run where every other
    mutant died, that test killed none of them.

    Reported separately because the repairs are different. The first is a missing
    measurement. The second is a missing mutant -- the fix is the one this test
    uniquely kills, never deleting the test.

    The measurement is looked for in the receipt, not only in the manifest. A
    hand-authored campaign declares its `value_analysis` up front; a generated
    one cannot, because nothing knows which tests kill which machine-generated
    mutants until the run has happened. Reading only the manifest therefore
    marked every generated campaign as never-measured no matter what its receipt
    carried, and the receipt is where the measurement actually lands.
    """

    unmeasured: list[dict[str, str]] = []
    inert: list[dict[str, str]] = []
    for campaign in campaigns:
        receipt = _acceptable_receipt(campaign, receipts, current, repo_root)
        value = receipt.get("test_value") if isinstance(receipt, Mapping) else None
        if value is None:
            if campaign.value_analysis is None and not campaign.generated:
                unmeasured.append(
                    {
                        "campaign_id": campaign.campaign_id,
                        "reason": "NO_VALUE_ANALYSIS",
                        "detail": "nothing measures which of its tests detect anything",
                    }
                )
            continue
        if not isinstance(value, Mapping) or not isinstance(value.get("tests"), list):
            unmeasured.append(
                {
                    "campaign_id": campaign.campaign_id,
                    "reason": "INVALID_VALUE_ANALYSIS",
                    "detail": "receipt test_value must contain a tests list",
                }
            )
            continue
        for row in value.get("tests") or []:
            if (
                not isinstance(row, Mapping)
                or not isinstance(row.get("nodeid"), str)
                or not isinstance(row.get("classification"), str)
            ):
                unmeasured.append(
                    {
                        "campaign_id": campaign.campaign_id,
                        "reason": "INVALID_VALUE_ANALYSIS",
                        "detail": "receipt test_value rows require nodeid and classification",
                    }
                )
                break
            if row.get("classification") == "DELETE_CANDIDATE":
                inert.append(
                    {
                        "campaign_id": campaign.campaign_id,
                        "nodeid": str(row.get("nodeid")),
                        "reason": "KILLS_NOTHING",
                    }
                )
    return unmeasured, inert


def audit_reproducibility(
    campaigns: Sequence[Campaign],
    registry: Mapping[str, Any],
    *,
    repo_root: Path = REPO_ROOT,
) -> dict[str, Any]:
    """Report the campaigns that cannot be re-run, and the ones nothing vouches for.

    Takes already-loaded campaigns because loading them is the expensive part and
    the patch audit has just paid it.
    """

    try:
        current = _runner_components_sha256()
    except CampaignError as exc:
        raise CampaignError(
            f"cannot read the current runner components: {exc}"
        ) from exc
    directories = registry.get("receipt_directories") or []
    receipts = _receipts_by_campaign(repo_root, [str(item) for item in directories])

    interpreters = [
        verdict
        for campaign in campaigns
        if (verdict := _interpreter_verdict(campaign, repo_root)) is not None
    ]
    evidence = [
        verdict
        for campaign in campaigns
        if (verdict := _evidence_verdict(campaign, receipts, current, repo_root))
        is not None
    ]
    absent = [row for row in interpreters if row["reason"] == "INTERPRETER_ABSENT"]
    unmeasured, inert = _value_verdicts(campaigns, receipts, current, repo_root)
    return {
        "campaigns": len(campaigns),
        "receipt_files": sum(len(rows) for rows in receipts.values()),
        "host_pinned_interpreters": len(interpreters),
        "absent_interpreters": len(absent),
        "uncovered_campaigns": len(evidence),
        "campaigns_without_value_analysis": len(unmeasured),
        "tests_that_kill_nothing": len(inert),
        "interpreters": sorted(interpreters, key=lambda row: row["campaign_id"]),
        "evidence": sorted(evidence, key=lambda row: row["campaign_id"]),
        "unmeasured": sorted(unmeasured, key=lambda row: row["campaign_id"]),
        "inert_tests": sorted(
            inert, key=lambda row: (row["campaign_id"], row["nodeid"])
        ),
    }


DEFAULT_BASELINE = Path(
    campaigns_relative(host_root()) / "reproducibility_baseline.json"
)
BASELINE_KEYS = (
    "stale_mutations",
    "unloadable_manifests",
    "host_pinned_interpreters",
    "uncovered_campaigns",
    "campaigns_without_value_analysis",
    "tests_that_kill_nothing",
)
PATCH_KEYS = ("stale_mutations", "unloadable_manifests")


def _load_baseline(path: Path, repo_root: Path) -> dict[str, set[str]]:
    """Read the recorded debt. A missing baseline means every finding is new."""

    resolved = path if path.is_absolute() else repo_root / path
    if not resolved.is_file():
        return {key: set() for key in BASELINE_KEYS}
    try:
        payload = json.loads(resolved.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise CampaignError(
            f"{path}: unreadable reproducibility baseline: {exc}"
        ) from exc
    if not isinstance(payload, dict):
        raise CampaignError(f"{path}: reproducibility baseline must be an object")
    out: dict[str, set[str]] = {}
    for key in BASELINE_KEYS:
        rows = payload.get(key, [])
        if not isinstance(rows, list) or not all(isinstance(row, str) for row in rows):
            raise CampaignError(f"{path}: {key} must be an array of ids")
        out[key] = set(rows)
    return out


def _findings(
    patches: Mapping[str, Any], repro: Mapping[str, Any]
) -> dict[str, set[str]]:
    """Every finding across every dimension, as comparable ids."""

    return {
        "stale_mutations": {
            f"{row['campaign_id']}::{row['mutation_id']}" for row in patches["stale"]
        },
        "unloadable_manifests": {str(row["manifest"]) for row in patches["unloadable"]},
        "host_pinned_interpreters": {
            str(row["campaign_id"]) for row in repro["interpreters"]
        },
        "uncovered_campaigns": {str(row["campaign_id"]) for row in repro["evidence"]},
        "campaigns_without_value_analysis": {
            str(row["campaign_id"]) for row in repro["unmeasured"]
        },
        "tests_that_kill_nothing": {
            f"{row['campaign_id']}::{row['nodeid']}" for row in repro["inert_tests"]
        },
    }


def _baseline_delta(
    found: Mapping[str, set[str]], recorded: Mapping[str, set[str]]
) -> dict[str, Any]:
    """Compare today's findings against the recorded debt, strictly in both directions.

    A finding outside the baseline is a regression. A baseline entry that no longer
    fails is *also* reported, because a ratchet that never tightens is the check-box
    this audit exists to avoid: the corpus carries 91 rotted mutants and 71 campaigns
    nothing vouches for, so an audit that simply fails on their existence is red on
    every run and read on none. The recorded number has to come down as the debt is
    paid, or it stops meaning anything.
    """

    delta: dict[str, Any] = {}
    regressed = False
    stale = False
    for key in BASELINE_KEYS:
        new = sorted(found[key] - recorded[key])
        resolved = sorted(recorded[key] - found[key])
        delta[f"new_{key}"] = new
        delta[f"resolved_{key}"] = resolved
        regressed = regressed or bool(new)
        stale = stale or bool(resolved)
    if regressed:
        delta["status"] = "REGRESSED"
    elif stale:
        delta["status"] = "BASELINE_STALE"
    else:
        delta["status"] = "CLEAN"
    return delta


def load_registered_campaigns(
    registry_path: Path, *, repo_root: Path = REPO_ROOT
) -> tuple[Mapping[str, Any], list[Campaign], list[dict[str, str]]]:
    """Load every registered manifest once, reporting the ones that will not load.

    Unloadable manifests are collected rather than raised: one lane's half-written
    campaign must not hide the corpus health of every other lane.
    """

    payload = _load_registry(registry_path, repo_root)
    campaigns: list[Campaign] = []
    unloadable: list[dict[str, str]] = []
    for index, row in enumerate(payload["campaigns"]):
        manifest = repo_root / _safe_relative_path(
            row["manifest"], f"registry.campaigns[{index}].manifest"
        )
        try:
            campaigns.append(load_campaign(manifest, repo_root=repo_root))
        except CampaignError as exc:
            unloadable.append(
                {
                    "manifest": manifest.relative_to(repo_root).as_posix(),
                    "detail": str(exc)[:400],
                }
            )
    return payload, campaigns, unloadable


def audit_patches(
    registry_path: Path,
    *,
    repo_root: Path = REPO_ROOT,
    max_workers: int = 16,
    loaded: tuple[Mapping[str, Any], list[Campaign], list[dict[str, str]]]
    | None = None,
) -> dict[str, Any]:
    """Report every registered mutant that can no longer be applied.

    Accepts an already-loaded corpus so a caller running several audits pays the
    manifest walk once.
    """

    if loaded is None:
        loaded = load_registered_campaigns(registry_path, repo_root=repo_root)
    _payload, campaigns, unloadable = loaded

    jobs = [
        (campaign, mutation)
        for campaign in campaigns
        for mutation in campaign.mutations
    ]
    with ThreadPoolExecutor(max_workers=max_workers) as pool:
        verdicts = list(
            pool.map(
                lambda job: _patch_verdict(job[0], job[1], repo_root),
                jobs,
            )
        )
    stale = [verdict for verdict in verdicts if verdict is not None]
    by_campaign: dict[str, int] = {}
    for verdict in stale:
        campaign_id = verdict["campaign_id"]
        by_campaign[campaign_id] = by_campaign.get(campaign_id, 0) + 1
    return {
        "status": "STALE" if stale or unloadable else "CLEAN",
        "repo_root": str(repo_root),
        "campaigns": len(campaigns),
        "mutations": len(jobs),
        "stale_mutations": len(stale),
        "stale_campaigns": dict(sorted(by_campaign.items())),
        "stale": sorted(
            stale, key=lambda row: (row["campaign_id"], row["mutation_id"])
        ),
        "unloadable": sorted(unloadable, key=lambda row: row["manifest"]),
    }


def _write_baseline(path: Path, repo_root: Path, found: Mapping[str, set[str]]) -> None:
    resolved = path if path.is_absolute() else repo_root / path
    payload: dict[str, Any] = {
        "schema_version": 1,
        "note": (
            "Registered mutants and campaigns that are not reproducible on a fresh "
            "checkout, plus the campaigns and tests nothing has measured to be worth "
            "running. This file is a ratchet: mutation_patch_audit fails on any id "
            "that appears and is not listed here, and on any id listed here that no "
            "longer fails. It only ever shrinks."
        ),
    }
    payload |= {key: sorted(found[key]) for key in BASELINE_KEYS}
    resolved.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def audit_corpus(
    registry_path: Path,
    *,
    repo_root: Path = REPO_ROOT,
    baseline: Path = DEFAULT_BASELINE,
    max_workers: int = 16,
    summary: bool = False,
    write_baseline: bool = False,
) -> dict[str, Any]:
    """One walk of the registered corpus, judged against the recorded debt.

    ``repo_root`` is a parameter and not ``REPO_ROOT`` because the gate runs this
    against its *exported* candidate tree rather than the working tree. Every
    reader here -- manifests, patches, receipts, the baseline -- resolves from
    disk, so in a shared checkout the working tree carries whatever campaigns
    other lanes have in flight, and those must not decide this lane's verdict.
    """

    loaded = load_registered_campaigns(registry_path, repo_root=repo_root)
    registry, campaigns, _unloadable = loaded
    patches = audit_patches(
        registry_path, repo_root=repo_root, max_workers=max_workers, loaded=loaded
    )
    repro = audit_reproducibility(campaigns, registry, repo_root=repo_root)

    found = _findings(patches, repro)
    if write_baseline:
        _write_baseline(baseline, repo_root, found)
        delta: dict[str, Any] = {"status": "RECORDED"}
    else:
        delta = _baseline_delta(found, _load_baseline(baseline, repo_root))
    repro = repro | {"baseline": delta}

    result = {
        "status": delta["status"],
        "repo_root": patches["repo_root"],
        "campaigns": patches["campaigns"],
        "patches": patches,
        "reproducibility": repro,
    }
    if summary:
        result["patches"] = {
            key: value for key, value in patches.items() if key != "stale"
        }
        result["reproducibility"] = {
            key: value
            for key, value in repro.items()
            if key not in ("interpreters", "evidence", "unmeasured", "inert_tests")
        }
    return result


def corpus_exit_code(result: Mapping[str, Any]) -> int:
    """The audit's verdict as an exit status, shared by the CLI and the gate.

    6 is reserved for the patch dimensions moving in either direction: a rotted
    mutant is the one finding a lane can introduce without ever touching the
    campaign that owns it.
    """

    delta = result["reproducibility"]["baseline"]
    if any(
        delta.get(f"{side}_{key}") for key in PATCH_KEYS for side in ("new", "resolved")
    ):
        return 6
    return 0 if delta["status"] in ("CLEAN", "RECORDED") else 7


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--registry",
        type=Path,
        default=Path(registry_relative(host_root())),
    )
    parser.add_argument(
        "--max-workers",
        type=int,
        default=16,
        help="concurrent `git apply --check` processes",
    )
    parser.add_argument(
        "--summary",
        action="store_true",
        help="omit the per-mutant and per-campaign rows and report only the counts",
    )
    parser.add_argument(
        "--baseline",
        type=Path,
        default=DEFAULT_BASELINE,
        help="recorded reproducibility debt; findings outside it fail the audit",
    )
    parser.add_argument(
        "--write-baseline",
        action="store_true",
        help="record today's findings as the baseline instead of judging against it",
    )
    args = parser.parse_args(argv)

    result = audit_corpus(
        args.registry,
        baseline=args.baseline,
        max_workers=args.max_workers,
        summary=args.summary,
        write_baseline=args.write_baseline,
    )
    print(json.dumps(result, indent=2, sort_keys=True))
    return corpus_exit_code(result)


if __name__ == "__main__":
    raise SystemExit(main())
