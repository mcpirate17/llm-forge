"""Sharded execution of the targeted-test sweep, with coverage recombination.

A large integration candidate selects tests from every changed file, and a single
process cannot finish that sweep inside one ``timeout_seconds`` budget. Running
each chunk as its own subprocess makes ``_limited_command``'s ``prlimit`` apply
the CPU and address-space budget per shard rather than to the whole sweep.
"""

from __future__ import annotations

import math
import os
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Sequence

from conductor.candidate_review.checks import ReviewContext
from conductor.candidate_review.command_runner import _run_process, _tail
from conductor.candidate_review.model import Finding, Severity
from conductor.candidate_review.policy import CheckPolicy


def shard_tests(tests: Sequence[str], shard_max_files: int) -> list[list[str]]:
    """Split selected test files into the fewest shards of at most ``shard_max_files``.

    Files are dealt round-robin rather than sliced contiguously: the selection is
    ordered by path, so contiguous slices would concentrate every ``research/`` test
    -- the slowest and heaviest -- into the same shards.
    """
    if shard_max_files <= 0 or len(tests) <= shard_max_files:
        return [list(tests)]
    count = math.ceil(len(tests) / shard_max_files)
    shards: list[list[str]] = [[] for _ in range(count)]
    for index, test in enumerate(tests):
        shards[index % count].append(test)
    return shards


_THREAD_PIN_VARIABLES = (
    "OMP_NUM_THREADS",
    "MKL_NUM_THREADS",
    "OPENBLAS_NUM_THREADS",
    "NUMEXPR_NUM_THREADS",
    "VECLIB_MAXIMUM_THREADS",
)


def shard_thread_environment(shard_workers: int) -> dict[str, str]:
    """Thread pins that stop concurrent shards from oversubscribing the CPU.

    Each shard imports torch/numpy, which size their pools from the machine's core
    count with no knowledge that ``shard_workers`` siblings are doing the same. On a
    4-vCPU runner that is 4 processes each claiming 4 cores: the work is unchanged but
    it takes far longer in WALL time, which is what ``timeout_seconds`` bounds. Giving
    each shard its fair share -- cores // workers -- removes the contention without
    reducing total parallelism.
    """
    cores = os.cpu_count() or 1
    share = max(1, cores // max(1, shard_workers))
    return {name: str(share) for name in _THREAD_PIN_VARIABLES}


def shard_data_file(coverage_file: Path, index: int, total: int) -> Path:
    """Per-shard coverage data file, or the shared one when the sweep is unsharded."""
    if total == 1:
        return coverage_file
    return coverage_file.with_name(f"{coverage_file.name}.shard{index}")


def combine_coverage(
    ctx: ReviewContext,
    coverage_file: Path,
    shard_files: Sequence[Path],
    check: CheckPolicy,
) -> Finding | None:
    """Union per-shard coverage data into the single file the coverage verdict reads.

    Without this every shard would report only the changed lines it happened to
    exercise, and ``_evaluate_changed_coverage`` would fail the candidate for a
    coverage shortfall that is an artefact of sharding.
    """
    present = [path for path in shard_files if path.exists()]
    if not present:
        return Finding(
            check_id=check.check_id,
            rule_id="coverage-incomplete",
            severity=Severity.HIGH,
            message="no shard produced coverage data to combine",
        )
    command = [
        sys.executable,
        "-m",
        "coverage",
        "combine",
        f"--data-file={coverage_file}",
        *[str(path) for path in present],
    ]
    try:
        completed = _run_process(
            command,
            ctx=ctx,
            timeout_seconds=check.timeout_seconds,
            memory_mb=check.memory_mb,
            include_git_metadata=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return Finding(
            check_id=check.check_id,
            rule_id="coverage-incomplete",
            severity=Severity.HIGH,
            message=f"combining shard coverage data failed: {type(exc).__name__}: {exc}",
        )
    if completed.returncode:
        return Finding(
            check_id=check.check_id,
            rule_id="coverage-incomplete",
            severity=Severity.HIGH,
            message=_tail(
                completed.stdout + "\n" + completed.stderr, check.max_output_chars
            ).strip()
            or "coverage combine failed",
            evidence={"exit_code": completed.returncode, "shards": len(present)},
        )
    return None


def execute_shards(
    ctx: ReviewContext,
    commands: Sequence[list[str]],
    check: CheckPolicy,
) -> tuple[list[subprocess.CompletedProcess[str] | None], list[int]]:
    """Run every shard, returning its result or ``None`` if it blew the wall budget.

    A timing-out shard used to abort the whole sweep: ``pool.map`` re-raises the first
    exception, so every finished shard's output was discarded and the candidate saw one
    opaque crash naming a single command line. Collecting the timeout instead keeps the
    other shards' verdicts and coverage, and ``shard_timeout_findings`` still fails the
    check -- the gate bites exactly as hard, it just stops destroying the evidence.
    """
    total = len(commands)
    timed_out: list[int] = []
    thread_pins = shard_thread_environment(check.shard_workers)

    def _execute(index: int) -> subprocess.CompletedProcess[str] | None:
        # Each shard is its own process, so `prlimit` in `_limited_command` applies the
        # CPU and address-space budget per shard instead of to the whole sweep.
        try:
            return _run_process(
                commands[index],
                ctx=ctx,
                timeout_seconds=check.timeout_seconds,
                memory_mb=check.memory_mb,
                include_git_metadata=False,
                extra_env=thread_pins,
                wall_timeout_seconds=check.wall_timeout_seconds,
            )
        except subprocess.TimeoutExpired:
            timed_out.append(index)
            return None

    if total == 1:
        return [_execute(0)], timed_out
    with ThreadPoolExecutor(max_workers=min(check.shard_workers, total)) as pool:
        return list(pool.map(_execute, range(total))), timed_out


def shard_timeout_findings(
    check: CheckPolicy,
    shards: Sequence[Sequence[str]],
    timed_out: Sequence[int],
    total: int,
) -> list[Finding]:
    """Report shards terminated by the wall budget, naming the files they held.

    Distinct from ``targeted-test-killed``: that is the CPU/address-space limit firing
    on a shard that ran away, this is a shard that was too slow in wall time. Only the
    latter is affected by how many shards share the machine, so conflating them sent
    the 2026-08-29 investigation after a packing bug that was really CPU starvation.
    """
    if not timed_out:
        return []
    stalled = sorted(timed_out)
    return [
        Finding(
            check_id=check.check_id,
            rule_id="targeted-test-timeout",
            severity=Severity.CRITICAL,
            message=(
                f"{len(stalled)} of {total} targeted-test shard(s) exceeded the "
                f"{check.wall_timeout_seconds}s wall budget and were terminated. The "
                f"per-shard CPU limit of {check.timeout_seconds}s was NOT reached, so "
                f"these shards were starved of CPU rather than looping. Stalled: "
                + "; ".join(
                    f"shard {index + 1} ({', '.join(shards[index])})"
                    for index in stalled[:3]
                )
            ),
            evidence={
                "timed_out_shards": [index + 1 for index in stalled],
                "shard_count": total,
                "wall_budget_seconds": check.wall_timeout_seconds,
                "cpu_budget_seconds": check.timeout_seconds,
            },
        )
    ]


def shard_outcome_findings(
    check: CheckPolicy,
    shards: Sequence[Sequence[str]],
    completed_all: Sequence[subprocess.CompletedProcess[str]],
    selected_count: int,
) -> tuple[list[Finding], list[int], list[int]]:
    """Classify shard exit codes into findings, separating kills from failures.

    Returns the findings, the per-shard exit codes, and the indices that failed.
    """
    total = len(shards)
    exit_codes = [completed.returncode for completed in completed_all]
    failed = [index for index, code in enumerate(exit_codes) if code]
    killed = [index for index in failed if exit_codes[index] < 0]
    combined_output = "\n".join(
        (
            f"--- shard {index + 1}/{total} ({len(shards[index])} files,"
            f" exit {exit_codes[index]}) ---\n"
            if total > 1
            else ""
        )
        + completed_all[index].stdout
        + "\n"
        + completed_all[index].stderr
        for index in (failed or range(total))
    )
    if killed:
        # A signal kill is not a test verdict. Reported as `targeted-test-failure`
        # it renders as a truncated pytest dump and reads like failing assertions,
        # which is how a budget overrun cost an evening on 2026-08-28.
        signals = sorted({-exit_codes[index] for index in killed})
        return (
            [
                Finding(
                    check_id=check.check_id,
                    rule_id="targeted-test-killed",
                    severity=Severity.HIGH,
                    message=(
                        f"{len(killed)} of {total} targeted-test shard(s) were killed "
                        f"by signal {signals} before finishing; this is a "
                        f"resource-budget overrun, not a test result. The sweep "
                        f"selected {selected_count} test files against a "
                        f"{check.timeout_seconds}s CPU and {check.memory_mb}MB budget "
                        f"per shard. Raise shard_workers/shard_max_files on "
                        f"checks.{check.check_id}, or its timeout_seconds/memory_mb."
                        "\n\n" + _tail(combined_output, check.max_output_chars).strip()
                    ),
                    evidence={
                        "exit_codes": exit_codes,
                        "killed_shards": [index + 1 for index in killed],
                        "selected_tests": selected_count,
                        "timeout_seconds": check.timeout_seconds,
                        "memory_mb": check.memory_mb,
                    },
                )
            ],
            exit_codes,
            failed,
        )
    if failed:
        return (
            [
                Finding(
                    check_id=check.check_id,
                    rule_id="targeted-test-failure",
                    severity=Severity.HIGH,
                    message=_tail(combined_output, check.max_output_chars).strip(),
                    evidence={"exit_code": exit_codes[failed[0]]}
                    if total == 1
                    else {
                        "exit_codes": exit_codes,
                        "failed_shards": [index + 1 for index in failed],
                    },
                )
            ],
            exit_codes,
            failed,
        )
    return [], exit_codes, failed
