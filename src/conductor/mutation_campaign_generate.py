#!/usr/bin/env python3
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
import os
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
from conductor.mutation_campaign_model import REPO_ROOT, _sha256
from conductor.mutation_plan_bridge import plan_native as _plan_native
from conductor.mutation_scope import CampaignError
from conductor.project_paths import campaigns_relative, campaigns_root


ENGINE_SLUG = {"fest": "fest", "cargo-mutants": "cargo", "mull": "mull"}

# Directories that hold no subject of ours: virtualenvs, build trees and vendored
# code. Matched against every path part, so a nested venv is skipped too.
SKIP_PARTS = frozenset(
    {
        ".git",
        ".mutation-native-crate",
        ".mull-build",
        ".tox",
        "__pycache__",
        "build",
        "node_modules",
        "site-packages",
        "target",
        "vendor",
    }
)


def _skip(path: Path) -> bool:
    """True for a path inside a venv, a build tree or vendored code."""

    return any(
        part in SKIP_PARTS or part == "venv" or part.startswith((".venv", "venv-"))
        for part in path.parts
    )


def _is_test(relative: str, name: str) -> bool:
    return name.startswith("test_") or name == "conftest.py" or "/tests/" in relative


def _mirrored_tests(source: str, candidates: Sequence[str]) -> list[str]:
    """Candidates in a test tree that mirrors the source's own package.

    A module ``a/b/c.py`` pairs with ``a/b/test_c.py`` beside it, and with any
    ``<prefix>/tests/<tail>/test_c.py`` cut from its own path -- the layouts
    this tree uses, where tests sit either beside the module or under a
    ``tests/`` directory of one of its ancestors. A same-basename test in an
    unrelated package mirrors nothing: `conductor/__main__.py` and
    `tooling/hooks/dispatch/__main__.py` each own exactly their own
    ``test___main__.py``, and basename-only pairing once crossed them.
    """
    directories = PurePosixPath(source).parts[:-1]

    def _mirrors(candidate: str) -> bool:
        parts = PurePosixPath(candidate).parts[:-1]
        if parts == directories:
            return True  # beside the module it tests
        return any(
            parts == directories[:prefix] + ("tests",) + directories[tail:]
            for prefix in range(len(directories) + 1)
            for tail in range(prefix, len(directories) + 1)
        )

    return sorted(candidate for candidate in candidates if _mirrors(candidate))


def python_subjects(repo_root: Path = REPO_ROOT) -> tuple[list[dict], list[dict]]:
    """Split every Python module into (has a test named for it, does not).

    Pairing is package-relative first: a module pairs with the ``test_<name>.py``
    beside it or under its own ``tests/`` mirror, and only a sole same-basename
    candidate with no mirror still pairs by name alone. Two same-basename
    candidates with no mirror are refused naming both -- guessing there pairs
    some other package's tests with the module and every mutant comes back
    unreached.

    Name matching is a proxy, not proof: a module with no `test_<name>.py` may
    still be exercised through one that has one. It is the cheap half of the
    question, and it is the half that costs nothing to run.
    """

    tests: dict[str, list[str]] = {}
    sources: list[Path] = []
    for path in sorted(repo_root.rglob("*.py")):
        if _skip(path.relative_to(repo_root)):
            continue
        relative = path.relative_to(repo_root).as_posix()
        if _is_test(relative, path.name):
            tests.setdefault(path.name, []).append(relative)
        else:
            sources.append(path)

    paired: list[dict] = []
    unpaired: list[dict] = []
    for path in sources:
        relative = path.relative_to(repo_root).as_posix()
        matches = tests.get(f"test_{path.name}", [])
        record = {"source": relative, "lines": _line_count(path)}
        mirrored = _mirrored_tests(relative, matches)
        if mirrored:
            paired.append({**record, "tests": mirrored})
        elif len(matches) == 1:
            # One same-basename test in no mirrored tree is still the test this
            # module has -- a legacy layout that predates the mirror rule.
            paired.append({**record, "tests": list(matches)})
        elif matches:
            raise CampaignError(
                f"{relative} has no test in its own package, and several "
                f"unrelated files share the name test_{path.name}: "
                f"{sorted(matches)}. Put the test beside the module (or under "
                "its tests/ mirror) so the pairing can say which one counts."
            )
        else:
            unpaired.append(record)
    return paired, unpaired


def rust_subjects(repo_root: Path = REPO_ROOT) -> list[dict]:
    """Every crate in the tree, with the package name cargo needs to target it.

    `--package` is not optional: without it cargo-mutants mutates whatever the
    surrounding workspace happens to contain.
    """

    crates: list[dict] = []
    for manifest in sorted(repo_root.rglob("Cargo.toml")):
        relative_manifest = manifest.relative_to(repo_root)
        if _skip(relative_manifest):
            continue
        package = _package_name(manifest)
        if package is None:
            continue  # a workspace root declares no package of its own
        root = relative_manifest.parent.as_posix()
        files = sorted(
            p.relative_to(repo_root).as_posix()
            for p in (manifest.parent / "src").rglob("*.rs")
            if not _skip(p.relative_to(repo_root))
        )
        if not files:
            raise CampaignError(f"crate {root} declares a package but has no src/*.rs")
        crates.append(
            {
                "package": package,
                "root": root,
                "manifest": relative_manifest.as_posix(),
                "files": files,
                "lines": sum(_line_count(repo_root / f) for f in files),
            }
        )
    return crates


def _package_name(manifest: Path) -> str | None:
    """The `[package] name` of a Cargo.toml, without a TOML dependency.

    Only the first `name =` after a `[package]` header counts: a `name` under
    `[[bin]]` or `[dependencies]` names something else entirely.
    """

    in_package = False
    for raw in manifest.read_text(encoding="utf-8", errors="ignore").splitlines():
        line = raw.strip()
        if line.startswith("["):
            in_package = line == "[package]"
            continue
        if in_package and line.startswith("name"):
            _, _, value = line.partition("=")
            return value.strip().strip('"').strip("'") or None


def _line_count(path: Path) -> int:
    try:
        return len(path.read_text(encoding="utf-8", errors="ignore").splitlines())
    except OSError:
        return 0


def _slug(stem: str) -> str:
    return "".join(c if c.isalnum() else "_" for c in stem).strip("_").lower()


def _unique_slugs(keys: Sequence[str]) -> dict[str, str]:
    """A campaign id fragment per subject, disambiguated only where it collides.

    `conductor/gate_rollout.py` stays `gate_rollout`. Two modules with the same
    file name in different packages both keep their parent directory, because a
    colliding campaign id would silently overwrite one manifest with the other.
    """

    stems = {key: _slug(Path(key).stem) for key in keys}
    counts: dict[str, int] = {}
    for stem in stems.values():
        counts[stem] = counts.get(stem, 0) + 1
    out: dict[str, str] = {}
    for key, stem in stems.items():
        if counts[stem] == 1:
            out[key] = stem
        else:
            parent = _slug(Path(key).parent.name)
            out[key] = f"{parent}_{stem}" if parent else stem
    return out


def _campaign_id(owner: str, slug: str, engine: str, day: str) -> str:
    return f"{owner}_{slug}_{ENGINE_SLUG[engine]}_{day}"


def fest_manifest(
    subject: Mapping[str, Any],
    *,
    campaign_id: str,
    repo_root: Path,
    run_timeout_seconds: int,
) -> dict[str, Any]:
    """One Python module, scored against the tests that name it.

    `jobs` is absent, which the model reads as 1. That is not a default worth
    overriding: fest was measured producing 4/5/5/6/5 survivors over five
    identical runs at host width, and a corpus that jitters cannot carry a
    ratchet.
    """

    source = str(subject["source"])
    tests = list(subject["tests"])
    return {
        "schema_version": 1,
        "campaign_id": campaign_id,
        "title": f"Generated mutants for {source}, scored on the survivor set",
        "language": "python",
        "mutation_engine": "fest",
        "generator": {
            "source": [source],
            "exclude": ["**/test_*.py", "**/conftest.py"],
            "operators": [],
            "seed": 0,
            # No mutant_timeout_seconds: the run derives it from the baseline
            # suite's wall time (3x, floored at 60 s) and records the value it
            # used in the receipt. A manifest that pins one overrides that.
            "run_timeout_seconds": run_timeout_seconds,
        },
        # --rootdir=. pins pytest to the repo root. Without it a nested
        # pytest.ini (research/, component_fab/) wins rootdir discovery and
        # every nodeid is reported relative to that subtree, so no nodeid in
        # the report matches one in test_argv: attribution comes back
        # NO_ATTRIBUTION and test_value is null.
        "test_argv": ["python", "-m", "pytest", "-q", "--rootdir=.", *tests],
        "environment": {},
        "source_sha256": {source: _sha256(repo_root / source)},
        "test_sha256": {test: _sha256(repo_root / test) for test in tests},
        "survivor_baseline": [],
        "survivor_baseline_recorded": False,
        "survivor_baseline_note": (
            "Empty and unrecorded. The FIRST engine run writes this list itself "
            "and flips survivor_baseline_recorded to true; every later run is "
            "scored against it, so a survivor that appears afterwards is a test "
            "that stopped defending its code. No agent ever writes this field -- "
            "hand-authored baselines are forbidden (KB-MUT-02), and an unrecorded "
            "baseline is why a fresh campaign used to be red on every run."
        ),
    }


def rust_test_files(
    subject: Mapping[str, Any], *, repo_root: Path = REPO_ROOT
) -> list[str]:
    """The test files one `cargo test` for this crate actually runs.

    Rust keeps tests in two places and `cargo test` runs both: integration tests
    under `tests/`, and unit tests written inline behind `#[cfg(test)]` in the
    source files themselves. Declaring only the first would let an edit to an
    inline test module go unnoticed while a receipt still vouched for it, and an
    inline module is where most of this repository's Rust tests live.
    """

    root = repo_root / str(subject["root"])
    found: set[str] = set()
    tests_dir = root / "tests"
    if tests_dir.is_dir():
        for path in tests_dir.rglob("*.rs"):
            if not _skip(path):
                found.add(path.relative_to(repo_root).as_posix())
    for relative in subject["files"]:
        path = repo_root / relative
        try:
            text = path.read_text(encoding="utf-8", errors="ignore")
        except OSError as exc:
            raise CampaignError(f"cannot read {relative}: {exc}") from exc
        if "#[cfg(test)]" in text:
            found.add(relative)
    if not found:
        raise CampaignError(
            f"crate {subject['package']} has no tests: no tests/*.rs and no "
            "#[cfg(test)] module. A mutation campaign over untested code would "
            "report every mutant as survived and prove nothing that reading the "
            "crate does not already say."
        )
    return sorted(found)


def cargo_manifest(
    subject: Mapping[str, Any],
    *,
    campaign_id: str,
    repo_root: Path,
    jobs: int,
    run_timeout_seconds: int,
    sources: Sequence[str] | None = None,
) -> dict[str, Any]:
    """One Rust crate, scored against its own `cargo test`.

    `environment` stays empty on purpose. Pinning CARGO_TARGET_DIR makes parallel
    workers share one build directory, which was measured giving 45/43/46
    survivors and 24/21/22 unviable over three runs of the same 196 mutants.
    Whether a mutant compiles cannot legitimately vary.
    """

    selected = sorted(subject["files"] if sources is None else set(sources))
    unknown = sorted(set(selected) - set(subject["files"]))
    if unknown:
        raise CampaignError(
            f"scoped source is not in {subject['package']!r}: {unknown}"
        )
    if not selected:
        raise CampaignError(
            f"cargo campaign for {subject['package']!r} has no scoped Rust source"
        )
    package_root = Path(str(subject["root"]))
    return {
        "schema_version": 1,
        "campaign_id": campaign_id,
        "title": (
            f"Generated mutants for the {subject['package']} crate, "
            "scored on the survivor set"
        ),
        "language": "rust",
        "mutation_engine": "cargo-mutants",
        "generator": {
            "source": [
                Path(relative).relative_to(package_root).as_posix()
                for relative in selected
            ],
            "exclude": [],
            "operators": [],
            "options": {
                "manifest_path": subject["manifest"],
                "package": subject["package"],
                "package_root": subject["root"],
            },
            "seed": 0,
            "jobs": jobs,
            # Derived per run from the baseline wall time (3x, floored at
            # 60 s); see the fest builder above for why it is not pinned.
            "run_timeout_seconds": run_timeout_seconds,
        },
        "test_argv": [
            "cargo",
            "test",
            "--manifest-path",
            str(subject["manifest"]),
            "--package",
            str(subject["package"]),
        ],
        "environment": {},
        "source_sha256": {
            relative: _sha256(repo_root / relative) for relative in selected
        },
        "test_sha256": {
            relative: _sha256(repo_root / relative)
            for relative in rust_test_files(subject, repo_root=repo_root)
        },
        "survivor_baseline": [],
        "survivor_baseline_recorded": False,
        "survivor_baseline_note": (
            "Empty and unrecorded. The FIRST engine run writes this list itself "
            "and flips survivor_baseline_recorded to true; every later run is "
            "scored against it, so a survivor that appears afterwards is a test "
            "that stopped defending its code. No agent ever writes this field -- "
            "hand-authored baselines are forbidden (KB-MUT-02), and an unrecorded "
            "baseline is why a fresh campaign used to be red on every run."
        ),
    }


def existing_subjects(repo_root: Path = REPO_ROOT) -> set[str]:
    """Every subject a committed generated campaign already covers.

    A second campaign for the same subject is worse than none: it would be born
    with an empty survivor baseline, so it would report green over exactly the
    survivors the first campaign is holding a ratchet against. Identified by
    what the campaign mutates -- the Rust package, or the Python module -- not by
    campaign id, because the id carries the date it was generated on.
    """

    covered: set[str] = set()
    for manifest in sorted(campaigns_root(repo_root).glob("*.json")):
        try:
            payload = json.loads(manifest.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if not isinstance(payload, dict):
            continue
        if payload.get("mutation_engine") not in ENGINE_SLUG:
            continue
        generator = payload.get("generator")
        if not isinstance(generator, dict):
            continue
        package = (generator.get("options") or {}).get("package")
        if isinstance(package, str):
            covered.add(package)
        for source in generator.get("source") or ():
            if isinstance(source, str):
                covered.add(source)
    return covered


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


def _admit_extra_tests(
    paired: list[dict], unpaired: list[dict], extra_tests: Mapping[str, Sequence[str]]
) -> tuple[list[dict], list[dict]]:
    """Bind tests that exercise a subject without being named after it.

    Name matching is the cheap proxy and `--extra-test source=test` is the
    operator's answer for the case it misses: a module defended only by a test
    named after something else otherwise stays unpaired and gets no campaign.
    """

    by_source = {str(s["source"]): s for s in paired}
    still_unpaired = [s for s in unpaired if s["source"] not in extra_tests]
    for subject in unpaired:
        if subject["source"] in extra_tests:
            by_source[str(subject["source"])] = {**subject, "tests": []}
    for source, tests in extra_tests.items():
        if source not in by_source:
            raise CampaignError(f"--extra-test names no python subject: {source}")
        by_source[source]["tests"] = sorted({*by_source[source]["tests"], *tests})
    return list(by_source.values()), still_unpaired


def _plan_python(
    *,
    repo_root: Path,
    covered: set[str],
    scope: set[str] | None,
    owner: str,
    day: str,
    run_timeout_seconds: int,
    extra_tests: Mapping[str, Sequence[str]],
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], list[str]]:
    """Manifests, unpaired subjects and already-covered sources for python."""

    paired, unpaired = _admit_extra_tests(*python_subjects(repo_root), extra_tests)
    if scope is not None:
        # A changed test is in scope as much as a changed source: editing only
        # `test_x.py` is exactly the case the campaign for `x.py` must cover.
        paired = [
            s
            for s in paired
            if s["source"] in scope or any(t in scope for t in s["tests"])
        ]
        unpaired = [s for s in unpaired if s["source"] in scope]
    already = sorted(s["source"] for s in paired if s["source"] in covered)
    paired = [s for s in paired if s["source"] not in covered]
    slugs = _unique_slugs([str(s["source"]) for s in paired])
    manifests = [
        fest_manifest(
            subject,
            campaign_id=_campaign_id(owner, slugs[str(subject["source"])], "fest", day),
            repo_root=repo_root,
            run_timeout_seconds=run_timeout_seconds,
        )
        for subject in paired
    ]
    return manifests, unpaired, already


def _plan_rust(
    *,
    repo_root: Path,
    covered: set[str],
    scope: set[str] | None,
    owner: str,
    day: str,
    jobs: int,
    run_timeout_seconds: int,
) -> tuple[list[dict[str, Any]], list[str], list[dict[str, Any]]]:
    """Manifests, already-covered packages and crates with no tests, for rust."""

    crates = rust_subjects(repo_root)
    scoped_sources: dict[str, list[str]] = {}
    if scope is not None:
        for crate in crates:
            selected = sorted(set(crate["files"]) & scope)
            if selected:
                scoped_sources[str(crate["package"])] = selected
        crates = [c for c in crates if str(c["package"]) in scoped_sources]
    already = sorted(c["package"] for c in crates if c["package"] in covered)
    crates = [c for c in crates if c["package"] not in covered]
    slugs = _unique_slugs([str(c["root"]) for c in crates])
    manifests: list[dict[str, Any]] = []
    untested: list[dict[str, Any]] = []
    for subject in crates:
        # A crate with no tests is reported rather than raised: it is a finding
        # about the repository, not a fault in the request, and one untested
        # crate must not stop the other eight being planned.
        try:
            manifests.append(
                cargo_manifest(
                    subject,
                    campaign_id=_campaign_id(
                        owner, slugs[str(subject["root"])], "cargo-mutants", day
                    ),
                    repo_root=repo_root,
                    jobs=jobs,
                    run_timeout_seconds=run_timeout_seconds,
                    sources=(
                        None
                        if scope is None
                        else scoped_sources[str(subject["package"])]
                    ),
                )
            )
        except CampaignError as exc:
            untested.append(
                {
                    "package": str(subject["package"]),
                    "root": str(subject["root"]),
                    "lines": int(subject.get("lines", 0)),
                    "reason": str(exc),
                }
            )
    return manifests, already, untested


def _plan_python_fallback(
    language: str, repo_root: Path, ctx: dict[str, Any]
) -> dict[str, Any]:
    """The pre-native `plan()` body; kept for `CONDUCTOR_PLAN_IMPL=python`."""
    covered = set() if ctx["include_covered"] else existing_subjects(repo_root)
    scope = set(ctx["only_sources"]) if ctx["only_sources"] is not None else None
    unpaired: list[dict[str, Any]] = []
    untested: list[dict[str, Any]] = []
    if language == "python":
        manifests, unpaired, already = _plan_python(
            repo_root=repo_root,
            covered=covered,
            scope=scope,
            owner=ctx["owner"],
            day=ctx["day"],
            run_timeout_seconds=ctx["run_timeout_seconds"],
            extra_tests=ctx["extra_tests"] or {},
        )
    else:
        manifests, already, untested = _plan_rust(
            repo_root=repo_root,
            covered=covered,
            scope=scope,
            owner=ctx["owner"],
            day=ctx["day"],
            jobs=ctx["jobs"],
            run_timeout_seconds=ctx["run_timeout_seconds"],
        )
    return {
        "language": language,
        "manifests": manifests,
        "unpaired": unpaired,
        "unpaired_lines": sum(int(u["lines"]) for u in unpaired),
        "already_covered": already,
        "untested": untested,
    }


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

    The walk, pairing and manifest emission run natively (`conductor.mutation_plan_bridge.plan_native`)
    unless `CONDUCTOR_PLAN_IMPL=python` selects the pure-Python fallback below.
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
    impl = (
        _plan_python_fallback
        if os.environ.get("CONDUCTOR_PLAN_IMPL") == "python"
        else _plan_native
    )
    return impl(language, repo_root, ctx)


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


def _recorded_test_list(existing: Mapping[str, Any], path: Path) -> list[str]:
    """The test list a generated campaign already recorded, in engine order."""
    recorded = existing.get("test_sha256")
    if not isinstance(recorded, dict) or not recorded:
        raise CampaignError(
            f"{path} has no recorded test list to carry forward; regenerate it "
            "with plan/write and let the engine record a fresh baseline"
        )
    return sorted(str(test) for test in recorded)


def refresh_python_campaign(campaign: str, *, repo_root: Path = REPO_ROOT) -> str:
    """Regenerate one generated Python campaign while retaining its engine baseline."""

    path, existing = _load_generated_cargo_campaign(campaign, repo_root)
    if existing.get("mutation_engine") != "fest":
        raise CampaignError(f"{path} is not a generated fest campaign")
    generator = existing.get("generator") or {}
    sources = generator.get("source")
    if (
        not isinstance(sources, list)
        or len(sources) != 1
        or not isinstance(sources[0], str)
    ):
        raise CampaignError(f"{path} must declare exactly one Python source to refresh")
    source = sources[0]
    paired, unpaired = python_subjects(repo_root)
    subject = next((item for item in paired if item["source"] == source), None)
    if subject is None:
        orphan = next((item for item in unpaired if item["source"] == source), None)
        if orphan is None:
            raise CampaignError(
                f"generated Python source {source!r} no longer exists in the tree"
            )
        # A campaign admitted with --extra-test names tests no file-name rule
        # can re-derive; its recorded list is the authority, carried forward
        # unchanged. Refusing here used to leave `write --force` as the only
        # path, which resets the survivor baseline the engine is holding.
        subject = {**orphan, "tests": _recorded_test_list(existing, path)}
    refreshed = fest_manifest(
        subject,
        campaign_id=str(existing.get("campaign_id") or path.stem),
        repo_root=repo_root,
        run_timeout_seconds=int(generator.get("run_timeout_seconds", 1800)),
    )
    for key in (
        "survivor_baseline",
        "survivor_baseline_recorded",
        "survivor_baseline_note",
        "survivor_baseline_recorded_at",
    ):
        if key in existing:
            refreshed[key] = existing[key]
    path.write_text(
        json.dumps(refreshed, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return path.relative_to(repo_root).as_posix()


def _generated_cargo_subject(
    existing: Mapping[str, Any], path: Path, repo_root: Path
) -> tuple[dict[str, Any], str]:
    """Find the crate a generated cargo manifest was produced from."""

    options = (existing.get("generator") or {}).get("options") or {}
    package = options.get("package")
    manifest_path = options.get("manifest_path")
    if not isinstance(package, str) or not isinstance(manifest_path, str):
        raise CampaignError(f"{path} has no generated cargo package")
    candidates = [row for row in rust_subjects(repo_root) if row["package"] == package]
    subject = next(
        (row for row in candidates if row["manifest"] == manifest_path), None
    )
    if subject is None and len(candidates) == 1:
        # A prior generator version could pin its own disposable snapshot. Once
        # snapshots are excluded, the sole real package is the unambiguous repair.
        subject = candidates[0]
    if subject is None:
        raise CampaignError(f"generated cargo package {package!r} no longer exists")
    return subject, package


def _scope_to_sources(
    refreshed: dict[str, Any],
    subject: Mapping[str, Any],
    sources: Sequence[str],
    package: str,
    repo_root: Path,
) -> None:
    """Narrow a refreshed manifest to the requested subset of the crate's files."""

    unknown = sorted(set(sources) - set(subject["files"]))
    if unknown:
        raise CampaignError(f"scoped source is not in {package!r}: {unknown}")
    selected = sorted(set(sources))
    package_root = repo_root / str(subject["root"])
    refreshed["generator"]["source"] = [
        (repo_root / path).relative_to(package_root).as_posix() for path in selected
    ]
    refreshed["source_sha256"] = {path: _sha256(repo_root / path) for path in selected}
    # Rust unit tests are colocated with their source.  Binding just these
    # files keeps the receipt specific to the requested test surface even
    # though cargo-mutants invokes Cargo's unit-test harness.
    refreshed["test_sha256"] = dict(refreshed["source_sha256"])


def _safe_declared_rust_scope(value: object) -> list[str] | None:
    """Return a generated relative Rust scope, or ``None`` when it is malformed."""

    if (
        isinstance(value, list)
        and value
        and not any(
            not isinstance(item, str)
            or not item
            or any(token in item for token in ("*", "?", "[", "]"))
            or Path(item).is_absolute()
            or ".." in Path(item).parts
            for item in value
        )
    ):
        return value


def _declared_rust_sources(
    generator: Mapping[str, Any],
    subject: Mapping[str, Any],
    path: Path,
    repo_root: Path,
) -> list[str]:
    """Resolve an existing generated Rust scope and refuse any crate escape."""

    raw_scope = generator.get("source")
    if not isinstance(raw_scope, list) or not raw_scope:
        raise CampaignError(f"{path} has no non-empty generated Rust source scope")
    declared = _safe_declared_rust_scope(raw_scope)
    if declared is None:
        raise CampaignError(
            f"{path} has malformed generated Rust source scope; pass --source explicitly"
        )
    package_root = repo_root / str(subject["root"])
    root_resolved = package_root.resolve()
    allowed = {
        (repo_root / item).relative_to(package_root).as_posix()
        for item in subject["files"]
    }
    resolved = [(package_root / item).resolve() for item in declared]
    if any(
        candidate == root_resolved or root_resolved not in candidate.parents
        for candidate in resolved
    ) or any(
        candidate.relative_to(root_resolved).as_posix() not in allowed
        for candidate in resolved
    ):
        raise CampaignError(
            f"{path} has Rust sources outside its current crate; pass --source explicitly"
        )
    return [
        candidate.relative_to(repo_root.resolve()).as_posix() for candidate in resolved
    ]


def _retain_survivor_baseline(
    refreshed: dict[str, Any], existing: Mapping[str, Any]
) -> None:
    """Carry engine-recorded ratchet metadata forward unchanged during refresh."""

    for key in (
        "survivor_baseline",
        "survivor_baseline_recorded",
        "survivor_baseline_note",
        "survivor_baseline_recorded_at",
    ):
        if key in existing:
            refreshed[key] = existing[key]


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
    subject, package = _generated_cargo_subject(existing, path, repo_root)
    generator = existing.get("generator") or {}
    sources = list(sources) or _declared_rust_sources(
        generator, subject, path, repo_root
    )
    refreshed = cargo_manifest(
        subject,
        campaign_id=str(existing.get("campaign_id") or path.stem),
        repo_root=repo_root,
        jobs=int(generator.get("jobs", 4)),
        run_timeout_seconds=int(generator.get("run_timeout_seconds", 1800)),
    )
    if sources:
        _scope_to_sources(refreshed, subject, sources, package, repo_root)
    _retain_survivor_baseline(refreshed, existing)
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
