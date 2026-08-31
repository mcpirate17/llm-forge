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

import pytest

from conductor.equivalence_ablations import generate_ablations
from conductor.equivalence_probe import Verdict, probe_function

MODULE = '''
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
'''

TESTS = '''
import torch
from fixture_mod import Lane, load_bearing, renormalised, scaled, validated


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


def test_validated_rejects_negative():
    import pytest as _pytest
    with _pytest.raises(ValueError):
        validated(-1)
    assert validated(3) == 6
'''


@pytest.fixture
def workspace(tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch) -> pathlib.Path:
    # Anchor pytest config discovery at tmp_path: without an inifile here the nested
    # run walks up to / looking for one, which the repo path guard rejects.
    (tmp_path / "pytest.ini").write_text("[pytest]\n")
    (tmp_path / "fixture_mod.py").write_text(textwrap.dedent(MODULE))
    (tmp_path / "test_fixture_mod.py").write_text(textwrap.dedent(TESTS))
    monkeypatch.chdir(tmp_path)
    monkeypatch.syspath_prepend(str(tmp_path))
    importlib.invalidate_caches()
    for name in ("fixture_mod", "test_fixture_mod"):
        sys.modules.pop(name, None)
    return tmp_path


def _verdicts(
    workspace: pathlib.Path, qualname: str, extra_rules: tuple[str, ...] = ()
) -> dict[str, str]:
    results = probe_function(
        workspace / "fixture_mod.py", qualname, ["test_fixture_mod.py"],
        module_name="fixture_mod", extra_rules=extra_rules,
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
        workspace, "scaled", extra_rules=("ablate_function_to_passthrough",))
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


def test_methods_record_their_receiver(workspace: pathlib.Path) -> None:
    """A method probe must bind `self`, or every replay raises and nothing is measured.

    The failure this pins is silent: an unbound recorder drops the receiver, every
    comparison is discarded as unusable, and the sweep returns a clean zero -- a
    verdict of "no difference" from a run that never executed the function.
    """
    (workspace / "fixture_mod.py").write_text(textwrap.dedent('''
        import torch


        class Lane:
            def __init__(self):
                self.gain = torch.tensor(3.0)

            def forward(self, x):
                return (x * self.gain).clamp_min(-1e30)
    '''))
    (workspace / "test_fixture_mod.py").write_text(textwrap.dedent('''
        import torch
        from fixture_mod import Lane


        def test_lane():
            assert Lane().forward(torch.ones(3)).sum() == 9.0
    '''))
    sys.modules.pop("fixture_mod", None)
    importlib.invalidate_caches()
    results = probe_function(
        workspace / "fixture_mod.py", "Lane.forward", ["test_fixture_mod.py"],
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

    tree = ast.parse(textwrap.dedent("""
        def unary(x):
            return x + 1


        def binary(x, y=2):
            return x + y


        def caller(v):
            return unary(v) + binary(v)
    """))
    fn = next(n for n in tree.body if getattr(n, "name", "") == "caller")
    dropped = [a.description for a in generate_ablations(fn, tree)
               if a.rule == "drop_unary_call"]
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
    assert len(targets) > 1, "fixture must have several targets for this to mean anything"

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
    """"Delete the whole function -- does anything miss it?" is the question the other
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
    import slop_core

    from conductor.equivalence_ablations import RULES as PYTHON_RULES

    native_default, _ = slop_core.rule_names()
    assert "ablate_function_to_none" in native_default
    assert len(native_default) > len(PYTHON_RULES)
    # and the probe is on the larger set, not merely able to reach it. Pinned on a
    # rule the Python engine never had, so a quiet revert of the import fails here
    # rather than silently shrinking the sweep back to 8 rules.
    assert "ablate_function_to_none" in _verdicts(workspace, "scaled")
