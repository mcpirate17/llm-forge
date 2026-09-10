"""Fail-closed, dry-run-first cleanup for linked Git worktrees."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path

from conductor.worktree_lease import LeaseError, is_linked_worktree, read_lease


class ReapError(RuntimeError):
    """The inventory could not be established safely."""


@dataclass(frozen=True)
class Worktree:
    path: Path
    head: str = ""
    branch: str = ""
    locked: bool = False
    prunable: bool = False
    missing: bool = False


@dataclass
class Decision:
    worktree: Worktree
    eligible: bool
    reasons: list[str] = field(default_factory=list)


def _run(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=False
    )


def _git(repo: Path, *args: str) -> str:
    done = _run(repo, *args)
    if done.returncode:
        raise ReapError(f"git {' '.join(args)} failed: {done.stderr.strip()}")
    return done.stdout


def parse_worktrees(output: str) -> list[Worktree]:
    """Parse porcelain worktree output, retaining lock/prunable markers."""
    rows: list[Worktree] = []
    fields: dict[str, str | bool] = {}

    def finish() -> None:
        nonlocal fields
        if "worktree" not in fields:
            return
        path = Path(str(fields["worktree"]))
        rows.append(
            Worktree(
                path,
                str(fields.get("HEAD", "")),
                str(fields.get("branch", "")).removeprefix("refs/heads/"),
                bool(fields.get("locked")),
                bool(fields.get("prunable")),
                not path.is_dir(),
            )
        )
        fields = {}

    for line in output.splitlines():
        if not line:
            finish()
            continue
        key, _, value = line.partition(" ")
        fields[key] = True if key in {"locked", "prunable"} else value
    finish()
    return rows


def inventory(repo: Path) -> list[Worktree]:
    """Return Git's worktree inventory or fail closed."""
    return parse_worktrees(_git(repo, "worktree", "list", "--porcelain"))


def _under(path: Path, parent: Path) -> bool:
    try:
        path.resolve().relative_to(parent.resolve())
    except ValueError:
        return False
    return True


def active_process_cwds(path: Path, proc_root: Path = Path("/proc")) -> list[str]:
    """List live process working directories inside ``path``."""
    found: list[str] = []
    try:
        entries = list(proc_root.iterdir())
    except OSError:
        return [f"unknown process state: cannot inspect {proc_root}"]
    for entry in entries:
        if not entry.name.isdigit():
            continue
        try:
            cwd = Path(os.readlink(entry / "cwd"))
        except FileNotFoundError:
            # guardrail: allow-fallback -- exited processes have no live cwd.
            continue
        except (OSError, RuntimeError) as exc:
            found.append(f"unknown process state for pid {entry.name}: {exc}")
            continue
        if _under(cwd, path):
            found.append(f"pid {entry.name}: {cwd}")
    return sorted(found)


def _status(path: Path) -> tuple[bool, list[str]]:
    done = subprocess.run(
        ["git", "-C", str(path), "status", "--porcelain", "--untracked-files=all"],
        capture_output=True,
        text=True,
        check=False,
    )
    if done.returncode:
        return False, [f"unknown git status: {done.stderr.strip() or 'status failed'}"]
    return True, [line for line in done.stdout.splitlines() if line.strip()]


def _lease_reason(path: Path, now: datetime) -> str | None:
    try:
        lease = read_lease(path)
    except LeaseError as exc:
        return f"untrusted lease: {exc}"
    if lease is not None:
        try:
            expires = datetime.fromisoformat(str(lease["expires_at"]))
        except (KeyError, TypeError, ValueError) as exc:
            return f"untrusted lease deadline: {exc}"
        if expires.tzinfo is None:
            expires = expires.replace(tzinfo=UTC)
        if expires > now:
            return f"active lease held by {lease.get('owner', 'unknown')} until {expires.isoformat()}"


def _merged_pr_proof(repo: Path, number: int) -> tuple[str, str]:
    """Return (branch, head sha) only for an independently verified merged PR."""
    done = subprocess.run(
        [
            "gh",
            "pr",
            "view",
            str(number),
            "--json",
            "state,mergedAt,headRefName,headRefOid",
        ],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    if done.returncode:
        raise ReapError(
            f"merged PR proof unavailable for #{number}: {done.stderr.strip() or 'gh failed'}"
        )
    try:
        record = json.loads(done.stdout)
    except json.JSONDecodeError as exc:
        raise ReapError(f"merged PR proof was not JSON for #{number}: {exc}") from exc
    if (
        not isinstance(record, dict)
        or record.get("state") != "MERGED"
        or not record.get("mergedAt")
    ):
        raise ReapError(f"PR #{number} is not proven MERGED")
    branch = record.get("headRefName")
    head = record.get("headRefOid")
    if (
        not isinstance(branch, str)
        or not branch
        or not isinstance(head, str)
        or not head
    ):
        raise ReapError(f"PR #{number} has incomplete exact-head proof")
    return branch, head


def _decision_reasons(
    repo: Path,
    row: Worktree,
    *,
    primary: Path,
    current: Path,
    moment: datetime,
    proc_root: Path,
    merged_pr: int | None,
    merged_proof: tuple[str, str] | None,
    integration_ref: str,
) -> list[str]:
    reasons: list[str] = []
    if not is_linked_worktree(row.path) or row.path.resolve() == primary:
        reasons.append("primary worktree")
    if row.path.resolve() == current or _under(current, row.path):
        reasons.append("current directory")
    if row.missing or row.prunable:
        reasons.append("missing/prunable registration")
    if row.locked:
        reasons.append("locked worktree")
    if not row.path.is_dir():
        reasons.append("worktree directory absent")
    else:
        clean, status = _status(row.path)
        if not clean:
            reasons.extend(status)
        elif status:
            reasons.append(f"dirty worktree ({len(status)} status entries)")
        reasons.extend(active_process_cwds(row.path, proc_root))
        lease_reason = _lease_reason(row.path, moment)
        if lease_reason:
            reasons.append(lease_reason)
    if not row.head:
        reasons.append("unknown HEAD")
    elif merged_proof is not None:
        if (row.branch, row.head) != merged_proof:
            reasons.append(f"does not match merged PR #{merged_pr} exact branch/HEAD")
        elif not reasons:
            reasons.append(f"merged PR #{merged_pr} exact branch/HEAD proven")
    elif not reasons:
        proof = _run(repo, "merge-base", "--is-ancestor", row.head, integration_ref)
        reasons.append(
            f"HEAD is not proven contained in {integration_ref}"
            if proof.returncode
            else f"ancestry proven in {integration_ref}"
        )
    return reasons


def decide(
    repo: Path,
    *,
    integration_ref: str = "origin/master",
    merged_pr: int | None = None,
    current: Path | None = None,
    now: datetime | None = None,
    proc_root: Path = Path("/proc"),
) -> list[Decision]:
    """Produce removal decisions without mutating Git or the filesystem."""
    rows = inventory(repo)
    if not rows:
        raise ReapError("git reported no worktrees")
    primary = rows[0].path.resolve()
    current_path = (current or Path.cwd()).resolve()
    moment = now or datetime.now(UTC)
    merged_proof = _merged_pr_proof(repo, merged_pr) if merged_pr is not None else None
    decisions: list[Decision] = []
    for row in rows:
        reasons = _decision_reasons(
            repo,
            row,
            primary=primary,
            current=current_path,
            moment=moment,
            proc_root=proc_root,
            merged_pr=merged_pr,
            merged_proof=merged_proof,
            integration_ref=integration_ref,
        )
        eligible = len(reasons) == 1 and (
            reasons[0].startswith("ancestry proven")
            or reasons[0].startswith("merged PR #")
        )
        decisions.append(Decision(row, eligible, reasons))
    return decisions


def _recheck_candidate(
    repo: Path,
    decision: Decision,
    *,
    integration_ref: str,
    current: Path,
    merged_pr: int | None,
) -> None:
    """Repeat destructive guards immediately before ``git worktree remove``."""
    all_rows = inventory(repo)
    rows = [
        row
        for row in all_rows
        if row.path.resolve() == decision.worktree.path.resolve()
    ]
    if len(rows) != 1:
        raise ReapError(f"worktree inventory changed for {decision.worktree.path}")
    row = rows[0]
    if row.head != decision.worktree.head or row.branch != decision.worktree.branch:
        raise ReapError(f"worktree HEAD/branch changed for {decision.worktree.path}")
    if merged_pr is not None and (row.branch, row.head) != _merged_pr_proof(
        repo, merged_pr
    ):
        raise ReapError(f"worktree no longer matches merged PR #{merged_pr}")
    _recheck_tree_state(repo, row, all_rows, current=current)
    _recheck_proof(repo, row, integration_ref=integration_ref, merged_pr=merged_pr)


def _recheck_tree_state(
    repo: Path, row: Worktree, all_rows: list[Worktree], *, current: Path
) -> None:
    if (
        not is_linked_worktree(row.path)
        or row.path.resolve() == all_rows[0].path.resolve()
    ):
        raise ReapError(f"refusing changed primary/non-linked worktree {row.path}")
    if row.locked or row.prunable or not row.path.is_dir():
        raise ReapError(f"refusing changed locked/prunable/missing worktree {row.path}")
    clean, status = _status(row.path)
    if not clean or status:
        raise ReapError(f"refusing changed dirty/unknown worktree {row.path}")
    if _under(current, row.path) or row.path.resolve() == current.resolve():
        raise ReapError(f"refusing current worktree {row.path}")
    if active_process_cwds(row.path):
        raise ReapError(f"refusing active-process worktree {row.path}")
    lease_reason = _lease_reason(row.path, datetime.now(UTC))
    if lease_reason:
        raise ReapError(f"refusing {lease_reason}")


def _recheck_proof(
    repo: Path, row: Worktree, *, integration_ref: str, merged_pr: int | None
) -> None:
    if merged_pr is not None:
        return
    proof = _run(repo, "merge-base", "--is-ancestor", row.head, integration_ref)
    if proof.returncode:
        raise ReapError(f"refusing unproven worktree {row.path}")


def apply(
    repo: Path,
    decisions: list[Decision],
    *,
    delete_branches: bool = False,
    integration_ref: str = "origin/master",
    merged_pr: int | None = None,
    current: Path | None = None,
) -> list[str]:
    """Remove only previously eligible worktrees; callers must opt in explicitly."""
    removed: list[str] = []
    for decision in decisions:
        if not decision.eligible:
            continue
        _recheck_candidate(
            repo,
            decision,
            integration_ref=integration_ref,
            current=(current or Path.cwd()).resolve(),
            merged_pr=merged_pr,
        )
        done = _run(repo, "worktree", "remove", str(decision.worktree.path))
        if done.returncode:
            raise ReapError(
                f"could not remove {decision.worktree.path}: {done.stderr.strip()}"
            )
        removed.append(str(decision.worktree.path))
        if delete_branches and decision.worktree.branch:
            done = _run(repo, "branch", "-d", decision.worktree.branch)
            if done.returncode:
                raise ReapError(
                    f"could not delete branch {decision.worktree.branch}: {done.stderr.strip()}"
                )
    return removed


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="conductor.worktree_reap")
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--integration-ref", default="origin/master")
    parser.add_argument(
        "--merged-pr",
        type=int,
        help="require exact GitHub MERGED PR branch and head proof",
    )
    parser.add_argument(
        "--apply", action="store_true", help="remove eligible worktrees"
    )
    parser.add_argument(
        "--delete-branches",
        action="store_true",
        help="with --apply, delete proven merged branches",
    )
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    if args.delete_branches and not args.apply:
        parser.error("--delete-branches requires --apply")
    try:
        decisions = decide(
            args.repo.resolve(),
            integration_ref=args.integration_ref,
            merged_pr=args.merged_pr,
            current=Path.cwd(),
        )
        removed = (
            apply(
                args.repo.resolve(),
                decisions,
                delete_branches=args.delete_branches,
                integration_ref=args.integration_ref,
                merged_pr=args.merged_pr,
                current=Path.cwd(),
            )
            if args.apply
            else []
        )
    except ReapError as exc:
        print(f"worktree-reap: {exc}", file=sys.stderr)
        return 2
    payload = [
        {
            "worktree": str(d.worktree.path),
            "branch": d.worktree.branch,
            "eligible": d.eligible,
            "reasons": d.reasons,
        }
        for d in decisions
    ]
    if args.json:
        print(
            json.dumps(
                {"dry_run": not args.apply, "decisions": payload, "removed": removed},
                indent=2,
            )
        )
    else:
        for row in payload:
            state = "REMOVE" if row["eligible"] else "KEEP"
            reasons = row["reasons"]
            summary = "; ".join(reasons[:5])
            if len(reasons) > 5:
                summary += f"; {len(reasons) - 5} more reasons (use --json)"
            print(f"{state} {row['worktree']} [{row['branch']}] -- {summary}")
        if not args.apply:
            print("dry-run only; pass --apply to remove eligible worktrees")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
