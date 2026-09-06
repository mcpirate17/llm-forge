"""Promotion and demotion policy for required status checks.

`candidate-review` was made a required check on `w7-trident-program` and `master`
before it had ever passed on a real PR. What followed was five days of one-failure-
class-per-CI-cycle discovery -- waiver `integration_base` mismatch, host-file test
dependencies, content-pin drift, a 1200 s CPU budget against a 451-file sweep,
secrets-baseline line drift, a `.git`-less export -- with the integration branch
frozen behind a gate nobody could get green. PR #48 had to be admin-merged through a
temporary ruleset bypass; #53 took eight rounds.

Two rules come out of that, and this module is where they live:

**Promotion.** A check becomes *required* only after it has passed on 5 merged PRs
as a NON-required check. Running advisory first is what surfaces the failure classes
while they are still cheap -- an advisory red costs a comment, a required red costs
the branch.

**Demotion.** A required check that goes red on 2 consecutive PRs *for reasons
unrelated to their diffs* is demoted automatically. "Unrelated to the diff" means a
blocking finding whose `path` is outside the PR's changed files -- the shape of the
jscpd failure (600 duplicate pairs in aria C++ the candidate never touched) and of
the waiver-base failure, where every finding named a test file the PR did not open.
It is **recorded by hand**: `record --unrelated-to-diff` sets the flag the demotion
count reads. A `findings_are_unrelated_to_diff` predicate that computed it from a
findings list shipped with this module and was never called by anything; it was
deleted on 2026-09-06 rather than left as a claim the code did not honour. Deciding
the flag from the gate's own findings file is unbuilt work, not a lost feature.

Promotion is Tim's decision and this module never takes it on its own: `promote`
enforces the precondition and then requires an explicit acknowledgement flag.
Demotion is the opposite -- it is safety, so `audit --apply` will take it alone and
write a handoff entry saying what it did.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path

LEDGER_PATH = Path("conductor/gate_rollout_ledger.json")
LEDGER_SCHEMA_VERSION = 1
# Both thresholds are policy, not tuning knobs. Lowering either one re-creates the
# incident: 5 is "enough PRs that the long tail of failure classes has shown itself",
# 2 is "once is noise, twice is a gate that does not work".
PROMOTION_GREEN_RUNS = 5
DEMOTION_CONSECUTIVE_REDS = 2


class RolloutError(RuntimeError):
    """The requested rollout transition is refused by policy."""


@dataclass(frozen=True, slots=True)
class CheckRun:
    """One check's verdict on one merged PR."""

    check: str
    pr_number: int
    conclusion: str
    required: bool
    merged_at: str
    unrelated_to_diff: bool
    evidence: str = ""

    @property
    def green(self) -> bool:
        return self.conclusion == "success"


def _utc_now() -> str:
    return datetime.now(timezone.utc).isoformat()


def load_ledger(path: Path) -> list[CheckRun]:
    if not path.is_file():
        return []
    raw = json.loads(path.read_text(encoding="utf-8"))
    if raw.get("schema_version") != LEDGER_SCHEMA_VERSION:
        raise RolloutError(
            f"unsupported gate rollout ledger schema_version: {raw.get('schema_version')!r}"
        )
    return [CheckRun(**entry) for entry in raw.get("runs", [])]


def save_ledger(path: Path, runs: list[CheckRun]) -> None:
    """Atomic write: a half-written ledger would silently reset a promotion count."""
    payload = {
        "schema_version": LEDGER_SCHEMA_VERSION,
        "updated_at": _utc_now(),
        "runs": [asdict(run) for run in runs],
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    temporary.replace(path)


# ---------------------------------------------------------------------------
# The two policy questions
# ---------------------------------------------------------------------------


def consecutive_green_as_advisory(runs: list[CheckRun], check: str) -> int:
    """Green runs for `check` on merged PRs while it was NOT required.

    Counted from the most recent backwards and reset by any red, because five greens
    with a red in the middle is not evidence the check is stable -- it is evidence it
    is flaky, which is the thing that must not become required.
    """
    count = 0
    for run in sorted(
        (run for run in runs if run.check == check and not run.required),
        key=lambda run: run.merged_at,
        reverse=True,
    ):
        if not run.green:
            break
        count += 1
    return count


def consecutive_unrelated_reds(runs: list[CheckRun], check: str) -> list[CheckRun]:
    """Trailing required-and-red runs whose findings did not name the PR's own diff."""
    trailing: list[CheckRun] = []
    for run in sorted(
        (run for run in runs if run.check == check and run.required),
        key=lambda run: run.merged_at,
        reverse=True,
    ):
        if run.green or not run.unrelated_to_diff:
            break
        trailing.append(run)
    return trailing


def may_promote(runs: list[CheckRun], check: str) -> tuple[bool, str]:
    green = consecutive_green_as_advisory(runs, check)
    if green >= PROMOTION_GREEN_RUNS:
        return True, f"{green} consecutive green advisory run(s) on merged PRs"
    return False, (
        f"{check} has {green} consecutive green advisory run(s); "
        f"{PROMOTION_GREEN_RUNS} are required before it may block a branch"
    )


def must_demote(runs: list[CheckRun], check: str) -> tuple[bool, str]:
    reds = consecutive_unrelated_reds(runs, check)
    if len(reds) >= DEMOTION_CONSECUTIVE_REDS:
        prs = ", ".join(f"#{run.pr_number}" for run in reds[:DEMOTION_CONSECUTIVE_REDS])
        return True, (
            f"{check} blocked {len(reds)} consecutive PR(s) ({prs}) on findings outside their diffs"
        )
    return False, f"{check} has {len(reds)} consecutive diff-unrelated red(s)"


# ---------------------------------------------------------------------------
# GitHub ruleset mutation
# ---------------------------------------------------------------------------


def _gh_api(args: list[str]) -> str:
    completed = subprocess.run(
        ["gh", "api", *args], capture_output=True, text=True, check=False
    )
    if completed.returncode != 0:
        raise RolloutError(
            f"gh api {' '.join(args)} failed: {completed.stderr.strip()}"
        )
    return completed.stdout


def ruleset_required_checks(repo: str, ruleset_id: int) -> list[str]:
    payload = json.loads(_gh_api([f"repos/{repo}/rulesets/{ruleset_id}"]))
    for rule in payload.get("rules", []):
        if rule.get("type") == "required_status_checks":
            parameters = rule.get("parameters") or {}
            return [
                str(entry.get("context"))
                for entry in parameters.get("required_status_checks", [])
            ]
    return []


def set_ruleset_required_checks(
    repo: str, ruleset_id: int, contexts: list[str]
) -> None:
    """Rewrite the ruleset's required_status_checks rule to exactly `contexts`."""
    payload = json.loads(_gh_api([f"repos/{repo}/rulesets/{ruleset_id}"]))
    rules = [
        rule
        for rule in payload.get("rules", [])
        if rule.get("type") != "required_status_checks"
    ]
    if contexts:
        rules.append(
            {
                "type": "required_status_checks",
                "parameters": {
                    "strict_required_status_checks_policy": False,
                    "required_status_checks": [
                        {"context": context} for context in contexts
                    ],
                },
            }
        )
    body = {
        "name": payload["name"],
        "enforcement": payload["enforcement"],
        "rules": rules,
    }
    if payload.get("conditions"):
        body["conditions"] = payload["conditions"]
    completed = subprocess.run(
        [
            "gh",
            "api",
            "--method",
            "PUT",
            f"repos/{repo}/rulesets/{ruleset_id}",
            "--input",
            "-",
        ],
        input=json.dumps(body),
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        raise RolloutError(f"ruleset update failed: {completed.stderr.strip()}")


def append_handoff(title: str, body: str) -> None:
    """Record an automatic demotion where the next agent will read it."""
    subprocess.run(
        [
            sys.executable,
            "-m",
            "conductor.handoff",
            "append",
            "--owner",
            "gate-rollout",
            "--title",
            title,
            "--body",
            body,
        ],
        check=False,
        capture_output=True,
    )


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def command_status(args: argparse.Namespace) -> int:
    runs = load_ledger(Path(args.ledger))
    checks = sorted({run.check for run in runs}) if args.check is None else [args.check]
    if not checks:
        print("gate-rollout: ledger is empty; no check has a rollout history yet")
        return 0
    for check in checks:
        promote_ok, promote_reason = may_promote(runs, check)
        demote_needed, demote_reason = must_demote(runs, check)
        print(f"{check}:")
        print(
            f"  promotion: {'ELIGIBLE' if promote_ok else 'blocked'} -- {promote_reason}"
        )
        print(
            f"  demotion:  {'REQUIRED' if demote_needed else 'not needed'} -- {demote_reason}"
        )
    return 0


def command_record(args: argparse.Namespace) -> int:
    ledger = Path(args.ledger)
    runs = load_ledger(ledger)
    runs.append(
        CheckRun(
            check=args.check,
            pr_number=args.pr,
            conclusion=args.conclusion,
            required=args.required,
            merged_at=args.merged_at or _utc_now(),
            unrelated_to_diff=args.unrelated_to_diff,
            evidence=args.evidence,
        )
    )
    save_ledger(ledger, runs)
    print(
        f"recorded {args.check} on #{args.pr}: {args.conclusion} (required={args.required})"
    )
    return 0


def command_promote(args: argparse.Namespace) -> int:
    runs = load_ledger(Path(args.ledger))
    allowed, reason = may_promote(runs, args.check)
    if not allowed:
        print(f"REFUSED: {reason}", file=sys.stderr)
        return 1
    if not args.acknowledge_owner_decision:
        print(
            f"REFUSED: {args.check} is eligible ({reason}), but making a check required is "
            "the repository owner's decision. Re-run with --acknowledge-owner-decision "
            "once that decision is on record.",
            file=sys.stderr,
        )
        return 1
    contexts = sorted(
        set(ruleset_required_checks(args.repo, args.ruleset)) | {args.check}
    )
    set_ruleset_required_checks(args.repo, args.ruleset, contexts)
    print(f"promoted {args.check} to required on ruleset {args.ruleset}: {reason}")
    return 0


def command_demote(args: argparse.Namespace) -> int:
    contexts = [
        c for c in ruleset_required_checks(args.repo, args.ruleset) if c != args.check
    ]
    set_ruleset_required_checks(args.repo, args.ruleset, contexts)
    append_handoff(
        f"{args.check} demoted from required on ruleset {args.ruleset}",
        args.reason or "manual demotion",
    )
    print(f"demoted {args.check} on ruleset {args.ruleset}")
    return 0


def command_audit(args: argparse.Namespace) -> int:
    runs = load_ledger(Path(args.ledger))
    checks = sorted({run.check for run in runs if run.required})
    exit_code = 0
    for check in checks:
        needed, reason = must_demote(runs, check)
        if not needed:
            continue
        exit_code = 1
        print(f"DEMOTION REQUIRED: {reason}")
        if args.apply:
            contexts = [
                c
                for c in ruleset_required_checks(args.repo, args.ruleset)
                if c != check
            ]
            set_ruleset_required_checks(args.repo, args.ruleset, contexts)
            append_handoff(f"{check} auto-demoted from required", reason)
            print(
                f"  demoted {check} on ruleset {args.ruleset} and wrote a handoff entry"
            )
    if exit_code == 0:
        print("no required check meets the demotion condition")
    return exit_code


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="python -m conductor.gate_rollout",
        description="Promotion and demotion policy for required status checks.",
    )
    parser.add_argument("--ledger", default=str(LEDGER_PATH))
    parser.add_argument("--repo", default="mcpirate17/LLM")
    subparsers = parser.add_subparsers(dest="command", required=True)

    status = subparsers.add_parser(
        "status", help="rollout state for one or every check"
    )
    status.add_argument("--check", default=None)
    status.set_defaults(func=command_status)

    record = subparsers.add_parser(
        "record", help="record one check verdict on a merged PR"
    )
    record.add_argument("--check", required=True)
    record.add_argument("--pr", type=int, required=True)
    record.add_argument(
        "--conclusion",
        required=True,
        choices=("success", "failure", "cancelled", "skipped"),
    )
    record.add_argument("--required", action="store_true")
    record.add_argument("--unrelated-to-diff", action="store_true")
    record.add_argument("--merged-at", default=None)
    record.add_argument("--evidence", default="")
    record.set_defaults(func=command_record)

    promote = subparsers.add_parser(
        "promote", help="make a check required (owner decision)"
    )
    promote.add_argument("--check", required=True)
    promote.add_argument("--ruleset", type=int, required=True)
    promote.add_argument("--acknowledge-owner-decision", action="store_true")
    promote.set_defaults(func=command_promote)

    demote = subparsers.add_parser("demote", help="remove a check from required")
    demote.add_argument("--check", required=True)
    demote.add_argument("--ruleset", type=int, required=True)
    demote.add_argument("--reason", default="")
    demote.set_defaults(func=command_demote)

    audit = subparsers.add_parser(
        "audit", help="demote any check meeting the demotion condition"
    )
    audit.add_argument("--ruleset", type=int, required=True)
    audit.add_argument("--apply", action="store_true")
    audit.set_defaults(func=command_audit)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return int(args.func(args))
    except RolloutError as exc:
        print(f"gate-rollout REFUSED: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
