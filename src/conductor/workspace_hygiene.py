"""Read-only workspace hygiene report: what is safe to remove, and what is unsafe.

Answers in one command the questions the 2026-08-28 cleanup had to re-derive by hand:
which branches carry no unique work, which worktrees are stale or hold unsalvaged
changes, which claims have expired, which mutation manifests are orphaned, and which
untracked modules a clean clone would fail to import.

Reports only. It never deletes, commits, or mutates git state.

Run: ``python -m conductor.workspace_hygiene`` (``--json`` for machine output).

Containment is measured against the **live line** (default: current HEAD branch), never
against ``master`` -- master has been an abandoned integration target since 2026-08-02 and
"is it merged to master?" answers "no" for every branch in this repo.

NOTE: the untracked-import closure here does NOT reuse ``conductor.dead_tests._record``,
which under-reports. For ``from pkg import submodule`` where ``pkg/__init__.py`` is tracked
but the submodule is not, ``_record`` resolves the *package*, adds it as a dep and returns
early, so ``resolve_untracked`` never runs and the untracked submodule is invisible. That
masking hides 18 of the 28 untracked modules in this repo. The fix belongs upstream in
``_record`` (try ``resolve_untracked(f"{base}.{attr}")`` before falling back to ``base``);
until it lands, ``_closure_record`` below applies the correct order.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
from collections.abc import Iterable
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path

from conductor import _native, branch_policy, worktree_reap
from conductor.candidate_review.ownership import (
    OwnershipClaim,
    load_claims,
    paths_overlap,
)
from conductor.dead_tests import tracked_files
from conductor.project_paths import (
    campaigns_relative,
    host_root,
    integration_branch,
    registry_relative,
)
from conductor.worktree_lease import is_linked_worktree, lease_state


class HygieneError(RuntimeError):
    """Raised when the report cannot be produced from trustworthy inputs."""


# ``ROOT`` used to be re-derived here with ``git rev-parse --show-toplevel``
# because ``Path(__file__).resolve().parents[1]`` -- the package parent -- is
# the repo root only in the layout conductor was extracted from; imported from
# this repo's ``src/`` layout (or an installed package) it lands one level
# short, silently re-pointing every defaulting caller at a directory with no
# ``.git``, no ``campaigns/`` and no ``.venv``. ``project_paths.host_root()``
# is the shared fix (nearest ``.git`` ancestor of the cwd, or
# ``$CONDUCTOR_HOST_ROOT``): it fails loud rather than guessing, and callers
# that can degrade (the session inject) catch ``HygieneError`` and say so.
ROOT = host_root()
# ``ROOT`` is the checkout root, so the host-relative campaign layout resolves
# against the repository that actually holds it.
CAMPAIGN_DIR = str(campaigns_relative(ROOT))
REGISTRY = str(registry_relative(ROOT))


def _git(*args: str) -> str:
    done = subprocess.run(
        ["git", *args], cwd=ROOT, capture_output=True, text=True, check=False
    )
    if done.returncode != 0:
        raise HygieneError(f"git {' '.join(args)} failed: {done.stderr.strip()}")
    return done.stdout


@dataclass
class Report:
    live_ref: str
    redundant_branches: list[dict[str, str]] = field(default_factory=list)
    stale_worktrees: list[str] = field(default_factory=list)
    dirty_worktrees: list[dict[str, object]] = field(default_factory=list)
    landed_worktrees: list[dict[str, object]] = field(default_factory=list)
    landed_ref: str = ""
    expired_claims: list[dict[str, str]] = field(default_factory=list)
    idle_claims: list[dict[str, str]] = field(default_factory=list)
    unpushed_work: list[dict[str, str]] = field(default_factory=list)
    orphan_manifests: list[str] = field(default_factory=list)
    dangling_manifests: list[str] = field(default_factory=list)
    untracked_imports: list[dict[str, object]] = field(default_factory=list)
    stale_feature_branches: list[dict[str, object]] = field(default_factory=list)
    gh_available: bool = True
    local_only_commit_exposure: list[dict[str, str]] = field(default_factory=list)
    stale_dirty_files: list[dict[str, str]] = field(default_factory=list)
    worktree_leases: list[dict[str, object]] = field(default_factory=list)
    checkout_drift: dict[str, object] = field(default_factory=dict)

    def overdue_leases(self) -> list[dict[str, object]]:
        return [r for r in self.worktree_leases if r["status"] != "leased"]

    def is_clean(self) -> bool:
        return not any(
            (
                self.redundant_branches,
                self.stale_worktrees,
                self.landed_worktrees,
                self.expired_claims,
                self.idle_claims,
                self.unpushed_work,
                self.orphan_manifests,
                self.dangling_manifests,
                self.untracked_imports,
                self.stale_feature_branches,
                self.local_only_commit_exposure,
                self.stale_dirty_files,
                self.overdue_leases(),
                self.checkout_drift.get("behind", 0),
            )
        )

    def has_exposure(self) -> bool:
        return bool(
            self.stale_feature_branches
            or self.local_only_commit_exposure
            or self.stale_dirty_files
        )


def _default_branch() -> str:
    """The repo's default branch, which is never a deletion candidate."""
    head = ROOT / ".git" / "refs" / "remotes" / "origin" / "HEAD"
    if head.is_file():
        return head.read_text().strip().rsplit("/", 1)[-1]
    return integration_branch(host_root(ROOT))


def redundant_branches(live_ref: str, protected: Iterable[str]) -> list[dict[str, str]]:
    """Branches with zero commits the live line does not already have.

    ``protected`` is excluded outright. Containment is necessary but NOT sufficient
    for deletion: a branch checked out in a worktree that holds uncommitted work is
    reported as blocked, because the ref is redundant while the working tree is not.
    Both guards exist because the first version of this report listed ``master`` and a
    branch with 18 uncommitted files as "safe to delete".
    """
    blocked = {row["branch"] for row in _dirty_worktree_branches()}
    protected_set = set(protected)
    out: list[dict[str, str]] = []
    for name in _git("for-each-ref", "--format=%(refname:short)", "refs/heads").split():
        if name == live_ref or name in protected_set:
            continue
        if _git("rev-list", "--count", f"{live_ref}..{name}").strip() != "0":
            continue
        row = {"branch": name, "sha": _git("rev-parse", name).strip()}
        if name in blocked:
            row["blocked_by"] = "worktree holds uncommitted work"
        out.append(row)
    return out


def _dirty_worktree_branches() -> list[dict[str, str]]:
    _, dirty = worktree_state()
    return [
        {"branch": str(row["branch"]).removeprefix("refs/heads/")}
        for row in dirty
        if row.get("branch")
    ]


def _worktree_entries(repo: Path = ROOT) -> list[dict[str, str]]:
    """Parse ``git worktree list --porcelain`` into one record per worktree."""
    current: dict[str, str] = {}
    entries: list[dict[str, str]] = []
    for line in _git_in(repo, "worktree", "list", "--porcelain").splitlines():
        if not line:
            if current:
                entries.append(current)
                current = {}
            continue
        key, _, value = line.partition(" ")
        current[key] = value
    if current:
        entries.append(current)
    return entries


def _uncommitted(path: str) -> list[str]:
    status = subprocess.run(
        ["git", "-C", path, "status", "--porcelain", "--untracked-files=all"],
        capture_output=True,
        text=True,
        check=False,
    )
    return [line for line in status.stdout.splitlines() if line.strip()]


def worktree_state() -> tuple[list[str], list[dict[str, object]]]:
    """(worktrees whose directory is gone, worktrees holding uncommitted work)."""
    stale: list[str] = []
    dirty: list[dict[str, object]] = []
    for entry in _worktree_entries():
        path = entry.get("worktree", "")
        if not path:
            continue
        if not Path(path).is_dir():
            stale.append(path)
            continue
        changes = _uncommitted(path)
        if changes:
            dirty.append(
                {
                    "worktree": path,
                    "branch": entry.get("branch", "(detached)"),
                    "entries": len(changes),
                    "sample": changes[:5],
                }
            )
    return stale, dirty


def _live_ref_or_default(repo: Path = ROOT, *, allow_network: bool = True) -> str:
    """The ref the hook judges containment against: the remote integration line.

    Prefers ``origin/<branch>`` over the local branch. A local branch can sit days
    behind origin -- this checkout was 3 commits behind while four PRs landed -- and a
    worktree judged against a stale local tip reads as unlanded long after its work
    merged, which is the opposite of the nag this check exists to produce.

    Resolution is shared with the reaper (``worktree_reap.default_integration_ref``):
    the configured ``[tool.conductor].integration_branch``, else the remote's own
    HEAD symref, else the one conventional ``origin/{master,main}`` ref -- offline.
    ``allow_network`` gates the ``ls-remote`` advertisement, exactly as in the
    reaper; the hook path (``cheap_exposure_counts``) passes false so session
    start never opens a network connection.

    Raises rather than guessing when none resolves: a containment check with no line
    to check against would silently report every worktree as finished.
    """
    try:
        return worktree_reap.default_integration_ref(repo, allow_network=allow_network)
    except worktree_reap.ReapError as exc:
        raise HygieneError(str(exc)) from exc


def landed_worktrees(live_ref: str, repo: Path = ROOT) -> list[dict[str, object]]:
    """Worktrees that have finished: clean, and holding nothing the live line lacks.

    This is the category the report was missing. Stale registrations and dirty trees
    were already classified, so a worktree whose work had *landed* fell through both
    and was reported as nothing at all -- which is why worktrees accumulated with
    nothing ever asking for them back.

    Cleanliness is required, never inferred from the branch. A worktree can sit exactly
    on its remote tip and still hold every line of value in unstaged files; deleting one
    on ref containment alone destroys work silently (codex/tooling-migration-20260906,
    2026-09-07: 59 unstaged paths behind a tip identical to its remote). The main
    checkout is excluded outright -- it is not a candidate for removal at any time.
    """
    main = repo.resolve()
    out: list[dict[str, object]] = []
    for entry in _worktree_entries(repo):
        path = entry.get("worktree", "")
        if not path or not Path(path).is_dir():
            continue
        if Path(path).resolve() == main:
            continue
        if _uncommitted(path):
            continue
        head = entry.get("HEAD", "")
        if not head:
            continue
        branch = entry.get("branch", "").removeprefix("refs/heads/")
        contained = _git_in(repo, "rev-list", "--count", f"{live_ref}..{head}").strip()
        if contained == "0":
            reason = f"contained in {live_ref}"
        elif _upstream_was_deleted(repo, branch):
            reason = "merged and pruned: its remote branch is gone"
        else:
            continue
        out.append(
            {
                "worktree": path,
                "branch": branch or "(detached)",
                "head": head[:8],
                "reason": reason,
            }
        )
    return out


def checkout_drift(repo: Path = ROOT) -> dict[str, object]:
    """How far the main checkout has fallen behind the remote line, and why.

    The shared checkout is a mirror: work is done in worktrees and lands through
    a PR, so the checkout should never be anything but even with origin. On
    2026-09-07 it was five commits behind with 314 dirty paths, and no check said
    so -- every existing measure looked at branches, worktrees and claims, none
    at the tree everybody actually sits in.

    ``blocked_by`` is the part that makes this actionable rather than another
    number: a fast-forward is refused only by *tracked* files that differ, so it
    names those and nothing else. Untracked files never block one, which is why
    the 314 dirty paths turned out to be a fast-forward away from clean.
    """

    remote = _live_ref_or_default(repo)
    behind = _git_in(repo, "rev-list", "--count", f"HEAD..{remote}").strip()
    ahead = _git_in(repo, "rev-list", "--count", f"{remote}..HEAD").strip()
    incoming = {
        line
        for line in _git_in(repo, "diff", "--name-only", f"HEAD..{remote}").splitlines()
        if line
    }
    modified = {
        line
        for line in _git_in(repo, "diff", "--name-only", "HEAD").splitlines()
        if line
    }
    blocked = sorted(incoming & modified)
    return {
        "remote": remote,
        "behind": int(behind),
        "ahead": int(ahead),
        "fast_forwardable": int(behind) > 0 and int(ahead) == 0 and not blocked,
        "blocked_by": blocked,
    }


def _upstream_was_deleted(repo: Path, branch: str) -> bool:
    """True when ``branch`` was pushed once and its remote ref no longer exists.

    Squash-merging rewrites the commits, so a landed branch is never an ancestor of the
    line it landed on -- ``git cherry`` and ``rev-list`` both call it unmerged forever.
    What does move is the remote ref: every PR here lands with ``--delete-branch``, so
    a branch that has upstream config pointing at a ref origin no longer has is a
    branch whose PR closed.

    A branch that was never pushed has no upstream config and is never matched, so
    unpushed work cannot be swept this way. The remaining false positive -- a remote
    branch deleted without merging -- costs nothing: ``git worktree remove`` deletes
    the working directory, never the branch or its commits. That is why the clean
    check above is the load-bearing guard, not this one.
    """
    if not branch:
        return False
    remote = _git_in_quiet(repo, "config", "--get", f"branch.{branch}.remote").strip()
    merge = _git_in_quiet(repo, "config", "--get", f"branch.{branch}.merge").strip()
    if not remote or not merge:
        return False
    tracked = f"refs/remotes/{remote}/{merge.removeprefix('refs/heads/')}"
    return not _git_in_quiet(repo, "rev-parse", "--verify", "--quiet", tracked).strip()


def _git_in_quiet(repo: Path, *args: str) -> str:
    """``_git_in`` for probes where a non-zero exit is an answer, not a failure."""
    done = subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=False
    )
    return done.stdout if done.returncode == 0 else ""


def _claim_store() -> list[dict[str, object]]:
    """Every claim the store holds. Both claim checks read the same query.

    Used to answer by spawning ``.venv/bin/python -m conductor.candidate_review.cli
    claims`` with ``cwd=ROOT``: a second interpreter per query, and once the
    package lived under ``src/`` a relative ``.venv/bin/python`` resolved
    against ``src/`` and the query stopped working at all. The CLI's own rows --
    ``load_claims`` serialized exactly as its ``claims`` command prints them --
    answer in-process with no subprocess.
    """
    from conductor.candidate_review.cli import _stored_fields

    claims, _digest = load_claims(ROOT)
    return [_stored_fields(claim) for claim in claims]


def expired_claims() -> list[dict[str, str]]:
    now = datetime.now(UTC)
    out: list[dict[str, str]] = []
    for claim in _claim_store():
        expires = datetime.fromisoformat(claim["expires_at"])
        if expires < now:
            out.append(
                {
                    "claim_id": claim["claim_id"],
                    "owner": claim["owner"],
                    "expired_at": claim["expires_at"],
                }
            )
    return out


def idle_claims(idle_minutes: int = 90) -> list[dict[str, str]]:
    """Live claims whose holder has stopped touching the paths it reserved.

    Expiry is not the only failure mode. On 2026-08-29 a whole-prefix claim over
    `conductor/mutation_campaigns` -- the most contended directory in the repo --
    was held by a session that had stopped writing to it two hours earlier, and
    blocked an authorized seat from taking a child claim. Nothing ages out a
    quiet holder, so the cost lands entirely on whoever comes next; the overlap
    rule only protects against two SIMULTANEOUS writers.
    """
    now = datetime.now(UTC)
    out: list[dict[str, str]] = []
    for claim in _claim_store():
        if datetime.fromisoformat(claim["expires_at"]) < now:
            continue  # already reported as expired
        paths = [ROOT / p for p in claim.get("paths", [])]
        touched = [p.stat().st_mtime for p in paths if p.exists()]
        if not touched:
            continue
        # A claim taken on a file nobody has edited yet is not idle: measure
        # from the later of the claim's own creation and the last write, or a
        # month-old untouched file reads as 'idle 237657m' the second it is
        # claimed.
        created = claim.get("created_at")
        floor = datetime.fromisoformat(created).timestamp() if created else 0.0
        idle = (now.timestamp() - max(max(touched), floor)) / 60
        if idle < idle_minutes:
            continue
        out.append(
            {
                "claim_id": claim["claim_id"],
                "owner": claim["owner"],
                "idle_minutes": f"{idle:.0f}",
                "expires_at": claim["expires_at"],
                "paths": ", ".join(claim.get("paths", [])),
            }
        )
    return sorted(out, key=lambda row: -float(row["idle_minutes"]))


def unpushed_work() -> list[dict[str, str]]:
    """Local branches carrying commits that exist on no remote.

    The 2026-08-29 collapse found 95 commits living only on one machine -- a
    larger exposure than any branch count, and the reason local and CI disagreed
    about everything. A branch with no remote counterpart at all is reported
    with its full commit count.
    """
    out: list[dict[str, str]] = []
    listing = _git(
        "for-each-ref",
        "--format=%(refname:short)%09%(upstream:short)",
        "refs/heads",
    )
    for line in listing.splitlines():
        branch, _, upstream = line.partition("\t")
        if not branch:
            continue
        remote = upstream or f"origin/{branch}"
        try:
            ahead = _git("rev-list", "--count", f"{remote}..{branch}").strip()
        except HygieneError:
            # No remote at all: every commit on the branch is unpushed, so the
            # count is the branch length rather than a delta.
            ahead = _git("rev-list", "--count", branch).strip()
            remote = "(NO REMOTE -- whole branch is unpushed)"
        if ahead == "0":
            continue
        out.append({"branch": branch, "ahead": ahead, "remote": remote})
    return sorted(out, key=lambda row: -int(row["ahead"]))


def manifest_state() -> tuple[list[str], list[str]]:
    """(on disk but unregistered, registered but missing from disk)."""
    registry_path = ROOT / REGISTRY
    if not registry_path.is_file():
        raise HygieneError(f"registry not found: {REGISTRY}")
    payload = json.loads(registry_path.read_text())
    registered = {
        row["manifest"] for row in payload.get("campaigns", []) if "manifest" in row
    }
    # glob("*.json") misses dotfile manifests; five such orphans hid from the
    # 2026-08-26 fleet sweep that way.
    on_disk = {
        f"{CAMPAIGN_DIR}/{p.name}"
        for p in (ROOT / CAMPAIGN_DIR).iterdir()
        if p.is_file() and p.suffix == ".json" and p.name != "registry.json"
    }
    orphans = sorted(on_disk - registered)
    dangling = sorted(m for m in registered if not (ROOT / m).is_file())
    return orphans, dangling


def untracked_import_closure(tracked: Iterable[str]) -> list[dict[str, object]]:
    """Untracked modules transitively reachable by import from tracked code.

    These are exactly the modules that break a clean clone.
    """
    native_scan = _native.scan_untracked_import_closure_native  # pyright: ignore[reportAttributeAccessIssue]
    payload = json.loads(native_scan(str(ROOT), list(tracked)))
    records = payload["records"]
    assert isinstance(records, list)
    return records


def _claim_for_path(
    claims: Iterable[OwnershipClaim], path: str, *, now: datetime
) -> str:
    """The claim id covering ``path``, or ``UNCLAIMED``. First match wins."""
    for claim in claims:
        if claim.active(now) and any(paths_overlap(p, path) for p in claim.paths):
            return claim.claim_id
    return "UNCLAIMED"


def _git_in(repo: Path, *args: str) -> str:
    """Like ``_git``, but against an explicit repo rather than the module's own ROOT.

    The exposure checks below are called from ``branch_policy``'s CLI and from tests
    against throwaway repos -- they must never fall back to inspecting this process's
    own checkout, so every git call in this section takes ``repo`` explicitly.
    """
    completed = subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=False
    )
    if completed.returncode != 0:
        raise HygieneError(f"git {' '.join(args)} failed: {completed.stderr.strip()}")
    return completed.stdout


def local_only_commit_exposure(repo: Path = ROOT) -> list[dict[str, str]]:
    """Every commit ``branch_policy.local_only_commits`` finds, with claim coverage.

    A local-only commit is one that exists on no pushed ref at all -- the 2026-08-29
    incident class this whole reset addresses. Each row names the claim (or
    ``UNCLAIMED``) covering the files it touches, so a reviewer knows who to ask.
    """
    commits = branch_policy.local_only_commits(repo)
    if not commits:
        return []
    claims, _digest = load_claims(repo)
    now = datetime.now(UTC)
    out: list[dict[str, str]] = []
    for row in commits:
        touched = [
            line
            for line in _git_in(
                repo, "show", "--name-only", "--format=", row["sha"]
            ).splitlines()
            if line
        ]
        claim_id = "UNCLAIMED"
        for path in touched:
            hit = _claim_for_path(claims, path, now=now)
            if hit != "UNCLAIMED":
                claim_id = hit
                break
        out.append({"sha": row["sha"], "subject": row["subject"], "claim_id": claim_id})
    return out


def stale_dirty_files(
    stale_hours: float = 24.0, repo: Path = ROOT
) -> list[dict[str, str]]:
    """Dirty working-tree files (by mtime) older than ``stale_hours``, with claim coverage."""
    now = datetime.now(UTC)
    claims, _digest = load_claims(repo)
    out: list[dict[str, str]] = []
    for line in _git_in(
        repo, "status", "--porcelain", "--untracked-files=all"
    ).splitlines():
        if not line.strip():
            continue
        path = line[3:].strip()
        if " -> " in path:  # rename: report the destination
            path = path.split(" -> ", 1)[1]
        full = repo / path
        if not full.exists():
            continue
        age_hours = (now.timestamp() - full.stat().st_mtime) / 3600.0
        if age_hours <= stale_hours:
            continue
        out.append(
            {
                "path": path,
                "age_hours": f"{age_hours:.1f}",
                "claim_id": _claim_for_path(claims, path, now=now),
            }
        )
    return sorted(out, key=lambda row: -float(row["age_hours"]))


def _branch_created_at(repo: Path, name: str, live_ref: str) -> datetime | None:
    """Committer time of the oldest commit unique to ``name`` vs ``live_ref``."""
    lines = [
        line
        for line in _git_in(
            repo, "log", "--format=%cI", f"{live_ref}..{name}"
        ).splitlines()
        if line
    ]
    return datetime.fromisoformat(lines[-1]) if lines else None


def _branch_last_push_at(
    repo: Path, name: str, bindings: dict[str, branch_policy.BranchBinding]
) -> tuple[datetime | None, str]:
    """(timestamp, source). Prefers our own push record over the remote tip's commit
    date, which is only a proxy for push time (commit time != push time)."""
    binding = bindings.get(name)
    if binding is not None and binding.last_push_at:
        return datetime.fromisoformat(binding.last_push_at), "recorded push"
    remote_ref = f"refs/remotes/origin/{name}"
    verify = subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", remote_ref],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    if verify.returncode != 0:
        return None, "never pushed"
    stamp = _git_in(repo, "log", "-1", "--format=%cI", remote_ref).strip()
    return (
        datetime.fromisoformat(stamp) if stamp else None
    ), "remote-tip commit date (approx)"


def _pr_rows_for_branch(repo: Path, name: str) -> list[dict[str, object]] | None:
    """``gh pr list`` for one branch, or None if the lookup itself failed."""
    completed = subprocess.run(
        ["gh", "pr", "list", "--head", name, "--json", "number,state"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        return None
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError:
        return None


def stale_feature_branches(
    live_ref: str, repo: Path = ROOT
) -> tuple[list[dict[str, object]], bool]:
    """(rows, gh_available). EXPOSED at >6h since last push, or >24h since creation
    with no open PR (PR check only runs when ``gh`` is on PATH -- see the module note
    on why an absent ``gh`` is reported rather than silently read as "no PR")."""
    gh_available = shutil.which("gh") is not None
    now = datetime.now(UTC)
    bindings = {b.branch: b for b in branch_policy.load_bindings(repo)}
    rows: list[dict[str, object]] = []
    for name in _git_in(
        repo, "for-each-ref", "--format=%(refname:short)", "refs/heads"
    ).split():
        if branch_policy.is_integration_branch(name) or name == live_ref:
            continue
        created = _branch_created_at(repo, name, live_ref)
        if created is None:
            continue  # no unique commits: redundant_branches' territory, not exposure
        last_push, push_source = _branch_last_push_at(repo, name, bindings)
        push_hours = (
            None if last_push is None else (now - last_push).total_seconds() / 3600.0
        )
        reasons: list[str] = []
        if push_hours is None or push_hours > branch_policy.STALE_PUSH_HOURS:
            seen = "never" if push_hours is None else f"{push_hours:.1f}h ago"
            reasons.append(
                f"no push in {branch_policy.STALE_PUSH_HOURS:g}h (last push {seen}, "
                f"source={push_source})"
            )
        if gh_available:
            prs = _pr_rows_for_branch(repo, name)
            create_hours = (now - created).total_seconds() / 3600.0
            if not prs and create_hours > branch_policy.STALE_PR_HOURS:
                reasons.append(
                    f"no PR in {branch_policy.STALE_PR_HOURS:g}h "
                    f"(created {create_hours:.1f}h ago)"
                )
        if not reasons:
            continue
        binding = bindings.get(name)
        rows.append(
            {
                "branch": name,
                "claim_id": binding.claim_id if binding else "UNCLAIMED",
                "owner": binding.owner if binding else "UNCLAIMED",
                "reasons": reasons,
            }
        )
    return rows, gh_available


def reap_preview(repo: Path = ROOT) -> list[dict[str, str]]:
    """Name the worktrees the next ``make worktree-reap REAP_ARGS=--apply`` takes.

    This report used to say only that finished worktrees existed, which is why
    63 GB of them could stand while every check stayed green: nobody could tell
    from here what removal would actually do. These rows are the reaper's own
    decision, so the line and the enforcement cannot drift apart.
    """
    try:
        decisions = worktree_reap.decide(repo)
    except worktree_reap.ReapError as exc:
        return [
            {"worktree": str(repo), "branch": "", "reason": f"reap unavailable: {exc}"}
        ]
    return [
        {
            "worktree": str(decision.worktree.path),
            "branch": decision.worktree.branch,
            "reason": "; ".join(decision.reasons[:3]),
        }
        for decision in decisions
        if decision.eligible
    ]


def exposure_report(
    repo: Path = ROOT, live_ref: str | None = None
) -> dict[str, object]:
    """The four EXPOSED checks in one payload, for the session-preamble hook and CLI."""
    ref = live_ref or _git_in(repo, "rev-parse", "--abbrev-ref", "HEAD").strip()
    branches, gh_available = stale_feature_branches(ref, repo)
    return {
        "stale_feature_branches": branches,
        "gh_available": gh_available,
        "local_only_commit_exposure": local_only_commit_exposure(repo),
        "stale_dirty_files": stale_dirty_files(repo=repo),
        "reap_preview": reap_preview(repo),
    }


def render_exposure(report: dict[str, object]) -> str:
    branches = report["stale_feature_branches"]  # type: ignore[assignment]
    gh_note = (
        "" if report["gh_available"] else "  [gh NOT on PATH: PR staleness skipped]"
    )
    lines = [f"EXPOSED BRANCHES -- stale by push or PR ({len(branches)}){gh_note}"]  # type: ignore[arg-type]
    for row in branches:  # type: ignore[assignment]
        lines.append(
            f"  {row['branch']}  claim={row['claim_id']}  owner={row['owner']}"
        )
        for reason in row["reasons"]:  # type: ignore[index]
            lines.append(f"      {reason}")
    commits = report["local_only_commit_exposure"]  # type: ignore[assignment]
    lines.append(f"EXPOSED LOCAL-ONLY COMMITS -- on no pushed ref ({len(commits)})")  # type: ignore[arg-type]
    for row in commits:  # type: ignore[assignment]
        lines.append(f"  {row['sha'][:8]}  claim={row['claim_id']}  {row['subject']}")
    files = report["stale_dirty_files"]  # type: ignore[assignment]
    lines.append(f"EXPOSED STALE DIRTY FILES -- >24h uncommitted ({len(files)})")  # type: ignore[arg-type]
    for row in files:  # type: ignore[assignment]
        lines.append(
            f"  {row['age_hours']:>6}h  claim={row['claim_id']}  {row['path']}"
        )
    reapable = report.get("reap_preview", [])
    lines.append(
        f"EXPOSED WORKTREES -- next reap archives then removes ({len(reapable)})"  # type: ignore[arg-type]
    )
    for row in reapable:  # type: ignore[assignment]
        lines.append(f"  {row['worktree']}  [{row['branch']}]  {row['reason']}")
    return "\n".join(lines)


def cheap_exposure_counts(repo: Path = ROOT) -> dict[str, object]:
    """Counts cheap enough for a SessionStart hook (measured <100ms, 2026-08-29).

    Deliberately excludes ``stale_feature_branches``: that check shells out to ``gh``
    once per feature branch and measured ~10s over 33 branches in this repo -- past
    what a 10s-timeout hook can spend on one line of context. Branch staleness is
    reported by the full report / ``exposed`` CLI instead; this function says so
    rather than silently omitting it.
    """
    try:
        # Offline resolution: a session start must never open a network
        # connection (the ls-remote advertisement is the reaper's, not the
        # hook's). No line -> landed=None + the reason, as below.
        landed: int | None = len(
            landed_worktrees(_live_ref_or_default(repo, allow_network=False), repo)
        )
        worktrees_skipped = None
    except HygieneError as exc:
        # A repo with no integration line cannot be judged for containment. Report that
        # instead of a 0, which would read as "nothing to clean up", and keep the other
        # two counts rather than losing the whole line to one unanswerable check.
        landed, worktrees_skipped = None, str(exc)
    return {
        "local_only_commits": len(branch_policy.local_only_commits(repo)),
        "stale_dirty_files": len(stale_dirty_files(repo=repo)),
        "landed_worktrees": landed,
        "worktrees_skipped": worktrees_skipped,
        "branches_skipped": "PR lookup needs `gh`; run `python -m conductor.workspace_hygiene`",
    }


def exposure_line(repo: Path = ROOT) -> str:
    """The one-line EXPOSED summary the SessionStart inject carries.

    Moved here from ``session_preamble._exposure_line`` when the line became
    its own registry hook (``workspace_exposure_session``) so the native port
    serves exactly this text. Degrades visibly rather than crashing the hook
    or silently disappearing -- see ``cheap_exposure_counts`` for why branch
    staleness (needs ``gh``, ~10s over this repo's branch count) is excluded
    from this hook-safe path.
    """
    try:
        counts = cheap_exposure_counts(repo)
    except (ImportError, RuntimeError, OSError) as exc:
        return f"EXPOSED: unavailable ({exc}). python -m conductor.workspace_hygiene"
    landed = (
        counts["landed_worktrees"] if counts["worktrees_skipped"] is None else "unknown"
    )
    return (
        f"EXPOSED: {counts['local_only_commits']} local-only commit(s), "
        f"{counts['stale_dirty_files']} stale dirty file(s), "
        f"{landed} finished worktree(s) to remove, "
        "branches skipped (needs gh). "
        "python -m conductor.workspace_hygiene"
    )


def build_report(live_ref: str) -> Report:
    stale, dirty = worktree_state()
    # Removal is judged against the remote line, not ``live_ref``. A worktree is
    # only finished once its work reached origin; a local branch that is behind
    # would report finished worktrees as unfinished and the nag would never fire.
    landed_ref = _live_ref_or_default()
    orphans, dangling = manifest_state()
    stale_branches, gh_available = stale_feature_branches(live_ref)
    return Report(
        live_ref=live_ref,
        redundant_branches=redundant_branches(live_ref, {_default_branch(), live_ref}),
        stale_worktrees=stale,
        dirty_worktrees=dirty,
        landed_worktrees=landed_worktrees(landed_ref),
        landed_ref=landed_ref,
        expired_claims=expired_claims(),
        idle_claims=idle_claims(),
        unpushed_work=unpushed_work(),
        orphan_manifests=orphans,
        dangling_manifests=dangling,
        untracked_imports=untracked_import_closure(tracked_files()),
        stale_feature_branches=stale_branches,
        gh_available=gh_available,
        local_only_commit_exposure=local_only_commit_exposure(),
        stale_dirty_files=stale_dirty_files(),
        worktree_leases=lease_state(
            Path(e["worktree"])
            for e in _worktree_entries()
            if e.get("worktree") and is_linked_worktree(Path(e["worktree"]))
        ),
        checkout_drift=checkout_drift(),
    )


def _render_drift_and_leases(report: Report) -> list[str]:
    """The two states that let the shared checkout and its worktrees go stale."""

    drift = report.checkout_drift
    lines: list[str] = []
    if drift.get("behind"):
        lines.append(
            f"CHECKOUT BEHIND -- {drift['behind']} commit(s) behind {drift['remote']}"
        )
        if drift.get("fast_forwardable"):
            lines.append("  a fast-forward makes it even; untracked files are kept")
            lines.append("  make checkout-sync")
        else:
            for path in drift.get("blocked_by") or []:
                lines.append(f"  blocked by local edit: {path}")
            if drift.get("ahead"):
                lines.append(
                    f"  and {drift['ahead']} commit(s) ahead: this checkout is "
                    "carrying work nobody else can see"
                )

    overdue = report.overdue_leases()
    lines.append(
        f"WORKTREES PAST THEIR LEASE -- ask the owner, then remove ({len(overdue)})"
    )
    for row in overdue:
        if row["status"] == "unleased":
            lines.append(
                f"  {row['worktree']} -- no lease: created outside `make worktree`, "
                "so nobody knows what it is for"
            )
            continue
        lines.append(
            f"  {row['worktree']} [{row['branch']}] -- {row['owner']}, "
            f"{row['overdue_minutes']} min overdue: {row['purpose']}"
        )
    return lines


def render(report: Report) -> str:
    lines = [f"workspace hygiene | live line: {report.live_ref}", ""]
    lines.extend(_render_drift_and_leases(report))
    lines.append("")

    safe = [r for r in report.redundant_branches if "blocked_by" not in r]
    blocked = [r for r in report.redundant_branches if "blocked_by" in r]
    lines.append(f"SAFE TO DELETE -- branches with 0 unique commits ({len(safe)})")
    for row in safe:
        lines.append(f"  {row['sha'][:8]}  {row['branch']}")
    if blocked:
        lines.append(f"REDUNDANT REF, BLOCKED -- do not delete yet ({len(blocked)})")
        for row in blocked:
            lines.append(
                f"  {row['sha'][:8]}  {row['branch']}  <-- {row['blocked_by']}"
            )
    lines.append(
        f"SAFE TO PRUNE -- worktree registrations with no directory ({len(report.stale_worktrees)})"
    )
    for path in report.stale_worktrees:
        lines.append(f"  {path}")

    lines.append("")
    lines.append(
        f"SAFE TO REMOVE -- worktrees whose work is already on {report.landed_ref} "
        f"({len(report.landed_worktrees)})"
    )
    for row in report.landed_worktrees:
        lines.append(
            f"  {row['head']}  {row['worktree']} [{row['branch']}] -- {row['reason']}\n"
            f"      git worktree remove {row['worktree']}"
        )

    lines.append("")
    lines.append(
        f"DO NOT REMOVE -- worktrees holding uncommitted work ({len(report.dirty_worktrees)})"
    )
    for row in report.dirty_worktrees:
        lines.append(
            f"  {row['worktree']} [{row['branch']}] -- {row['entries']} entries"
        )
        for sample in row["sample"]:  # type: ignore[index]
            lines.append(f"      {sample}")

    lines.append("")
    lines.append(
        f"UNPUSHED -- commits that exist on no remote ({len(report.unpushed_work)})"
    )
    for row in report.unpushed_work:
        lines.append(f"  {row['ahead']:>4} ahead  {row['branch']}  vs {row['remote']}")
    lines.append("")
    lines.append(
        f"IDLE CLAIMS -- live, but the holder stopped writing ({len(report.idle_claims)})"
    )
    for row in report.idle_claims:
        lines.append(
            f"  {row['claim_id']}  {row['owner']}  idle {row['idle_minutes']}m  {row['paths']}"
        )
    lines.append("")
    lines.append(f"EXPIRED CLAIMS ({len(report.expired_claims)})")
    for claim in report.expired_claims:
        lines.append(
            f"  {claim['claim_id']}  owner={claim['owner']}  expired={claim['expired_at']}"
        )

    lines.append("")
    lines.append(
        f"ORPHAN MANIFESTS -- on disk, unregistered ({len(report.orphan_manifests)})"
    )
    for path in report.orphan_manifests:
        lines.append(f"  {path}")
    lines.append(
        f"DANGLING -- registered, file missing ({len(report.dangling_manifests)})"
    )
    for path in report.dangling_manifests:
        lines.append(f"  {path}  <-- repo-wide REFUSE risk")

    lines.append("")
    lines.append(
        f"BREAKS A CLEAN CLONE -- untracked, import-reachable from tracked code ({len(report.untracked_imports)})"
    )
    for row in report.untracked_imports:
        tracked_n = len(row["tracked_importers"])  # type: ignore[arg-type]
        total = len(row["importers"])  # type: ignore[arg-type]
        lines.append(f"  {row['module']}  <- {total} importer(s), {tracked_n} tracked")

    lines.append("")
    lines.append(
        render_exposure(
            {
                "stale_feature_branches": report.stale_feature_branches,
                "gh_available": report.gh_available,
                "local_only_commit_exposure": report.local_only_commit_exposure,
                "stale_dirty_files": report.stale_dirty_files,
                "reap_preview": reap_preview(),
            }
        )
    )
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--live-ref",
        default=None,
        help="branch treated as the integration line (default: current HEAD branch)",
    )
    parser.add_argument("--json", action="store_true", help="emit JSON")
    parser.add_argument(
        "--exit-nonzero-on-exposure",
        action="store_true",
        help="exit 1 if any EXPOSED finding is present (for hook use; default always exits 0)",
    )
    args = parser.parse_args()

    live_ref = args.live_ref or _git("rev-parse", "--abbrev-ref", "HEAD").strip()
    if live_ref == "HEAD":
        raise HygieneError("detached HEAD: pass --live-ref explicitly")
    report = build_report(live_ref)

    if args.json:
        print(json.dumps(report.__dict__, indent=2, sort_keys=True))
    else:
        print(render(report))
    if args.exit_nonzero_on_exposure and report.has_exposure():
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
