"""Command-line entrypoints for candidate review, attestation, and coordination."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from dataclasses import asdict
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from typing import Sequence

from conductor.candidate_review import SCHEMA_VERSION
from conductor.candidate_review.checks import ReviewContext
from conductor.candidate_review.engine import (
    append_attestation,
    governance_lock,
    run_locked_git_commit,
    run_review,
    verify_receipt_payload,
)
from conductor.candidate_review.git_source import (
    Candidate,
    GitSourceError,
    classify_candidate,
    git_common_dir,
    materialize_tree,
    repository_root,
    resolve_candidate,
    run_git,
)
from conductor.candidate_review.model import sha256_json, write_json_atomic
from conductor.candidate_review.identity import (
    OwnerIdentityError,
    is_vendor,
    normalize,
    require_lane_owner,
    resolve_owner,
)
from conductor.candidate_review.ownership import (
    EXPECTED_DEFAULT_MINUTES,
    MAX_DEFAULT_MINUTES,
    OwnershipClaim,
    OwnershipError,
    create_claim,
    load_claims,
    paths_overlap,
    release_claim,
    touch_claim,
)
from conductor.candidate_review.policy import PolicyError, load_policy
from conductor.candidate_review.policy_path import resolve_policy_path
from conductor.candidate_review.reporters import human_summary, write_outputs


def _output_path(repo: Path, raw: str) -> Path | None:
    if not raw:
        return None
    path = Path(raw)
    return path if path.is_absolute() else repo / path


def _write_failure_receipt(
    repo: Path,
    *,
    surface: str,
    profile: str,
    candidate: Candidate | None,
    error: Exception,
    json_out: Path | None,
) -> Path:
    timestamp = datetime.now(timezone.utc).isoformat()
    tree = candidate.tree_oid if candidate else "unresolved"
    payload: dict[str, object] = {
        "schema_version": SCHEMA_VERSION,
        "receipt_id": "",
        "receipt_digest": "",
        "surface": surface,
        "profile": profile,
        "decision": "fail",
        "candidate": {
            "kind": candidate.kind if candidate else "unresolved",
            "tree_oid": tree,
            "base_tree_oid": candidate.base_tree_oid if candidate else None,
            "base_commit_oid": candidate.base_commit_oid if candidate else None,
            "commit_oid": candidate.commit_oid if candidate else None,
        },
        "policy": {},
        "engine": {},
        "graph": {},
        "bypass": {},
        "timings": {
            "started_at": timestamp,
            "finished_at": timestamp,
            "duration_ms": 0,
        },
        "cache": {"hits": 0, "misses": 0},
        "baselines": [],
        "checks": [],
        "findings": [
            {
                "check_id": "policy-engine",
                "rule_id": "review-incomplete",
                "severity": "critical",
                "message": f"{type(error).__name__}: {error}",
                "path": None,
                "line": None,
                "column": None,
                "help": "Repair candidate identity/config/tooling and rerun; incomplete evidence never passes.",
                "evidence": {},
                "fingerprint": sha256_json(
                    {"type": type(error).__name__, "message": str(error)}
                )[:24],
                "exception_id": None,
            }
        ],
        "binding": sha256_json({"tree_oid": tree, "error": str(error)}),
    }
    digest_payload = dict(payload)
    digest_payload["receipt_id"] = ""
    digest_payload["receipt_digest"] = ""
    digest = sha256_json(digest_payload)
    payload["receipt_digest"] = digest
    payload["receipt_id"] = f"gr-{digest[:24]}"
    identity = candidate.commit_oid if candidate and candidate.commit_oid else tree
    path = (
        git_common_dir(repo)
        / "governance"
        / "receipts"
        / surface
        / f"{identity}-{profile}-failed.json"
    )
    write_json_atomic(path, payload)
    if json_out:
        write_json_atomic(json_out, payload)
    return path


def review_command(args: argparse.Namespace) -> int:
    repo = repository_root(Path(args.repo))
    candidate: Candidate | None = None
    json_out = _output_path(repo, args.json_out)
    try:
        candidate = resolve_candidate(
            repo,
            kind=args.candidate,
            target_ref=args.target_ref,
            base_ref=args.base_ref or None,
        )
        with materialize_tree(repo, candidate.tree_oid) as (snapshot, entries):
            policy = load_policy(resolve_policy_path(args.policy, tree=snapshot))
            candidate = classify_candidate(candidate, policy)
            with tempfile.TemporaryDirectory(
                prefix="llm-governance-runtime-"
            ) as runtime_raw:
                context = ReviewContext(
                    repo=repo,
                    snapshot=snapshot,
                    candidate=candidate,
                    entries=entries,
                    policy=policy,
                    surface=args.surface,
                    profile=args.profile,
                    owner=args.owner or os.environ.get("GOVERNANCE_OWNER"),
                    runtime_dir=Path(runtime_raw),
                )
                with governance_lock(
                    repo, exclusive=False, timeout_seconds=args.lock_timeout
                ):
                    outcome = run_review(context)
        sys.stdout.write(human_summary(outcome.receipt))
        sys.stdout.write(f"durable receipt: {outcome.receipt_path}\n")
        write_outputs(
            outcome.receipt,
            json_out=json_out,
            sarif_out=_output_path(repo, args.sarif_out),
            junit_out=_output_path(repo, args.junit_out),
        )
        return 0 if outcome.receipt.decision == "pass" else 1
    except (
        GitSourceError,
        PolicyError,
        OSError,
        RuntimeError,
        TimeoutError,
        ValueError,
    ) as exc:
        path = _write_failure_receipt(
            repo,
            surface=args.surface,
            profile=args.profile,
            candidate=candidate,
            error=exc,
            json_out=json_out,
        )
        print(
            f"candidate-review FAIL CLOSED: {type(exc).__name__}: {exc}",
            file=sys.stderr,
        )
        print(f"failure receipt: {path}", file=sys.stderr)
        return 2


def attest_command(args: argparse.Namespace) -> int:
    repo = repository_root(Path(args.repo))
    try:
        trailers = append_attestation(Path(args.message_file), repo)
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as exc:
        print(f"commit attestation failed closed: {exc}", file=sys.stderr)
        return 1
    print("commit message attested for " + trailers["Governance-Tree"][:12])
    return 0


def verify_command(args: argparse.Namespace) -> int:
    path = Path(args.receipt)
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        print(f"receipt unreadable: {exc}", file=sys.stderr)
        return 1
    valid, detail = verify_receipt_payload(payload)
    if not valid:
        print(detail, file=sys.stderr)
        return 1
    if args.repo and args.ref:
        repo = repository_root(Path(args.repo))
        commit = (
            run_git(repo, ["rev-parse", "--verify", f"{args.ref}^{{commit}}"])
            .stdout.decode()
            .strip()
        )
        tree = (
            run_git(repo, ["rev-parse", f"{commit}^{{tree}}"]).stdout.decode().strip()
        )
        candidate = payload.get("candidate")
        if not isinstance(candidate, dict) or candidate.get("tree_oid") != tree:
            print(f"receipt tree does not match {args.ref}: {tree}", file=sys.stderr)
            return 1
    print(
        f"valid receipt {payload.get('receipt_id')} decision={payload.get('decision')}"
    )
    return 0


def commit_command(args: argparse.Namespace) -> int:
    repo = repository_root(Path(args.repo))
    commit_args = list(args.git_args)
    if commit_args and commit_args[0] == "--":
        commit_args = commit_args[1:]
    return run_locked_git_commit(repo, ["commit", *commit_args])


def _claim_owner(repo: Path, declared: str) -> str:
    """The name this claim is filed under: the lane, and only the lane.

    Claiming as ``codex`` and writing as ``codex-rust-hotpath-next-20260903`` is
    how the store came to hold claims their own owner's gate could not recognise,
    and claiming as a bare vendor is how every session of that vendor came to
    satisfy one claim. Both are refused here, at the one place claims are born.
    """
    declared = normalize(declared)
    if declared:
        require_lane_owner(declared)  # a vendor name fails here, and says why
    try:
        resolved = resolve_owner(repo)
    except OwnerIdentityError:
        if not declared:
            raise
        resolved = ""
    if declared and resolved and not is_vendor(resolved) and declared != resolved:
        raise OwnerIdentityError(
            f"owner {declared!r} is not this lane: writes from here are gated as "
            f"{resolved!r}, so the claim would hold paths its own writer cannot "
            "touch. Drop --owner, or export GOVERNANCE_OWNER to rename the lane"
        )
    return require_lane_owner(declared or resolved)


def claim_command(args: argparse.Namespace) -> int:
    repo = repository_root(Path(args.repo))
    try:
        owner = _claim_owner(repo, args.owner)
    except OwnerIdentityError as exc:
        print(f"ownership claim failed closed: {exc}", file=sys.stderr)
        return 1
    try:
        with governance_lock(repo, exclusive=True, timeout_seconds=30.0):
            claim = create_claim(
                repo,
                owner=owner,
                paths=args.paths,
                justification=args.justification,
                expected_minutes=args.expected_minutes,
                max_minutes=(
                    args.hours * 60.0 if args.hours is not None else args.max_minutes
                ),
            )
    except (OSError, OwnershipError, TimeoutError) as exc:
        print(f"ownership claim failed closed: {exc}", file=sys.stderr)
        return 1
    print(
        f"created {claim.claim_id} owner={claim.owner} "
        f"expected={claim.expected_at} max={claim.expires_at} "
        f"paths={','.join(claim.paths)}"
    )
    return 0


def touch_claim_command(args: argparse.Namespace) -> int:
    """Heartbeat: say the work under a claim is still moving.

    The idle timer measures time since the last *write*, so long legitimate work
    that writes nothing -- a benchmark, a mutation campaign, a build -- would
    otherwise lapse a claim its owner is actively using. This is how such an owner
    keeps it, and the reason the idle window can be short enough to be useful.
    """
    repo = repository_root(Path(args.repo))
    try:
        with governance_lock(repo, exclusive=True, timeout_seconds=30.0):
            claims, _digest = load_claims(repo)
            match = next(
                (claim for claim in claims if claim.claim_id == args.claim_id), None
            )
            if match is None:
                raise OwnershipError(f"no claim {args.claim_id}")
            if match.owner != args.owner:
                raise OwnershipError(
                    f"claim {args.claim_id} is owned by {match.owner!r}, "
                    f"not {args.owner!r}"
                )
            now = datetime.now(timezone.utc)
            if not match.active(now):
                raise OwnershipError(
                    f"claim {args.claim_id} already lapsed: {match.lapse_reason(now)}"
                )
            touch_claim(repo, args.claim_id, now=now)
    except (OSError, OwnershipError, TimeoutError) as exc:
        print(f"claim heartbeat failed closed: {exc}", file=sys.stderr)
        return 1
    print(f"touched {args.claim_id}; idle timer reset, max time unchanged")
    return 0


def release_claim_command(args: argparse.Namespace) -> int:
    repo = repository_root(Path(args.repo))
    try:
        with governance_lock(repo, exclusive=True, timeout_seconds=30.0):
            removed = release_claim(repo, claim_id=args.claim_id, owner=args.owner)
    except (OSError, OwnershipError, TimeoutError) as exc:
        print(f"ownership release failed closed: {exc}", file=sys.stderr)
        return 1
    if not removed:
        print(f"ownership claim does not exist: {args.claim_id}", file=sys.stderr)
        return 1
    print(f"released {args.claim_id}")
    return 0


COMPACT_JUSTIFICATION_CHARS = 72


def _claims_overlapping(
    claims: Sequence[OwnershipClaim], targets: Sequence[str]
) -> list[OwnershipClaim]:
    if not targets:
        return list(claims)
    return [
        claim
        for claim in claims
        if any(paths_overlap(t, p) for t in targets for p in claim.paths)
    ]


def _dir_summary(paths: Sequence[str]) -> str:
    """``conductor/(3) research/tools/(5)`` — directories with counts, sorted."""
    counts: dict[str, int] = {}
    for path in paths:
        parent = str(PurePosixPath(path).parent)
        key = "." if parent == "." else parent + "/"
        counts[key] = counts.get(key, 0) + 1
    return " ".join(f"{key}({n})" for key, n in sorted(counts.items()))


def compact_claims_text(
    claims: Sequence[OwnershipClaim],
    digest: str,
    *,
    now: datetime,
    with_paths: bool = False,
) -> str:
    """One line per active claim: id owner state expected/max idle paths dirs why.

    The full JSON store is ~60 KB on a busy tree (~15k tokens). Full path lists
    only with *with_paths*; use ``--path`` to answer "is X claimed" instead.
    """
    active = [claim for claim in claims if claim.active(now)]
    expired = len(claims) - len(active)
    overrun = sum(1 for claim in active if claim.overrun(now))
    lines = [
        f"claims: {len(active)} active ({overrun} overrun), {expired} expired, "
        f"sha256 {digest[:12]}"
    ]
    for claim in active:
        just = claim.justification.strip().replace("\n", " ")
        if len(just) > COMPACT_JUSTIFICATION_CHARS:
            just = just[: COMPACT_JUSTIFICATION_CHARS - 1] + "…"
        idle = (now - (claim.activity or claim.creation)).total_seconds() / 60
        state = "OVERRUN" if claim.overrun(now) else "on-time"
        lines.append(
            f"{claim.claim_id}  {claim.owner:<14} {state:<7} "
            f"due {claim.expected:%m-%d %H:%MZ} ends {claim.deadline(now):%m-%d %H:%MZ}  "
            f"idle {idle:>3.0f}/{claim.idle_window(now).total_seconds() / 60:g}m  "
            f"{len(claim.paths):>2} paths  {_dir_summary(claim.paths)}  {just}"
        )
        if with_paths:
            lines.append("    " + " ".join(claim.paths))
    return "\n".join(lines)


def _stored_fields(claim: OwnershipClaim) -> dict[str, object]:
    """The claim as the store holds it: the idle stamp lives in a sidecar, and this
    view is the store. ``--compact`` is where idle time is shown."""
    payload = asdict(claim)
    payload.pop("last_seen", None)
    return payload


def claims_command(args: argparse.Namespace) -> int:
    repo = repository_root(Path(args.repo))
    try:
        claims, digest = load_claims(repo)
    except (OSError, OwnershipError) as exc:
        print(f"ownership claims unavailable: {exc}", file=sys.stderr)
        return 1
    selected = _claims_overlapping(claims, args.path or [])
    if args.compact:
        print(
            compact_claims_text(
                selected,
                digest,
                now=datetime.now(timezone.utc),
                with_paths=bool(args.paths or args.path),
            )
        )
        return 0
    print(
        json.dumps(
            {"sha256": digest, "claims": [_stored_fields(item) for item in selected]},
            indent=2,
        )
    )
    return 0


def fix_command(args: argparse.Namespace) -> int:
    repo = repository_root(Path(args.repo))
    if not args.paths:
        print(
            "fix requires explicit paths; broad worktree mutation is forbidden",
            file=sys.stderr,
        )
        return 2
    commands = [
        ["uv", "run", "ruff", "check", "--fix", *args.paths],
        ["uv", "run", "ruff", "format", *args.paths],
    ]
    for command in commands:
        completed = subprocess.run(command, cwd=repo, check=False)
        if completed.returncode:
            return completed.returncode
    return 0


def _add_review_parsers(
    subparsers: argparse._SubParsersAction[argparse.ArgumentParser],
) -> None:
    """Reviewing a candidate, and the receipts that bind one to a commit."""
    review = subparsers.add_parser("review", help="Review an exact Git candidate")
    review.add_argument("--repo", default=".")
    review.add_argument(
        "--surface",
        choices=("pre-commit", "post-commit", "ci", "manual"),
        required=True,
    )
    review.add_argument(
        "--candidate", choices=("index", "commit", "range"), required=True
    )
    review.add_argument("--profile", choices=("fast", "full"), required=True)
    review.add_argument("--target-ref", default="HEAD")
    review.add_argument("--base-ref", default="")
    review.add_argument(
        "--policy",
        default=None,
        help="candidate-relative policy path (default: $CONDUCTOR_POLICY, then "
        "conductor/candidate_policy.toml in the candidate)",
    )
    review.add_argument("--owner", default="")
    review.add_argument("--lock-timeout", type=float, default=30.0)
    review.add_argument("--json-out", default="")
    review.add_argument("--sarif-out", default="")
    review.add_argument("--junit-out", default="")
    review.set_defaults(func=review_command)
    attest = subparsers.add_parser(
        "attest-message", help="Bind a commit message to a passing index receipt"
    )
    attest.add_argument("message_file")
    attest.add_argument("--repo", default=".")
    attest.set_defaults(func=attest_command)
    verify = subparsers.add_parser(
        "verify-receipt", help="Verify receipt digest and optional Git ref binding"
    )
    verify.add_argument("receipt")
    verify.add_argument("--repo", default="")
    verify.add_argument("--ref", default="")
    verify.set_defaults(func=verify_command)
    commit = subparsers.add_parser(
        "commit", help="Hold the governance mutex across git commit"
    )
    commit.add_argument("--repo", default=".")
    commit.add_argument("git_args", nargs=argparse.REMAINDER)
    commit.set_defaults(func=commit_command)


def _add_claim_parsers(
    subparsers: argparse._SubParsersAction[argparse.ArgumentParser],
) -> None:
    """Creating, refreshing, releasing and reading ownership claims."""
    claim = subparsers.add_parser(
        "claim", help="Create a narrow, expiring ownership claim"
    )
    claim.add_argument("--repo", default=".")
    claim.add_argument(
        "--owner",
        default="",
        help="lane name; defaults to this worktree's, and must agree with it",
    )
    claim.add_argument("--justification", required=True)
    claim.add_argument(
        "--expected-minutes",
        type=float,
        default=EXPECTED_DEFAULT_MINUTES,
        help="how long you expect to need; past it the idle window shrinks",
    )
    claim.add_argument(
        "--max-minutes",
        type=float,
        default=MAX_DEFAULT_MINUTES,
        help="the most this claim may ever hold the paths",
    )
    claim.add_argument(
        "--hours",
        type=float,
        default=None,
        help="deprecated alias for --max-minutes, in hours",
    )
    claim.add_argument("paths", nargs="+")
    claim.set_defaults(func=claim_command)
    touch = subparsers.add_parser(
        "touch-claim", help="Reset a claim's idle timer while work is still running"
    )
    touch.add_argument("claim_id")
    touch.add_argument("--owner", required=True)
    touch.add_argument("--repo", default=".")
    touch.set_defaults(func=touch_claim_command)
    release = subparsers.add_parser(
        "release-claim", help="Release an ownership claim as its owner"
    )
    release.add_argument("claim_id")
    release.add_argument("--owner", required=True)
    release.add_argument("--repo", default=".")
    release.set_defaults(func=release_claim_command)
    claims = subparsers.add_parser("claims", help="Show structured ownership claims")
    claims.add_argument("--repo", default=".")
    claims.add_argument(
        "--compact",
        action="store_true",
        help="one line per active claim instead of the full JSON store",
    )
    claims.add_argument(
        "--path",
        action="append",
        help="only claims overlapping this repo-relative path (repeatable)",
    )
    claims.add_argument(
        "--paths",
        action="store_true",
        help="with --compact: also list every claimed path (implied by --path)",
    )
    claims.set_defaults(func=claims_command)


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.set_defaults(func=None)
    subparsers = parser.add_subparsers(dest="command", required=True)
    _add_review_parsers(subparsers)
    _add_claim_parsers(subparsers)
    fix = subparsers.add_parser(
        "fix", help="Explicitly mutate only named worktree paths"
    )
    fix.add_argument("--repo", default=".")
    fix.add_argument("paths", nargs="+")
    fix.set_defaults(func=fix_command)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
