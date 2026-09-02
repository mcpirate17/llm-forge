"""Branch policy: naming, fast-forward integration pushes, and claim binding.

Encodes the rules that the 2026-08-29 governance reset requires so a pre-push hook can
enforce them instead of relying on agents to self-police:

1. Exactly one integration branch (``w7-trident-program``) plus its mirror (``master``).
   Both are fast-forward only.
2. Feature branches are named ``<agent>/<topic>-<yyyymmdd>``.
3. A feature branch is bound to one claim id; an agent may not fan out a second live
   branch onto the same claim (the failure mode this module exists to close).
4. A non-fast-forward update to a feature branch requires ``--force-with-lease``; a bare
   ``--force`` is refused, and so is an update whose force mode this process cannot
   establish (see ``detect_force_mode``).
5. ``merged_branches`` names feature branches already an ancestor of the integration
   branch -- safe to delete.
6. An integration branch may not receive a commit that exists on no other pushed ref
   (``refs/remotes/*`` or ``refs/snapshots/**``) -- i.e. direct, unreviewed commits to
   the integration line are refused even when the push itself is a fast-forward.

Library API plus ``python -m conductor.branch_policy <subcommand>`` CLI; see ``main()``
for the subcommand list. Reports and refusals only -- this module never deletes a ref,
never pushes, and never mutates the working tree.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Sequence

from conductor.candidate_review.git_source import git_common_dir
from conductor.candidate_review.model import write_json_atomic
from conductor.candidate_review.ownership import (
    OwnershipClaim,
    load_claims,
    paths_overlap,
)

INTEGRATION_BRANCH = "w7-trident-program"
INTEGRATION_MIRROR = "master"
INTEGRATION_BRANCHES: tuple[str, ...] = (INTEGRATION_BRANCH, INTEGRATION_MIRROR)

BRANCH_BINDINGS_SCHEMA_VERSION = 1
_BINDING_FIELDS = frozenset(
    {"branch", "claim_id", "owner", "created_at", "last_push_at", "pr_number"}
)

# A feature branch is EXPOSED (workspace_hygiene.stale_feature_branches) once it has
# gone this long without a push, or this long without an associated PR. Defined once
# here so the binding-staleness check in `status` and the hygiene report agree.
STALE_PUSH_HOURS = 6.0
STALE_PR_HOURS = 24.0

_AGENT_RE = re.compile(r"^[a-z][a-z0-9-]*$")
_TAIL_RE = re.compile(r"^(?P<topic>[a-z0-9][a-z0-9-]*)-(?P<date>\d{8})$")
_EXPECTED_SHAPE = (
    "expected shape: <agent>/<topic>-<yyyymmdd> "
    "(agent=[a-z][a-z0-9-]*, topic=[a-z0-9][a-z0-9-]*, date=8-digit valid calendar date)"
)
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


def _slugify(raw: str) -> str:
    lowered = raw.strip().lower()
    slug = re.sub(r"[^a-z0-9-]+", "-", lowered).strip("-")
    return re.sub(r"-{2,}", "-", slug)


def suggest_branch_name(name: str) -> str:
    """Best-effort corrected name for an error message. Never authoritative."""
    agent_raw, _, rest_raw = name.partition("/")
    agent = _slugify(agent_raw) or "agent"
    if not agent[0].isalpha():
        agent = f"a{agent}"
    date_match = re.search(r"(\d{8})$", rest_raw)
    if date_match and _valid_calendar_date(date_match.group(1)):
        date = date_match.group(1)
        topic_raw = rest_raw[: date_match.start()].rstrip("-")
    else:
        date = datetime.now(UTC).strftime("%Y%m%d")
        topic_raw = (
            rest_raw if not date_match else rest_raw[: date_match.start()].rstrip("-")
        )
    topic = _slugify(topic_raw) or "topic"
    return f"{agent}/{topic}-{date}"


def _valid_calendar_date(date: str) -> bool:
    try:
        datetime.strptime(date, "%Y%m%d")
    except ValueError:
        return False
    return True


def validate_branch_name(name: str) -> ParsedBranch:
    """Validate ``<agent>/<topic>-<yyyymmdd>``, raising with the specific reason.

    Each violated rule raises with a distinct message so a caller (or a test) can
    verify which check fired, rather than a single opaque "invalid name".
    """
    suggestion = suggest_branch_name(name)
    if "/" not in name:
        raise BranchPolicyError(
            f"branch name {name!r} has no '<agent>/' segment; {_EXPECTED_SHAPE}; "
            f"try {suggestion!r}"
        )
    agent, _, rest = name.partition("/")
    if not _AGENT_RE.match(agent):
        raise BranchPolicyError(
            f"branch name {name!r} has an invalid agent slug {agent!r}; "
            f"{_EXPECTED_SHAPE}; try {suggestion!r}"
        )
    tail = _TAIL_RE.match(rest)
    if not tail:
        raise BranchPolicyError(
            f"branch name {name!r} has no '<topic>-<yyyymmdd>' segment after "
            f"'{agent}/'; {_EXPECTED_SHAPE}; try {suggestion!r}"
        )
    topic, date = tail["topic"], tail["date"]
    if not _valid_calendar_date(date):
        raise BranchPolicyError(
            f"branch name {name!r} has an invalid calendar date {date!r} (yyyymmdd); "
            f"{_EXPECTED_SHAPE}; try {suggestion!r}"
        )
    return ParsedBranch(raw=name, agent=agent, topic=topic, date=date)


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


def merged_branches(
    repo: Path, integration_branch: str = INTEGRATION_BRANCH
) -> tuple[str, ...]:
    """Feature branches whose tip is an ancestor of ``integration_branch`` -- safe to delete."""
    out = []
    for ref in _refs(repo, "refs/heads"):
        short = ref.removeprefix("refs/heads/")
        if is_integration_branch(short) or short == integration_branch:
            continue
        if is_fast_forward(repo, old=short, new=integration_branch):
            out.append(short)
    return tuple(sorted(out))


def changed_files(repo: Path, base: str, branch: str) -> tuple[str, ...]:
    output = _run_git(repo, ["diff", "--name-only", f"{base}...{branch}"])
    return tuple(sorted(line for line in output.splitlines() if line))


def branch_claim_binding(
    repo: Path, branch: str, *, integration_branch: str = INTEGRATION_BRANCH
) -> tuple[OwnershipClaim, ...]:
    """Active claims owned by the branch's agent whose paths intersect its changed files."""
    parsed = validate_branch_name(branch)
    changed = changed_files(repo, integration_branch, branch)
    claims, _digest = load_claims(repo)
    now = datetime.now(UTC)
    return tuple(
        claim
        for claim in claims
        if claim.owner == parsed.agent
        and claim.active(now)
        and any(paths_overlap(path, f) for path in claim.paths for f in changed)
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


def _binding_from_payload(payload: object) -> BranchBinding:
    if not isinstance(payload, dict) or set(payload) != _BINDING_FIELDS:
        raise BranchPolicyError("branch binding has an invalid schema")
    branch = str(payload["branch"])
    claim_id = str(payload["claim_id"])
    owner = str(payload["owner"])
    created_at = str(payload["created_at"])
    last_push_raw = payload["last_push_at"]
    last_push_at = None if last_push_raw is None else str(last_push_raw)
    pr_number = payload["pr_number"]
    if pr_number is not None and not isinstance(pr_number, int):
        raise BranchPolicyError(f"binding {branch!r} pr_number must be int or null")
    if not branch or not claim_id or not owner or not created_at:
        raise BranchPolicyError("branch binding is missing a required field")
    return BranchBinding(branch, claim_id, owner, created_at, last_push_at, pr_number)


def load_bindings(repo: Path) -> tuple[BranchBinding, ...]:
    path = binding_store_path(repo)
    if not path.is_file():
        return ()
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise BranchPolicyError(f"branch binding store is unreadable: {exc}") from exc
    if not isinstance(payload, dict) or set(payload) != {"schema_version", "bindings"}:
        raise BranchPolicyError("branch binding store has an invalid top-level schema")
    if payload["schema_version"] != BRANCH_BINDINGS_SCHEMA_VERSION or not isinstance(
        payload["bindings"], list
    ):
        raise BranchPolicyError(
            "branch binding store schema version or bindings are invalid"
        )
    bindings = tuple(_binding_from_payload(item) for item in payload["bindings"])
    names = [binding.branch for binding in bindings]
    if len(names) != len(set(names)):
        raise BranchPolicyError("branch binding store contains duplicate branches")
    return bindings


def _write_bindings(repo: Path, bindings: Sequence[BranchBinding]) -> Path:
    path = binding_store_path(repo)
    write_json_atomic(
        path,
        {
            "schema_version": BRANCH_BINDINGS_SCHEMA_VERSION,
            "bindings": [asdict(binding) for binding in bindings],
        },
    )
    return path


def _second_branch_conflict(
    live_branches: Sequence[str],
    existing: Sequence[BranchBinding],
    *,
    branch: str,
    claim_id: str,
) -> str | None:
    """Refuse when ``claim_id`` is already bound to a different LIVE branch.

    This is the fan-out guard: one claim id may back exactly one live branch at a time.
    """
    live = set(live_branches)
    for binding in existing:
        if (
            binding.claim_id == claim_id
            and binding.branch != branch
            and binding.branch in live
        ):
            return (
                f"claim {claim_id!r} is already bound to live branch {binding.branch!r}; "
                "a second live branch on the same claim is refused"
            )
    return None


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
    conflict = _second_branch_conflict(live, existing, branch=branch, claim_id=claim_id)
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


def record_push(
    repo: Path, *, branch: str, when: datetime | None = None
) -> BranchBinding | None:
    """Stamp ``last_push_at`` on an existing binding after an ALLOWED push. No-op if unbound."""
    existing = load_bindings(repo)
    moment = (when or datetime.now(UTC)).isoformat()
    updated: list[BranchBinding] = []
    found: BranchBinding | None = None
    for binding in existing:
        if binding.branch == branch:
            found = BranchBinding(
                branch=binding.branch,
                claim_id=binding.claim_id,
                owner=binding.owner,
                created_at=binding.created_at,
                last_push_at=moment,
                pr_number=binding.pr_number,
            )
            updated.append(found)
        else:
            updated.append(binding)
    if found is not None:
        _write_bindings(repo, updated)
    return found


def binding_is_stale(binding: BranchBinding, *, now: datetime) -> bool:
    """No push in ``STALE_PUSH_HOURS`` is stale regardless of PR state (offline signal)."""
    reference = datetime.fromisoformat(binding.last_push_at or binding.created_at)
    hours = (now - reference).total_seconds() / 3600.0
    return hours > STALE_PUSH_HOURS


def _force_mode_from_tokens(tokens: Sequence[str]) -> str:
    """Pure token-classification core of ``detect_force_mode`` (unit-testable).

    "push" must appear in ``tokens`` at all -- otherwise we are not even looking at a
    ``git push`` invocation and the result is "unknown", not "none".
    """
    if "push" not in tokens:
        return "unknown"
    if any(
        t == "--force-with-lease" or t.startswith("--force-with-lease=") for t in tokens
    ):
        return "lease"
    if any(t in ("--force", "-f") for t in tokens):
        return "bare"
    return "none"


def detect_force_mode() -> str:
    """Best-effort read of the parent git process's argv (Linux ``/proc`` only).

    Git's pre-push hook protocol does not pass the ``--force``/``--force-with-lease``
    flag to the hook via argv or env -- there is no portable signal. On Linux the
    invoking ``git push`` process is our parent, so its ``/proc/<ppid>/cmdline`` is a
    best-effort proxy. Returns one of "lease", "bare", "none" (push seen, no force
    flag), or "unknown" (detection was not possible; callers must not treat this as
    "no force flag" -- see ``evaluate_push``).
    """
    try:
        raw = Path(f"/proc/{os.getppid()}/cmdline").read_bytes()
    except OSError:
        return "unknown"
    tokens = [part.decode("utf-8", "replace") for part in raw.split(b"\x00") if part]
    return _force_mode_from_tokens(tokens)


def _missing_provenance(
    repo: Path, *, branch: str, remote_old: str, remote_new: str
) -> tuple[str, ...]:
    """Commits about to land on an integration branch present on no OTHER pushed ref."""
    other_refs = [
        ref for ref in _refs(repo, "refs/remotes") if not ref.endswith(f"/{branch}")
    ]
    other_refs += _refs(repo, "refs/snapshots")
    exclude = other_refs if remote_old in ("", _ZERO_SHA) else [remote_old, *other_refs]
    args = ["rev-list", remote_new, "--not", *exclude]
    return tuple(line for line in _run_git(repo, args).splitlines() if line)


@dataclass(frozen=True, slots=True)
class PushDecision:
    allowed: bool
    branch: str
    force_mode: str
    reasons: tuple[str, ...]


def evaluate_push(
    repo: Path,
    *,
    branch: str,
    remote_old: str,
    remote_new: str,
    declared_force_with_lease: bool = False,
    declared_force: bool = False,
) -> PushDecision:
    """The full pre-push decision for one ref update. Never mutates state."""
    if declared_force_with_lease and declared_force:
        raise BranchPolicyError("pass at most one of --force-with-lease / --force")
    force_mode = (
        "lease"
        if declared_force_with_lease
        else "bare"
        if declared_force
        else detect_force_mode()
    )
    fast_forward = is_fast_forward(repo, old=remote_old, new=remote_new)
    reasons: list[str] = []
    if is_integration_branch(branch):
        reasons.extend(
            _evaluate_integration_push(
                repo, branch, remote_old, remote_new, fast_forward
            )
        )
    else:
        reasons.extend(_evaluate_feature_push(repo, branch, fast_forward, force_mode))
    return PushDecision(
        allowed=not reasons,
        branch=branch,
        force_mode=force_mode,
        reasons=tuple(reasons),
    )


def _evaluate_integration_push(
    repo: Path, branch: str, remote_old: str, remote_new: str, fast_forward: bool
) -> list[str]:
    if not fast_forward:
        return [
            f"integration branch {branch!r} requires a fast-forward push; "
            f"{remote_old[:8] or '(new)'} is not an ancestor of {remote_new[:8]}"
        ]
    missing = _missing_provenance(
        repo, branch=branch, remote_old=remote_old, remote_new=remote_new
    )
    if not missing:
        return []
    return [
        f"integration branch {branch!r} would carry {len(missing)} commit(s) not already "
        "present on any other pushed ref: " + ", ".join(sha[:8] for sha in missing[:5])
    ]


def _evaluate_feature_push(
    repo: Path, branch: str, fast_forward: bool, force_mode: str
) -> list[str]:
    reasons: list[str] = []
    try:
        validate_branch_name(branch)
    except BranchPolicyError as exc:
        reasons.append(str(exc))
    if not fast_forward:
        if force_mode == "bare":
            reasons.append(
                f"non-fast-forward push to {branch!r} used a bare --force; "
                "policy requires --force-with-lease"
            )
        elif force_mode != "lease":
            detail = (
                "no force flag was declared"
                if force_mode == "none"
                else "force mode could not be established from hook context (git's "
                "pre-push protocol exposes no argv/env for it); re-run explicitly as "
                "`check-push --force-with-lease` once lease safety is confirmed"
            )
            reasons.append(
                f"non-fast-forward push to {branch!r} is refused by default: {detail}"
            )
    binding = next((b for b in load_bindings(repo) if b.branch == branch), None)
    if binding is not None:
        live = [ref.removeprefix("refs/heads/") for ref in _refs(repo, "refs/heads")]
        conflict = _second_branch_conflict(
            live, load_bindings(repo), branch=branch, claim_id=binding.claim_id
        )
        if conflict is not None:
            reasons.append(conflict)
    return reasons


# --------------------------------------------------------------------------- CLI


def _rev_parse_or_zero(repo: Path, ref: str) -> str:
    completed = subprocess.run(
        ["git", "rev-parse", ref], cwd=repo, capture_output=True, text=True, check=False
    )
    return completed.stdout.strip() if completed.returncode == 0 else _ZERO_SHA


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


def _stdin_ref_updates() -> list[tuple[str, str, str, str]]:
    """Parse git's pre-push hook stdin protocol.

    Git feeds a real pre-push hook one line per ref update: ``<local ref> <local sha1>
    <remote ref> <remote sha1>``. It does NOT pass ``--branch`` -- that flag exists for
    manual/test invocation of this CLI. When stdin is a TTY (nothing piped in) there is
    nothing to read; an empty list means "no ref updates", not "read failed".
    """
    import sys

    if sys.stdin.isatty():
        return []
    updates: list[tuple[str, str, str, str]] = []
    for line in sys.stdin:
        parts = line.split()
        if len(parts) == 4:
            updates.append((parts[0], parts[1], parts[2], parts[3]))
    return updates


def _decide_explicit(repo: Path, args: argparse.Namespace) -> PushDecision:
    remote_ref = args.remote_ref or f"refs/remotes/origin/{args.branch}"
    remote_old = _rev_parse_or_zero(repo, remote_ref)
    remote_new = _run_git(repo, ["rev-parse", args.branch]).strip()
    return evaluate_push(
        repo,
        branch=args.branch,
        remote_old=remote_old,
        remote_new=remote_new,
        declared_force_with_lease=args.force_with_lease,
        declared_force=args.force,
    )


def _decide_from_update(
    repo: Path, update: tuple[str, str, str, str], args: argparse.Namespace
) -> PushDecision:
    local_ref, local_sha, _remote_ref, remote_sha = update
    branch = local_ref.removeprefix("refs/heads/")
    return evaluate_push(
        repo,
        branch=branch,
        remote_old=remote_sha,
        remote_new=local_sha,
        declared_force_with_lease=args.force_with_lease,
        declared_force=args.force,
    )


def _report_decision(decision: PushDecision, *, as_json: bool) -> None:
    if as_json:
        print(json.dumps(asdict(decision)))
    elif decision.allowed:
        print(f"ALLOW: {decision.branch} (force_mode={decision.force_mode})")
    else:
        print(f"REFUSE: {decision.branch}")
        for reason in decision.reasons:
            print(f"  - {reason}")


def _cmd_check_push(args: argparse.Namespace) -> int:
    """``--branch`` given: single explicit decision (manual/test use). Otherwise: read
    git's real pre-push stdin protocol, one decision per ref update (hook use)."""
    repo = Path.cwd()
    if args.branch:
        decisions = [_decide_explicit(repo, args)]
    else:
        updates = _stdin_ref_updates()
        if not updates:
            print("no ref updates on stdin; nothing to check")
            return 0
        decisions = [_decide_from_update(repo, update, args) for update in updates]
    for decision in decisions:
        if decision.allowed and not is_integration_branch(decision.branch):
            record_push(repo, branch=decision.branch)
        _report_decision(decision, as_json=args.json)
    return 0 if all(decision.allowed for decision in decisions) else 1


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


def _cmd_status(args: argparse.Namespace) -> int:
    repo = Path.cwd()
    now = datetime.now(UTC)
    rows = []
    for binding in load_bindings(repo):
        reference = datetime.fromisoformat(binding.last_push_at or binding.created_at)
        age_hours = (now - reference).total_seconds() / 3600.0
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

    check_push = sub.add_parser("check-push", help="the full pre-push decision")
    check_push.add_argument(
        "--branch",
        default=None,
        help="explicit single-branch mode; omit to read git's pre-push stdin protocol",
    )
    check_push.add_argument("--remote-ref", default=None)
    force_group = check_push.add_mutually_exclusive_group()
    force_group.add_argument("--force-with-lease", action="store_true")
    force_group.add_argument("--force", action="store_true")
    check_push.add_argument("--json", action="store_true")
    check_push.set_defaults(func=_cmd_check_push)

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
