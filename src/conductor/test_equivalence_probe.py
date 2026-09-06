"""Tests for the tier-1 differential equivalence probe.

Each test pins one discipline the probe exists to enforce, and every one of them
corresponds to a verdict that was first reached -- wrongly -- by hand:

* a construct removable with no effect must not be called live (decorative code);
* a construct only reachable at extreme parameters must not be called dead, because
  ordinary inputs cannot get there (this is the case a surviving mutant reports
  ambiguously and a human then withdraws);
* a float32 round-off must not be called a real difference;
* and a sweep in which the unmodified function never ran must fail loudly instead of
  reporting a clean bill of health.
"""

from __future__ import annotations

import importlib
import pathlib
import sys
import textwrap
import threading

import pytest

from conductor.equivalence_ablations import generate_ablations
from conductor.equivalence_probe import Verdict, probe_function

MODULE = '''
import dataclasses
import functools
import time

import torch


def scaled(x, gain):
    """`.clamp_min` here can never bind: gain is always positive in the tests."""
    return x * gain.clamp_min(-1e30)


def masked_softmax(scores, used=None):
    """The production shape: a guard passed as an argument the helper defaults away."""
    if used is None:
        return torch.softmax(scores, dim=-1)
    masked = torch.where(used, scores, torch.full_like(scores, -1e30))
    return torch.softmax(masked, dim=-1)


class Lane(torch.nn.Module):
    """A slot lane whose empty-slot mask is unreachable from the input alone.

    `h` is a tanh, so no input scale can saturate the router -- exactly the shape
    that hides a live guard from an ordinary test, and from a probe that only
    amplifies inputs. Scaling the PARAMETERS underflows the small route entries to
    zero, the mask starts excluding slots whose scores are competitive, and the
    result moves by order one.
    """

    def __init__(self, slots=4, dim=3):
        super().__init__()
        self.route = torch.nn.Linear(dim, slots)
        self.value = torch.nn.Parameter(torch.randn(slots, dim))

    def forward(self, x):
        h = torch.tanh(x)
        route = torch.softmax(self.route(h), dim=-1)
        used = route.cumsum(dim=0) > 1e-6
        scores = h @ self.value.t()
        return masked_softmax(scores, used) @ self.value


def renormalised(x):
    """Dividing a unit-norm vector by its own norm moves only the last bits."""
    unit = x / x.norm().clamp_min(1e-12)
    return unit / unit.norm().clamp_min(1e-12)


def load_bearing(x):
    return x.clamp_min(0.0)


def validated(x):
    """A guard whose only reachable input is the one that makes it fire."""
    if x < 0:
        raise ValueError("x must be non-negative")
    return x * 2


def doubling(fn):
    """A decorator that changes the result, so removing it has to read LIVE."""

    @functools.wraps(fn)
    def inner(*a, **k):
        return fn(*a, **k) * 2

    return inner


@doubling
def decorated(x):
    return x.sum()


@dataclasses.dataclass
class Timing:
    """A result object carrying a wall-clock field, as the real one did.

    A DATACLASS, not a dict, and that is the whole point: `_difference` recurses into
    a dict and compares the fields numerically, but for an arbitrary object it falls
    back to `==`, which is all-or-nothing. So a microsecond of timing jitter is
    scored as an INFINITE relative change and can never meet a noise threshold.
    """

    total: float = 0.0
    elapsed_ms: float = 0.0


def timed(x, scale=1.0):
    """The shape that produced 11 of 16 false findings in the first real sweep.

    Two calls with the same input never agree, because one field is a clock.
    """
    total = float((x * scale).sum())
    # The RECORDED calls agree, so the probe gets past its first screen, and only the
    # amplified regime reaches the branch that reads a clock. That is why the real
    # findings all read max_diff_recorded=0.0 with max_diff_amplified=inf.
    if abs(total) > 1e3:
        return Timing(total=total, elapsed_ms=time.perf_counter() * 1e3)
    return Timing(total=total, elapsed_ms=0.0)
'''

TESTS = """
import torch
from fixture_mod import (
    Lane, decorated, load_bearing, renormalised, scaled, timed, validated,
)


def test_scaled():
    assert scaled(torch.ones(4), torch.full((4,), 2.0)).sum() == 8.0


def test_lane():
    torch.manual_seed(0)
    assert torch.isfinite(Lane()(torch.randn(5, 3))).all()


def test_renormalised():
    torch.manual_seed(0)
    assert torch.isfinite(renormalised(torch.randn(8)).sum())


def test_load_bearing():
    assert load_bearing(torch.tensor([-1.0, 2.0])).tolist() == [0.0, 2.0]


def test_decorated():
    assert decorated(torch.ones(4)) == 8.0


def test_timed():
    assert timed(torch.ones(4)).total == 4.0


def test_validated_rejects_negative():
    import pytest as _pytest
    with _pytest.raises(ValueError):
        validated(-1)
    assert validated(3) == 6
"""


@pytest.fixture
def workspace(probe_workspace) -> pathlib.Path:
    return probe_workspace(MODULE, TESTS)


def _verdicts(
    workspace: pathlib.Path, qualname: str, extra_rules: tuple[str, ...] = ()
) -> dict[str, str]:
    results = probe_function(
        workspace / "fixture_mod.py",
        qualname,
        ["test_fixture_mod.py"],
        module_name="fixture_mod",
        extra_rules=extra_rules,
    )
    return {r.rule: r.verdict for r in results}


def test_inert_guard_is_reported_as_no_difference(workspace: pathlib.Path) -> None:
    """`clamp_min(-1e30)` cannot bind, so removing it must read as decorative.

    Asserted per rule, not over the whole verdict set: other rules ablate other
    constructs in the same function and are expected to be LIVE, so a set-wide
    assertion would break every time a rule is added.
    """
    verdicts = _verdicts(workspace, "scaled")
    assert verdicts["drop_clamp_min"] == Verdict.NO_DIFFERENCE_OBSERVED


def test_expensive_unproven_rules_are_off_unless_asked_for(
    workspace: pathlib.Path,
) -> None:
    """ablate_function_to_passthrough was 44.6% of a sweep's ablations and never
    discriminated (it was `body_to_passthrough` before the engine moved to Rust).

    Over 26 real modules it returned 72 LIVE verdicts and no non-LIVE verdict at all.
    Most functions are not identities, so that is the expected shape -- but a rule that
    has not yet distinguished anything must not tax every run by default.
    """
    assert "ablate_function_to_passthrough" not in _verdicts(workspace, "scaled")
    opted_in = _verdicts(
        workspace, "scaled", extra_rules=("ablate_function_to_passthrough",)
    )
    assert opted_in["ablate_function_to_passthrough"] == Verdict.LIVE

    # The bare `OPTIONAL_RULES[name]` lookup already raises KeyError, so the explicit
    # guard only earns its place by reporting EVERY unknown name at once instead of
    # dying on the first. Assert that contract, or the guard is redundant code.
    with pytest.raises(KeyError) as caught:
        _verdicts(workspace, "scaled", extra_rules=("no_such_rule", "also_missing"))
    message = str(caught.value)
    assert "no_such_rule" in message and "also_missing" in message


def test_load_bearing_construct_is_reported_live(workspace: pathlib.Path) -> None:
    verdicts = _verdicts(workspace, "load_bearing")
    assert verdicts["drop_clamp_min"] == Verdict.LIVE


def test_saturation_only_guard_is_untested_rather_than_dead(
    workspace: pathlib.Path,
) -> None:
    """The whole point: ordinary inputs cannot reach it, so it must not read as dead.

    A probe that only scales inputs reports NO_DIFFERENCE here and invites someone to
    delete a live guard. Only amplifying the parameters reaches the branch.
    """
    verdicts = _verdicts(workspace, "Lane.forward")
    assert verdicts["drop_trailing_arg"] == Verdict.REACHABLE_BUT_UNTESTED


def test_float_round_off_is_not_reported_as_a_real_difference(
    workspace: pathlib.Path,
) -> None:
    verdicts = _verdicts(workspace, "renormalised")
    assert verdicts["drop_normalisation"] == Verdict.WITHIN_NUMERIC_NOISE


def test_a_guard_driven_only_by_its_error_path_reads_live(
    workspace: pathlib.Path,
) -> None:
    """Raising is a behaviour, not a failure to measure.

    A validation guard exists so that the call raises. Treating a raising baseline as
    an unusable input threw away the only argument that reaches the guard, so every
    guard in the repo came back "no difference" -- 19 of 19 in the first sweep.
    """
    verdicts = _verdicts(workspace, "validated")
    assert verdicts["drop_raise_guard"] == Verdict.LIVE


def test_both_ends_failing_identically_is_inconclusive_not_agreement(
    workspace: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A malformed replay breaks both ends the same way; that must not read as clean."""
    from conductor import equivalence_probe as ep

    def boom(*_a: object, **_k: object) -> None:
        raise TypeError("missing receiver")

    assert ep._compare_one(boom, boom, (), {}) is None


def test_a_call_that_exits_the_process_is_measured_not_propagated(
    workspace: pathlib.Path,
) -> None:
    """SystemExit is a BaseException, so it used to escape the probe entirely.

    A module's argparse `main()` is recorded by the test that fakes `sys.argv` and
    then replayed without it, so argparse calls `parser.error()`. One such function
    took down the probe for its whole module: 148.7 s spent, `probed=0`, and a gate
    that reported nothing for the file. Exiting is a behaviour of the call.
    """
    from conductor import equivalence_probe as ep

    def exits(*_a: object, **_k: object) -> None:
        raise SystemExit(2)

    def returns(*_a: object, **_k: object) -> int:
        return 1

    # Both ends exit the same way: inconclusive, exactly as for any other raise.
    assert ep._compare_one(exits, exits, (), {}) is None
    # One end exits and the other does not: a difference, not an escaped exception.
    assert ep._compare_one(returns, exits, (), {}) == float("inf")
    assert ep._compare_one(exits, returns, (), {}) == float("inf")


def test_methods_record_their_receiver(workspace: pathlib.Path) -> None:
    """A method probe must bind `self`, or every replay raises and nothing is measured.

    The failure this pins is silent: an unbound recorder drops the receiver, every
    comparison is discarded as unusable, and the sweep returns a clean zero -- a
    verdict of "no difference" from a run that never executed the function.
    """
    (workspace / "fixture_mod.py").write_text(
        textwrap.dedent("""
        import torch


        class Lane:
            def __init__(self):
                self.gain = torch.tensor(3.0)

            def forward(self, x):
                return (x * self.gain).clamp_min(-1e30)
    """)
    )
    (workspace / "test_fixture_mod.py").write_text(
        textwrap.dedent("""
        import torch
        from fixture_mod import Lane


        def test_lane():
            assert Lane().forward(torch.ones(3)).sum() == 9.0
    """)
    )
    sys.modules.pop("fixture_mod", None)
    importlib.invalidate_caches()
    results = probe_function(
        workspace / "fixture_mod.py",
        "Lane.forward",
        ["test_fixture_mod.py"],
        module_name="fixture_mod",
    )
    assert results, "the method produced no ablations"
    assert all(r.usable_calls > 0 for r in results)
    assert Verdict.BASELINE_UNUSABLE not in {r.verdict for r in results}


def test_trailing_argument_rule_needs_a_defaulted_callee() -> None:
    """Without resolving the callee the rule matches every two-argument call.

    That buries the one finding that matters under every ``isinstance`` in the file,
    so the rule fires only when the module actually defines a callee whose trailing
    parameter has a default.
    """
    import ast

    tree = ast.parse(textwrap.dedent(MODULE))
    lane = next(n for n in tree.body if getattr(n, "name", "") == "Lane")
    fn = next(n for n in lane.body if getattr(n, "name", "") == "forward")
    assert "drop_trailing_arg" in {a.rule for a in generate_ablations(fn, tree)}
    assert "drop_trailing_arg" not in {a.rule for a in generate_ablations(fn)}

    # The mask itself lives in the helper, and the guard-removal rule must reach it
    # there: a rule that fires only in the caller would miss every extracted guard.
    helper = next(n for n in tree.body if getattr(n, "name", "") == "masked_softmax")
    assert "drop_where" in {a.rule for a in generate_ablations(helper, tree)}


def test_unary_call_rule_needs_a_one_argument_callee() -> None:
    """`f(x)` -> `x` is only type-preserving when f takes exactly one argument.

    Applied to a multi-argument or defaulted callee the substitution changes what the
    value IS, and the difference it reports describes the rule rather than the code.
    """
    import ast

    tree = ast.parse(
        textwrap.dedent("""
        def unary(x):
            return x + 1


        def binary(x, y=2):
            return x + y


        def caller(v):
            return unary(v) + binary(v)
    """)
    )
    fn = next(n for n in tree.body if getattr(n, "name", "") == "caller")
    dropped = [
        a.description
        for a in generate_ablations(fn, tree)
        if a.rule == "drop_unary_call"
    ]
    assert any("unary" in d for d in dropped)
    assert not any("binary" in d for d in dropped)


def test_a_module_probe_shares_one_driver_run_across_its_functions(
    workspace: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Driver runs must scale with batches, not with functions.

    The driver suite does not care which function is being watched, so running it
    once per public function was pure repetition -- a measured mean of 11.0 public
    functions per module across this repo, worst case 174.
    """
    import pytest as _pytest

    from conductor import equivalence_probe as ep

    runs = []
    real_main = _pytest.main
    monkeypatch.setattr(
        _pytest, "main", lambda *a, **k: (runs.append(a), real_main(*a, **k))[1]
    )
    module_path = workspace / "fixture_mod.py"
    targets = ep.public_functions(module_path)
    assert len(targets) > 1, (
        "fixture must have several targets for this to mean anything"
    )

    ep.probe_module(module_path, ["test_fixture_mod.py"])
    assert len(runs) == 1, f"expected one shared driver run, got {len(runs)}"


def test_recorders_are_removed_after_a_shared_run(
    workspace: pathlib.Path,
) -> None:
    """Every patched site must be restored, or the next probe records through a
    stale recorder and reports a clean verdict from a measurement that never ran."""
    from conductor import equivalence_probe as ep

    module = importlib.import_module("fixture_mod")
    targets = ep.public_functions(workspace / "fixture_mod.py")
    ep._record_many(module, targets, ["test_fixture_mod.py"])

    for qualname in targets:
        owner = module
        for part in qualname.split(".")[:-1]:
            owner = getattr(owner, part)
        bound = getattr(owner, qualname.split(".")[-1], None)
        assert not getattr(bound, "__equivalence_recorder__", False), (
            f"{qualname} left wrapped in a recorder"
        )


def test_batching_bounds_recorder_memory_without_losing_a_target(
    workspace: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A batch smaller than the target list costs an extra driver run, never a
    dropped target -- a target silently skipped would read as NOT_EXERCISED, a
    clean verdict for a function whose tests do drive it."""
    import pytest as _pytest

    from conductor import equivalence_probe as ep

    module = importlib.import_module("fixture_mod")
    targets = ep.public_functions(workspace / "fixture_mod.py")
    assert len(targets) >= 2

    runs = []
    real_main = _pytest.main
    monkeypatch.setattr(
        _pytest, "main", lambda *a, **k: (runs.append(a), real_main(*a, **k))[1]
    )
    recorded = ep._record_many(module, targets, ["test_fixture_mod.py"], batch=1)
    assert len(runs) == len(targets), "batch=1 must run the drivers once per target"
    assert set(recorded) == set(targets), "batching dropped a target"


def test_whole_function_knockout_reaches_the_probe(workspace: pathlib.Path) -> None:
    """ "Delete the whole function -- does anything miss it?" is the question the other
    rules cannot ask.

    Every other rule removes a construct *inside* a function, so a function whose body
    is entirely decorative gets a clean verdict from each of them individually. This
    rule existed in the engine from the start and never fired, because the probe
    generated its ablations with the Python rule set (8 rules) rather than the engine's
    (25). Asserting it reaches the probe is what stops that regressing.
    """
    assert "ablate_function_to_none" in _verdicts(workspace, "scaled")


def test_the_probe_uses_the_native_engine_not_the_python_one(
    workspace: pathlib.Path,
) -> None:
    """The two rule sets differ 25 to 8, and the probe must be on the larger one.

    Pinned by a rule the Python engine never had, so this fails if the import is
    quietly reverted rather than merely renamed.
    """
    from conductor._native import slop_core
    from conductor.equivalence_ablations import RULES as PYTHON_RULES

    native_default, _ = slop_core().rule_names()
    assert "ablate_function_to_none" in native_default
    assert len(native_default) > len(PYTHON_RULES)
    # and the probe is on the larger set, not merely able to reach it. Pinned on a
    # rule the Python engine never had, so a quiet revert of the import fails here
    # rather than silently shrinking the sweep back to 8 rules.
    assert "ablate_function_to_none" in _verdicts(workspace, "scaled")


def test_a_function_that_disagrees_with_itself_yields_no_blocking_finding(
    workspace: pathlib.Path,
) -> None:
    """The false-positive class the first real sweep was almost entirely made of.

    `timed` returns a wall-clock field, so the unmodified function already differs
    from itself between two calls. Before the control existed this was reported as
    REACHABLE_BUT_UNTESTED with an INFINITE relative change -- the strongest verdict
    the probe can issue, on a difference that is not attributable to the ablation at
    all. Sixteen of eighteen findings over seven real modules were this.
    """
    verdicts = _verdicts(workspace, "timed")
    assert verdicts, "the fixture produced no ablations"
    assert Verdict.REACHABLE_BUT_UNTESTED not in verdicts.values()
    assert Verdict.NONDETERMINISTIC in verdicts.values()


def test_the_jitter_control_reports_rather_than_swallows(
    workspace: pathlib.Path,
) -> None:
    """An unstable return value is a real defect in the code under test.

    It defeats every differential tool, not just this one, so it gets a verdict of
    its own and reaches the report. Folding it into NO_DIFFERENCE_OBSERVED would
    have made the probe quietly less capable and told nobody.
    """
    results = probe_function(
        workspace / "fixture_mod.py",
        "timed",
        ["test_fixture_mod.py"],
        module_name="fixture_mod",
    )
    unstable = [r for r in results if r.verdict == Verdict.NONDETERMINISTIC]
    assert unstable, "instability must be reported, not silently dropped"
    assert all(r.max_diff_control and r.max_diff_control > 0.0 for r in unstable), (
        "a NONDETERMINISTIC verdict must carry the control measurement that earned it"
    )


def test_the_control_does_not_suppress_a_deterministic_finding(
    workspace: pathlib.Path,
) -> None:
    """The discriminating half: a control that suppresses everything is worthless.

    `Lane.forward` is stable, so its control measures zero and the saturation-only
    guard -- reachable only by amplifying the PARAMETERS -- must still block. Without
    this, dropping every finding on the floor would pass the test above.
    """
    results = probe_function(
        workspace / "fixture_mod.py",
        "Lane.forward",
        ["test_fixture_mod.py"],
        module_name="fixture_mod",
    )
    blocking = [r for r in results if r.verdict == Verdict.REACHABLE_BUT_UNTESTED]
    assert blocking, "a stable function's real finding must survive the control"
    assert all(r.max_diff_control == 0.0 for r in blocking), (
        "a stable function must measure zero jitter, so the finding is attributable"
    )


def test_the_jitter_floor_is_pooled_across_the_functions_ablations(
    workspace: pathlib.Path,
) -> None:
    """Three control repeats per ablation is still a small sample.

    A real function unstable enough to be caught 39 times over five runs cleared its
    own per-ablation control by luck in 1 run of 6, and that run reported a finding
    the other five did not. Every ablation of a function measures the SAME underlying
    instability, so the worst jitter any of them saw is the floor all of them clear.
    """
    results = probe_function(
        workspace / "fixture_mod.py",
        "timed",
        ["test_fixture_mod.py"],
        module_name="fixture_mod",
    )
    floor = max((r.max_diff_control or 0.0) for r in results)
    assert floor > 0.0, (
        "the fixture must actually jitter for this test to mean anything"
    )
    survivors = [r for r in results if r.verdict == Verdict.REACHABLE_BUT_UNTESTED]
    assert not survivors, (
        "no ablation may block on a difference under the function's own jitter floor: "
        f"{[(r.rule, r.max_diff_amplified) for r in survivors]}"
    )


def test_repeated_probes_of_unchanged_code_agree(workspace: pathlib.Path) -> None:
    """Reproducibility is the property that makes a verdict fit to gate on.

    Over seven real modules the blocking count was 13 / 10 / 7 on identical code
    before this; afterwards the finding set was byte-identical across eight
    consecutive runs. A gate whose findings move on their own cannot be enforced.
    """
    runs = [
        {
            r.rule: r.verdict
            for r in probe_function(
                workspace / "fixture_mod.py",
                "timed",
                ["test_fixture_mod.py"],
                module_name="fixture_mod",
            )
        }
        for _ in range(3)
    ]
    assert runs[0] == runs[1] == runs[2], f"verdicts moved between runs: {runs}"
    # Agreeing is not enough on its own -- three runs that all block would agree too.
    # The classification has to be the right one, every time.
    for run in runs:
        assert Verdict.REACHABLE_BUT_UNTESTED not in run.values(), run


def test_a_decorator_ablation_reaches_the_probe(workspace: pathlib.Path) -> None:
    """`drop_decorator` shipped in the engine but had never once been measured.

    It emitted an EMPTY qualname, because a decorator is a sibling of the definition
    it applies to rather than a node inside it, and probe_function filters ablations
    with `qualname == target`. So every decorator ablation was generated and then
    silently discarded -- a rule in the default set, counted in the rule total,
    contributing nothing. Here the decorator doubles the result, so it must read LIVE.
    """
    verdicts = _verdicts(workspace, "decorated")
    assert "drop_decorator" in verdicts, (
        f"the decorator ablation never reached the probe: {sorted(verdicts)}"
    )
    assert verdicts["drop_decorator"] == Verdict.LIVE


def test_an_amplifier_that_cannot_touch_an_argument_is_not_replayed() -> None:
    """The amplified sweep must skip plans that reproduce the recorded call.

    Eight plans over arguments none of them can scale is eight byte-identical
    repeats of a comparison the unamplified sweep already made -- and the sweep only
    reaches here when that comparison came back within noise. Replaying it can only
    re-sample jitter, which the control path downstream then pays to reject. The
    skip is what makes the gate affordable on modules that take paths and dicts
    (nothing in conductor/ takes a tensor), so its absence is a silent, expensive
    regression that no verdict would reveal.
    """

    import torch

    from conductor import equivalence_probe

    seen: list[tuple] = []

    def record(*args, **kwargs):
        seen.append((args, tuple(sorted(kwargs))))
        return 1.0

    inert = [((pathlib.Path("x"), {"a": 1}, "s"), {"flag": True})]
    worst, which = equivalence_probe._sweep_amplified(record, record, inert)
    assert (worst, which, seen) == (0.0, None, [])

    # A float tensor is scalable, so every input amplifier still runs. The three
    # parameter amplifiers cannot touch a tensor and are skipped -- a plan is
    # judged per call, not once for the whole sweep.
    equivalence_probe._sweep_amplified(record, record, [((torch.ones(3),), {})])
    assert len(seen) == 2 * len(equivalence_probe.AMPLIFIERS)

    # A tensor nested in a list is reached by the amplifier, so it is not skipped;
    # the same tensor behind a dict key is not, and skipping it is correct.
    assert not equivalence_probe._unamplified([torch.ones(2)], [torch.ones(2) * 2])
    assert equivalence_probe._unamplified([1, "a"], [1, "a"])
    # A rebuilt container of untouched elements is still an unamplified call.
    original = [1, [2, 3]]
    assert equivalence_probe._unamplified(original, [1, [2, 3]])
    # Type and length are part of the answer: a tuple is not its list.
    assert not equivalence_probe._unamplified((1, 2), [1, 2])
    assert not equivalence_probe._unamplified([1, 2], [1])


LOOPING_TESTS = """
import torch
from fixture_mod import load_bearing


def test_load_bearing_over_and_over():
    for _ in range(5):
        assert load_bearing(torch.tensor([-1.0, 2.0])).tolist() == [0.0, 2.0]
"""


def test_a_settled_sweep_stops_early_and_says_the_number_is_a_lower_bound(
    probe_workspace,
) -> None:
    """LIVE needs one call past the floor; the other 23 re-answer a settled question.

    The saving is the whole point -- `conductor/equivalence_probe.py` replays 24
    recorded calls for each of its 144 ablations -- but the report has to admit what
    it did, because `usable_calls` now counts calls SWEPT and `max_diff_recorded` is
    the first difference past the floor rather than the largest. Reporting a
    truncated sweep in the shape of a complete one is the fail-open this probe has
    already been bitten by twice.
    """
    workspace = probe_workspace(MODULE, LOOPING_TESTS)
    results = probe_function(
        workspace / "fixture_mod.py",
        "load_bearing",
        ["test_fixture_mod.py"],
        module_name="fixture_mod",
    )
    live = [r for r in results if r.verdict == Verdict.LIVE]
    assert live, "dropping the clamp must still read LIVE"
    for result in live:
        # "of 5" is the anti-vacuity half: a driver that called the function once
        # would stop after 1 of 1 and satisfy every other assertion here vacuously.
        assert "stopped after 1 of 5 recorded calls" in (result.detail or "")
        assert result.usable_calls == 1
        assert (result.max_diff_recorded or 0.0) > 0.0


def test_a_sweep_with_no_floor_returns_the_true_maximum() -> None:
    """The amplified sweep passes no floor, and must not be quietly truncated.

    Its magnitude is weighed against the function's own jitter in
    `_adjudicate_amplified`, so a first-past-the-post value understates the effect in
    exactly the direction that turns a real REACHABLE_BUT_UNTESTED into
    NONDETERMINISTIC. The two calls below are ordered small-difference-first so that
    stopping early and taking the maximum cannot return the same number.
    """
    from conductor import equivalence_probe

    calls = [((1.0,), {}), ((100.0,), {})]

    def baseline(x: float) -> float:
        return x

    def variant(x: float) -> float:
        return x * (1.001 if x < 10 else 2.0)

    worst, usable, settled = equivalence_probe._sweep(baseline, variant, calls)
    assert (usable, settled) == (2, False)
    assert worst == pytest.approx(1.0, rel=1e-6)

    first, usable, settled = equivalence_probe._sweep(
        baseline, variant, calls, settle_above=1e-6
    )
    assert (usable, settled) == (1, True)
    assert first == pytest.approx(0.001, rel=1e-3)
    assert first < worst


# ------------------------------------------------------- replay isolation


class _Uncopyable:
    """Mutable state that ``copy.deepcopy`` refuses, exactly as a real one does.

    A lock, a socket, an open file, a live database handle: an object carrying one
    is common in this repo's own call traffic, and every one of them makes
    ``deepcopy`` raise. It is mutable as well, which is the half that matters --
    an immutable uncopyable value shared between two runs is harmless.
    """

    def __init__(self, items: list[str]) -> None:
        self.items = items
        self.lock = threading.Lock()


def _consume(payload: _Uncopyable) -> int:
    """Reads its argument destructively, like any queue or cursor consumer."""

    return len(payload.items.pop()) if payload.items else 0


def test_an_uncopyable_argument_is_refused_rather_than_shared() -> None:
    """_clone must raise, not hand back the original.

    Returning the original is the same object on both ends of the comparison. The
    baseline mutates it, the variant then runs against the mutated value, and the
    difference the probe reports is its own aliasing rather than the ablation's
    effect -- in either direction: a real effect erased, or an invented one.
    """
    from conductor import equivalence_probe as ep

    payload = _Uncopyable(["abcd"])
    with pytest.raises(ep.UncopyableValue) as caught:
        ep._clone(payload)
    assert caught.value.type_name == "_Uncopyable"
    # The nesting matters as much as the top level: an uncopyable value reached
    # through a list or a dict is the ordinary case, since arguments arrive as a
    # tuple. A guard that only fires on a bare value would let every real call past.
    with pytest.raises(ep.UncopyableValue):
        ep._clone(([payload], {"k": payload}))
    assert payload.items == ["abcd"], "refusing to clone must not disturb the original"


def test_the_baseline_never_differs_from_itself_on_an_uncopyable_argument() -> None:
    """The control that catches replay aliasing: identical code, identical inputs.

    Comparing a function against ITSELF can only ever report no difference. When
    _clone handed back the original, `_consume` popped the single item during the
    baseline run and the second run found the list empty -- 4 against 0, an
    infinite relative difference reported for a function compared with itself, and
    every construct around it read LIVE for a reason that is not in the code.
    """
    from conductor import equivalence_probe as ep

    payload = _Uncopyable(["abcd"])
    assert ep._compare_one(_consume, _consume, (payload,), {}) is None
    assert payload.items == ["abcd"], "an unusable input must not be consumed"


def test_a_dropped_recording_is_counted_rather_than_swallowed() -> None:
    """The recorder must say what it lost.

    A silently skipped call is indistinguishable from a call that never happened,
    and the two have opposite repairs: one needs a copyable argument, the other
    needs a driver test.
    """
    from conductor import equivalence_probe as ep

    recorder, recording = ep._make_recorder(_consume)
    assert recorder(_Uncopyable(["ab"])) == 2
    assert recorder(_Uncopyable(["xyz"])) == 3
    assert recording.calls == [], "an uncopyable argument must not be recorded"
    assert recording.dropped == ["_Uncopyable", "_Uncopyable"]
    # The call still reaches the real function: the recorder is installed while the
    # driver suite runs, so refusing to record must never change what the suite does.
    kept, keeping = ep._make_recorder(_consume)
    assert kept(_Uncopyable(["ab"])) == 2
    assert keeping.dropped == ["_Uncopyable"]


def test_the_drop_limit_counts_refused_calls_too() -> None:
    """Otherwise a function whose arguments are all uncopyable retries forever.

    The cap exists to bound recorder memory and the driver's own runtime; counting
    only the kept calls means a function called ten thousand times with an
    uncopyable argument pays the clone cost ten thousand times and keeps nothing.
    """
    from conductor import equivalence_probe as ep

    recorder, recording = ep._make_recorder(_consume, limit=3)
    for _ in range(10):
        recorder(_Uncopyable(["ab"]))
    assert len(recording.dropped) == 3


def test_uncopyable_arguments_report_unusable_evidence_not_a_clean_sweep(
    workspace: pathlib.Path,
) -> None:
    """ARGUMENTS_UNCOPYABLE, never NOT_EXERCISED.

    NOT_EXERCISED says the driver tests do not reach this function -- the repair is
    to write one. Here the tests reach it on every call and the probe cannot keep
    what they pass. Reporting the second as the first sends the reader to fix a
    test that already exists.
    """
    from conductor import equivalence_probe as ep

    module_path = workspace / "fixture_mod.py"
    qualname = ep.public_functions(module_path)[0]
    results = ep.probe_function(
        module_path,
        qualname,
        ["test_fixture_mod.py"],
        calls=[],
        dropped=("_Uncopyable", "Lock"),
    )
    assert results, "the fixture function must have at least one ablation"
    for result in results:
        assert result.verdict == ep.Verdict.ARGUMENTS_UNCOPYABLE
        assert result.dropped_calls == 2
        assert "_Uncopyable" in result.detail and "Lock" in result.detail
    # And with nothing dropped the verdict is still the honest NOT_EXERCISED, or
    # the new one would swallow the case it was carved out of.
    untouched = ep.probe_function(
        module_path, qualname, ["test_fixture_mod.py"], calls=[]
    )
    assert {r.verdict for r in untouched} == {ep.Verdict.NOT_EXERCISED}
    assert {r.dropped_calls for r in untouched} == {0}
