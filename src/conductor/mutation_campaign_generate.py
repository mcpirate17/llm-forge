"""Automatically derive conductor engine-campaign manifests from the tree.

SCOPE RULE -- mutate only what you changed. A campaign covers the files this
branch modified and the tests that exercise them, nothing else. That is the
default here: with no flags, planning is restricted to ``git diff --name-only
<base>...HEAD`` plus the dirty working tree, so a three-file change plans three
campaigns in about a second. A whole-tree sweep requires ``--all-files`` and is
a maintenance operation, not something an agent does while landing a change.

Automatic conductor mutation testing has two deliberate stages: this module
enumerates mutable subjects, pairs them with tests, and writes engine manifests;
``mutation_engine_generated`` then invokes the engine in a disposable snapshot
and produces source-bound evidence. Agents do neither stage manually.

Historic patch campaigns are archival provenance only. They do not participate in
manifest discovery, mutation generation, campaign execution, or acceptance.

`plan` is also the answer to a question that is not about mutation at all: a
subject it reports as unpaired has no test named after it, and mutating code no
test reaches measures nothing. That list is where to look before deleting.
"""

from __future__ import annotations

import argparse
import json
import subprocess
from collections.abc import Iterable, Mapping, Sequence
from datetime import UTC, datetime
from pathlib import Path, PurePosixPath
from typing import Any

from conductor.candidate_review.identity import OwnerIdentityError, resolve_owner
from conductor.candidate_review.ownership import (
    OwnershipError,
    load_claims,
    paths_overlap,
)
from conductor.mutation_campaign_model import REPO_ROOT, _native_json_call
from conductor.mutation_plan_bridge import plan_native as _plan_native
from conductor.mutation_scope import CampaignError
from conductor.project_paths import campaigns_relative, campaigns_root


def _is_test(relative: str, name: str) -> bool:
    return name.startswith("test_") or name == "conftest.py" or "/tests/" in relative


def changed_sources(
    base: str = "origin/master",
    *,
    repo_root: Path = REPO_ROOT,
    owner: str | None = None,
) -> set[str]:
    """Repo-relative paths this branch touched, bounded by live lane claims.

    This is the default scope of a campaign. An agent that changed three files
    mutates three files; a repo-wide sweep is a separate, explicitly requested
    maintenance run (``--all-files``). Untracked files count -- a brand new
    module is exactly the thing whose tests have never been mutated.
    """

    def _git(*args: str) -> list[str]:
        proc = subprocess.run(
            ("git", "-C", str(repo_root), *args),
            capture_output=True,
            text=True,
            check=False,
        )
        if proc.returncode != 0:
            detail = proc.stderr.strip() or proc.stdout.strip() or "no output"
            raise CampaignError(
                f"cannot determine mutation scope with git {' '.join(args)}: {detail}"
            )
        return [line for line in proc.stdout.splitlines() if line.strip()]

    paths = set(_git("diff", "--name-only", f"{base}...HEAD"))
    dirty = set(_git("diff", "--name-only", "HEAD"))
    dirty |= set(_git("ls-files", "--others", "--exclude-standard"))
    if dirty:
        try:
            lane = owner or resolve_owner(repo_root)
            claims, _ = load_claims(repo_root)
        except (OwnerIdentityError, OwnershipError) as exc:
            raise CampaignError(f"cannot resolve ownership scope: {exc}") from exc
        now = datetime.now(UTC)
        owned_paths = [
            path
            for claim in claims
            if claim.owner == lane and claim.active(now)
            for path in claim.paths
        ]
        if not owned_paths:
            raise CampaignError(
                f"dirty mutation scope exists but {lane!r} has no active ownership "
                "claim. Create a narrow claim or pass --only for the exact files "
                "you changed."
            )
        dirty = {
            candidate
            for candidate in dirty
            if any(paths_overlap(candidate, claimed) for claimed in owned_paths)
        }
    paths |= dirty
    return {p for p in paths if (repo_root / p).exists()}


def plan(
    language: str,
    *,
    repo_root: Path = REPO_ROOT,
    owner: str = "claude",
    day: str | None = None,
    jobs: int = 4,
    run_timeout_seconds: int = 1800,
    only_sources: Sequence[str] | None = None,
    include_covered: bool = False,
    extra_tests: Mapping[str, Sequence[str]] | None = None,
) -> dict[str, Any]:
    """What `write` would emit, plus the subjects it refuses to emit anything for.

    ``only_sources`` is the scope rule: pass the paths this branch changed and
    nothing else is planned. ``None`` means the whole tree, which callers should
    reach only when the operator asked for a maintenance sweep.

    ``include_covered`` plans a subject an existing campaign already names. The
    incumbent is usually broader than the change -- one campaign over five engine
    modules, rotted by an edit to one of them -- and re-running it costs an hour
    to answer a question about a single file. A second, narrower campaign is the
    supported answer; the newest valid receipt wins the evidence row, and the
    incumbent stays registered rather than being retired to make room.

    The walk, pairing and manifest emission run through the native planner.
    """
    if language not in ("python", "rust"):
        raise CampaignError(f"unknown language {language!r}; known: python, rust")
    if extra_tests and language != "python":
        raise CampaignError("--extra-test pairs python subjects only")
    ctx = {
        "owner": owner,
        "day": day or datetime.now(UTC).strftime("%Y%m%d"),
        "jobs": jobs,
        "run_timeout_seconds": run_timeout_seconds,
        "only_sources": only_sources,
        "include_covered": include_covered,
        "extra_tests": extra_tests,
    }
    return _plan_native(language, repo_root, ctx)


def write(
    manifests: Iterable[Mapping[str, Any]],
    *,
    repo_root: Path = REPO_ROOT,
    force: bool = False,
) -> list[str]:
    """Write each manifest under `conductor/mutation_campaigns/`.

    Refuses to overwrite an existing manifest without `force`: a manifest that
    has run carries a recorded survivor baseline, and replacing it with an empty
    one would erase the ratchet while still reporting green.
    """

    written: list[str] = []
    for manifest in manifests:
        relative = f"{campaigns_relative(repo_root)}/{manifest['campaign_id']}.json"
        destination = repo_root / relative
        if destination.exists() and not force:
            raise CampaignError(
                f"{relative} already exists; refusing to replace a recorded "
                "survivor baseline with an empty one (pass --force to overwrite)"
            )
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(
            json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        written.append(relative)
    return written


def _load_generated_cargo_campaign(
    campaign: str, repo_root: Path
) -> tuple[Path, dict[str, Any]]:
    """Resolve a campaign name to its path and parsed generated manifest."""

    relative = campaign.removeprefix(f"{campaigns_relative(repo_root)}/")
    if not relative.endswith(".json"):
        relative = f"{relative}.json"
    path = campaigns_root(repo_root) / relative
    try:
        existing = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise CampaignError(f"cannot load generated campaign {path}: {exc}") from exc
    if not isinstance(existing, dict):
        raise CampaignError(f"{path} is not a generated campaign")
    return path, existing


def refresh_python_campaign(campaign: str, *, repo_root: Path = REPO_ROOT) -> str:
    """Regenerate one generated Python campaign while retaining its engine baseline."""

    path, existing = _load_generated_cargo_campaign(campaign, repo_root)
    if existing.get("mutation_engine") != "fest":
        raise CampaignError(f"{path} is not a generated fest campaign")
    generator = existing.get("generator") or {}
    refreshed = _native_json_call(
        "mutation_refresh_native",
        {
            "language": "python",
            "repo_root": str(repo_root),
            "manifest_path": str(path),
            "campaign_id": str(existing.get("campaign_id") or path.stem),
            "existing": existing,
            "run_timeout_seconds": int(generator.get("run_timeout_seconds", 1800)),
        },
    )
    path.write_text(
        json.dumps(refreshed, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return path.relative_to(repo_root).as_posix()


def refresh_rust_campaign(
    campaign: str,
    *,
    sources: Sequence[str] = (),
    repo_root: Path = REPO_ROOT,
) -> str:
    """Regenerate one existing cargo-mutants manifest without erasing its ratchet.

    This is the automatic refresh path after a source or inline-test change.  It
    derives every pin from the current crate tree and retains only the recorded
    survivor baseline, which is evidence produced by the engine rather than an
    agent-authored field.
    """

    path, existing = _load_generated_cargo_campaign(campaign, repo_root)
    if existing.get("mutation_engine") != "cargo-mutants":
        raise CampaignError(f"{path} is not a cargo-mutants generated campaign")
    generator = existing.get("generator") or {}
    refreshed = _native_json_call(
        "mutation_refresh_native",
        {
            "language": "rust",
            "repo_root": str(repo_root),
            "manifest_path": str(path),
            "campaign_id": str(existing.get("campaign_id") or path.stem),
            "existing": existing,
            "sources": list(sources),
            "jobs": int(generator.get("jobs", 4)),
            "run_timeout_seconds": int(generator.get("run_timeout_seconds", 1800)),
        },
    )
    path.write_text(
        json.dumps(refreshed, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return path.relative_to(repo_root).as_posix()


def _summarise(result: Mapping[str, Any], *, verbose: bool) -> dict[str, Any]:
    manifests = list(result["manifests"])
    payload: dict[str, Any] = {
        "status": "READY",
        "scope": result.get("scope", "all-files"),
        "language": result["language"],
        "campaigns": len(manifests),
        "subjects_without_a_named_test": len(result["unpaired"]),
        "lines_without_a_named_test": result["unpaired_lines"],
        "already_covered": len(result["already_covered"]),
        "crates_with_no_tests": len(result.get("untested", ())),
    }
    if verbose:
        payload["campaign_ids"] = [m["campaign_id"] for m in manifests]
        payload["unpaired"] = result["unpaired"]
        payload["already_covered_subjects"] = result["already_covered"]
    return payload


def _branch_scope(
    base: str,
    *,
    repo_root: Path = REPO_ROOT,
    owner: str | None = None,
) -> list[str]:
    """The default subject set: what this branch changed, and nothing else.

    Refusing an empty scope is the point. A campaign that plans nothing because
    the branch changed nothing is a maintenance sweep asked for by accident, and
    the whole-tree run it would become is the behaviour this rule exists to stop.
    """

    scope = sorted(changed_sources(base, repo_root=repo_root, owner=owner))
    if not scope:
        raise CampaignError(
            f"nothing changed against {base}: mutation campaigns cover the files "
            "this branch modified and the tests that exercise them. Pass "
            "--all-files only for an explicitly requested whole-tree maintenance "
            "sweep."
        )
    return scope


def _explicit_scope(paths: Sequence[str], *, repo_root: Path = REPO_ROOT) -> list[str]:
    """Validate an exact subject list without inspecting shared checkout dirt."""

    scope: list[str] = []
    for raw in paths:
        normalized = raw.replace("\\", "/")
        candidate = PurePosixPath(normalized)
        if candidate.is_absolute() or any(
            part in {".", ".."} for part in normalized.split("/")
        ):
            raise CampaignError(f"--only must name a repository-relative file: {raw!r}")
        relative = candidate.as_posix()
        if not (repo_root / relative).is_file():
            raise CampaignError(f"--only source does not exist: {relative}")
        scope.append(relative)
    if not scope:
        raise CampaignError("--only needs at least one exact repository-relative file")
    return sorted(set(scope))


def _extra_tests(
    pairs: Sequence[str], *, repo_root: Path = REPO_ROOT
) -> dict[str, list[str]]:
    """Parse `--extra-test SOURCE=TEST` into exact repository-relative pairs."""

    extra: dict[str, list[str]] = {}
    for raw in pairs:
        source, sep, test = raw.partition("=")
        if not sep:
            raise CampaignError(f"--extra-test wants SOURCE=TEST, got {raw!r}")
        (source,) = _explicit_scope([source], repo_root=repo_root)
        (test,) = _explicit_scope([test], repo_root=repo_root)
        if not _is_test(test, PurePosixPath(test).name):
            raise CampaignError(f"--extra-test target is not a test file: {test}")
        extra.setdefault(source, []).append(test)
    return extra


def _cli_parser() -> argparse.ArgumentParser:
    """Build the generated-campaign CLI without coupling parsing to execution."""

    parser = argparse.ArgumentParser(description=__doc__)
    shared = argparse.ArgumentParser(add_help=False)
    shared.add_argument("language", choices=("python", "rust"))
    shared.add_argument(
        "--owner",
        help="lane that owns the dirty scope and prefixes generated campaign IDs",
    )
    shared.add_argument("--jobs", type=int, default=4, help="Rust workers per crate")
    shared.add_argument("--run-timeout", type=int, default=1800)
    shared.add_argument("--only", action="append", default=[])
    shared.add_argument(
        "--extra-test",
        action="append",
        default=[],
        metavar="SOURCE=TEST",
        help="also score SOURCE with TEST when no test is named after SOURCE",
    )
    shared.add_argument(
        "--include-covered",
        action="store_true",
        help=(
            "also plan sources an existing campaign already names. Use when the "
            "incumbent campaign is broader than your change and a narrow second "
            "campaign is cheaper than re-running it; the incumbent stays."
        ),
    )
    shared.add_argument(
        "--base",
        default="origin/master",
        help="branch base for the changed-file scope (default: origin/master)",
    )
    shared.add_argument(
        "--all-files",
        action="store_true",
        help=(
            "maintenance sweep of the WHOLE TREE. The default -- and the only "
            "thing an agent landing a change should run -- is the files this "
            "branch changed."
        ),
    )
    subparsers = parser.add_subparsers(dest="command", required=True)
    plan_parser = subparsers.add_parser(
        "plan", parents=[shared], help="report what would be written; writes nothing"
    )
    plan_parser.add_argument("--verbose", action="store_true")
    write_parser = subparsers.add_parser(
        "write", parents=[shared], help="write one manifest per subject"
    )
    write_parser.add_argument("--force", action="store_true")
    refresh_parser = subparsers.add_parser(
        "refresh", help="automatically refresh one existing generated engine campaign"
    )
    refresh_parser.add_argument("campaign", help="campaign id or manifest filename")
    refresh_parser.add_argument(
        "--source",
        action="append",
        default=[],
        help="repository-relative Rust source to mutate and bind as its test surface",
    )
    return parser


def _refresh_command(args: argparse.Namespace) -> str:
    """Refresh exactly one generated campaign using its engine-specific refresh path."""

    path, existing = _load_generated_cargo_campaign(args.campaign, REPO_ROOT)
    if existing.get("mutation_engine") == "cargo-mutants":
        return refresh_rust_campaign(args.campaign, sources=args.source)
    if existing.get("mutation_engine") == "fest":
        if args.source:
            raise CampaignError(
                "fest refresh retains its declared source; --source is Rust-only"
            )
        return refresh_python_campaign(args.campaign)
    raise CampaignError(f"{path} is not a generated supported-engine campaign")


def _campaign_scope(args: argparse.Namespace, owner: str) -> list[str] | None:
    """Select whole-tree maintenance, explicit files, or this lane's changed files."""

    if args.all_files:
        return None
    if args.only:
        return _explicit_scope(args.only)
    return _branch_scope(args.base, owner=owner)


def _run_plan_or_write(args: argparse.Namespace) -> dict[str, Any]:
    """Plan one bounded language surface and annotate the selected scope."""

    try:
        campaign_owner = args.owner or resolve_owner(REPO_ROOT)
    except OwnerIdentityError as exc:
        raise CampaignError(f"cannot resolve campaign owner: {exc}") from exc
    scope = _campaign_scope(args, campaign_owner)
    result = plan(
        args.language,
        owner=campaign_owner,
        jobs=args.jobs,
        run_timeout_seconds=args.run_timeout,
        only_sources=scope,
        include_covered=args.include_covered,
        extra_tests=_extra_tests(args.extra_test),
    )
    return {
        **result,
        "scope": (
            "all-files"
            if args.all_files
            else "exact --only paths"
            if args.only
            else f"changed vs {args.base}"
        ),
        "scoped_paths": 0 if scope is None else len(scope),
    }


def _run_command(args: argparse.Namespace) -> dict[str, Any]:
    """Execute parsed command work and return the compact JSON response payload."""

    if args.command == "refresh":
        return {"refreshed": _refresh_command(args)}
    result = _run_plan_or_write(args)
    if args.command == "plan":
        return _summarise(result, verbose=args.verbose)
    return {
        **_summarise(result, verbose=False),
        "written": write(result["manifests"], force=args.force),
    }


def main(argv: list[str] | None = None) -> int:
    """CLI: `plan` reports what would be written, `write` writes it."""

    args = _cli_parser().parse_args(argv)

    try:
        print(json.dumps(_run_command(args), indent=2))
        return 0
    except CampaignError as exc:
        print(json.dumps({"status": "REFUSED", "error": str(exc)}, indent=2))
        return 4


if __name__ == "__main__":
    raise SystemExit(main())
