"""Generated mutants for the Python lanes, via fest.

fest (Rust, ruff's parser, coverage-guided) walks the source tree and emits its
own mutants, so nobody chooses which ones get written and the score stops being
a statement about the author's taste. The shared core in
`mutation_engine_generated` owns the manifest model, the receipt and the
survivor-set verdict; this module owns the two things that are fest's alone --
where its binary is, and how one run becomes receipt rows.

The awkward part is coverage. fest's coverage phase does not use the configured
test command: `run_pytest_cov` in its own source runs `python -m pytest --cov`
with no path arguments, which in a monorepo means collecting and running every
test in the tree, and makes a campaign scoped to one module depend on the whole
repository being green. So the adapter collects the map itself, from exactly the
tests the campaign names, and hands it over with `--coverage-from`. That costs
one run -- the same run that records the baseline.
"""

from __future__ import annotations

import json
import os
import shutil
import sys
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

from conductor import mutation_attribution as _attribution
from conductor import mutation_engine_generated as _core
from conductor.bytecode_isolation import scratch_root_for
from conductor.mutation_pycache_evict import (
    PLUGIN_NAME,
    SCRATCH_ENV,
    SOURCES_ENV,
)
from conductor.mutation_scope import CampaignError

ENGINE = "fest"

# fest's own status vocabulary mapped onto the receipt's. `NoCoverage` has no
# hand-written equivalent: it is a mutant no test reaches at all, which is the
# measurement this whole exercise exists to surface.
_OUTCOMES = {
    "Killed": _core.KILLED,
    "Survived": _core.SURVIVED,
    "Timeout": _core.TIMED_OUT,
    "NoCoverage": _core.NO_COVERAGE,
    "Error": _core.ERROR,
}

SUMMARY_KEYS = (
    "files_scanned",
    "mutants_generated",
    "mutants_tested",
    "no_coverage",
    "killed",
    "survived",
    "timeouts",
    "errors",
)


def binary() -> str:
    """The fest executable beside the running interpreter, or on PATH."""

    candidate = Path(sys.executable).parent / "fest"
    if candidate.is_file() and os.access(candidate, os.X_OK):
        return str(candidate)
    found = shutil.which("fest")
    if found is None:
        raise CampaignError(
            "fest is not installed; `uv sync` should provide fest-mutate "
            "(declared in pyproject, never only in a CI job)"
        )
    return found


def _config(campaign: _core.GeneratedCampaign, interpreter: str) -> str:
    """The fest.toml written into the snapshot, pinned to this interpreter."""

    test_command = _core.pinned(campaign.test_argv, interpreter)
    lines = [
        "[fest]",
        f"source = {json.dumps(list(campaign.source))}",
        f"exclude = {json.dumps(list(campaign.exclude))}",
        f"timeout = {campaign.mutant_timeout_seconds}",
        f"seed = {campaign.seed}",
        # fest otherwise picks a worker count from the host's CPU count, and
        # the survivor set then moves run to run: measured over five identical
        # runs of this campaign, 4/5/5/6/5 survivors, with the flipping mutant
        # carrying the same covering-test set every time. A corpus that jitters
        # cannot carry a ratchet, so the manifest pins the width.
        f"workers = {campaign.jobs}",
        f"test_command = {json.dumps(test_command)}",
        'output = "json"',
        'backend = "subprocess"',
        "",
    ]
    return "\n".join(lines)


def _row(
    relative_path: str, result: Mapping[str, Any], identifier: str
) -> dict[str, Any]:
    mutant = result["mutant"]
    duration = result.get("duration") or {}
    seconds = duration.get("secs", 0) + duration.get("nanos", 0) / 1e9
    status = str(result.get("status", ""))
    if status not in _OUTCOMES:
        raise CampaignError(f"fest reported unknown status {status!r}")
    return {
        "id": identifier,
        "outcome": _OUTCOMES[status],
        "path": relative_path,
        "line": mutant["line"],
        # Byte span, not just the line: attribution re-applies the mutation
        # after the run, and a line number cannot locate two mutants that sit
        # on the same line.
        "byte_offset": mutant["byte_offset"],
        "byte_length": mutant["byte_length"],
        "operator": mutant["mutator_name"],
        "original_text": mutant["original_text"],
        "mutated_text": mutant["mutated_text"],
        "tests_run": list(result.get("tests_run") or ()),
        "duration_seconds": round(seconds, 6),
    }


def _relative(absolute: Path, worktree: Path) -> str:
    try:
        return absolute.relative_to(worktree).as_posix()
    except ValueError:
        return absolute.name


def _rows(report: Mapping[str, Any], worktree: Path) -> list[dict[str, Any]]:
    """Name every generated mutant, in file order, deterministically."""

    results = sorted(
        report.get("results", ()),
        key=lambda item: (item["mutant"]["file_path"], item["mutant"]["byte_offset"]),
    )
    paths = [_relative(Path(r["mutant"]["file_path"]), worktree) for r in results]
    names = _core.identify(
        [
            (
                path,
                str(result["mutant"]["mutator_name"]),
                str(result["mutant"]["original_text"]),
                str(result["mutant"]["mutated_text"]),
            )
            for path, result in zip(paths, results, strict=True)
        ]
    )
    return [
        _row(path, result, identifier)
        for path, result, identifier in zip(paths, results, names, strict=True)
    ]


def _coverage_targets(patterns: Sequence[str]) -> list[str]:
    """The `--cov=` roots implied by the campaign's source globs."""

    targets: list[str] = []
    for pattern in patterns:
        # coverage measures packages and directories; handed a single file it
        # records nothing and every mutant comes back NO_COVERAGE.
        head = pattern.split("*", 1)[0].rstrip("/")
        root = head.rpartition("/")[0] if head.endswith(".py") else head
        if root not in targets:
            targets.append(root or ".")
    return targets


def _coverage_argv(campaign: _core.GeneratedCampaign, interpreter: str) -> list[str]:
    """The campaign's own test command, instrumented for per-test coverage."""

    argv = _core.pinned(campaign.test_argv, interpreter)
    return [
        *argv,
        *(f"--cov={target}" for target in _coverage_targets(campaign.source)),
        "--cov-context=test",
        "--cov-fail-under=0",
        "--cov-report=",
    ]


def _import_roots(worktree: Path) -> list[str]:
    """The snapshot's own import roots, ahead of anything site-packages injects.

    An editable install writes a .pth naming the ORIGINAL checkout, and site
    processing appends it to sys.path regardless of where the interpreter is run
    from. Under a flat layout the snapshot still won, because `python -m pytest`
    puts cwd first and the package sits at the repository root. Under a src
    layout it does not: `import conductor` resolves through the .pth to the
    unmutated checkout, every mutant is reported unreached, and the campaign
    scores nothing while reporting success. PYTHONPATH entries are placed ahead
    of site-packages, so naming them here is what binds the run to the snapshot.
    """

    return [str(path) for path in (worktree, worktree / "src") if path.is_dir()]


def _environment(
    campaign: _core.GeneratedCampaign, worktree: Path
) -> dict[str, str]:
    """The environment fest runs under, bound to this venv and this snapshot.

    fest probes for pytest-cov by spawning a bare `python`, so a runner started
    from a venv that carries pytest-cov still fails when PATH resolves `python`
    somewhere else. Binding PATH and VIRTUAL_ENV to the interpreter that is
    already pinned in the receipt keeps the probe and the test command in the
    same environment.

    fest also builds each mutant's pytest command itself, so no launcher of
    ours sits between a mutant's rewrite and the child that imports it. Every
    such child is pytest, though, so PYTEST_ADDOPTS loads the eviction plugin
    before collection imports anything, and the two CONDUCTOR variables tell
    it where this run's caches live and which files this run mutates --
    per-child eviction without touching fest.
    """

    bin_dir = str(Path(sys.executable).parent)
    declared = campaign.environment.get("PYTHONPATH", "").split(os.pathsep)
    roots = [*_import_roots(worktree), *declared]
    inherited_addopts = campaign.environment.get(
        "PYTEST_ADDOPTS", os.environ.get("PYTEST_ADDOPTS", "")
    )
    return {
        **campaign.environment,
        "PYTHONPATH": os.pathsep.join(root for root in roots if root),
        "VIRTUAL_ENV": str(Path(sys.executable).parents[1]),
        "PATH": bin_dir + os.pathsep + os.environ.get("PATH", ""),
        "PYTEST_ADDOPTS": " ".join(
            part
            for part in (inherited_addopts.strip(), f"-p {PLUGIN_NAME}")
            if part
        ),
        SCRATCH_ENV: str(scratch_root_for(worktree)),
        SOURCES_ENV: os.pathsep.join(
            str(worktree / relative) for relative in campaign.source_sha256
        ),
    }


def _engine_argv(
    campaign: _core.GeneratedCampaign, binary_path: str, coverage: Path
) -> list[str]:
    """The fest invocation: JSON on stdout, and our coverage map, not its own."""

    argv = [binary_path, "run", "--progress", "quiet", "--output", "json"]
    for pattern in campaign.operators:
        argv += ["--filter-operators", pattern]
    return [*argv, "--coverage-from", str(coverage)]


def execute(
    campaign: _core.GeneratedCampaign,
    receipt: dict[str, Any],
    *,
    binary: str,
    worktree: Path,
    output_path: Path,
) -> None:
    """Collect coverage, generate and run the mutants, fill in the receipt."""

    if drifted := _core.drift(campaign, worktree):
        raise CampaignError(f"snapshot source hashes drifted: {drifted}")
    coverage_path = worktree / ".coverage"
    environment = {
        **_environment(campaign, worktree),
        "COVERAGE_FILE": str(coverage_path),
    }
    baseline_argv = _coverage_argv(campaign, sys.executable)
    baseline, _ = _core.run(
        baseline_argv,
        cwd=worktree,
        timeout_seconds=campaign.run_timeout_seconds,
        environment=environment,
    )
    _core.note_baseline(campaign, receipt, baseline, baseline_argv, output_path)
    if not coverage_path.is_file():
        raise CampaignError(
            f"baseline produced no coverage database at {coverage_path}"
        )
    # fest.toml is written after the baseline on purpose: the per-mutant
    # timeout it pins is derived from that baseline's wall time when the
    # manifest does not pin one, and the coverage run above does not read it.
    (worktree / "fest.toml").write_text(
        _config(campaign, sys.executable), encoding="utf-8"
    )

    result, stdout = _core.run(
        _engine_argv(campaign, binary, coverage_path),
        cwd=worktree,
        timeout_seconds=campaign.run_timeout_seconds,
        environment=environment,
        # The engine's own per-mutant children evict again at pytest startup
        # (the plugin above); this parent-side eviction keeps the same
        # guarantee for this launch whatever fest does internally.
        mutated_paths=[worktree / relative for relative in campaign.source_sha256],
    )
    receipt["engine_result"] = result.as_dict()
    if result.timed_out:
        raise CampaignError(f"fest timed out after {campaign.run_timeout_seconds}s")
    try:
        report = json.loads(stdout)
    except json.JSONDecodeError as exc:
        raise CampaignError(
            f"fest produced no JSON report (rc={result.returncode}): "
            f"{result.stderr_tail[-800:]}"
        ) from exc
    receipt["engine_summary"] = {k: report[k] for k in SUMMARY_KEYS if k in report}
    receipt["mutants"] = _rows(report, worktree)
    _core.require_executed(
        int(report.get("mutants_generated") or 0),
        int(report.get("mutants_tested") or 0),
        campaign.source,
    )
    # Inside the snapshot, and only here: attribution re-applies every killed
    # mutant in this worktree, which the caller destroys the moment `execute`
    # returns.
    _attribution.attribute(
        campaign,
        receipt,
        worktree=worktree,
        environment=_environment(campaign, worktree),
        interpreter=sys.executable,
    )


def main(argv: list[str] | None = None) -> int:
    """Run a fest campaign; the shared CLI dispatches by engine."""

    return _core.main(argv)


if __name__ == "__main__":
    raise SystemExit(main())
