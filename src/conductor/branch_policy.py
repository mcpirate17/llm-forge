"""Branch policy: naming, claim binding, and the rules checkable without a push.

Encodes the rules the 2026-08-29 governance reset requires. They were originally
written to be enforced by a pre-push hook. Git hooks are disabled in this repo on
purpose (``core.hooksPath`` -> ``.git/governance/nohooks``, see that README), so the
hook never ran and the push-decision half of this module sat with no caller for
eight days. That half is gone as of 2026-09-06 -- deleted rather than tested, on
Tim's decision. What remains is checked *at rest*, by ``audit_repo`` below, which
runs on demand instead of waiting for a hook that will not fire.

Rules and where each is checked now:

1. Exactly one integration branch (``master``; ``w7-trident-program`` is retired but
   still recognised). It may not diverge from its remote -- ``audit_repo``.
2. Feature branches are named ``<agent>/<topic>-<yyyymmdd>`` -- ``audit_repo``, and
   ``bind_branch`` refuses an unparseable name.
3. A feature branch is bound to one claim id; an agent may not fan out a second live
   branch onto the same claim (the failure mode this module exists to close) --
   ``bind_branch`` at write time, ``audit_repo`` for drift after the fact.
4. *(withdrawn)* A non-fast-forward feature push required ``--force-with-lease``.
   This one is genuinely unenforceable at rest: nothing in the repo records how a
   push was invoked, and the old code guessed by reading the parent process's
   ``/proc/<ppid>/cmdline``. It needs a hook to mean anything. It is not enforced,
   and this module no longer pretends otherwise.
5. *(moved, not lost)* Feature branches already an ancestor of the integration branch
   are safe to delete. ``merged_branches`` computed that list here and
   ``branch_claim_binding`` matched a branch's diff against its agent's live claims;
   the docstring claimed ``workspace_hygiene`` reported them, and it never called
   either. Both were deleted on 2026-09-06 as duplicates, not as withdrawn rules:
   the live safe-to-delete list is ``workspace_hygiene.redundant_branches``, which is
   wired into the exposure report and is the stricter of the two -- it refuses to call
   a branch deletable while a worktree on it holds uncommitted work. Claim-to-branch
   binding is owned by ``bind_branch`` / ``load_bindings`` / ``audit_repo`` below.
6. Commits on no other pushed ref -- ``local_only_commits``, reported by
   ``workspace_hygiene.local_only_commit_exposure``.

Library API plus ``python -m conductor.branch_policy <subcommand>`` CLI; see ``main()``
for the subcommand list. Reports and refusals only -- this module never deletes a ref,
never pushes, and never mutates the working tree.
"""

from __future__ import annotations

import argparse
import json
import subprocess
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Sequence

from conductor._native import (
    branch_policy_second_branch_conflict_native,
    branch_policy_stamp_age_hours_native,
    branch_policy_validate_bindings_native,
    branch_policy_validate_name_native,
)
from conductor.candidate_review.git_source import git_common_dir
from conductor.candidate_review.model import write_json_atomic

# The integration line. `w7-trident-program` held this until 2026-08-30, when it was
# retired and master became the line; the constant was not moved with it until
# 2026-09-05, so every default-argument caller resolved a ref that no checkout had.
INTEGRATION_BRANCH = "master"

# Retired integration lines. They no longer exist as refs, but a name that was once the
# integration line must never be classified as a deletable feature branch if it turns up
# on an old worktree or a stale remote, so `is_integration_branch` still recognises it.
RETIRED_INTEGRATION_BRANCHES: tuple[str, ...] = ("w7-trident-program",)

INTEGRATION_BRANCHES: tuple[str, ...] = (
    INTEGRATION_BRANCH,
    *RETIRED_INTEGRATION_BRANCHES,
)

BRANCH_BINDINGS_SCHEMA_VERSION = 1

# A feature branch is EXPOSED (workspace_hygiene.stale_feature_branches) once it has
# gone this long without a push, or this long without an associated PR. Defined once
# here so the binding-staleness check in `status` and the hygiene report agree.
STALE_PUSH_HOURS = 6.0
STALE_PR_HOURS = 24.0

_ZERO_SHA = "0" * 40


class BranchPolicyError(RuntimeError):
    """A branch-policy rule refuses the requested operation."""


def is_integration_branch(name: str) -> bool:
    return name in INTEGRATION_BRANCHES


@dataclass(frozen=True, slots=True)
class ParsedBranch:
    raw: str
    agent: str
    topic: str
    date: str


def validate_branch_name(name: str) -> ParsedBranch:
    """Validate ``<agent>/<topic>-<yyyymmdd>``, raising with the specific reason.

    Each violated rule raises with a distinct message so a caller (or a test) can
    verify which check fired, rather than a single opaque "invalid name".
    """
    try:
        raw, agent, topic, date = branch_policy_validate_name_native(
            name, datetime.now(UTC).strftime("%Y%m%d")
        )
    except ValueError as exc:
        raise BranchPolicyError(str(exc)) from exc
    return ParsedBranch(raw=raw, agent=agent, topic=topic, date=date)


def _run_git(repo: Path, args: Sequence[str]) -> str:
    completed = subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=False
    )
    if completed.returncode != 0:
        raise BranchPolicyError(
            f"git {' '.join(args)} failed: {completed.stderr.strip()}"
        )
    return completed.stdout


def _refs(repo: Path, pattern: str) -> list[str]:
    return [
        line
        for line in _run_git(
            repo, ["for-each-ref", "--format=%(refname)", pattern]
        ).splitlines()
        if line
    ]


def is_fast_forward(repo: Path, *, old: str, new: str) -> bool:
    """True if ``old`` is an ancestor of ``new`` (a push from old to new is FF-safe).

    A missing/zero ``old`` (new ref) or ``old == new`` is trivially fast-forward.
    """
    if old in ("", _ZERO_SHA) or old == new:
        return True
    completed = subprocess.run(
        ["git", "merge-base", "--is-ancestor", old, new],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode not in (0, 1):
        raise BranchPolicyError(
            f"git merge-base --is-ancestor {old} {new} failed: {completed.stderr.strip()}"
        )
    return completed.returncode == 0


def local_only_commits(repo: Path, branch: str = "HEAD") -> tuple[dict[str, str], ...]:
    """Commits on ``branch`` reachable from no ``refs/remotes/*`` or ``refs/snapshots/**``."""
    excluded = _refs(repo, "refs/remotes") + _refs(repo, "refs/snapshots")
    args = ["rev-list", branch, *(["--not", *excluded] if excluded else [])]
    shas = [line for line in _run_git(repo, args).splitlines() if line]
    return tuple(
        {
            "sha": sha,
            "subject": _run_git(repo, ["log", "-1", "--format=%s", sha]).strip(),
        }
        for sha in shas
    )


@dataclass(frozen=True, slots=True)
class BranchBinding:
    branch: str
    claim_id: str
    owner: str
    created_at: str
    last_push_at: str | None
    pr_number: int | None


def binding_store_path(repo: Path) -> Path:
    return git_common_dir(repo) / "governance" / "branch-bindings.json"


def _read_binding_store_text(repo: Path) -> str | None:
    """Raw binding-store text, or ``None`` when the store does not exist."""
    path = binding_store_path(repo)
    if not path.is_file():
        return None
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        raise BranchPolicyError(f"branch binding store is unreadable: {exc}") from exc


def load_bindings(repo: Path) -> tuple[BranchBinding, ...]:
    """Parse and validate the store in Rust; the file policy stays in Python."""
    text = _read_binding_store_text(repo)
    if text is None:
        return ()
    try:
        rows = json.loads(branch_policy_validate_bindings_native(text))
    except ValueError as exc:
        raise BranchPolicyError(str(exc)) from exc
    return tuple(BranchBinding(**row) for row in rows)


def _write_bindings(repo: Path, bindings: Sequence[BranchBinding]) -> None:
    write_json_atomic(
        binding_store_path(repo),
        {
            "schema_version": BRANCH_BINDINGS_SCHEMA_VERSION,
            "bindings": [asdict(binding) for binding in bindings],
        },
    )


def bind_branch(
    repo: Path, *, branch: str, claim_id: str, owner: str, now: datetime | None = None
) -> BranchBinding:
    parsed = validate_branch_name(branch)
    if parsed.agent != owner:
        raise BranchPolicyError(
            f"branch {branch!r} agent segment {parsed.agent!r} does not match owner {owner!r}"
        )
    existing = load_bindings(repo)
    live = [ref.removeprefix("refs/heads/") for ref in _refs(repo, "refs/heads")]
    conflict = branch_policy_second_branch_conflict_native(
        live, _read_binding_store_text(repo), branch, claim_id
    )
    if conflict is not None:
        raise BranchPolicyError(conflict)
    moment = now or datetime.now(UTC)
    retained = [binding for binding in existing if binding.branch != branch]
    binding = BranchBinding(
        branch=branch,
        claim_id=claim_id,
        owner=owner,
        created_at=moment.isoformat(),
        last_push_at=None,
        pr_number=None,
    )
    _write_bindings(repo, [*retained, binding])
    return binding


def unbind_branch(repo: Path, *, branch: str) -> bool:
    existing = load_bindings(repo)
    retained = [binding for binding in existing if binding.branch != branch]
    if len(retained) == len(existing):
        return False
    _write_bindings(repo, retained)
    return True


def binding_is_stale(binding: BranchBinding, *, now: datetime) -> bool:
    """No push in ``STALE_PUSH_HOURS`` is stale regardless of PR state (offline signal)."""
    reference = binding.last_push_at or binding.created_at
    hours = branch_policy_stamp_age_hours_native(reference, now.isoformat())
    return hours > STALE_PUSH_HOURS


def audit_repo(repo: Path) -> tuple[str, ...]:
    """Every branch-policy rule that can be decided with no push in flight.

    One string per violation, empty when the repo is clean. Rules 1-3 of the module
    docstring; rule 4 is unenforceable without a hook and rules 5-6 are reported by
    ``workspace_hygiene``, which already renders them for a human.
    """
    findings: list[str] = []
    live = [ref.removeprefix("refs/heads/") for ref in _refs(repo, "refs/heads")]

    # Rule 1: an integration branch must not have diverged from its remote.
    for name in live:
        if not is_integration_branch(name):
            continue
        remote = f"refs/remotes/origin/{name}"
        if remote not in _refs(repo, "refs/remotes/origin"):
            continue
        if not is_fast_forward(repo, old=remote, new=name):
            findings.append(
                f"rule 1: integration branch {name!r} has diverged from origin/{name}; "
                "landing it would rewrite the integration line"
            )

    # Rule 2: every live feature branch parses as <agent>/<topic>-<yyyymmdd>.
    for name in live:
        if is_integration_branch(name):
            continue
        try:
            validate_branch_name(name)
        except BranchPolicyError as exc:
            # Every naming refusal already carries its own suggested name.
            findings.append(f"rule 2: branch {name!r} is misnamed: {exc}")

    # Rule 3: no claim carries more than one live branch.
    by_claim: dict[str, list[str]] = {}
    for binding in load_bindings(repo):
        if binding.branch in live:
            by_claim.setdefault(binding.claim_id, []).append(binding.branch)
    for claim_id, branches in sorted(by_claim.items()):
        if len(branches) > 1:
            findings.append(
                f"rule 3: claim {claim_id} has {len(branches)} live branches "
                f"({', '.join(sorted(branches))}); one claim, one branch"
            )

    return tuple(findings)


# --------------------------------------------------------------------------- CLI


def _cmd_check_branch(args: argparse.Namespace) -> int:
    try:
        parsed = validate_branch_name(args.name)
    except BranchPolicyError as exc:
        if args.json:
            print(json.dumps({"ok": False, "branch": args.name, "reason": str(exc)}))
        else:
            print(f"REFUSED: {exc}")
        return 1
    if args.json:
        print(json.dumps({"ok": True, "branch": args.name, **asdict(parsed)}))
    else:
        print(
            f"OK: {args.name} -> agent={parsed.agent} topic={parsed.topic} date={parsed.date}"
        )
    return 0


def _cmd_bind(args: argparse.Namespace) -> int:
    repo = Path.cwd()
    parsed = validate_branch_name(args.branch)
    try:
        binding = bind_branch(
            repo, branch=args.branch, claim_id=args.claim, owner=parsed.agent
        )
    except BranchPolicyError as exc:
        if args.json:
            print(json.dumps({"ok": False, "reason": str(exc)}))
        else:
            print(f"REFUSED: {exc}")
        return 1
    if args.json:
        print(json.dumps({"ok": True, **asdict(binding)}))
    else:
        print(f"BOUND: {binding.branch} -> {binding.claim_id} (owner={binding.owner})")
    return 0


def _cmd_unbind(args: argparse.Namespace) -> int:
    removed = unbind_branch(Path.cwd(), branch=args.branch)
    if args.json:
        print(json.dumps({"removed": removed, "branch": args.branch}))
    else:
        print(f"{'UNBOUND' if removed else 'NO BINDING'}: {args.branch}")
    return 0 if removed else 1


def _cmd_audit(args: argparse.Namespace) -> int:
    findings = audit_repo(Path.cwd())
    if args.json:
        print(json.dumps({"ok": not findings, "findings": list(findings)}))
    elif not findings:
        print("OK: no branch-policy violations")
    else:
        print(f"VIOLATIONS: {len(findings)}")
        for finding in findings:
            print(f"  - {finding}")
    return 1 if findings else 0


def _cmd_status(args: argparse.Namespace) -> int:
    repo = Path.cwd()
    now = datetime.now(UTC)
    rows = []
    for binding in load_bindings(repo):
        age_hours = branch_policy_stamp_age_hours_native(
            binding.last_push_at or binding.created_at, now.isoformat()
        )
        rows.append(
            {
                **asdict(binding),
                "age_hours": round(age_hours, 2),
                "stale": binding_is_stale(binding, now=now),
            }
        )
    if args.json:
        print(json.dumps({"bindings": rows}))
        return 0
    if not rows:
        print("no branch bindings")
        return 0
    for row in rows:
        flag = " STALE" if row["stale"] else ""
        pr = row["pr_number"] if row["pr_number"] is not None else "-"
        print(
            f"  {row['branch']}  claim={row['claim_id']}  owner={row['owner']}  age={row['age_hours']:.1f}h  pr={pr}{flag}"
        )
    return 0


def _cmd_exposed(args: argparse.Namespace) -> int:
    from conductor import workspace_hygiene  # local import: avoids an import cycle

    report = workspace_hygiene.exposure_report(Path.cwd())
    if args.json:
        print(json.dumps(report))
    else:
        print(workspace_hygiene.render_exposure(report))
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Branch policy: naming, FF-only integration, claim binding"
    )
    sub = parser.add_subparsers(dest="command", required=True)

    check_branch = sub.add_parser("check-branch", help="validate a branch name")
    check_branch.add_argument("name")
    check_branch.add_argument("--json", action="store_true")
    check_branch.set_defaults(func=_cmd_check_branch)

    audit = sub.add_parser(
        "audit", help="check every rule decidable without a push; exits 1 on violations"
    )
    audit.add_argument("--json", action="store_true")
    audit.set_defaults(func=_cmd_audit)

    bind = sub.add_parser("bind", help="bind a branch to a claim id")
    bind.add_argument("--branch", required=True)
    bind.add_argument("--claim", required=True)
    bind.add_argument("--json", action="store_true")
    bind.set_defaults(func=_cmd_bind)

    unbind = sub.add_parser("unbind", help="remove a branch's binding")
    unbind.add_argument("--branch", required=True)
    unbind.add_argument("--json", action="store_true")
    unbind.set_defaults(func=_cmd_unbind)

    status = sub.add_parser("status", help="bindings, age, PR number, staleness")
    status.add_argument("--json", action="store_true")
    status.set_defaults(func=_cmd_status)

    exposed = sub.add_parser("exposed", help="exposure list; always exits 0")
    exposed.add_argument("--json", action="store_true")
    exposed.set_defaults(func=_cmd_exposed)

    args = parser.parse_args(argv)
    try:
        return args.func(args)
    except BranchPolicyError as exc:
        print(f"ERROR: {exc}")
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
