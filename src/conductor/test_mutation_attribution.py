"""The kill matrix must come from re-running mutated code, not from the engine.

Every test here drives `attribute` through a runner that decides outcomes by
*reading the source file on disk*. A splice that never lands, or a restore that
never happens, therefore changes the answer rather than going unnoticed.
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Any

import pytest

from conductor import mutation_attribution as attribution
from conductor.mutation_campaign_model import CommandResult
from conductor.mutation_engine_generated import KILLED, SURVIVED
from conductor.mutation_scope import CampaignError
from conductor.mutation_value import ValueEvidenceError

SOURCE = "def add(a, b):\n    return a + b\n\n\ndef mul(a, b):\n    return a * b\n"
MODULE = "pkg/calc.py"
TESTS = "pkg/test_calc.py"
ADD = f"{TESTS}::test_add"
MUL = f"{TESTS}::test_mul"

# What each test asserts, expressed as the source it needs to survive. The fake
# runner checks these against the file as it stands, so the mutant has to be on
# disk at the moment the run happens for the test to fail.
HOLDS = {ADD: "a + b", MUL: "a * b"}


def _junit(path: Path, outcomes: dict[str, str]) -> None:
    cases = []
    for nodeid, outcome in outcomes.items():
        module, _, name = nodeid.rpartition("::")
        classname = module[:-3].replace("/", ".")
        body = "" if outcome == "PASSED" else "<failure>assert</failure>"
        cases.append(
            f'<testcase classname="{classname}" name="{name}" time="0.01">'
            f"{body}</testcase>"
        )
    path.write_text(
        f'<testsuites><testsuite name="pytest" tests="{len(cases)}">'
        f"{''.join(cases)}</testsuite></testsuites>",
        encoding="utf-8",
    )


class FakeRunner:
    """Runs nothing; grades the selected tests against the file on disk."""

    def __init__(self, worktree: Path) -> None:
        self.worktree = worktree
        self.commands: list[list[str]] = []
        self.environments: list[dict[str, str]] = []
        self.reports: list[Path] = []

    def __call__(
        self,
        argv: list[str],
        *,
        cwd: Path,
        timeout_seconds: int,
        environment: dict[str, str],
    ) -> tuple[CommandResult, str]:
        self.commands.append(list(argv))
        self.environments.append(dict(environment))
        report = next(
            Path(arg.split("=", 1)[1]) for arg in argv if arg.startswith("--junitxml=")
        )
        self.reports.append(report)
        text = (self.worktree / MODULE).read_text(encoding="utf-8")
        outcomes = {
            nodeid: ("PASSED" if HOLDS[nodeid] in text else "FAILED")
            for nodeid in argv
            if nodeid in HOLDS
        }
        _junit(report, outcomes)
        failed = any(outcome == "FAILED" for outcome in outcomes.values())
        return CommandResult(1 if failed else 0, False, 0.01, "", ""), ""


class Campaign:
    """The three fields `attribute` reads off a generated campaign."""

    campaign_id = "attribution-demo"
    run_timeout_seconds = 60
    test_argv = ("python", "-m", "pytest", "-q", TESTS)


def mutant(
    identifier: str,
    fragment: str,
    mutated: str,
    *,
    outcome: str = KILLED,
    tests_run: tuple[str, ...] = (ADD, MUL),
) -> dict[str, Any]:
    """One receipt row, with the byte span the engine would have reported."""

    offset = SOURCE.index(fragment)
    return {
        "id": identifier,
        "outcome": outcome,
        "path": MODULE,
        "line": SOURCE[:offset].count("\n") + 1,
        "byte_offset": offset,
        "byte_length": len(fragment.encode("utf-8")),
        "operator": "binary_operator",
        "original_text": fragment,
        "mutated_text": mutated,
        "tests_run": list(tests_run),
        "duration_seconds": 0.01,
    }


@pytest.fixture
def worktree(tmp_path: Path) -> Path:
    package = tmp_path / "pkg"
    package.mkdir()
    (package / "calc.py").write_text(SOURCE, encoding="utf-8")
    (package / "test_calc.py").write_text("# graded by FakeRunner\n", encoding="utf-8")
    return tmp_path


def run_attribution(worktree: Path, rows: list[dict[str, Any]]) -> dict[str, Any]:
    receipt: dict[str, Any] = {"mutants": rows, "test_value": None}
    attribution.attribute(
        Campaign(),
        receipt,
        worktree=worktree,
        environment={},
        interpreter=sys.executable,
        run=FakeRunner(worktree),
    )
    return receipt


def test_attribution_charges_each_mutant_to_the_test_that_actually_failed(
    worktree: Path,
) -> None:
    receipt = run_attribution(
        worktree,
        [
            mutant("add-op", "a + b", "a - b"),
            mutant("mul-op", "a * b", "a / b"),
        ],
    )

    assert receipt["attribution"]["status"] == "ATTRIBUTED"
    assert receipt["attribution"]["attributed_mutants"] == 2
    assert receipt["attribution"]["unattributed"] == []
    value = receipt["test_value"]
    assert value is not None
    # Both tests cover both mutants; only one of them can fail for each.
    assert value["killers_by_mutant"] == {"add-op": [ADD], "mul-op": [MUL]}
    # Which is the whole point: charged this way each test is the sole killer
    # of one mutant, so both grade CORE. Charged by coverage they would tie.
    classification = {row["nodeid"]: row["classification"] for row in value["tests"]}
    assert classification == {ADD: "CORE", MUL: "CORE"}


def test_a_mutant_whose_covering_set_still_passes_is_recorded_not_dropped(
    worktree: Path,
) -> None:
    # The engine called it killed; re-running its covering tests does not
    # reproduce that. Charging it to a test anyway would invent a kill.
    receipt = run_attribution(worktree, [mutant("dead-op", "def mul", "def  mul")])

    assert receipt["test_value"] is None
    assert receipt["attribution"]["status"] == "NO_ATTRIBUTION"
    assert receipt["attribution"]["unattributed"] == [
        {"id": "dead-op", "reason": "covering set passed on re-run"}
    ]

    # A span that does not match the file is the other half of the same rule.
    # It is not a soft outcome: splicing at the wrong offset would corrupt the
    # source underneath the run and charge the damage to whatever failed.
    misaligned = mutant("add-op", "a + b", "a - b")
    misaligned["byte_offset"] += 1
    with pytest.raises(CampaignError, match="does not match"):
        run_attribution(worktree, [misaligned])


def test_only_killed_mutants_are_re_applied_and_none_says_why(
    worktree: Path,
) -> None:
    receipt = run_attribution(
        worktree,
        [
            mutant("add-op", "a + b", "a - b"),
            mutant("lived", "a * b", "a / b", outcome=SURVIVED),
        ],
    )

    assert receipt["attribution"]["killed_mutants"] == 1
    assert list(receipt["test_value"]["killers_by_mutant"]) == ["add-op"]

    # With nothing killed there is no matrix to build, and the summary has to
    # say so rather than leaving `test_value` null with no reason attached.
    receipt = run_attribution(
        worktree, [mutant("lived", "a + b", "a - b", outcome=SURVIVED)]
    )

    assert receipt["test_value"] is None
    assert receipt["attribution"]["status"] == "NOT_ATTEMPTED"
    assert receipt["attribution"]["reason"] == "no mutant was killed"
    assert receipt["attribution"]["attributed_mutants"] == 0


def test_the_coverage_map_is_ranked_by_what_a_junit_report_can_name(
    worktree: Path,
) -> None:
    # A JUnit report carries a classname and a name, so a nodeid that names no
    # `.py` module cannot be matched back to a case.
    receipt = run_attribution(
        worktree, [mutant("add-op", "a + b", "a - b", tests_run=("crate::suite",))]
    )

    assert receipt["attribution"]["status"] == "NOT_ATTEMPTED"
    assert receipt["attribution"]["unusable_nodeids"] == ["crate::suite"]
    assert receipt["attribution"]["ranked_tests"] == 0
    assert (
        receipt["attribution"]["reason"]
        == "the coverage map names no usable pytest nodeid"
    )

    # The same rule cuts the other way for parameter cases. fest reports the
    # parametrized nodeid; a JUnit report aggregates every case of a function
    # into one row under the bare name. Keeping the bracket would make every
    # parametrized test read as missing, which makes the whole report
    # INCOMPLETE and fails the run closed.
    receipt = run_attribution(
        worktree,
        [
            mutant(
                "add-op",
                "a + b",
                "a - b",
                tests_run=(f"{ADD}[one]", f"{ADD}[two]", MUL),
            )
        ],
    )

    assert receipt["attribution"]["ranked_tests"] == 2
    assert receipt["attribution"]["unusable_nodeids"] == []
    assert receipt["test_value"]["killers_by_mutant"] == {"add-op": [ADD]}


def test_the_baseline_precedes_the_mutants_and_both_select_only_covering_tests(
    worktree: Path,
) -> None:
    runner = FakeRunner(worktree)
    attribution.attribute(
        Campaign(),
        {"mutants": [mutant("add-op", "a + b", "a - b")]},
        worktree=worktree,
        environment={},
        interpreter=sys.executable,
        run=runner,
    )

    baselines = [
        command
        for command in runner.commands
        if any("baseline-" in arg for arg in command)
    ]
    assert len(baselines) == attribution.BASELINE_REPETITIONS
    assert runner.commands[: len(baselines)] == baselines

    # Every run names its nodeids outright, so the campaign's own path argument
    # has to go: leaving it in would select the whole file and grade tests that
    # never touched the mutated bytes.
    mutant_command = runner.commands[-1]
    assert TESTS not in mutant_command
    assert mutant_command[:4] == [sys.executable, "-m", "pytest", "-q"]
    assert [arg for arg in mutant_command if arg in HOLDS] == [ADD, MUL]


@pytest.mark.parametrize(
    ("argv", "expected"),
    [
        (["pytest", "-q", "pkg/test_calc.py"], ["pytest", "-q"]),
        (["pytest", "-x", "-q"], ["pytest", "-q"]),
        (["pytest", "--maxfail", "3", "-q"], ["pytest", "-q"]),
        (["pytest", "--maxfail=3", "-q"], ["pytest", "-q"]),
        (["pytest", "-k", "add", "-q"], ["pytest", "-q"]),
        (["pytest", "-q", f"{TESTS}::test_add"], ["pytest", "-q"]),
        (["pytest", "-p", "no:cacheprovider"], ["pytest", "-p", "no:cacheprovider"]),
    ],
)
def test_narrowing_keeps_the_flags_and_cuts_selection_and_early_exit(
    worktree: Path, argv: list[str], expected: list[str]
) -> None:
    assert attribution._narrowed(argv, worktree) == expected


def test_the_source_and_its_bytecode_are_restored_around_every_mutation(
    worktree: Path,
) -> None:
    run_attribution(
        worktree,
        [
            mutant("add-op", "a + b", "a - b"),
            mutant("mul-op", "a * b", "a / b"),
        ],
    )

    assert (worktree / MODULE).read_text(encoding="utf-8") == SOURCE

    # Restoring the source is not enough on its own. CPython validates a `.pyc`
    # against the source's size and its mtime in whole seconds, so a mutation
    # that changes neither would be imported from a cache the restore left.
    cache = worktree / "pkg" / "__pycache__"
    cache.mkdir()
    stale = cache / "calc.cpython-312.pyc"
    stale.write_bytes(b"stale")

    with attribution._applied(worktree, mutant("add-op", "a + b", "a - b")):
        assert not stale.exists()
        # A `.pyc` written while the mutant is live would outlive the restore.
        stale.write_bytes(b"mutated")
    assert not stale.exists()


def test_the_summary_names_every_field_a_reader_of_it_needs(worktree: Path) -> None:
    # The summary is the whole diagnostic: an attribution rate nobody can read
    # is the silent drop this module exists to avoid.
    receipt = run_attribution(worktree, [mutant("add-op", "a + b", "a - b")])

    assert set(receipt["attribution"]) == {
        "schema_version",
        "baseline_repetitions",
        "ranked_tests",
        "unusable_nodeids",
        "killed_mutants",
        "attributed_mutants",
        "unattributed",
        "status",
    }


def test_an_absolute_pytest_is_recognised_and_survives_narrowing(
    worktree: Path,
) -> None:
    # `pinned` rewrites a bare `python` to an absolute interpreter, and a
    # campaign may name pytest by path directly. Matching on the whole string,
    # or dropping it because the path exists, breaks the command either way.
    binary = worktree / "bin" / "pytest"
    binary.parent.mkdir()
    binary.write_text("#!/bin/sh\n", encoding="utf-8")

    assert attribution._pytest_index([str(binary), "-q"]) == 0
    assert attribution._narrowed([str(binary), "-q"], worktree) == [str(binary), "-q"]


def test_narrowing_keeps_what_follows_a_selector_and_refuses_what_it_cannot(
    worktree: Path,
) -> None:
    # Dropping a selector must not stop the scan: the flags after it are the
    # ones that decide how the run reports.
    assert attribution._narrowed(["pytest", TESTS, "-q", "--tb=no"], worktree) == [
        "pytest",
        "-q",
        "--tb=no",
    ]

    # Narrowing only means anything for pytest, and it owns the JUnit report it
    # reads back. Both are refusals rather than best-effort rewrites, because a
    # silently mis-narrowed command produces a wrong kill matrix, not no matrix.
    with pytest.raises(CampaignError, match="needs a pytest test command"):
        attribution._narrowed(["cargo", "test"], worktree)
    with pytest.raises(ValueEvidenceError, match="junitxml"):
        attribution._selection(
            ["pytest", "--junitxml=other.xml"], worktree, [ADD], Path("x.xml")
        )


def test_no_run_is_allowed_to_write_bytecode(worktree: Path) -> None:
    runner = FakeRunner(worktree)
    attribution.attribute(
        Campaign(),
        {"mutants": [mutant("add-op", "a + b", "a - b")]},
        worktree=worktree,
        environment={"EXISTING": "kept"},
        interpreter=sys.executable,
        run=runner,
    )

    assert runner.environments
    for environment in runner.environments:
        assert environment["PYTHONDONTWRITEBYTECODE"] == "1"
        assert environment["EXISTING"] == "kept"


def test_reports_are_written_to_their_own_directory(worktree: Path) -> None:
    # They are written while mutants are live in this worktree. Dropping them
    # at its root leaves XML beside the sources the next mutant patches.
    runner = FakeRunner(worktree)
    attribution.attribute(
        Campaign(),
        {"mutants": [mutant("add-op", "a + b", "a - b")]},
        worktree=worktree,
        environment={},
        interpreter=sys.executable,
        run=runner,
    )

    assert runner.reports
    for report in runner.reports:
        assert report.parent == worktree / ".attribution"
