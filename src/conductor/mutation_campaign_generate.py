#!/usr/bin/env python3
"""Automatically derive conductor engine-campaign manifests from the tree.

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
from collections.abc import Iterable, Mapping, Sequence
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from conductor.mutation_campaign_model import REPO_ROOT, _sha256
from conductor.mutation_scope import CampaignError

CAMPAIGN_DIR = "conductor/mutation_campaigns"
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


def python_subjects(repo_root: Path = REPO_ROOT) -> tuple[list[dict], list[dict]]:
    """Split every Python module into (has a test named for it, does not).

    Two layouts coexist here: `conductor/` keeps `test_x.py` beside `x.py`, while
    `research/tools/` and `component_fab/` keep theirs under a `tests/` directory.
    Matching on the file name rather than the directory covers both.

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
        matches = tests.get(f"test_{path.name}")
        record = {"source": relative, "lines": _line_count(path)}
        if matches:
            paired.append({**record, "tests": matches})
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
    return None


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
            "mutant_timeout_seconds": 30,
            "run_timeout_seconds": run_timeout_seconds,
        },
        "test_argv": ["python", "-m", "pytest", "-q", *tests],
        "environment": {},
        "source_sha256": {source: _sha256(repo_root / source)},
        "test_sha256": {test: _sha256(repo_root / test) for test in tests},
        "survivor_baseline": [],
        "survivor_baseline_note": (
            "Empty until the first run records it. An empty baseline means every "
            "survivor is a new survivor, so the first run reports the whole gap "
            "rather than silently accepting it."
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
) -> dict[str, Any]:
    """One Rust crate, scored against its own `cargo test`.

    `environment` stays empty on purpose. Pinning CARGO_TARGET_DIR makes parallel
    workers share one build directory, which was measured giving 45/43/46
    survivors and 24/21/22 unviable over three runs of the same 196 mutants.
    Whether a mutant compiles cannot legitimately vary.
    """

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
            "source": ["src/**/*.rs"],
            "exclude": [],
            "operators": [],
            "options": {
                "manifest_path": subject["manifest"],
                "package": subject["package"],
                "package_root": subject["root"],
            },
            "seed": 0,
            "jobs": jobs,
            "mutant_timeout_seconds": 30,
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
            relative: _sha256(repo_root / relative) for relative in subject["files"]
        },
        "test_sha256": {
            relative: _sha256(repo_root / relative)
            for relative in rust_test_files(subject, repo_root=repo_root)
        },
        "survivor_baseline": [],
        "survivor_baseline_note": (
            "Empty until the first run records it. An empty baseline means every "
            "survivor is a new survivor, so the first run reports the whole gap "
            "rather than silently accepting it."
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
    for manifest in sorted((repo_root / CAMPAIGN_DIR).glob("*.json")):
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


def plan(
    language: str,
    *,
    repo_root: Path = REPO_ROOT,
    owner: str = "claude",
    day: str | None = None,
    jobs: int = 4,
    run_timeout_seconds: int = 1800,
) -> dict[str, Any]:
    """What `write` would emit, plus the subjects it refuses to emit anything for."""

    if language not in ("python", "rust"):
        raise CampaignError(f"unknown language {language!r}; known: python, rust")
    day = day or datetime.now(UTC).strftime("%Y%m%d")
    covered = existing_subjects(repo_root)
    untested: list[dict[str, Any]] = []
    manifests: list[dict[str, Any]] = []
    unpaired: list[dict[str, Any]] = []
    already: list[str] = []

    if language == "python":
        paired, unpaired = python_subjects(repo_root)
        already = sorted(s["source"] for s in paired if s["source"] in covered)
        paired = [s for s in paired if s["source"] not in covered]
        slugs = _unique_slugs([str(s["source"]) for s in paired])
        for subject in paired:
            key = str(subject["source"])
            manifests.append(
                fest_manifest(
                    subject,
                    campaign_id=_campaign_id(owner, slugs[key], "fest", day),
                    repo_root=repo_root,
                    run_timeout_seconds=run_timeout_seconds,
                )
            )
    else:
        crates = rust_subjects(repo_root)
        already = sorted(c["package"] for c in crates if c["package"] in covered)
        crates = [c for c in crates if c["package"] not in covered]
        slugs = _unique_slugs([str(c["root"]) for c in crates])
        for subject in crates:
            key = str(subject["root"])
            # A crate with no tests is reported rather than raised: it is a
            # finding about the repository, not a fault in the request, and one
            # untested crate must not stop the other eight being planned.
            try:
                manifests.append(
                    cargo_manifest(
                        subject,
                        campaign_id=_campaign_id(
                            owner, slugs[key], "cargo-mutants", day
                        ),
                        repo_root=repo_root,
                        jobs=jobs,
                        run_timeout_seconds=run_timeout_seconds,
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

    return {
        "language": language,
        "manifests": manifests,
        "unpaired": unpaired,
        "unpaired_lines": sum(int(u["lines"]) for u in unpaired),
        "already_covered": already,
        "untested": untested,
    }


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
        relative = f"{CAMPAIGN_DIR}/{manifest['campaign_id']}.json"
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

    relative = campaign.removeprefix(f"{CAMPAIGN_DIR}/")
    if not relative.endswith(".json"):
        relative = f"{relative}.json"
    path = repo_root / CAMPAIGN_DIR / relative
    try:
        existing = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise CampaignError(f"cannot load generated campaign {path}: {exc}") from exc
    if not isinstance(existing, dict) or existing.get("mutation_engine") != "cargo-mutants":
        raise CampaignError(f"{path} is not a cargo-mutants generated campaign")
    options = (existing.get("generator") or {}).get("options") or {}
    package = options.get("package")
    manifest_path = options.get("manifest_path")
    if not isinstance(package, str) or not isinstance(manifest_path, str):
        raise CampaignError(f"{path} has no generated cargo package")
    candidates = [row for row in rust_subjects(repo_root) if row["package"] == package]
    subject = next(
        (
            row
            for row in candidates
            if row["manifest"] == manifest_path
        ),
        None,
    )
    if subject is None and len(candidates) == 1:
        # A prior generator version could pin its own disposable snapshot. Once
        # snapshots are excluded, the sole real package is the unambiguous repair.
        subject = candidates[0]
    if subject is None:
        raise CampaignError(f"generated cargo package {package!r} no longer exists")
    generator = existing.get("generator") or {}
    refreshed = cargo_manifest(
        subject,
        campaign_id=str(existing.get("campaign_id") or path.stem),
        repo_root=repo_root,
        jobs=int(generator.get("jobs", 4)),
        run_timeout_seconds=int(generator.get("run_timeout_seconds", 1800)),
    )
    if sources:
        requested = set(sources)
        available = set(subject["files"])
        unknown = sorted(requested - available)
        if unknown:
            raise CampaignError(
                f"scoped source is not in {package!r}: {unknown}"
            )
        selected = sorted(requested)
        package_root = Path(subject["root"])
        refreshed["generator"]["source"] = [
            (Path(path).relative_to(package_root)).as_posix() for path in selected
        ]
        refreshed["source_sha256"] = {
            path: _sha256(repo_root / path) for path in selected
        }
        # Rust unit tests are colocated with their source.  Binding just these
        # files keeps the receipt specific to the requested test surface even
        # though cargo-mutants invokes Cargo's unit-test harness.
        refreshed["test_sha256"] = dict(refreshed["source_sha256"])
    refreshed["survivor_baseline"] = list(existing.get("survivor_baseline") or [])
    if "survivor_baseline_note" in existing:
        refreshed["survivor_baseline_note"] = existing["survivor_baseline_note"]
    path.write_text(json.dumps(refreshed, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return path.relative_to(repo_root).as_posix()


def _summarise(result: Mapping[str, Any], *, verbose: bool) -> dict[str, Any]:
    manifests = list(result["manifests"])
    payload: dict[str, Any] = {
        "status": "READY",
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


def main(argv: list[str] | None = None) -> int:
    """CLI: `plan` reports what would be written, `write` writes it."""

    parser = argparse.ArgumentParser(description=__doc__)
    shared = argparse.ArgumentParser(add_help=False)
    shared.add_argument("language", choices=("python", "rust"))
    shared.add_argument("--owner", default="claude")
    shared.add_argument("--jobs", type=int, default=4, help="Rust workers per crate")
    shared.add_argument("--run-timeout", type=int, default=1800)
    shared.add_argument("--only", action="append", default=[])
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
        "refresh", help="automatically refresh one existing generated Rust campaign"
    )
    refresh_parser.add_argument("campaign", help="campaign id or manifest filename")
    refresh_parser.add_argument(
        "--source",
        action="append",
        default=[],
        help="repository-relative Rust source to mutate and bind as its test surface",
    )
    args = parser.parse_args(argv)

    try:
        if args.command == "refresh":
            print(
                json.dumps(
                    {
                        "refreshed": refresh_rust_campaign(
                            args.campaign, sources=args.source
                        )
                    },
                    indent=2,
                )
            )
            return 0
        result = plan(
            args.language,
            owner=args.owner,
            jobs=args.jobs,
            run_timeout_seconds=args.run_timeout,
        )
        if args.only:
            wanted = set(args.only)
            kept = [
                m
                for m in result["manifests"]
                if wanted & {m["campaign_id"], *m["source_sha256"]}
            ]
            if not kept:
                raise CampaignError(f"--only matched no subject: {sorted(wanted)}")
            result = {**result, "manifests": kept}
        if args.command == "plan":
            print(json.dumps(_summarise(result, verbose=args.verbose), indent=2))
            return 0
        written = write(result["manifests"], force=args.force)
        print(
            json.dumps(
                {**_summarise(result, verbose=False), "written": written}, indent=2
            )
        )
        return 0
    except CampaignError as exc:
        print(json.dumps({"status": "REFUSED", "error": str(exc)}, indent=2))
        return 4


if __name__ == "__main__":
    raise SystemExit(main())
