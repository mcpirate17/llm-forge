"""Check mutation targets against the invoking agent's changed files before execution."""

from __future__ import annotations

import json
import re
from fnmatch import fnmatchcase
from collections.abc import Sequence
from pathlib import Path
from typing import TYPE_CHECKING

from conductor.mutation_campaign_generate import changed_sources
from conductor.mutation_scope import CampaignError


if TYPE_CHECKING:
    from conductor.mutation_engine_generated import GeneratedCampaign


def _exact_file(root: Path, relative: str) -> str:
    """Resolve a literal file inside the checkout, refusing globs and escapes."""

    path = Path(relative)
    if path.is_absolute() or ".." in path.parts or any(c in relative for c in "*?["):
        raise CampaignError(
            f"mutation scope requires exact repository files: {relative!r}"
        )
    target = root / path
    if not target.is_file() or not target.resolve().is_relative_to(root.resolve()):
        raise CampaignError(
            f"mutation scope file is absent or outside the checkout: {relative!r}"
        )
    return path.as_posix()


def _mull_sources(campaign: GeneratedCampaign, root: Path) -> list[str]:
    """Expand archival Mull globs into pinned targets for its engine allowlist."""

    selected: set[str] = set()
    for pattern in campaign.source:
        matches = {
            source for source in campaign.source_sha256 if fnmatchcase(source, pattern)
        }
        if not matches:
            raise CampaignError(f"Mull source has no pinned target: {pattern!r}")
        selected.update(_exact_file(root, source) for source in matches)
    if not selected:
        raise CampaignError("Mull requires at least one exact source file")
    return sorted(selected)


def validate_run_scope(
    campaign: GeneratedCampaign,
    *,
    repo_root: Path,
    owner: str | None = None,
    base: str = "origin/master",
    only: Sequence[str] = (),
) -> list[str]:
    """Refuse broad/unchanged targets before an engine or snapshot is started.

    A changed Python test permits its named paired source, not every source in
    the manifest. Explicit selectors narrow the ownership-filtered diff; they
    cannot authorize unchanged files or another agent's dirty work.
    """

    prefix = (
        str(campaign.options.get("package_root", ""))
        if campaign.mutation_engine == "cargo-mutants"
        else ""
    )
    targets = (
        set(_mull_sources(campaign, repo_root))
        if campaign.mutation_engine == "mull"
        else {
            _exact_file(repo_root, str(Path(prefix) / source))
            for source in campaign.source
        }
    )
    if not targets or not targets <= set(campaign.source_sha256):
        raise CampaignError("every mutation target must be pinned in source_sha256")
    changed = changed_sources(base, repo_root=repo_root, owner=owner)
    if only:
        selected = {_exact_file(repo_root, source) for source in only}
        if not selected <= changed:
            raise CampaignError(
                f"--only names files outside this agent's changes: {sorted(selected - changed)}"
            )
        changed &= selected
    allowed = set(changed)
    changed_tests = set(campaign.test_sha256) & changed
    for test in changed_tests:
        paired = {
            source
            for source in targets
            if Path(source).suffix == ".py"
            and Path(test)
            in {
                directory / f"test_{Path(source).name}"
                for directory in (
                    Path(source).parent,
                    Path(source).parent / "tests",
                    *(
                        (Path(source).parent.parent / "tests",)
                        if Path(source).parent.name in {"src", "tools"}
                        else ()
                    ),
                )
            }
        }
        if len(paired) == 1:
            allowed.update(paired)
    outside = targets - allowed
    if outside:
        raise CampaignError(
            f"campaign mutates files outside this agent's changes: {sorted(outside)}; "
            "generate a campaign for the exact changed files with the correct --owner"
        )
    return sorted(targets)


def mull_scope_config(campaign: GeneratedCampaign, worktree: Path) -> Path:
    """Give Mull's compiler plugin and runner the same exact path allowlist.

    Mull reads ``includePaths`` regexes through MULL_CONFIG before selecting
    mutants. Quoted YAML strings preserve regex escapes and unusual filenames.
    """

    sources = _mull_sources(campaign, worktree)
    lines = ["includePaths:"]
    for source in sources:
        pattern = "^" + re.escape(str((worktree / source).resolve())) + "$"
        lines.append("  - " + json.dumps(pattern))
    if campaign.operators:
        lines.append("mutators:")
        lines.extend("  - " + json.dumps(operator) for operator in campaign.operators)
    config = worktree / ".mutation-mull-scope.yml"
    config.write_text("\n".join(lines), encoding="utf-8")
    return config
