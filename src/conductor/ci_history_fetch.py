"""Fetch the ``ci_history`` cache the ledger's outcome join reads.

`native/forge`'s outcome join (`ledger/outcome.rs`) fills three
`task_dispatch` columns -- `landed`, `required_rework`,
`ci_red_on_first_push` -- from a cache file it reads on disk and never
writes: ``ledger/ci_history/<owner>_<name>.json``. This module is the
fetcher that writes it (Python glue over ``gh`` and local ``git``,
per `CLAUDE.md`'s language hierarchy; the schema is `docs/ledger.md`'s
"ci_history cache schema" and is frozen by the Rust fixture).

Shape, per merged PR (keyed by PR number as a string): the branch name,
the first commit ever pushed to the branch with its first-push CI
verdict (green / red / unknown), and the pre-squash commit list with
each commit's ``Agent:`` / ``Claude-Session:`` trailers. `git log
--first-parent main` cannot see any of this -- squash-merged PRs leave
one commit on ``main`` -- which is why the join needs this cache.

Trailers come from ``git interpret-trailers --parse`` on each commit's
full message read out of the local clone (the PR head is fetched when
the pre-squash commits are missing), never from a regex: the tool owns
what a trailer block is. Incremental by ``fetched_utc``: a PR already
cached and merged no later than the last fetch is left alone, so
re-runs cost one ``pr list`` plus whatever is new. Failures are honest
about how much they know: an unusable environment (no ``gh``, no
authentication, an unreadable existing file) exits 2 and writes
nothing; a PR that could not be resolved is named on stderr while every
PR that did resolve is still written, and the run exits 1.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Mapping, Sequence

from pydantic import BaseModel, ConfigDict, Field

EXIT_OK = 0
EXIT_PARTIAL = 1  # some PRs unresolved; everything resolvable was written
EXIT_ENV = 2  # gh missing/unauthenticated, unreadable inputs; nothing written

# gh check-run conclusions: a completed run that did not succeed, and the
# ones that count as success. `skipped`/`neutral` completed neither passed
# nor failed; they do not make a first push red.
_FAILED_CONCLUSIONS = frozenset(
    {"failure", "timed_out", "cancelled", "startup_failure"}
)
_SUCCEEDED_CONCLUSIONS = frozenset({"success", "skipped", "neutral"})
# gh's own words for "slow down", matched case-insensitively against
# stderr so a rate limit is never mistaken for a per-PR defect.
_RATE_LIMIT_MARKERS = ("rate limit", "too many requests")
_BACKOFF_SECONDS = 30.0
_BACKOFF_ATTEMPTS = 3
_AUTH_MARKERS = ("not logged in", "authentication required", "gh auth login")
# gh caps `pr list` at its own default of 30 when no --limit is passed, and
# "the merged PRs" means all of them: the join treats a PR missing from the
# cache as unknown, so a silent 30-newest cap would quietly blank the outcome
# columns of every older PR. An explicit large default; the incremental rule
# keeps re-runs at one `pr list` plus whatever is new regardless.
_DEFAULT_LIMIT = 1000


class EnvError(RuntimeError):
    """The fetch cannot run at all: gh/git missing, unauthenticated, bad input."""


class GhError(RuntimeError):
    """A gh call failed for reasons other than the environment."""


class CiTrailers(BaseModel):
    """The two trailers the outcome join reads; absent means absent key."""

    model_config = ConfigDict(extra="forbid")

    agent: str | None = Field(default=None, alias="Agent")
    claude_session: str | None = Field(default=None, alias="Claude-Session")


class CiCommit(BaseModel):
    model_config = ConfigDict(extra="forbid")

    sha: str
    subject: str
    trailers: CiTrailers = Field(default_factory=CiTrailers)


class CiPr(BaseModel):
    model_config = ConfigDict(extra="forbid")

    branch: str
    first_push_sha: str
    first_push_ci: str  # "green" | "red" | "unknown"; pinned by the fixture test
    commits: list[CiCommit]


class CiHistory(BaseModel):
    model_config = ConfigDict(extra="forbid")

    fetched_utc: str
    prs: dict[str, CiPr]

    def dump(self) -> str:
        """Canonical JSON: the shape the Rust fixture freezes, key for key."""

        payload = self.model_dump(by_alias=True, exclude_none=True, mode="json")
        return json.dumps(payload, indent=2, sort_keys=True) + "\n"


def _run(
    argv: Sequence[str], *, stdin: str | None = None, cwd: Path | None = None
) -> str:
    """One subprocess call; stdout as text, everything else raised loud."""

    completed = subprocess.run(
        list(argv),
        input=stdin,
        cwd=cwd,
        capture_output=True,
        text=True,
        timeout=600,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"{argv[0]} exited {completed.returncode}: "
            f"{completed.stderr.strip() or completed.stdout.strip()}"
        )
    return completed.stdout


def parse_owner_repo(url: str) -> tuple[str, str] | None:
    """``owner, name`` out of an origin URL, `.git`-stripped (the Rust slug)."""

    stripped = url.strip().removesuffix(".git").rstrip("/")
    tail = stripped.split("github.com", 1)
    if len(tail) != 2:
        return None
    parts = tail[1].lstrip(":/").split("/")
    if len(parts) != 2 or not all(parts):
        return None
    return parts[0], parts[1]


def origin_slug(repo: Path) -> tuple[str, str]:
    """The repo's ``origin`` owner/name, or a loud refusal when there is none."""

    url = _run(["git", "-C", str(repo), "remote", "get-url", "origin"]).strip()
    parsed = parse_owner_repo(url)
    if parsed is None:
        raise EnvError(f"could not parse an owner/repo out of origin URL {url!r}")
    return parsed


class GhClient:
    """Every gh call the fetcher makes, rate-limit aware."""

    def __init__(self, repo: Path, owner: str, name: str) -> None:
        self.repo = repo
        self.owner = owner
        self.name = name

    def _run_gh(self, *args: str) -> str:
        argv = ["gh", *args]
        for attempt in range(_BACKOFF_ATTEMPTS):
            try:
                completed = subprocess.run(
                    argv, capture_output=True, text=True, timeout=600
                )
            except FileNotFoundError as exc:
                raise EnvError("gh is not on PATH") from exc
            if completed.returncode == 0:
                return completed.stdout
            stderr = completed.stderr.strip()
            if any(marker in stderr.lower() for marker in _AUTH_MARKERS):
                raise EnvError(f"gh is not authenticated: {stderr}")
            if any(marker in stderr.lower() for marker in _RATE_LIMIT_MARKERS):
                if attempt + 1 < _BACKOFF_ATTEMPTS:
                    print(
                        f"gh rate-limited ({args[0]}); backing off "
                        f"{_BACKOFF_SECONDS:g}s",
                        file=sys.stderr,
                    )
                    time.sleep(_BACKOFF_SECONDS)
                    continue
            raise GhError(f"gh {' '.join(args)} exited {completed.returncode}: {stderr}")
        raise GhError(f"gh {' '.join(args)} kept rate-limiting; gave up")

    def require_ready(self) -> None:
        """Fail loud up front when gh is missing or unauthenticated."""

        self._run_gh("auth", "status")

    def list_merged(self, *, limit: int | None = None) -> list[dict[str, Any]]:
        args = [
            "pr",
            "list",
            "--state",
            "merged",
            "--json",
            "number,headRefName,mergedAt",
            "--limit",
            str(limit if limit is not None else _DEFAULT_LIMIT),
        ]
        return json.loads(self._run_gh(*args))

    def view_pr(self, number: int) -> dict[str, Any]:
        payload = self._run_gh(
            "pr",
            "view",
            str(number),
            "--json",
            "number,headRefName,commits",
        )
        return json.loads(payload)

    def check_runs(self, sha: str) -> dict[str, Any]:
        payload = self._run_gh(
            "api",
            f"repos/{self.owner}/{self.name}/commits/{sha}/check-runs",
        )
        return json.loads(payload)


def trailers_of(message: str, repo: Path) -> dict[str, str]:
    """The trailer block of ``message``, via ``git interpret-trailers --parse``.

    The tool decides what a trailer is; this only splits its output lines
    on the first colon and folds indented continuation lines into the
    value above them.
    """

    parsed = _run(
        ["git", "-C", str(repo), "interpret-trailers", "--parse"],
        stdin=message,
    )
    trailers: dict[str, str] = {}
    last_key: str | None = None
    for line in parsed.splitlines():
        if not line.strip():
            last_key = None
            continue
        if line[:1].isspace() and last_key is not None:
            trailers[last_key] += " " + line.strip()
            continue
        key, separator, value = line.partition(":")
        if not separator:
            last_key = None
            continue
        last_key = key.strip()
        trailers[last_key] = value.strip()
    return trailers


def commit_messages(repo: Path, number: int, shas: Sequence[str]) -> dict[str, str]:
    """Full commit messages out of the local clone, fetching the PR head first.

    The pre-squash commits of a merged PR are not on ``main``; the PR head
    ref is permanent on GitHub, so one fetch makes every message readable
    locally. A sha that is still missing afterwards is the caller's loud
    per-PR failure, never a guess.
    """

    _run(["git", "-C", str(repo), "fetch", "-q", "origin", f"refs/pull/{number}/head"])
    messages: dict[str, str] = {}
    for sha in shas:
        messages[sha] = _run(
            ["git", "-C", str(repo), "show", "-s", "--format=%B", sha]
        )
    return messages


def first_push_ci(check_runs: Mapping[str, Any]) -> str:
    """green iff every completed check succeeded, red iff any failed, else unknown."""

    runs = check_runs.get("check_runs") or []
    completed = [
        run.get("conclusion") for run in runs if run.get("status") == "completed"
    ]
    if any(conclusion in _FAILED_CONCLUSIONS for conclusion in completed):
        return "red"
    if completed and all(
        conclusion in _SUCCEEDED_CONCLUSIONS for conclusion in completed
    ):
        return "green"
    return "unknown"


def _ci_trailers(message: str, repo: Path) -> CiTrailers:
    parsed = trailers_of(message, repo)
    return CiTrailers.model_validate(
        {
            key: value
            for key, value in parsed.items()
            if key in ("Agent", "Claude-Session")
        }
    )


def build_pr(gh: GhClient, repo: Path, number: int) -> CiPr:
    """One PR's cache row: pre-squash commits, trailers, first-push verdict."""

    view = gh.view_pr(number)
    entries = view["commits"]
    if not entries:
        raise GhError(f"PR {number} has no commits")
    shas = [entry["oid"] for entry in entries]
    messages = commit_messages(repo, number, shas)
    return CiPr(
        branch=view["headRefName"],
        first_push_sha=shas[0],
        first_push_ci=first_push_ci(gh.check_runs(shas[0])),
        commits=[
            CiCommit(
                sha=entry["oid"],
                subject=entry["messageHeadline"],
                trailers=_ci_trailers(messages[entry["oid"]], repo),
            )
            for entry in entries
        ],
    )


def load_existing(path: Path) -> CiHistory | None:
    """The cache as it stands; absent is no-data-yet, malformed is a loud error."""

    if not path.is_file():
        return None
    try:
        return CiHistory.model_validate_json(path.read_text(encoding="utf-8"))
    except ValueError as exc:
        raise EnvError(f"existing ci_history cache {path} is unreadable: {exc}") from exc


def merge(
    existing: CiHistory | None,
    fetched: Mapping[str, CiPr],
    now_utc: str,
) -> CiHistory:
    """Cached rows survive verbatim; fetched rows land on top; one timestamp."""

    prs = dict(existing.prs) if existing else {}
    prs.update(fetched)
    return CiHistory(fetched_utc=now_utc, prs=prs)


def write_atomic(history: CiHistory, out: Path) -> None:
    """Write beside the target, then rename -- a reader never sees half a file."""

    out.parent.mkdir(parents=True, exist_ok=True)
    temp = out.with_name(f".{out.name}.tmp")
    temp.write_text(history.dump(), encoding="utf-8")
    os.replace(temp, out)


def _rfc3339_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _merged_before(merged_at: str, fetched_utc: str) -> bool:
    """Both are RFC 3339 UTC; naive parsing is fine because both end in Z."""

    return merged_at <= fetched_utc


def fetch(
    gh: GhClient,
    repo: Path,
    out: Path,
    *,
    since_days: int | None = None,
    max_prs: int | None = None,
) -> tuple[int, list[int]]:
    """Merge a fresh fetch into ``out``; (exit code, unresolved PR numbers)."""

    gh.require_ready()
    existing = load_existing(out)
    merged = gh.list_merged(limit=max_prs)
    if since_days is not None:
        cutoff = datetime.now(timezone.utc) - timedelta(days=since_days)
        merged = [
            entry
            for entry in merged
            if _parse_rfc3339(entry["mergedAt"]) >= cutoff
        ]
    fetched: dict[str, CiPr] = {}
    unresolved: list[int] = []
    for entry in merged:
        number = entry["number"]
        if (
            existing is not None
            and str(number) in existing.prs
            and _merged_before(entry["mergedAt"], existing.fetched_utc)
        ):
            continue
        try:
            fetched[str(number)] = build_pr(gh, repo, number)
        except EnvError:
            raise  # an environment that died mid-run is not a per-PR defect
        except (GhError, RuntimeError) as exc:
            print(f"PR {number} unresolved: {exc}", file=sys.stderr)
            unresolved.append(number)
    if fetched or unresolved:
        write_atomic(merge(existing, fetched, _rfc3339_now()), out)
    return (EXIT_PARTIAL if unresolved else EXIT_OK), unresolved


def _parse_rfc3339(stamp: str) -> datetime:
    return datetime.fromisoformat(stamp.replace("Z", "+00:00"))


def _dry_run(gh: GhClient, repo: Path, out: Path) -> None:
    """Print the plan without one gh call or one byte written."""

    existing = load_existing(out)
    slug = f"{gh.owner}/{gh.name}"
    plan = {
        "dry_run": True,
        "repo": str(repo),
        "slug": slug,
        "out": str(out),
        "would_run": [
            "gh auth status",
            "gh pr list --state merged --json number,headRefName,mergedAt",
            f"gh pr view <n> --json number,headRefName,commits  (uncached PRs of {slug})",
            f"gh api repos/{slug}/commits/<first-sha>/check-runs  (uncached PRs)",
            f"git -C {repo} fetch -q origin refs/pull/<n>/head  (missing PR heads)",
        ],
        "cached_prs_left_alone": sorted(existing.prs) if existing else [],
    }
    print(json.dumps(plan, indent=2, sort_keys=True))


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="conductor.ci_history_fetch",
        description="Fetch the ci_history cache the ledger's outcome join reads.",
    )
    parser.add_argument("--repo", default=".", help="repository checkout (default .)")
    parser.add_argument("--out", required=True, help="cache file to write")
    parser.add_argument(
        "--since-days",
        type=int,
        default=None,
        help="only PRs merged in the last N days",
    )
    parser.add_argument(
        "--max-prs", type=int, default=None, help="at most N merged PRs (gh --limit)"
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="print what would be fetched/written; no gh calls, no write",
    )
    args = parser.parse_args(argv)

    repo = Path(args.repo).resolve()
    if not repo.is_dir():
        print(f"repository {repo} is not a directory", file=sys.stderr)
        return EXIT_ENV
    try:
        owner, name = origin_slug(repo)
    except (EnvError, RuntimeError) as exc:
        print(exc, file=sys.stderr)
        return EXIT_ENV
    gh = GhClient(repo, owner, name)
    out = Path(args.out)
    if args.dry_run:
        try:
            _dry_run(gh, repo, out)
        except EnvError as exc:
            print(exc, file=sys.stderr)
            return EXIT_ENV
        return EXIT_OK
    try:
        code, unresolved = fetch(
            gh,
            repo,
            out,
            since_days=args.since_days,
            max_prs=args.max_prs,
        )
    except EnvError as exc:
        print(exc, file=sys.stderr)
        return EXIT_ENV
    if unresolved:
        print(f"unresolved PRs: {', '.join(str(n) for n in unresolved)}", file=sys.stderr)
    return code


if __name__ == "__main__":
    sys.exit(main())
