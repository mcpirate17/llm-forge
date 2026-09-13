"""Per-test kill attribution for machine-generated mutants.

A generated engine reports one outcome per mutant and the set of tests that
*covered* it. That covering set is an input, not an attribution -- a survivor
carries a non-empty one too, and a mutant killed by one test out of forty
reports all forty. The value analysis in `mutation_value` needs the other
direction: which tests actually *failed* while a given mutant was live. No
generated engine emits that, which is why every generated receipt in the
repository carried `test_value: null` and why a new Python test could not
produce the evidence the corpus ratchet demands of it.

So this module measures it. For each killed mutant it re-applies the mutation
at the byte offsets the engine reported, runs only that mutant's covering set
under `--junitxml`, and reads the failures back out. The resulting kill matrix
is exactly the input `analyze_test_value` already accepts.

The re-run is narrow on purpose. Only killed mutants are re-applied -- the
validator rejects any other outcome in `mutation_contracts` -- and only their
covering tests are selected, because a test that never executes the mutated
bytes cannot fail because of them. Cost is therefore the size of the coverage
map, not the size of the suite times the number of mutants.

Attribution is best-effort per mutant and fail-loud per run. A mutant whose
re-run produces no failure, or whose report cannot be mapped back to nodeids,
is recorded in `attribution.unattributed` with its reason rather than silently
dropped: an attribution rate is a diagnostic, and a silent one is worthless.
"""

from __future__ import annotations

import sys
from collections.abc import Callable, Iterator, Mapping, Sequence
from contextlib import contextmanager
from pathlib import Path
from typing import Any

from conductor import mutation_engine_generated as _core
from conductor.bytecode_isolation import evict_mutated_caches, scratch_root_for
from conductor.mutation_scope import CampaignError
from conductor.mutation_value import (
    ADAPTER,
    ValueAnalysisSpec,
    ValueContract,
    ValueTest,
    analyze_test_value,
    parse_pytest_junit,
    pytest_junit_argv,
)

ATTRIBUTION_SCHEMA = "llm.mutation-testing.attribution.v1"

# The validator accepts 2..=5 baseline repetitions and requires every ranked
# nodeid to pass in every one of them. Two is the floor that can still catch a
# test whose outcome moves on its own -- and a test that flickers would
# otherwise be read as a kill.
BASELINE_REPETITIONS = 2

# `criticality` is a closed vocabulary in the validator: "critical" or "high".
# A generated campaign has no author to grade its subject, so it claims the
# lower of the two rather than asserting a grade nobody made.
CRITICALITY = "high"

# Flags that stop a run early. Attribution needs every covering test's outcome,
# so a truncated report is a wrong report rather than a cheaper one.
_TRUNCATING = frozenset({"-x", "--exitfirst"})

# Flags that deselect. The nodeids are named explicitly here, so a surviving
# `-k` or `-m` can only remove one and turn its absence into an INCOMPLETE
# report. Both take a separate value argument.
_DESELECTING = frozenset({"-k", "-m"})


def _function(nodeid: str) -> str | None:
    """The function-level nodeid a JUnit report can be matched back to.

    A JUnit case carries a classname and a name, and `parse_pytest_junit`
    aggregates the parameter cases of one function into a single row keyed by
    the *unparametrized* nodeid. A coverage map that names
    `test_x.py::test_a[case]` therefore matches nothing, every parametrized
    test reads as missing, and the whole report comes back INCOMPLETE. Cutting
    at the first `[` is what makes the two ends agree -- and selecting the
    function on the command line runs all of its cases, which is the coverage
    the map was describing in the first place.

    Returns None for a nodeid the adapter cannot use at all: no `.py` module
    means no classname to reconstruct.
    """

    module, separator, rest = nodeid.partition("::")
    if separator and module.endswith(".py"):
        return f"{module}::{rest.split('[', 1)[0]}"


def _ranked(rows: Sequence[Mapping[str, Any]]) -> tuple[list[str], list[str]]:
    """Every nodeid the engine's coverage map names, and the ones it cannot use."""

    usable: set[str] = set()
    unusable: set[str] = set()
    for row in rows:
        for nodeid in row.get("tests_run") or ():
            function = _function(nodeid)
            if function is None:
                unusable.add(nodeid)
            else:
                usable.add(function)
    return sorted(usable), sorted(unusable)


def _pytest_index(argv: Sequence[str]) -> int:
    """Where the pytest invocation starts, so only its arguments are rewritten."""

    for index, arg in enumerate(argv):
        if arg.rsplit("/", 1)[-1] == "pytest":
            return index
    raise CampaignError(
        f"attribution needs a pytest test command; campaign runs {list(argv)!r}"
    )


def _narrowed(argv: Sequence[str], worktree: Path) -> list[str]:
    """The campaign's pytest flags, with its own selection and early exits cut."""

    start = _pytest_index(argv) + 1
    kept = list(argv[:start])
    skip_value = False
    for arg in argv[start:]:
        if skip_value:
            skip_value = False
            continue
        if arg in _TRUNCATING or arg.startswith("--maxfail"):
            skip_value = arg == "--maxfail"
            continue
        if arg in _DESELECTING:
            skip_value = True
            continue
        if arg.startswith("-"):
            kept.append(arg)
            continue
        if "::" in arg or (worktree / arg).exists():
            continue
        kept.append(arg)
    return kept


def _selection(
    argv: Sequence[str], worktree: Path, nodeids: Sequence[str], junit: Path
) -> list[str]:
    """The campaign's command, narrowed to exactly these tests, reporting JUnit."""

    return list(pytest_junit_argv([*_narrowed(argv, worktree), *nodeids], junit))


def _invalidate(target: Path, worktree: Path) -> None:
    """Drop the target's cached bytecode, beside the source and in the run's cache.

    CPython validates a `.pyc` against the source's size and its mtime in whole
    seconds. A one-operator mutation frequently changes neither, so a re-run
    inside the same second would import the *unmutated* module and every mutant
    would come back unattributed with nothing to show for it. The launcher
    points every child at the run's private prefix, so that copy is evicted
    with the same stroke; the beside-source glob stays for any run whose
    runner does not isolate.
    """

    cache = target.parent / "__pycache__"
    if cache.is_dir():
        for entry in cache.glob(f"{target.stem}.*.pyc"):
            entry.unlink()
    evict_mutated_caches([target], scratch_root_for(worktree))


@contextmanager
def _applied(worktree: Path, row: Mapping[str, Any]) -> Iterator[None]:
    """Re-apply one mutant byte-exactly, then put the file back."""

    target = worktree / str(row["path"])
    offset = int(row["byte_offset"])
    length = int(row["byte_length"])
    original = target.read_bytes()
    expected = str(row["original_text"]).encode("utf-8")
    found = original[offset : offset + length]
    if found != expected:
        raise CampaignError(
            f"mutant {row['id']} does not match {row['path']} at byte {offset}: "
            f"found {found!r}, expected {expected!r}"
        )
    mutated = str(row["mutated_text"]).encode("utf-8")
    target.write_bytes(original[:offset] + mutated + original[offset + length :])
    _invalidate(target, worktree)
    try:
        yield
    finally:
        target.write_bytes(original)
        _invalidate(target, worktree)


def _spec(
    campaign: Any, ranked: Sequence[str], paths: Sequence[str], ids: Sequence[str]
) -> ValueAnalysisSpec:
    """One campaign-wide contract, because the killers are not partitioned by file.

    Per-file contracts read better and do not work: a mutant in one source file
    is routinely killed only by a test the pairing assigns to another, and the
    validator then refuses it as having no killer bound to its contract. The
    campaign is the unit that was actually measured, so it is the contract.
    """

    contract = campaign.campaign_id
    return ValueAnalysisSpec(
        adapter=ADAPTER,
        baseline_repetitions=BASELINE_REPETITIONS,
        contracts=(ValueContract(contract, CRITICALITY, tuple(paths)),),
        tests=tuple(ValueTest(nodeid, contract, False) for nodeid in ranked),
        mutation_contracts={mutation_id: contract for mutation_id in ids},
    )


class _Session:
    """One attribution run's fixed inputs: where, how and under what limits."""

    __slots__ = ("argv", "environment", "reports", "run", "timeout", "worktree")

    def __init__(
        self,
        campaign: Any,
        *,
        worktree: Path,
        environment: Mapping[str, str],
        interpreter: str,
        run: Callable[..., tuple[Any, str]],
    ) -> None:
        self.worktree = worktree
        self.argv = _core.pinned(campaign.test_argv, interpreter)
        # Children cache into the run's private prefix (the shared launcher
        # binds it), and the mutated file's cache is evicted around every
        # re-apply below and again by the engine's plugin at each child's
        # startup -- so no re-run imports a mutant's predecessor.
        self.environment = dict(environment)
        self.timeout = campaign.run_timeout_seconds
        self.run = run
        self.reports = worktree / ".attribution"
        self.reports.mkdir(exist_ok=True)

    def measure(self, nodeids: Sequence[str], name: str) -> tuple[Any, dict[str, Any]]:
        """Run exactly these tests and read their outcomes back out."""

        junit = self.reports / f"{name}.xml"
        result, _ = self.run(
            _selection(self.argv, self.worktree, nodeids, junit),
            cwd=self.worktree,
            timeout_seconds=self.timeout,
            environment=self.environment,
        )
        if result.timed_out:
            return result, {}
        return result, parse_pytest_junit(junit, nodeids)


def _baselines(session: _Session, ranked: Sequence[str]) -> list[dict[str, Any]]:
    """The unmutated runs the validator compares every mutant against."""

    reports = []
    for index in range(BASELINE_REPETITIONS):
        result, report = session.measure(ranked, f"baseline-{index}")
        if result.timed_out:
            raise CampaignError(f"attribution baseline {index} timed out")
        reports.append(report)
    return reports


def _stderr_progress(position: int, total: int) -> None:
    """Default heartbeat: print(flush) to stderr, bounded to ~10 lines.

    The re-run loop below is the quietest, longest-running part of a generated
    campaign -- one pytest subprocess per killed mutant, nothing on stdout or
    stderr in between. A file with hundreds of killed mutants (or a host under
    load from other agents) can hold that silence for many minutes, and a
    silent process for 20 minutes is indistinguishable from a hung one: an
    operator watching it has no signal to tell "still working" from "stuck",
    and the reasonable response to a hang is to kill it -- which loses the
    whole receipt, not just the time. This makes the loop's progress visible
    without flooding the log: one line per roughly 10% of the work, plus the
    first and last mutant.
    """

    step = max(1, total // 10)
    if position == 0 or position == total - 1 or (position + 1) % step == 0:
        print(
            f"attribution: {position + 1}/{total} killed mutants re-run",
            file=sys.stderr,
            flush=True,
        )


def _matrix(
    session: _Session,
    killed: Sequence[Mapping[str, Any]],
    ranked: Sequence[str],
    summary: dict[str, Any],
    *,
    progress: Callable[[int, int], None] = _stderr_progress,
) -> dict[str, dict[str, Any]]:
    """One JUnit report per killed mutant, over that mutant's covering set."""

    covered = set(ranked)
    total = len(killed)
    matrix: dict[str, dict[str, Any]] = {}
    for position, row in enumerate(killed):
        progress(position, total)
        mutation_id = str(row["id"])
        covering = sorted(
            {
                function
                for nodeid in row.get("tests_run") or ()
                if (function := _function(nodeid)) in covered
            }
        )
        if not covering:
            _unattributed(summary, mutation_id, "no usable covering test")
            continue
        with _applied(session.worktree, row):
            result, report = session.measure(covering, f"mutant-{position}")
        if result.timed_out:
            _unattributed(summary, mutation_id, "covering set timed out")
        elif report["status"] != "COMPLETE":
            # Usually an import-time break: every covering test errors during
            # collection, so the report carries cases that map to no nodeid.
            # A real kill, but not one that can be charged to a test.
            _unattributed(summary, mutation_id, "report does not map to nodeids")
        elif not report["failed_nodeids"]:
            _unattributed(summary, mutation_id, "covering set passed on re-run")
        else:
            matrix[mutation_id] = report
    return matrix


def _unattributed(summary: dict[str, Any], mutation_id: str, reason: str) -> None:
    summary["unattributed"].append({"id": mutation_id, "reason": reason})


def attribute(
    campaign: Any,
    receipt: dict[str, Any],
    *,
    worktree: Path,
    environment: Mapping[str, str],
    interpreter: str,
    run: Callable[..., tuple[Any, str]] = _core.run,
    progress: Callable[[int, int], None] = _stderr_progress,
) -> None:
    """Measure the kill matrix and write `test_value` into the receipt.

    Must be called while the snapshot worktree still exists: the mutants are
    re-applied inside it, and it is destroyed as soon as the run's context
    manager exits.
    """

    rows = list(receipt.get("mutants") or ())
    ranked, unusable = _ranked(rows)
    killed = [row for row in rows if row.get("outcome") == _core.KILLED]
    summary: dict[str, Any] = {
        "schema_version": ATTRIBUTION_SCHEMA,
        "baseline_repetitions": BASELINE_REPETITIONS,
        "ranked_tests": len(ranked),
        "unusable_nodeids": unusable,
        "killed_mutants": len(killed),
        "attributed_mutants": 0,
        "unattributed": [],
        "status": "NOT_ATTEMPTED",
    }
    receipt["attribution"] = summary
    if not ranked or not killed:
        summary["reason"] = (
            "the coverage map names no usable pytest nodeid"
            if not ranked
            else "no mutant was killed"
        )
        return

    session = _Session(
        campaign,
        worktree=worktree,
        environment=environment,
        interpreter=interpreter,
        run=run,
    )
    baseline_reports = _baselines(session, ranked)
    matrix = _matrix(session, killed, ranked, summary, progress=progress)
    summary["attributed_mutants"] = len(matrix)
    if not matrix:
        summary["status"] = "NO_ATTRIBUTION"
        return

    paths = sorted({str(row["path"]) for row in killed if str(row["id"]) in matrix})
    receipt["test_value"] = analyze_test_value(
        _spec(campaign, ranked, paths, sorted(matrix)),
        baseline_reports=baseline_reports,
        mutant_reports=matrix,
        mutant_outcomes=dict.fromkeys(matrix, _core.KILLED),
    )
    summary["status"] = "ATTRIBUTED"
