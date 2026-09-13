"""Generated mutants for the Rust lanes, via cargo-mutants.

This project's compute is meant to live in Rust, C and CUDA -- so the engine
that mutates Python is the smaller half of the job. cargo-mutants parses each
crate with `syn`, rewrites function bodies and operators in the source text,
copies the tree, and runs `cargo build` then `cargo test` against every mutant.

Two behaviours of the tool matter to the adapter:

* **The exit code is not the verdict.** cargo-mutants exits 2 whenever any
  viable mutant survived, which for an uncurated corpus is the normal case and
  would make every campaign permanently red. The verdict comes from the
  survivor SET in `outcomes.json`, scored by the shared core.
* **Unviable is not a failure.** A mutant that does not compile was never a
  test of anything. It is recorded and excluded from the score rather than
  counted as a kill, which is what a kill-fraction reading would silently do.

There is no coverage phase: cargo-mutants has no coverage filter, so `cargo
test` runs in full for each mutant. That is affordable because the crates are
small and the target directory is shared -- 196 mutants of `snapshot-retention`
in 43 seconds -- but it is why `jobs` is pinned in the manifest rather than
left to the tool, and the per-mutant bound is either pinned there too or
derived from the baseline suite's own wall time.
"""

from __future__ import annotations

import json
import os
import shutil
from collections.abc import Mapping
from pathlib import Path
from typing import Any

from conductor import mutation_engine_generated as _core
from conductor.mutation_scope import CampaignError

ENGINE = "cargo-mutants"

# cargo-mutants' own vocabulary mapped onto the receipt's.
_OUTCOMES = {
    "CaughtMutant": _core.KILLED,
    "MissedMutant": _core.SURVIVED,
    "Timeout": _core.TIMED_OUT,
    "Unviable": _core.UNVIABLE,
    "Failure": _core.ERROR,
}


def binary() -> str:
    """The cargo-mutants executable, which ships as a cargo subcommand."""

    found = shutil.which("cargo-mutants")
    if found is None:
        raise CampaignError(
            "cargo-mutants is not installed; `cargo install cargo-mutants` "
            "(it is a developer tool, not a shipped dependency, so it belongs "
            "in the toolchain rather than in pyproject)"
        )
    return found


def _mutant_of(outcome: Mapping[str, Any]) -> Mapping[str, Any] | None:
    """The mutant a scenario describes, or None for the baseline scenario."""

    scenario = outcome.get("scenario")
    if isinstance(scenario, str):
        return None
    if not isinstance(scenario, Mapping) or "Mutant" not in scenario:
        raise CampaignError(f"cargo-mutants reported an unknown scenario: {scenario!r}")
    return scenario["Mutant"]


def _position(mutant: Mapping[str, Any]) -> tuple[int, int]:
    start = mutant["span"]["start"]
    return int(start["line"]), int(start["column"])


def _original(mutant: Mapping[str, Any]) -> str:
    """What the mutant replaced, named by function rather than by text.

    cargo-mutants publishes the replacement but not the text it replaced -- the
    original only appears inside the human-readable diff. The function it lives
    in and its genre identify it just as well and, unlike the surrounding text,
    do not change when the body above it is edited.
    """

    function = mutant.get("function") or {}
    name = function.get("function_name") or "<file>"
    return f"{name}{function.get('return_type', '')}"


def _rows(report: Mapping[str, Any], package_root: str) -> list[dict[str, Any]]:
    """Name and record every mutant the run produced, deterministically."""

    mutants = []
    for outcome in report.get("outcomes", ()):
        mutant = _mutant_of(outcome)
        if mutant is not None:
            mutants.append((mutant, outcome))
    mutants.sort(key=lambda item: (item[0]["file"], _position(item[0])))

    entries = [
        (
            f"{package_root}/{mutant['file']}".lstrip("/"),
            str(mutant["genre"]),
            _original(mutant),
            str(mutant["replacement"]),
        )
        for mutant, _ in mutants
    ]
    names = _core.identify(entries)

    rows: list[dict[str, Any]] = []
    for (mutant, outcome), identifier, entry in zip(
        mutants, names, entries, strict=True
    ):
        summary = str(outcome.get("summary", ""))
        if summary not in _OUTCOMES:
            raise CampaignError(f"cargo-mutants reported unknown summary {summary!r}")
        rows.append(
            {
                "id": identifier,
                "outcome": _OUTCOMES[summary],
                "path": entry[0],
                "line": _position(mutant)[0],
                "operator": entry[1],
                "original_text": entry[2],
                "mutated_text": entry[3],
                "function": (mutant.get("function") or {}).get("function_name"),
                "package": mutant.get("package"),
                "duration_seconds": round(_duration(outcome), 6),
            }
        )
    return rows


def _duration(outcome: Mapping[str, Any]) -> float:
    return sum(
        float(phase.get("duration", 0.0)) for phase in outcome.get("phase_results", ())
    )


def _require_baseline(report: Mapping[str, Any]) -> None:
    """Refuse a run whose own unmutated baseline did not pass.

    cargo-mutants runs the clean tree first and reports it as a scenario. If
    that failed, every mutant after it is meaningless -- they would all read as
    caught by an already-red suite.
    """

    for outcome in report.get("outcomes", ()):
        if _mutant_of(outcome) is None:
            if str(outcome.get("summary")) != "Success":
                raise CampaignError(
                    f"cargo-mutants' unmutated baseline failed: {outcome.get('summary')}"
                )
            return
    raise CampaignError("cargo-mutants reported no baseline scenario")


def _require_isolated_builds(campaign: _core.GeneratedCampaign) -> None:
    """Refuse a campaign whose workers would share one cargo build directory.

    cargo-mutants gives every parallel job its own build directory. Pinning
    CARGO_TARGET_DIR in the manifest overrides exactly that, and the workers
    then race in one target dir: builds clobber each other, so mutants that
    compile fine get recorded as unviable and test results move run to run.
    Measured on `snapshot-retention` at jobs=4 with a shared target dir --
    three runs of the same 196 mutants gave 45/43/46 survivors and 24/21/22
    unviable. A corpus that jitters cannot carry a ratchet, so this is refused
    rather than warned about.
    """

    if campaign.jobs > 1 and "CARGO_TARGET_DIR" in campaign.environment:
        raise CampaignError(
            "environment.CARGO_TARGET_DIR is set with generator.jobs="
            f"{campaign.jobs}: parallel cargo-mutants workers would share one "
            "build directory and classify the same mutant differently run to "
            "run. Drop it, or set jobs to 1."
        )


def _engine_argv(
    campaign: _core.GeneratedCampaign, binary_path: str, output: Path
) -> list[str]:
    """The cargo-mutants invocation, every bound pinned by the manifest."""

    _require_isolated_builds(campaign)
    manifest_path = campaign.options.get("manifest_path")
    if not manifest_path:
        raise CampaignError(
            "generator.options.manifest_path must name the crate's Cargo.toml"
        )
    argv = [
        binary_path,
        "mutants",
        "--manifest-path",
        str(manifest_path),
        "--jobs",
        str(campaign.jobs),
        "--timeout",
        str(campaign.mutant_timeout_seconds),
        "--output",
        str(output),
    ]
    if package := campaign.options.get("package"):
        argv += ["--package", str(package)]
    for pattern in campaign.source:
        argv += ["--file", pattern]
    for pattern in campaign.exclude:
        argv += ["--exclude", pattern]
    return argv


def _read_report(output: Path) -> Mapping[str, Any]:
    outcomes = output / "mutants.out" / "outcomes.json"
    if not outcomes.is_file():
        raise CampaignError(f"cargo-mutants wrote no outcomes.json at {outcomes}")
    try:
        report = json.loads(outcomes.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise CampaignError(f"cargo-mutants outcomes.json is not JSON: {exc}") from exc
    if not isinstance(report, dict):
        raise CampaignError("cargo-mutants outcomes.json root must be a JSON object")
    return report


def execute(
    campaign: _core.GeneratedCampaign,
    receipt: dict[str, Any],
    *,
    binary: str,
    worktree: Path,
    output_path: Path,
) -> None:
    """Run the crate's own tests, then cargo-mutants, into the receipt."""

    if drifted := _core.drift(campaign, worktree):
        raise CampaignError(f"snapshot source hashes drifted: {drifted}")
    environment = {
        **campaign.environment,
        "PATH": os.environ.get("PATH", ""),
        "CARGO_TERM_COLOR": "never",
    }

    baseline_argv = list(campaign.test_argv)
    baseline, _ = _core.run(
        baseline_argv,
        cwd=worktree,
        timeout_seconds=campaign.run_timeout_seconds,
        environment=environment,
    )
    receipt["baseline_argv"] = baseline_argv
    receipt["baseline"] = baseline.as_dict()
    if baseline.timed_out or baseline.returncode != 0:
        receipt["status"] = "BASELINE_FAILED"
        _core.atomic_json(output_path, receipt)
        raise CampaignError(f"unmutated baseline failed; receipt={output_path}")
    receipt["mutant_timeout_seconds"] = _core.resolve_mutant_timeout(
        campaign, baseline.duration_seconds
    )

    output = worktree / ".cargo-mutants-out"
    result, _ = _core.run(
        _engine_argv(campaign, binary, output),
        cwd=worktree,
        timeout_seconds=campaign.run_timeout_seconds,
        environment=environment,
    )
    receipt["engine_result"] = result.as_dict()
    if result.timed_out:
        raise CampaignError(
            f"cargo-mutants timed out after {campaign.run_timeout_seconds}s"
        )

    # Exit 2 means "viable mutants survived", which is the ordinary reading of
    # an uncurated corpus. The survivor set decides the verdict, not the code.
    report = _read_report(output)
    _require_baseline(report)
    receipt["engine_summary"] = {
        key: report[key]
        for key in ("total_mutants", "caught", "missed", "timeout", "unviable")
        if key in report
    }
    receipt["engine_version"] = report.get("cargo_mutants_version")
    package_root = str(campaign.options.get("package_root", ""))
    receipt["mutants"] = _rows(report, package_root)
    tested = sum(
        1
        for row in receipt["mutants"]
        if row["outcome"] in (_core.KILLED, _core.SURVIVED)
    )
    _core.require_executed(len(receipt["mutants"]), tested, campaign.source)


def main(argv: list[str] | None = None) -> int:
    """Run a cargo-mutants campaign; the shared CLI dispatches by engine."""

    return _core.main(argv)


if __name__ == "__main__":
    raise SystemExit(main())
