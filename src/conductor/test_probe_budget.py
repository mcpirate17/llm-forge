"""Tests for the probe's per-function sweep budget, and for the gate that reports it.

The budget exists because `slop_gate.PER_MODULE_TIMEOUT` kills the whole child: one
slow function used to cost every other function in its module its verdict.
`conductor/mutation_coverage.py` spent 470.5 s under an unbounded sweep -- 221.9 s of
it in `main` alone -- and so was killed at 180 s having reported nothing about any of
its twelve functions, eleven of which cost 0.5 s between them. Under a 30 s budget the
same module finishes in 32.0 s, 41 of its 68 constructs get real verdicts, and the 27
that were never swept are named.

That trade is only sound if a sweep which ran out of time can never be mistaken for a
sweep which ran and found nothing. Every test here pins one edge of that:

* a truncated sweep reports OVER_BUDGET, not NO_DIFFERENCE_OBSERVED;
* and not BASELINE_UNUSABLE either, which is the same silence wearing a different hat;
* it carries no difference numbers, because it measured none;
* an unbudgeted call still measures everything, so the CLI and the direct API keep
  the contract they had;
* and the gate counts the result rather than dropping it -- which is what it did to
  any verdict it did not recognise, silently, while still printing PASS.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

import pytest

from conductor import equivalence_probe, slop_gate
from conductor.equivalence_probe import Verdict, probe_function

MODULE = '''
def clipped(value, ceiling=8):
    """`min` here is load-bearing; the guard below it never binds in the tests."""
    value = min(value, ceiling)
    if value < -1000:
        value = -1000
    return value * 2
'''

TESTS = """
from fixture_mod import clipped


def test_clipped():
    assert clipped(3) == 6
    assert clipped(50) == 16
"""


class _Clock:
    """A monotonic clock that advances one second per reading.

    Deterministic on purpose: the boundaries worth testing are *between* two clock
    reads, and with a real clock reaching them is a race that fails one run in ten.
    """

    def __init__(self) -> None:
        self.reads = 0

    def __call__(self) -> float:
        self.reads += 1
        return float(self.reads - 1)


@pytest.fixture
def workspace(probe_workspace) -> pathlib.Path:
    return probe_workspace(MODULE, TESTS)


def _probe(workspace: pathlib.Path, budget: float | None) -> list:
    return probe_function(
        workspace / "fixture_mod.py",
        "clipped",
        ["test_fixture_mod.py"],
        module_name="fixture_mod",
        budget_seconds=budget,
    )


# Captured at import, not inside `_pin_clock`: pinning twice in one test would
# otherwise wrap the previous lambda, which takes no `clock`.
_REAL_BUDGET = equivalence_probe._Budget


def _pin_clock(monkeypatch: pytest.MonkeyPatch, clock: _Clock) -> None:
    """Make every `_Budget` the probe builds read `clock` instead of the wall."""
    monkeypatch.setattr(
        equivalence_probe,
        "_Budget",
        lambda seconds: _REAL_BUDGET(seconds, clock=clock),
    )


def test_a_sweep_cut_at_any_point_is_never_reported_as_clean(
    workspace: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The fail-open this whole slice exists to close, at every point it can happen.

    A sweep can run out of budget in three places -- before an ablation, between two
    recorded calls, and between two amplifier plans -- and each has its own check.
    Testing one budget tests one of them. So this sweeps the budget across the range
    that reaches all three, and asserts the property that has to hold at every point:
    a truncated sweep may report OVER_BUDGET, and otherwise must report exactly what
    an unbounded sweep of the same construct reports. It may never invent a verdict.

    The clock is pinned so the boundaries are hit deterministically; with a real one
    reaching the amplified-sweep checkpoint is a race.
    """
    unbounded = {r.rule: r.verdict for r in _probe(workspace, None)}
    assert Verdict.NO_DIFFERENCE_OBSERVED in unbounded.values(), (
        "the fixture must reach a clean verdict, or there is nothing to fail open to"
    )

    usable_when_cut = set()
    # 5.0 is the only budget in this range that now lands BETWEEN two recorded calls:
    # a sweep that settles early returns without reaching the next budget check, so
    # the tick that used to be spent there is spent further along. The walk is a
    # search for the three checkpoints, not a fixed schedule -- when the sweep's
    # control flow changes, re-derive it rather than dropping the assertion.
    for budget in (1.0, 2.0, 3.0, 5.0, 7.0, 12.0):
        clock = _Clock()
        _pin_clock(monkeypatch, clock)
        results = _probe(workspace, budget)
        assert results
        cut = [r for r in results if r.verdict == Verdict.OVER_BUDGET]
        assert cut, f"budget={budget} was expected to run out"
        for r in results:
            assert r.verdict in (Verdict.OVER_BUDGET, unbounded[r.rule]), (
                f"budget={budget} turned {r.rule} from {unbounded[r.rule]} "
                f"into {r.verdict}"
            )
        usable_when_cut |= {r.usable_calls for r in cut}

    # Proof the sweep above actually reached the two checkpoints inside the sweeps,
    # rather than tripping the cheap per-ablation gate five times. A construct cut
    # with some but not all of its calls compared was cut inside `_sweep`; one cut
    # with ALL of them compared got through `_sweep` whole, so it was cut inside
    # `_sweep_amplified`. The fixture records two calls.
    assert 0 in usable_when_cut
    assert 1 in usable_when_cut, "no construct was cut between two recorded calls"
    assert 2 in usable_when_cut, "no construct was cut between two amplifier plans"


def test_a_budget_spent_inside_the_sweep_is_not_reported_as_unusable(
    workspace: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A sweep cut before its first call also counts zero usable calls.

    BASELINE_UNUSABLE is a claim about the recorded arguments -- that they never drove
    the unmodified function. Reaching that branch on a truncated sweep would report a
    defect in the fixture's own tests where there is none, and it is one `if` away:
    the budget check has to come first.
    """
    clock = _Clock()
    _pin_clock(monkeypatch, clock)
    # Reads: 1 builds the deadline, 2 is probe_function's per-ablation gate, 3 is the
    # first check inside `_sweep`. A 1.5 s budget survives read 2 (t=1.0) and expires
    # at read 3 (t=2.0), so the sweep is cut with zero calls compared.
    results = _probe(workspace, 1.5)

    assert results
    first = results[0]
    assert first.verdict == Verdict.OVER_BUDGET
    assert first.usable_calls == 0, "the sweep must really have compared nothing"


def test_an_unswept_construct_carries_no_difference_numbers(
    workspace: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A partial sweep measured 0.0 because it did not look, not because there is none.

    The report is read by both the gate and a human, and `max_diff_recorded = 0.0`
    beside OVER_BUDGET reads as evidence of equivalence. It is the absence of it.

    A budget that expires before the first ablation is the easy half: nothing ever
    assigned those fields, so they are None whether or not anything clears them. The
    half that needs clearing is a sweep that ran, recorded a difference of 0.0, and
    only then ran out -- so this walks the budget out to where that happens and
    asserts it was reached, or the check above it passes for the wrong reason.
    """
    swept_before_the_cut = 0
    for budget in (None, 2.0, 3.0, 7.0, 12.0):
        if budget is None:
            results = _probe(workspace, 1e-9)
        else:
            _pin_clock(monkeypatch, _Clock())
            results = _probe(workspace, budget)
        cut = [r for r in results if r.verdict == Verdict.OVER_BUDGET]
        assert cut, f"budget={budget} was expected to run out"
        for result in cut:
            assert result.max_diff_recorded is None
            assert result.max_diff_amplified is None
            assert result.amplifier is None
            assert "budget" in (result.detail or "")
        swept_before_the_cut = max(
            [swept_before_the_cut] + [r.usable_calls or 0 for r in cut]
        )
    assert swept_before_the_cut > 0, (
        "every cut landed before its first recorded call, so nothing above tested "
        "whether a measurement taken before the cut is cleared"
    )


def test_the_direct_api_and_the_cli_still_sweep_everything_by_default(
    workspace: pathlib.Path,
) -> None:
    """Two different defaults, and both are deliberate.

    `probe_function` is unbounded: a caller naming one function is asking for that
    function to be measured, and truncating it would silently answer a question it did
    not ask. `probe_module` is budgeted, because there the cost of an unbounded
    function is paid by the other functions. A single shared default would be wrong
    for one of them.
    """
    assert equivalence_probe.probe_function.__defaults__[-1] is None
    assert (
        equivalence_probe.probe_module.__defaults__[-1]
        == equivalence_probe.FUNCTION_BUDGET_SECONDS
    )
    # 0 is the documented escape hatch, and it must mean unbounded rather than
    # "expire immediately" -- the remediation text tells operators to use it.
    assert equivalence_probe._Budget(0).deadline is None
    assert equivalence_probe._Budget(None).deadline is None
    verdicts = {r.verdict for r in _probe(workspace, 0)}
    assert Verdict.OVER_BUDGET not in verdicts


def test_the_budget_latches_so_one_function_reports_one_answer(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """`expired()` is read once per call and once per ablation, on a moving clock.

    Without the latch a budget could report expired for one construct and unexpired
    for the next only if the clock went backwards -- but the latch also means the
    clock is read at most once after expiry, which is what keeps a cut sweep from
    paying for a `monotonic()` per remaining call.
    """
    clock = _Clock()
    budget = equivalence_probe._Budget(1.5, clock=clock)
    assert not budget.expired()  # t=1.0
    assert budget.expired()  # t=2.0
    reads_at_expiry = clock.reads
    assert budget.expired() and clock.reads == reads_at_expiry


def test_the_remediation_names_a_flag_the_probe_accepts() -> None:
    """The detail line tells an operator how to sweep the construct anyway.

    A remediation naming a flag the parser rejects is worse than none: it costs the
    reader a run to find out. Asserted against the real parser, not against a copy of
    the flag name.
    """
    detail = "".join(
        equivalence_probe._over_budget.__doc__ or ""
    )  # the docstring is not the contract; the emitted detail is
    base = equivalence_probe.AblationResult("f", "r", "d", 1, Verdict.LIVE, 1)
    equivalence_probe._over_budget(base, 30.0)
    assert "--budget-seconds 0" in (base.detail or ""), detail

    help_text = subprocess.run(
        [sys.executable, "-m", "conductor.equivalence_probe", "--help"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    assert "--budget-seconds" in help_text


def test_an_over_budget_construct_does_not_erase_its_module_from_the_report() -> None:
    """OVER_BUDGET is per construct; TIMEOUT and PROBE_FAILED are per module.

    They must not share a bucket. `modules_probed` subtracts every module named in
    `incomplete`, on the correct reasoning that a module which timed out was not
    measured. A module holding one over-budget function WAS measured -- folding the
    two together would hide the rest of its verdicts behind the one that ran long.
    """
    assert Verdict.OVER_BUDGET in slop_gate.UNMEASURED
    assert not set(slop_gate.UNMEASURED) & {
        *slop_gate.INCOMPLETE,
        *slop_gate.BLOCKING,
        *slop_gate.ADVISORY,
        slop_gate.UNTESTED,
    }
    # The two that predate the budget belong here for the same reason it does: the
    # probe formed no answer about that construct.
    assert {Verdict.BASELINE_UNUSABLE, Verdict.UNCOMPILABLE} <= set(
        slop_gate.UNMEASURED
    )


def test_every_verdict_the_probe_can_emit_lands_in_a_gate_bucket() -> None:
    """The gate's bucketing was an if/elif chain with no else.

    A verdict in none of the four buckets fell off the end and out of every summary
    list: not blocking, not advisory, not counted, not printed -- and the run still
    said PASS. This asserts the property rather than today's verdict list, so adding a
    verdict to the probe and forgetting the gate fails here instead of in silence.
    """
    known = {
        *slop_gate.BLOCKING,
        *slop_gate.ADVISORY,
        *slop_gate.INCOMPLETE,
        *slop_gate.UNMEASURED,
        slop_gate.UNTESTED,
        slop_gate.UNREACHED,
        slop_gate.LIVE,
    }
    emitted = {
        value
        for name, value in vars(Verdict).items()
        if not name.startswith("_") and isinstance(value, str)
    }
    assert emitted <= known, f"the gate cannot classify {sorted(emitted - known)}"


def _repo(tmp_path: pathlib.Path) -> pathlib.Path:
    tmp_path.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    subprocess.run(
        [
            "git",
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "root",
        ],
        cwd=tmp_path,
        check=True,
    )
    (tmp_path / "conductor").mkdir()
    (tmp_path / slop_gate.WAIVERS).write_text('{"waivers": []}')
    return tmp_path


def _finding(verdict: str, qualname: str = "Lane.forward") -> dict:
    return {
        "qualname": qualname,
        "rule": "drop_where",
        "lineno": 12,
        "verdict": verdict,
        "description": "torch.where(...) collapsed",
    }


def _run_with(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch, findings: list[dict]
) -> dict:
    repo = _repo(tmp_path)
    monkeypatch.setattr(slop_gate, "drivers_for", lambda m, r, i=None: ["test_lane.py"])
    monkeypatch.setattr(slop_gate, "probe", lambda m, t, r, i=None: findings)
    return slop_gate.run("HEAD", repo, only=["lane.py"])[1]


def test_every_verdict_reaches_a_summary_list_or_a_count(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """One finding of every verdict in, the same number accounted for out.

    This is the test that would have caught the original defect, and it is written as
    a conservation law rather than a list of expectations so that it keeps catching
    it. Before this change four verdicts reached no bucket at all -- LIVE,
    BASELINE_UNUSABLE, UNCOMPILABLE and NOT_REACHED_BY_DRIVERS -- and the run
    reported PASS having discarded every one of them without a word.
    """
    verdicts = sorted(
        {
            *slop_gate.BLOCKING,
            *slop_gate.ADVISORY,
            *slop_gate.INCOMPLETE,
            *slop_gate.UNMEASURED,
            slop_gate.UNTESTED,
            slop_gate.UNREACHED,
            slop_gate.LIVE,
        }
    )
    summary = _run_with(
        tmp_path,
        monkeypatch,
        [_finding(v, f"Lane.f{i}") for i, v in enumerate(verdicts)],
    )
    accounted = summary["live"] + sum(
        len(summary[k])
        for k in (
            "blocking",
            "advisory",
            "untested",
            "incomplete",
            "unmeasured",
            "unreached",
        )
    )
    assert accounted == len(verdicts)


def test_an_unclassified_verdict_stops_the_run_rather_than_vanishing(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Adding a verdict to the probe and forgetting the gate must be loud.

    The chain that classifies findings had no `else`, so this case was indistinguish-
    able from a clean module. Failing closed is the only option that stays correct as
    the probe grows: the gate cannot decide whether an unknown verdict blocks.
    """
    with pytest.raises(AssertionError, match="does not classify"):
        _run_with(tmp_path, monkeypatch, [_finding("A_VERDICT_FROM_THE_FUTURE")])


def test_an_over_budget_module_is_still_counted_as_probed(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The distinction that keeps OVER_BUDGET out of INCOMPLETE.

    `modules_probed` subtracts every module named in `incomplete`, because a module
    that timed out was not measured. A module with one over-budget function was: the
    other functions have real verdicts, and subtracting the module would throw away
    the whole point of budgeting instead of timing out.
    """
    summary = _run_with(
        tmp_path,
        monkeypatch,
        [
            _finding(Verdict.OVER_BUDGET, "Lane.slow"),
            _finding(Verdict.LIVE, "Lane.fast"),
        ],
    )
    assert summary["modules_probed"] == 1
    assert len(summary["unmeasured"]) == 1
    assert summary["live"] == 1

    timed_out = _run_with(tmp_path / "b", monkeypatch, [_finding(slop_gate.TIMEOUT)])
    assert timed_out["modules_probed"] == 0
