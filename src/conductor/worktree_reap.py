"""Archive-then-remove cleanup for linked Git worktrees.

Tim's rule (2026-09-10): a worktree is a temporary sandbox for one run. It holds
only the directories that run imports, never checkpoints, and it goes away when
the run ends. Five trees holding 63 GB -- 56 GB of it training checkpoints that
``runs.db`` still pointed at -- were standing that morning because the previous
reaper could not remove any of them:

* every *reason to keep* was absolute, so a dirty or unlanded tree was protected
  forever -- and those are exactly the trees that accumulate;
* ``git worktree remove`` was called without ``--force``, so even an eligible
  dirty tree would have been refused by Git itself;
* the ``/proc`` scan turned every ``EACCES`` on another user's process into an
  "unknown process state" keep-reason. On a real machine that is ~450 root pids
  per tree, so *no* worktree was ever eligible. Nothing ran it automatically
  either, so the no-op was invisible.

The policy here is the inverse, and it is safe because nothing is lost:

**Blockers** (never removed): the primary checkout, the caller's own directory,
a tree with a live process of this user cwd'd inside it, a Git-locked tree, and
a tree whose lease has not expired.

**Triggers** (at least one required): the branch is merged (a MERGED PR for it,
its pushed branch is gone from the remote, or its HEAD is an ancestor of the
integration line), the tree is idle (no live process and no file modified in
``--idle-hours``), its lease expired, or its registration is stale.

**Archive before removal**: ``git format-patch <integration-ref>..<branch>``, the
working-tree diff and ``git status --porcelain`` are written under
``<archive root>/<slug>/``, and every ``research/reports`` entry and ``*.pt``
checkpoint is *moved* to ``<checkpoint root>/<slug>/``. Only then does the tree
get ``git worktree remove --force``. Both roots are configuration -- the data
volume's path belongs to the machine, not to this module -- and ``--apply``
refuses to run without them (``WORKTREE_ARCHIVE_ROOT`` /
``WORKTREE_CHECKPOINT_ROOT``, or the Makefile's ``ARCHIVE_ROOT`` /
``CHECKPOINT_ROOT``).

Other users' processes are skipped rather than treated as unknown: their
``cwd`` link is unreadable by design, so "unknown" was never recoverable and
only ever meant "never reap anything".
"""

from __future__ import annotations

import argparse
import errno
import fcntl
import json
import os
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path

from conductor.worktree_lease import LeaseError, is_linked_worktree, read_lease

DEFAULT_IDLE_HOURS = 6.0
ARCHIVE_ROOT_ENV = "WORKTREE_ARCHIVE_ROOT"
CHECKPOINT_ROOT_ENV = "WORKTREE_CHECKPOINT_ROOT"
#: Never walked when probing for recent work: ``.venv`` is hardlinked from the
#: primary checkout, so its mtimes belong to another tree and would make every
#: worktree look busy.
SKIP_DIRS = (".git", ".venv", "node_modules", "__pycache__")
PROBE_TIMEOUT_SECONDS = 120.0

PRIMARY = "PRIMARY"
CURRENT = "CURRENT"
ACTIVE = "ACTIVE"
LOCKED = "LOCKED"
LEASED = "LEASED"
HELD = "HELD"
REMOVE = "REMOVE"


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
    state: str = HELD


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


def active_process_cwds(
    path: Path, proc_root: Path = Path("/proc")
) -> tuple[list[str], int]:
    """Return (live cwds inside ``path``, processes that could not be inspected).

    A ``cwd`` symlink we are not allowed to read yields no information at all,
    ever -- another user's process, or a non-dumpable one such as
    ``systemd --user`` and ``(sd-pam)``, which exist in every login session.
    Counting those as "unknown, therefore keep" is what made the old reaper a
    permanent no-op, so they are counted and reported instead of blocking. A
    proc root we cannot list at all is still fatal: that is not one opaque
    process, it is the whole probe failing.
    """
    found: list[str] = []
    unreadable = 0
    try:
        entries = list(proc_root.iterdir())
    except OSError:
        return [f"unknown process state: cannot inspect {proc_root}"], 0
    for entry in entries:
        if not entry.name.isdigit():
            continue
        try:
            cwd = Path(os.readlink(entry / "cwd"))
        except FileNotFoundError:
            # guardrail: allow-fallback -- exited processes have no live cwd.
            continue
        except PermissionError:
            unreadable += 1
            continue
        except (OSError, RuntimeError) as exc:
            found.append(f"unknown process state for pid {entry.name}: {exc}")
            continue
        if _under(cwd, path):
            found.append(f"pid {entry.name}: {cwd}")
    return sorted(found), unreadable


def _lease(path: Path, now: datetime) -> tuple[str | None, str | None]:
    """Return (live-lease reason, expired-lease reason); an unreadable lease blocks."""
    try:
        record = read_lease(path)
    except LeaseError as exc:
        return f"untrusted lease: {exc}", None
    if record is None:
        return None, None
    try:
        expires = datetime.fromisoformat(str(record["expires_at"]))
    except (KeyError, TypeError, ValueError) as exc:
        return f"untrusted lease deadline: {exc}", None
    if expires.tzinfo is None:
        expires = expires.replace(tzinfo=UTC)
    owner = record.get("owner", "unknown")
    if expires > now:
        return f"active lease held by {owner} until {expires.isoformat()}", None
    return None, f"lease from {owner} expired at {expires.isoformat()}"


def recent_change(
    path: Path, idle_hours: float, timeout: float = PROBE_TIMEOUT_SECONDS
) -> str | None:
    """Return one file modified inside ``path`` within ``idle_hours``, else ``None``.

    One ``find`` walk with ``-quit`` -- it stops at the first hit rather than
    stat-ing a 9 GB tree, and the whole scan stays in C. A failed or timed-out
    probe reports a fake hit, so an unanswerable question never reads as idle.
    """
    prune: list[str] = ["("]
    for index, name in enumerate(SKIP_DIRS):
        if index:
            prune.append("-o")
        prune.extend(["-name", name])
    prune.append(")")
    # An absolute epoch, not "-6 hours": GNU find rejects a fractional relative
    # date outright ("cannot figure out how to interpret '-6.0 hours'"), and a
    # rejected probe reads as "not idle", which is another silent no-op.
    cutoff = datetime.now(UTC).timestamp() - idle_hours * 3600
    command = [
        "find",
        str(path),
        "-xdev",
        *prune,
        "-prune",
        "-o",
        "-type",
        "f",
        "-newermt",
        f"@{cutoff:.0f}",
        "-print",
        "-quit",
    ]
    try:
        done = subprocess.run(
            command, capture_output=True, text=True, timeout=timeout, check=False
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return f"idle probe failed: {exc}"
    hits = [line for line in done.stdout.splitlines() if line.strip()]
    if hits:
        return hits[0]
    if done.returncode and not done.stdout:
        return f"idle probe failed: {done.stderr.strip() or f'exit {done.returncode}'}"
    return None


def _pr_states(repo: Path, branch: str) -> list[str] | None:
    """Return the states of every PR opened from ``branch``, or ``None`` without ``gh``."""
    done = subprocess.run(
        ["gh", "pr", "list", "--head", branch, "--state", "all", "--json", "state"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    if done.returncode:
        return None
    try:
        rows = json.loads(done.stdout or "[]")
    except json.JSONDecodeError:
        return None
    if not isinstance(rows, list):
        return None
    return [str(row.get("state", "")) for row in rows if isinstance(row, dict)]


def _was_pushed(repo: Path, branch: str) -> bool:
    """True when ``branch`` was published under its own name at least once.

    Two independent traces, because each outlives the other: the
    remote-tracking ref (dropped by ``fetch --prune`` and by a local delete
    push) and ``branch.<name>.merge`` pointing at the same name upstream (which
    ``git worktree add -b`` never writes -- that one inherits the base branch).
    """
    if not _run(
        repo, "rev-parse", "--verify", f"refs/remotes/origin/{branch}"
    ).returncode:
        return True
    upstream = _run(repo, "config", "--get", f"branch.{branch}.merge").stdout.strip()
    return upstream == f"refs/heads/{branch}"


def _remote_branch_gone(repo: Path, branch: str) -> bool:
    """True only for a branch that was pushed once and has since been deleted."""
    if not _was_pushed(repo, branch):
        return False
    listed = _run(repo, "ls-remote", "--heads", "origin", branch)
    return listed.returncode == 0 and not listed.stdout.strip()


def blockers(
    row: Worktree,
    *,
    primary: Path,
    current: Path,
    proc_root: Path,
    moment: datetime,
) -> list[tuple[str, str]]:
    """Reasons this worktree must not be touched, as (state, reason) pairs."""
    found: list[tuple[str, str]] = []
    # ``is_linked_worktree`` reads the tree's ``.git`` file, so an absent
    # directory answers "not linked" and would be mistaken for the primary
    # checkout -- which is how a deleted tree's registration became permanent.
    if row.path.resolve() == primary or (
        row.path.is_dir() and not is_linked_worktree(row.path)
    ):
        found.append((PRIMARY, "primary worktree"))
    if row.path.resolve() == current or _under(current, row.path):
        found.append((CURRENT, "current directory"))
    if row.locked:
        found.append((LOCKED, "locked worktree"))
    if row.path.is_dir():
        live_cwds, _ = active_process_cwds(row.path, proc_root)
        found.extend((ACTIVE, reason) for reason in live_cwds)
        live, _ = _lease(row.path, moment)
        if live:
            found.append((LEASED, live))
    return found


def triggers(
    repo: Path,
    row: Worktree,
    *,
    integration_ref: str,
    idle_hours: float,
    moment: datetime,
    proc_root: Path = Path("/proc"),
) -> list[str]:
    """Reasons this worktree's run is over."""
    if row.missing or row.prunable or not row.path.is_dir():
        return ["stale registration: worktree directory is absent"]
    found: list[str] = []
    if (
        row.head
        and not _run(
            repo, "merge-base", "--is-ancestor", row.head, integration_ref
        ).returncode
    ):
        found.append(f"HEAD is contained in {integration_ref}")
    if row.branch:
        states = _pr_states(repo, row.branch)
        if states and "MERGED" in states:
            found.append(f"a merged PR exists for {row.branch}")
        if _remote_branch_gone(repo, row.branch):
            found.append(f"{row.branch} was pushed and is gone from origin")
    _, expired = _lease(row.path, moment)
    if expired:
        found.append(expired)
    recent = recent_change(row.path, idle_hours)
    if recent is None:
        found.append(f"idle: nothing modified in {idle_hours}h")
    _, unreadable = active_process_cwds(row.path, proc_root)
    if found and unreadable:
        found.append(f"note: {unreadable} processes could not be inspected")
    return found


def decide(
    repo: Path,
    *,
    integration_ref: str = "origin/master",
    idle_hours: float = DEFAULT_IDLE_HOURS,
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
    decisions: list[Decision] = []
    for row in rows:
        held = blockers(
            row,
            primary=primary,
            current=current_path,
            proc_root=proc_root,
            moment=moment,
        )
        if held:
            decisions.append(Decision(row, False, [r for _, r in held], held[0][0]))
            continue
        fired = triggers(
            repo,
            row,
            integration_ref=integration_ref,
            idle_hours=idle_hours,
            moment=moment,
            proc_root=proc_root,
        )
        if fired:
            decisions.append(Decision(row, True, fired, REMOVE))
        else:
            decisions.append(Decision(row, False, ["run is not over"], HELD))
    return decisions


def slug_for(row: Worktree) -> str:
    """A filesystem-safe archive directory name for this worktree's branch."""
    raw = row.branch or row.path.name
    cleaned = re.sub(r"[^A-Za-z0-9._-]+", "-", raw).strip("-.")
    return cleaned or "worktree"


def _capture(repo: Path, dest: Path, name: str, *args: str) -> None:
    done = subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True, text=True, check=False
    )
    dest.joinpath(name).write_text(done.stdout or done.stderr, encoding="utf-8")


def move_artifacts(worktree: Path, dest: Path) -> list[str]:
    """Move reports and ``*.pt`` checkpoints out of ``worktree`` into ``dest``."""
    sources: list[Path] = []
    reports = worktree / "research" / "reports"
    if reports.is_dir():
        sources.extend(sorted(reports.iterdir()))
    prune: list[str] = ["("]
    for index, name in enumerate(SKIP_DIRS):
        if index:
            prune.append("-o")
        prune.extend(["-name", name])
    prune.append(")")
    found = subprocess.run(
        [
            "find",
            str(worktree),
            "-xdev",
            *prune,
            "-prune",
            "-o",
            "-name",
            "*.pt",
            "-print",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    sources.extend(Path(line) for line in found.stdout.splitlines() if line.strip())
    moved: list[str] = []
    for source in sources:
        if not source.exists():
            continue  # already moved with the reports directory above
        target = dest / source.relative_to(worktree)
        target.parent.mkdir(parents=True, exist_ok=True)
        if target.exists():
            target = target.with_name(f"{target.name}.{os.getpid()}")
        shutil.move(str(source), str(target))
        moved.append(str(target))
    return moved


def archive_worktree(
    repo: Path,
    row: Worktree,
    *,
    integration_ref: str,
    archive_root: Path,
    checkpoint_root: Path,
) -> dict[str, object]:
    """Preserve the branch, the working tree and the heavy artifacts, then report where."""
    slug = slug_for(row)
    dest = Path(archive_root) / slug
    dest.mkdir(parents=True, exist_ok=True)
    tip = row.branch or row.head
    patches = _run(repo, "format-patch", f"{integration_ref}..{tip}", "-o", str(dest))
    dest.joinpath("format-patch.log").write_text(
        patches.stdout + patches.stderr, encoding="utf-8"
    )
    _capture(row.path, dest, "worktree.diff", "diff", "HEAD")
    _capture(
        row.path, dest, "status.txt", "status", "--porcelain", "--untracked-files=all"
    )
    moved = move_artifacts(row.path, Path(checkpoint_root) / slug)
    record: dict[str, object] = {
        "worktree": str(row.path),
        "branch": row.branch,
        "head": row.head,
        "integration_ref": integration_ref,
        "archived_at": datetime.now(UTC).isoformat(),
        "archive": str(dest),
        "moved_artifacts": moved,
    }
    dest.joinpath("archived.json").write_text(
        json.dumps(record, indent=2), encoding="utf-8"
    )
    return record


def _recheck(
    repo: Path,
    decision: Decision,
    *,
    integration_ref: str,
    idle_hours: float,
    current: Path,
    proc_root: Path,
) -> Worktree:
    """Repeat every guard immediately before the destructive step."""
    rows = [
        row
        for row in inventory(repo)
        if row.path.resolve() == decision.worktree.path.resolve()
    ]
    if len(rows) != 1:
        raise ReapError(f"worktree inventory changed for {decision.worktree.path}")
    row = rows[0]
    if row.head != decision.worktree.head or row.branch != decision.worktree.branch:
        raise ReapError(f"worktree HEAD/branch changed for {decision.worktree.path}")
    primary = inventory(repo)[0].path.resolve()
    held = blockers(
        row,
        primary=primary,
        current=current,
        proc_root=proc_root,
        moment=datetime.now(UTC),
    )
    if held:
        raise ReapError(f"refusing {row.path}: {held[0][1]}")
    if not triggers(
        repo,
        row,
        integration_ref=integration_ref,
        idle_hours=idle_hours,
        moment=datetime.now(UTC),
        proc_root=proc_root,
    ):
        raise ReapError(f"refusing {row.path}: its run is no longer over")
    return row


def _delete_branch(repo: Path, branch: str, *, delete_remote: bool) -> list[str]:
    """Delete the local branch; the remote one only behind a merged/closed PR."""
    done: list[str] = []
    if not branch:
        return done
    dropped = _run(repo, "branch", "-D", branch)
    if dropped.returncode:
        raise ReapError(f"could not delete branch {branch}: {dropped.stderr.strip()}")
    done.append(f"local {branch}")
    if not delete_remote:
        return done
    states = _pr_states(repo, branch)
    if not states or "OPEN" in states or not {"MERGED", "CLOSED"} & set(states):
        return done
    pushed = _run(repo, "push", "origin", "--delete", branch)
    if pushed.returncode == 0:
        done.append(f"origin/{branch}")
    return done


def apply(
    repo: Path,
    decisions: list[Decision],
    *,
    integration_ref: str = "origin/master",
    idle_hours: float = DEFAULT_IDLE_HOURS,
    archive_root: Path,
    checkpoint_root: Path,
    delete_branches: bool = True,
    delete_remote: bool = True,
    current: Path | None = None,
    proc_root: Path = Path("/proc"),
) -> list[dict[str, object]]:
    """Archive, then force-remove, every eligible worktree."""
    here = (current or Path.cwd()).resolve()
    removed: list[dict[str, object]] = []
    for decision in decisions:
        if not decision.eligible:
            continue
        row = _recheck(
            repo,
            decision,
            integration_ref=integration_ref,
            idle_hours=idle_hours,
            current=here,
            proc_root=proc_root,
        )
        record: dict[str, object] = {"worktree": str(row.path), "branch": row.branch}
        if row.path.is_dir():
            record["archived"] = archive_worktree(
                repo,
                row,
                integration_ref=integration_ref,
                archive_root=archive_root,
                checkpoint_root=checkpoint_root,
            )
            gone = _run(repo, "worktree", "remove", "--force", str(row.path))
            if gone.returncode:
                raise ReapError(f"could not remove {row.path}: {gone.stderr.strip()}")
        _run(repo, "worktree", "prune")
        if delete_branches:
            record["branches_deleted"] = _delete_branch(
                repo, row.branch, delete_remote=delete_remote
            )
        removed.append(record)
    return removed


def _hold_lock(repo: Path):  # noqa: ANN202 -- the handle is opaque to callers
    """Take the single-reaper lock, or return ``None`` when one is already running."""
    lock = repo / ".git" / "worktree-reap.lock"
    try:
        handle = lock.open("w")
    except OSError as exc:
        raise ReapError(f"cannot open reap lock {lock}: {exc}") from exc
    # `held` plus `finally` rather than a close() per failure branch: the handle
    # is only handed to the caller when the lock is actually ours, and every
    # other way out of this block -- refusal, error, or an unexpected raise --
    # goes through the same release.
    held = False
    try:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        held = True
    except OSError as exc:
        if exc.errno not in {errno.EACCES, errno.EAGAIN}:
            raise ReapError(f"cannot take reap lock {lock}: {exc}") from exc
        return None
    finally:
        if not held:
            handle.close()
    return handle


def _root_from_env(name: str) -> Path | None:
    value = os.environ.get(name)
    return Path(value) if value else None


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="conductor.worktree_reap")
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--integration-ref", default="origin/master")
    parser.add_argument("--idle-hours", type=float, default=DEFAULT_IDLE_HOURS)
    parser.add_argument(
        "--archive-root", type=Path, default=_root_from_env(ARCHIVE_ROOT_ENV)
    )
    parser.add_argument(
        "--checkpoint-root", type=Path, default=_root_from_env(CHECKPOINT_ROOT_ENV)
    )
    parser.add_argument(
        "--apply", action="store_true", help="archive and remove eligible worktrees"
    )
    parser.add_argument(
        "--keep-branches",
        action="store_true",
        help="with --apply, leave the local branch in place",
    )
    parser.add_argument(
        "--keep-remote-branches",
        action="store_true",
        help="with --apply, never delete a merged/closed PR's remote branch",
    )
    parser.add_argument("--json", action="store_true")
    return parser


def _render(
    decisions: list[Decision], removed: list[dict[str, object]], apply_mode: bool
) -> str:
    lines: list[str] = []
    for decision in decisions:
        reasons = "; ".join(decision.reasons[:5])
        if len(decision.reasons) > 5:
            reasons += f"; {len(decision.reasons) - 5} more reasons (use --json)"
        lines.append(
            f"{decision.state} {decision.worktree.path} [{decision.worktree.branch}] -- {reasons}"
        )
    for record in removed:
        lines.append(f"removed {record['worktree']} [{record['branch']}]")
    if not apply_mode:
        lines.append(
            "dry-run only; pass --apply to archive and remove eligible worktrees"
        )
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    repo = args.repo.resolve()
    handle = None
    try:
        if args.apply:
            if args.archive_root is None or args.checkpoint_root is None:
                raise ReapError(
                    "--apply needs somewhere to archive to: pass --archive-root and "
                    f"--checkpoint-root, or set {ARCHIVE_ROOT_ENV} and {CHECKPOINT_ROOT_ENV}"
                )
            handle = _hold_lock(repo)
            if handle is None:
                print("worktree-reap: another reap is running", file=sys.stderr)
                return 0
        decisions = decide(
            repo,
            integration_ref=args.integration_ref,
            idle_hours=args.idle_hours,
            current=Path.cwd(),
        )
        removed = (
            apply(
                repo,
                decisions,
                integration_ref=args.integration_ref,
                idle_hours=args.idle_hours,
                archive_root=args.archive_root,
                checkpoint_root=args.checkpoint_root,
                delete_branches=not args.keep_branches,
                delete_remote=not args.keep_remote_branches,
                current=Path.cwd(),
            )
            if args.apply
            else []
        )
    except ReapError as exc:
        print(f"worktree-reap: {exc}", file=sys.stderr)
        return 2
    finally:
        if handle is not None:
            handle.close()
    if args.json:
        print(
            json.dumps(
                {
                    "dry_run": not args.apply,
                    "decisions": [
                        {
                            "worktree": str(d.worktree.path),
                            "branch": d.worktree.branch,
                            "state": d.state,
                            "eligible": d.eligible,
                            "reasons": d.reasons,
                        }
                        for d in decisions
                    ],
                    "removed": removed,
                },
                indent=2,
            )
        )
    else:
        print(_render(decisions, removed, args.apply))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
