"""Tier-1 differential equivalence probe: is this code doing anything?

A surviving mutant is ambiguous. It means either "the tests cannot see this" or
"this code has no effect at all", and those want opposite fixes -- write a test, or
delete the code. Mutation testing cannot separate them, because its oracle IS the
test suite. This module separates them by comparing VALUES instead: it drives the
real function with the arguments the real tests already produce, then re-drives it
with one construct ablated, and asks whether the return value ever moves.

Three disciplines, each of which was learned by getting a verdict wrong by hand:

* Inputs from ``randn`` under-approximate reachability. A guard that only fires
  when a softmax saturates is invisible to ordinary inputs, so every recorded
  argument set is also replayed AMPLIFIED into extremal regimes.
* The observation point must be the caller's boundary -- the function's return --
  not the ablated line, because a difference can be introduced and then cancelled
  downstream by the very same function.
* Sampling never proves equivalence. A clean sweep is reported as
  NO_DIFFERENCE_OBSERVED and routed to a human, never as "equivalent".

Python is the right layer here: this is analysis tooling around pytest and the
import system, and the numerical work is torch reductions, not interpreter loops.
"""

from __future__ import annotations

import argparse
import ast
import copy
import dataclasses
import functools
import importlib
import inspect
import json
import pathlib
import sys
import time
from typing import Any, Callable, Sequence

from conductor.native_ablations import Ablation as NativeAblation
from conductor.probe_replay_stubs import replay_stub_overrides
from conductor.native_ablations import ablations as native_ablations

__all__ = ["Verdict", "AblationResult", "probe_function", "probe_module"]

MAX_RECORDED_CALLS = 24
# Wall clock ONE function's sweeps may spend before its remaining constructs are
# reported unmeasured instead of measured. Not a taste. Measured per function over
# five modules of this repo, 62 functions: 60 of them finish in under 1 s (p95 =
# 0.71 s), the slowest that finishes at all is `slop_gate.probe` at 11.0 s, and then
# the distribution jumps -- `equivalence_probe._record_many` 55.3 s,
# `equivalence_probe.probe_function` 93.3 s, `mutation_coverage.main` 221.9 s. Those
# are the functions whose replayed call runs a pytest suite, so one recorded call
# costs a whole test run, and they are why the modules holding them exceeded
# `slop_gate.PER_MODULE_TIMEOUT` and were killed -- throwing away the verdicts of
# every OTHER function in the module. `mutation_coverage` spent 222.5 s to report
# nothing about any of its twelve functions, eleven of which cost 0.5 s combined.
#
# 30 s is ~3x the slowest function that finishes, and NOTHING was measured between
# 11.0 s and 93.3 s, so no verdict this probe reports today can change under it. It
# reclaims 192 s of the 240 s that sample spent.
FUNCTION_BUDGET_SECONDS = 30.0
# Targets shared per driver run. Bounds recorder memory; see _record_many.
RECORD_BATCH = 32
AMPLIFIERS: tuple[tuple[str, float], ...] = (
    ("x1e3", 1e3),
    ("x1e-3", 1e-3),
    ("x1e6", 1e6),
    ("neg", -1.0),
    ("zero", 0.0),
)
# Scaling the INPUT is not enough to reach every branch. A lane whose router reads a
# `tanh` output is bounded however large its input grows, so a guard that only fires
# when that router saturates stays invisible. Reaching it means scaling the WEIGHTS,
# which is why parameter amplification is a separate lever rather than a larger factor.
# Repeats of the baseline-against-itself control. Three, because the blocking count
# over seven modules was 13 / 10 / 7 on identical code with a single control.
CONTROL_REPEATS = 3
# How far a reproduced effect must clear the function's own jitter before it blocks.
# Not a taste: over five repeats of the same seven modules the two findings that held
# 5/5 both measured control == 0.0, so they clear any margin, while the one that
# appeared 1/5 sat at 1.22e-6 against a control of 2.91e-7 -- a ratio of 4.2. A bar of
# 10 separates those two populations; it does not merely make the flake go away.
CONTROL_MARGIN = 10.0
PARAM_AMPLIFIERS: tuple[tuple[str, float], ...] = (
    ("params_x50", 50.0),
    ("params_x1e3", 1e3),
    ("params_zero", 0.0),
)


class Verdict(str):
    NOT_EXERCISED = "NOT_EXERCISED"
    LIVE = "LIVE"
    REACHABLE_BUT_UNTESTED = "REACHABLE_BUT_UNTESTED"
    WITHIN_NUMERIC_NOISE = "WITHIN_NUMERIC_NOISE"
    BASELINE_UNUSABLE = "BASELINE_UNUSABLE"
    NO_DIFFERENCE_OBSERVED = "NO_DIFFERENCE_OBSERVED"
    UNCOMPILABLE = "UNCOMPILABLE"
    # The function does not return the same value twice for the same input, so no
    # difference under it can be attributed to an ablation. A property of the code
    # under test, not of the probe -- reported, never silently folded into "clean".
    NONDETERMINISTIC = "NONDETERMINISTIC"
    # The function's sweeps used their whole wall-clock allowance, so this construct
    # was never ablated. A verdict about the probe, not about the code -- reported
    # so a clean module can be read against how much of it actually ran.
    OVER_BUDGET = "OVER_BUDGET"
    # The driver tests DID call the function, but none of the arguments they handed
    # it could be isolated from the baseline run, so replaying them would have let
    # the two ends share mutable state. Distinct from NOT_EXERCISED, which claims
    # the tests never reached the function at all -- reporting one as the other is
    # a clean verdict standing in for a measurement that could not be made.
    ARGUMENTS_UNCOPYABLE = "ARGUMENTS_UNCOPYABLE"


@dataclasses.dataclass
class AblationResult:
    qualname: str
    rule: str
    description: str
    lineno: int
    verdict: str
    calls_seen: int
    usable_calls: int = 0
    # Calls the driver tests made that were never recorded because their arguments
    # could not be isolated. Carried on every result so a clean sweep can be read
    # against how much of the function's real traffic it actually saw.
    dropped_calls: int = 0
    max_diff_recorded: float | None = None
    max_diff_amplified: float | None = None
    amplifier: str | None = None
    # Worst difference the baseline shows against ITSELF on the same inputs. Only
    # measured for a candidate finding, so it stays None on the clean path.
    max_diff_control: float | None = None
    detail: str = ""

    def as_dict(self) -> dict[str, Any]:
        return dataclasses.asdict(self)


# --------------------------------------------------------------------------- copy


class UncopyableValue(Exception):
    """A recorded argument that cannot be isolated from the run that produced it."""

    def __init__(self, value: Any, cause: BaseException) -> None:
        self.type_name = type(value).__qualname__
        self.cause = cause
        super().__init__(f"{self.type_name}: {cause}")


def _clone(value: Any) -> Any:
    """Deep copy that understands tensors, so in-place callees cannot poison replay.

    Raises rather than handing back the original on failure. Returning the original
    is the same object on both ends of the comparison: an in-place callee mutates it
    during the baseline run, the variant then runs against the already-mutated value,
    the difference disappears, and the construct reports NO_DIFFERENCE_OBSERVED --
    a clean verdict produced by the probe's own aliasing rather than by the code.
    An argument that cannot be isolated is evidence that cannot be collected.
    """
    torch = sys.modules.get("torch")
    if torch is not None and torch.is_tensor(value):
        return value.detach().clone()
    if isinstance(value, (list, tuple)):
        return type(value)(_clone(v) for v in value)
    if isinstance(value, dict):
        return {k: _clone(v) for k, v in value.items()}
    try:
        return copy.deepcopy(value)
    except UncopyableValue:
        raise
    except Exception as exc:  # noqa: BLE001 - any failure to isolate is the same fact
        raise UncopyableValue(value, exc) from exc


def _magnitude(value: Any) -> float:
    """Scale of a return value, for turning absolute differences into relative ones."""
    torch = sys.modules.get("torch")
    if torch is not None and torch.is_tensor(value):
        return float(value.detach().abs().max()) if value.numel() else 0.0
    if isinstance(value, (list, tuple)):
        return max((_magnitude(v) for v in value), default=0.0)
    if isinstance(value, dict):
        return max((_magnitude(v) for v in value.values()), default=0.0)
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return abs(float(value))
    return 0.0


def _difference(a: Any, b: Any) -> float:
    """Largest observable disagreement between two return values; inf if structural."""
    torch = sys.modules.get("torch")
    if torch is not None and torch.is_tensor(a) and torch.is_tensor(b):
        if a.shape != b.shape or a.dtype != b.dtype:
            return float("inf")
        if a.is_floating_point():
            d = (a.detach().double() - b.detach().double()).abs()
            return float(d.max()) if d.numel() else 0.0
        return 0.0 if bool(torch.equal(a, b)) else float("inf")
    if isinstance(a, (list, tuple)) and isinstance(b, (list, tuple)):
        if len(a) != len(b):
            return float("inf")
        return max((_difference(x, y) for x, y in zip(a, b)), default=0.0)
    if isinstance(a, dict) and isinstance(b, dict):
        if a.keys() != b.keys():
            return float("inf")
        return max((_difference(a[k], b[k]) for k in a), default=0.0)
    if isinstance(a, (int, float)) and isinstance(b, (int, float)):
        return abs(float(a) - float(b))
    try:
        return 0.0 if a == b else float("inf")
    except Exception:
        return float("inf")


def _amplify(value: Any, factor: float) -> Any:
    torch = sys.modules.get("torch")
    if torch is not None and torch.is_tensor(value) and value.is_floating_point():
        return value * factor
    if isinstance(value, (list, tuple)):
        return type(value)(_amplify(v, factor) for v in value)
    return value


def _amplify_parameters(value: Any, factor: float) -> Any:
    """Scale a module's float parameters, leaving inputs alone.

    Returns the value unchanged when it is not a module, so a caller can apply this
    blindly across an argument tuple.
    """
    torch = sys.modules.get("torch")
    if torch is None or not isinstance(value, torch.nn.Module):
        return value
    clone = copy.deepcopy(value)
    with torch.no_grad():
        for param in clone.parameters():
            if param.is_floating_point():
                param.mul_(factor)
    return clone


# ------------------------------------------------------------------- compilation


def _node_for_qualname(tree: ast.AST, qualname: str, where: object) -> ast.AST:
    """The function node a dotted ``Class.method`` or ``func`` names, in ``tree``."""
    scope: Any = tree
    node: Any = None
    for part in qualname.split("."):
        node = next(
            (
                n
                for n in ast.iter_child_nodes(scope)
                if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))
                and n.name == part
            ),
            None,
        )
        if node is None:
            raise LookupError(f"{qualname!r} not found in {where}")
        scope = node
    if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
        raise LookupError(f"{qualname!r} is not a function")
    return node


def _load_function_ast(path: pathlib.Path, qualname: str) -> tuple[ast.AST, ast.AST]:
    """Return (module_tree, function_node) for a dotted ``Class.method`` or ``func``."""
    tree = ast.parse(path.read_text())
    return tree, _node_for_qualname(tree, qualname, path)


def _compile_variant(
    fn_ast: ast.AST, module: Any, name: str, decorators: bool = False
) -> Callable[..., Any]:
    """Compile one function definition against the real module globals.

    Decorators are dropped by default, and deliberately: re-evaluating `@app.route`
    or `@register` once per ablation would re-run its side effect against the live
    module. The exception is the rule that ablates the decorator itself, which cannot
    measure anything if both ends are compiled without it -- there, BOTH ends are
    compiled with decorators so the comparison stays fair.
    """
    stripped = copy.deepcopy(fn_ast)
    if not decorators:
        stripped.decorator_list = []
    holder = ast.Module(body=[stripped], type_ignores=[])
    ast.fix_missing_locations(holder)
    namespace: dict[str, Any] = {}
    code = compile(holder, getattr(module, "__file__", "<ablation>"), "exec")
    # The ablated function has to close over the module's OWN globals -- its
    # callees, constants and decorators are resolved there -- so a sandboxed
    # namespace would change the thing under measurement. The source executed
    # here is this repository's own file, re-emitted from its parsed AST; no
    # caller-supplied text reaches it.
    # nosemgrep: python-dangerous-eval-exec
    exec(code, module.__dict__, namespace)  # noqa: S102  # nosec B102
    return namespace[name]


# ---------------------------------------------------------------------- recording


@dataclasses.dataclass
class Recording:
    """What one driver run saw of a function: the calls kept, and the calls lost.

    Two lists rather than one, because a dropped call is not a call that did not
    happen. Folding them together is what let a function whose every argument is
    uncopyable report NOT_EXERCISED -- indistinguishable from a function the tests
    never touch, and repaired by a different change entirely.
    """

    calls: list[tuple[tuple, dict]] = dataclasses.field(default_factory=list)
    # One entry per dropped call, naming the type that could not be isolated.
    dropped: list[str] = dataclasses.field(default_factory=list)


def _make_recorder(
    target: Callable[..., Any], limit: int = MAX_RECORDED_CALLS
) -> tuple[Callable[..., Any], Recording]:
    """Wrap ``target`` in a real function that keeps the arguments the tests hand it.

    It has to be a function, not a callable object: a method is reached through the
    descriptor protocol, and only a function binds ``self`` into the call. A callable
    instance silently drops the receiver, every replay then raises, and every ablation
    reports "no difference" -- a clean sweep that measured nothing.
    """
    recording = Recording()

    @functools.wraps(target)
    def recorder(*args: Any, **kwargs: Any) -> Any:
        if len(recording.calls) + len(recording.dropped) < limit:
            try:
                recording.calls.append((_clone(args), _clone(kwargs)))
            except UncopyableValue as exc:
                # Recorded as a loss, never as silence. An argument that cannot be
                # isolated here would alias across the two ends of every replay, so
                # the call is refused rather than kept -- and saying so is what
                # separates "the tests never called this" from "we could not keep
                # what they called it with".
                recording.dropped.append(exc.type_name)
            except Exception as exc:  # noqa: BLE001 - cloning must not break the suite
                recording.dropped.append(type(exc).__qualname__)
        return target(*args, **kwargs)

    recorder.__equivalence_recorder__ = True  # type: ignore[attr-defined]
    return recorder, recording


def _bind_sites(module: Any, qualname: str) -> list[tuple[Any, str]]:
    """Every (owner, attribute) pair through which the target is reachable."""
    parts = qualname.split(".")
    owner: Any = module
    for part in parts[:-1]:
        owner = getattr(owner, part)
    sites = [(owner, parts[-1])]
    if owner is module:  # module-level function: catch `from mod import fn` aliases
        original = getattr(module, parts[-1])
        for other in list(sys.modules.values()):
            if other is None or other is module:
                continue
            # Guarded: a module may define __getattr__ and answer for ANY name.
            # torch._classes returns a lazy namespace for every attribute, which then
            # raises RuntimeError -- not AttributeError -- on the next access, so a
            # getattr default is not enough to make this safe.
            try:
                bound = getattr(other, parts[-1], None)
                # A recorder from an EARLIER probe of the same function also counts as
                # an alias. Without this a second probe in the same process patches
                # only the defining module, the test module keeps calling the stale
                # recorder, and the new one records nothing -- reported as
                # NOT_EXERCISED, a clean verdict from a measurement that never ran.
                alias = bound is original or getattr(
                    bound, "__equivalence_recorder__", False
                )
            except Exception:  # noqa: BLE001 - a hostile __getattr__ is not our problem
                continue
            if alias:
                sites.append((other, parts[-1]))
    return sites


def _install_recorder(
    module: Any, qualname: str
) -> tuple[list[tuple], Recording] | None:
    """Patch every site `qualname` is reachable through. Returns (restores, recording)."""
    try:
        sites = _bind_sites(module, qualname)
    except (AttributeError, LookupError, RuntimeError):
        return None
    owner, attr = sites[0]
    original = inspect.getattr_static(owner, attr)
    original = (
        original.__func__
        if isinstance(original, staticmethod)
        else getattr(owner, attr)
    )
    recorder, recording = _make_recorder(original)
    restores = []
    for site_owner, site_attr in sites:
        restores.append(
            (site_owner, site_attr, getattr(site_owner, site_attr, original))
        )
        setattr(site_owner, site_attr, recorder)
    return restores, recording


def _record_calls(module: Any, qualname: str, test_args: Sequence[str]) -> Recording:
    """Record one function's real arguments by running its driver tests."""
    return _record_many(module, [qualname], test_args).get(qualname, Recording())


def _record_many(
    module: Any,
    qualnames: Sequence[str],
    test_args: Sequence[str],
    batch: int = RECORD_BATCH,
) -> dict[str, Recording]:
    """Record every target's real arguments, sharing driver runs between them.

    The driver suite does not care which function is being watched, so running it
    once per function was pure repetition -- measured at a mean of 11.0 public
    functions per module across this repo, worst case 174.

    Batched rather than all-at-once because a recorder holds cloned tensor
    arguments: 174 targets x MAX_RECORDED_CALLS is gigabytes. Exceeding a batch
    costs one more driver run, never a wrong verdict, which is the only tradeoff
    worth making here -- a shared budget that silently stopped recording would
    report NOT_EXERCISED for a function whose tests do drive it.
    """
    import pytest

    out: dict[str, Recording] = {}
    for start in range(0, len(qualnames), batch):
        chunk = qualnames[start : start + batch]
        restores: list[tuple] = []
        pending: dict[str, Recording] = {}
        for qualname in chunk:
            installed = _install_recorder(module, qualname)
            if installed is None:
                continue
            site_restores, recording = installed
            restores.extend(site_restores)
            pending[qualname] = recording
        if not pending:
            continue
        try:
            pytest.main(["-q", "-p", "no:cacheprovider", "-o", "addopts=", *test_args])
        finally:
            # Reversed: two targets can share a site, and the last write must be
            # undone first for the original to come back.
            for owner, attr, original in reversed(restores):
                setattr(owner, attr, original)
        out.update(pending)
    return out


# ------------------------------------------------------------------------ probing


def _compare_one(
    baseline: Callable[..., Any],
    variant: Callable[..., Any],
    args: tuple,
    kwargs: dict,
) -> float | None:
    """Relative difference for one argument set, or None when the input is unusable.

    Relative, because an absolute threshold of zero reports a float32 round-off as a
    real effect -- which is exactly how a hand analysis of this project's own VSA code
    bank first went wrong.
    """
    raised_baseline = raised_variant = None
    expected = actual = None
    # Both ends get their own copy, made BEFORE either runs. Cloning inside the
    # try below would let a failure to isolate be caught as though the call itself
    # raised -- one side "raising" and the other not reads as an infinite
    # difference, so an argument the probe cannot copy would report every construct
    # around it LIVE. It is the absence of evidence, so it returns None.
    try:
        baseline_args, baseline_kwargs = _clone(args), _clone(kwargs)
        variant_args, variant_kwargs = _clone(args), _clone(kwargs)
    except UncopyableValue:
        return None
    # SystemExit is a BaseException, so `except Exception` used to let it past both
    # handlers and out of the probe process entirely. A recorded call to a module's
    # argparse `main()` is replayed outside the test that faked `sys.argv`, argparse
    # calls `parser.error()`, and one such function cost the whole module its
    # measurement -- 148.7 s spent to report nothing for `uncurated_kill_rate.py`.
    # Exiting is a behaviour of the call like any other raise; measure it as one.
    try:
        expected = baseline(*baseline_args, **baseline_kwargs)
    except (Exception, SystemExit) as exc:  # noqa: BLE001 - the outcome IS the measurement
        raised_baseline = exc
    try:
        actual = variant(*variant_args, **variant_kwargs)
    except (Exception, SystemExit) as exc:  # noqa: BLE001
        raised_variant = exc

    if raised_baseline is not None or raised_variant is not None:
        # Raising is a behaviour, not a failure to measure. A validation guard exists
        # precisely so the call raises; discarding those inputs made every guard look
        # inert because the only input that reaches it was thrown away.
        if raised_baseline is not None and raised_variant is not None:
            if type(raised_baseline) is type(raised_variant):
                # Both ends broke identically. That is what a malformed replay looks
                # like too, so it stays inconclusive and cannot be read as agreement.
                return None
            return float("inf")
        return float("inf")

    absolute = _difference(expected, actual)
    if absolute in (0.0, float("inf")):
        return absolute
    return absolute / max(_magnitude(expected), 1e-30)


class _Budget:
    """Wall clock a function's sweeps may spend, and whether they ran out of it.

    Held as an object rather than a deadline float because the caller needs the
    second half: a sweep that stopped early returns the same `(0.0, n)` shape as one
    that swept everything and found nothing, and those two must never be reported as
    the same thing. `cut` is what separates "measured, no difference" from "never
    sampled". Once tripped it stays tripped, so a function reports one verdict about
    its budget rather than alternating as the clock is re-read.
    """

    __slots__ = ("clock", "deadline", "cut")

    def __init__(
        self, seconds: float | None, clock: Callable[[], float] = time.monotonic
    ) -> None:
        # The clock is a parameter because it is this object's only input, and a test
        # that cannot set it has to race a real one to reach the boundaries that
        # matter -- a sweep cut before its first call, or between two ablations.
        self.clock = clock
        self.deadline = None if seconds is None or seconds <= 0 else clock() + seconds
        self.cut = False

    def expired(self) -> bool:
        if not self.cut and self.deadline is not None:
            self.cut = self.clock() >= self.deadline
        return self.cut


def _sweep(
    baseline: Callable,
    variant: Callable,
    calls: Sequence[tuple],
    budget: _Budget | None = None,
    settle_above: float | None = None,
) -> tuple[float, int, bool]:
    """Worst relative difference, how many argument sets the baseline accepted, and
    whether the sweep stopped early because the answer was already decided.

    The budget is checked BETWEEN calls, never inside one: a replay that has started
    is either allowed to finish or is killed mid-call, and killing it would leave the
    module under test in whatever state the ablated function got it to.

    `settle_above` is the caller's noise floor, and passing it says the caller wants a
    BOOLEAN -- "did anything exceed this?" -- not a maximum. The first call that
    exceeds it settles that question, and every later call is a second answer to a
    question already answered. Only the recorded sweep may pass it: the amplified
    sweep's magnitude is compared against the function's own jitter, so there a
    truncated maximum would understate the effect and turn a real finding into
    NONDETERMINISTIC. Measured on `conductor/equivalence_probe.py`, whose 144
    ablations replay 24 recorded calls apiece.
    """
    worst, usable = 0.0, 0
    for args, kwargs in calls:
        if budget is not None and budget.expired():
            break
        diff = _compare_one(baseline, variant, args, kwargs)
        if diff is not None:
            worst = max(worst, diff)
            usable += 1
            if settle_above is not None and worst > settle_above:
                return worst, usable, True
    return worst, usable, False


def _unamplified(original: Any, transformed: Any) -> bool:
    """Did the amplifier leave this argument alone?

    Mirrors the only two paths in `_amplify`/`_amplify_parameters` that build a new
    object: a float tensor becomes a scaled tensor, a module becomes a scaled
    deepcopy, and a list or tuple is rebuilt around whatever its elements became.
    Everything else is returned by identity, so `is` decides it.
    """
    if original is transformed:
        return True
    if isinstance(original, (list, tuple)) and type(original) is type(transformed):
        return len(original) == len(transformed) and all(
            _unamplified(a, b) for a, b in zip(original, transformed)
        )
    return False


def _sweep_amplified(
    baseline: Callable,
    variant: Callable,
    calls: Sequence[tuple],
    budget: _Budget | None = None,
) -> tuple[float, str | None]:
    """Worst relative difference over the amplified regimes, and which one found it.

    A plan that cannot touch a single argument is skipped, and that is exact rather
    than heuristic: an amplifier which returns every argument by identity replays the
    call `_sweep` already made, and this function is only reached when that sweep came
    back within noise. Such a plan can contribute nothing but a re-sample of a
    quantity already measured -- and a re-sample landing above noise is jitter by
    construction, which is the false candidate the CONTROL_REPEATS path below then
    pays to reject. Measured over four modules: 613 findings before and after, with 3
    NONDETERMINISTIC verdicts on `run_targeted_tests` becoming NO_DIFFERENCE_OBSERVED
    because the wall-clock field they were reading no longer got re-sampled.

    It is also where the gate's time went. Nothing in `conductor/` takes a tensor or a
    module, so all eight plans were byte-identical repeats of a function that shells
    out to git: `project_init` alone spent 176 s of a 352 s review doing that, and is
    now 31.8 s against 180.9 s. A probe with a real tensor or module argument still
    runs every plan that changes it.
    """
    worst, which = 0.0, None
    plans: list[tuple[str, Callable[[Any], Any]]] = [
        (label, lambda v, f=factor: _amplify(v, f)) for label, factor in AMPLIFIERS
    ]
    plans += [
        (label, lambda v, f=factor: _amplify_parameters(v, f))
        for label, factor in PARAM_AMPLIFIERS
    ]
    for label, transform in plans:
        if budget is not None and budget.expired():
            break
        for args, kwargs in calls:
            if budget is not None and budget.expired():
                break
            amp_args = tuple(transform(a) for a in args)
            amp_kwargs = {k: transform(v) for k, v in kwargs.items()}
            if all(_unamplified(a, b) for a, b in zip(args, amp_args)) and all(
                _unamplified(v, amp_kwargs[k]) for k, v in kwargs.items()
            ):
                continue
            diff = _compare_one(baseline, variant, amp_args, amp_kwargs)
            if diff is not None and diff > worst:
                worst, which = diff, label
    return worst, which


@dataclasses.dataclass
class _ProbeContext:
    """Everything one construct's ablation needs that is fixed for the function.

    A record rather than ten parameters: the ablation is the only thing that varies
    across the loop below, and threading the rest through by hand is how a caller
    ends up passing the unmodified baseline where the decorated one belongs.
    """

    qualname: str
    source: str
    fn_ast: ast.AST
    module: Any
    module_path: pathlib.Path
    baseline: Callable
    calls: list[tuple]
    # Calls the driver made that could not be recorded. Held beside `calls` so a
    # construct with nothing to sweep can say which of the two reasons it is.
    dropped: tuple[str, ...]
    noise: float
    budget: "_Budget"
    budget_seconds: float | None


def _probe_construct(ablation: NativeAblation, ctx: _ProbeContext) -> AblationResult:
    """Ablate one construct and classify what the recorded calls say about it."""
    base = AblationResult(
        ctx.qualname,
        ablation.rule,
        ablation.description,
        ablation.line,
        Verdict.NOT_EXERCISED,
        len(ctx.calls),
        dropped_calls=len(ctx.dropped),
    )
    if not ctx.calls:
        if ctx.dropped:
            # The tests DID drive this function. Every argument they handed it was
            # refused because replaying it would have aliased mutable state across
            # the two ends, so there is no evidence here either way -- which is a
            # different claim from "nothing calls this", and is reported as one.
            base.verdict = Verdict.ARGUMENTS_UNCOPYABLE
            names = sorted(set(ctx.dropped))
            base.detail = (
                f"{len(ctx.dropped)} recorded call(s) dropped: their arguments "
                f"({', '.join(names[:5])}) cannot be isolated from the run that "
                "produced them, so a replay would share mutable state with the "
                "baseline; nothing was measured"
            )
        return base
    if ctx.budget.expired():
        # Cheap guard, not the correctness one: `_sweep` below would break on its
        # first check anyway and reach the same verdict. What this saves is
        # compiling a variant for every remaining construct in order to sweep it
        # zero times -- 35 needless compiles on `mutation_coverage.main` alone.
        _over_budget(base, ctx.budget_seconds)
        return base
    try:
        # The engine edits module source, so the variant is read back out of the
        # mutated module rather than handed over as a tree. Rules like
        # ablate_function_to_none and drop_decorator rewrite the definition
        # itself, which no function-scoped AST swap can express.
        mutated = _node_for_qualname(
            ast.parse(ablation.apply(ctx.source)), ctx.qualname, ctx.module_path
        )
        # For the decorator rule the decorator IS the construct under test, so
        # both ends have to carry it -- otherwise the ablation and the baseline
        # compile to the same object and every decorator reads as decorative.
        decorated = ablation.rule == "drop_decorator"
        against = (
            _compile_variant(ctx.fn_ast, ctx.module, ctx.fn_ast.name, decorators=True)
            if decorated
            else ctx.baseline
        )
        variant = _compile_variant(
            mutated, ctx.module, ctx.fn_ast.name, decorators=decorated
        )
    except (SyntaxError, LookupError) as exc:
        base.verdict, base.detail = Verdict.UNCOMPILABLE, str(exc)
        return base
    recorded, usable, settled = _sweep(
        against, variant, ctx.calls, ctx.budget, settle_above=ctx.noise
    )
    base.max_diff_recorded = recorded
    base.usable_calls = usable
    if ctx.budget.cut:
        # Ordered before the `usable` check on purpose: a sweep cut before its
        # first call also reports zero usable calls, and BASELINE_UNUSABLE is a
        # claim about the recorded arguments, which nothing here measured.
        _over_budget(base, ctx.budget_seconds)
        return base
    if not usable:
        base.verdict = Verdict.BASELINE_UNUSABLE
        base.detail = "the recorded arguments never drove the unmodified function"
        return base
    if recorded > ctx.noise:
        base.verdict = Verdict.LIVE
        if settled:
            # Say it, because `usable_calls` now counts the calls SWEPT rather than
            # the calls the baseline accepted, and `max_diff_recorded` is the first
            # difference past the floor rather than the largest one. Both readings
            # are what a reader assumes unless told otherwise -- the same misreading
            # OVER_BUDGET carries, and the reason that verdict clears its numbers.
            base.detail = (
                f"stopped after {usable} of {len(ctx.calls)} recorded calls once the "
                f"difference passed the {ctx.noise:g} noise floor; the construct is "
                "live either way, so max_diff_recorded is a lower bound"
            )
        return base
    amplified, which = _sweep_amplified(against, variant, ctx.calls, ctx.budget)
    base.max_diff_amplified, base.amplifier = amplified, which
    if ctx.budget.cut:
        _over_budget(base, ctx.budget_seconds)
        return base
    if amplified > ctx.noise:
        if _adjudicate_amplified(base, against, variant, ctx.calls, ctx.noise):
            return base
    elif max(recorded, amplified) > 0.0:
        base.verdict = Verdict.WITHIN_NUMERIC_NOISE
    else:
        base.verdict = Verdict.NO_DIFFERENCE_OBSERVED
    return base


def probe_function(
    module_path: pathlib.Path,
    qualname: str,
    test_args: Sequence[str],
    module_name: str | None = None,
    noise: float = 1e-6,
    extra_rules: Sequence[str] = (),
    calls: list[tuple] | None = None,
    dropped: Sequence[str] = (),
    ablations: Sequence[NativeAblation] | None = None,
    source: str | None = None,
    budget_seconds: float | None = None,
) -> list[AblationResult]:
    """Ablate every construct in one function and classify each by differential value.

    `budget_seconds` bounds the wall clock this ONE function's sweeps may spend;
    whatever is left unswept is reported OVER_BUDGET rather than clean. It defaults to
    unbounded so a direct call measures everything it is asked to measure -- the
    budget belongs to a sweep of a whole module, where an unbounded function costs the
    other functions their verdicts. `probe_module` supplies it.
    """
    module_name = module_name or _module_name_for(module_path)
    module = importlib.import_module(module_name)
    if source is None:
        source = module_path.read_text()
    if ablations is None:
        ablations = [
            a
            for a in native_ablations(source, extra=list(extra_rules))
            if a.qualname == qualname
        ]
    fn_ast = _node_for_qualname(ast.parse(source), qualname, module_path)
    if not ablations:
        return []

    if calls is None:
        recording = _record_calls(module, qualname, test_args)
        calls, dropped = recording.calls, tuple(recording.dropped)
    ctx = _ProbeContext(
        qualname=qualname,
        source=source,
        fn_ast=fn_ast,
        module=module,
        module_path=module_path,
        baseline=_compile_variant(fn_ast, module, fn_ast.name),
        calls=calls,
        dropped=tuple(dropped),
        noise=noise,
        # One budget for the whole function: the point is to bound what this
        # function's sweeps cost the module, not what any single construct costs.
        budget=_Budget(budget_seconds),
        budget_seconds=budget_seconds,
    )
    results = []
    with replay_stub_overrides(module):
        for ablation in ablations:
            results.append(_probe_construct(ablation, ctx))
    _pool_jitter_floor(results)
    return results


def _over_budget(base: AblationResult, seconds: float | None) -> None:
    """Mark one construct as never sampled, and say what it would cost to sample it."""
    base.verdict = Verdict.OVER_BUDGET
    base.detail = (
        f"this function used its whole {seconds:g}s sweep budget before reaching "
        "this construct, which was never ablated; re-run "
        "`python -m conductor.equivalence_probe <module> <tests> "
        "--budget-seconds 0` to sweep it without a bound"
    )
    # A partial sweep measured no difference because it did not look, not because
    # there is none. Carrying the numbers would invite exactly that misreading.
    base.max_diff_recorded = None
    base.max_diff_amplified = None
    base.amplifier = None


def _adjudicate_amplified(
    base: AblationResult,
    against: Any,
    variant: Any,
    calls: list[tuple],
    noise: float,
) -> bool:
    """Settle a candidate finding against the function's own instability.

    Returns True when the difference is not attributable to the ablation, in
    which case ``base`` already carries the NONDETERMINISTIC verdict.
    """
    # This verdict is the one that blocks a merge, so it has to be ATTRIBUTABLE to
    # the ablation. Measured on the first real sweep: 11 of 16 structural findings
    # in research/eval differed only in an `elapsed_ms` wall-clock field, which
    # changes between any two calls. The amplified sweep scored that jitter as an
    # infinite relative change because the `==` fallback in _difference is
    # all-or-nothing and never meets a noise threshold. Re-running the baseline
    # against ITSELF separates instability from effect; it costs one extra sweep
    # and is paid only when a finding is about to be reported.
    # The screening sweep is a SCREEN, not a verdict: it is one sample of a
    # quantity that varies between runs whenever the function under test does.
    # Repeating the same seven modules gave blocking counts of 13 / 10 / 7 on
    # unchanged code. So a candidate has to clear two bars:
    #   * REPRODUCE -- the smallest effect seen over several repeats, not a lucky
    #     largest one;
    #   * EXCEED THE FUNCTION'S OWN JITTER -- the largest difference the unmodified
    #     function shows against ITSELF over the same repeats.
    # Both are paid only here, on a candidate finding, so the clean path keeps its
    # single sweep.
    # Deliberately NOT budgeted: this path is reached only for a candidate that is
    # about to block a merge, and a truncated control would understate the function's
    # jitter -- which is the one direction that turns noise into a blocking finding.
    # Six full sweeps is the price of reporting REACHABLE_BUT_UNTESTED at all.
    effect = min(
        _sweep_amplified(against, variant, calls)[0] for _ in range(CONTROL_REPEATS)
    )
    control = max(
        _sweep_amplified(against, against, calls)[0] for _ in range(CONTROL_REPEATS)
    )
    base.max_diff_amplified = effect
    base.max_diff_control = control
    if effect <= max(noise, control * CONTROL_MARGIN):
        base.verdict = Verdict.NONDETERMINISTIC
        base.detail = (
            "the unmodified function disagrees with itself by as much as the "
            "ablation does, so the difference is not attributable to it"
        )
        return True
    base.verdict = Verdict.REACHABLE_BUT_UNTESTED
    return False


def _pool_jitter_floor(results: list[AblationResult]) -> None:
    """Hold every ablation of one function to the worst jitter any of them saw."""
    # Jitter belongs to the FUNCTION, not to one ablation. Three control repeats is
    # still a small sample, and _run_binding_intermediate_on -- unstable enough to be
    # caught 39 times over five runs -- cleared its own control by luck in 1 run of 6.
    # Every ablation of this function measured the same underlying instability, so
    # pool them: the worst jitter any of them saw is the floor all of them must clear.
    # Costs nothing; the controls are already measured.
    floor = max((r.max_diff_control or 0.0) for r in results) if results else 0.0
    if floor <= 0.0:
        return
    for r in results:
        if (
            r.verdict == Verdict.REACHABLE_BUT_UNTESTED
            and (r.max_diff_amplified or 0.0) <= floor * CONTROL_MARGIN
        ):
            r.verdict = Verdict.NONDETERMINISTIC
            r.detail = (
                "another ablation of this function measured the unmodified code "
                f"disagreeing with itself by {floor:.3e}, which this difference "
                "does not clear"
            )


def _module_name_for(path: pathlib.Path) -> str:
    rel = path.resolve().relative_to(pathlib.Path.cwd().resolve())
    return str(rel.with_suffix("")).replace("/", ".")


def public_functions(module_path: pathlib.Path) -> list[str]:
    """Qualnames worth probing: module-level functions and public methods."""
    tree = ast.parse(module_path.read_text())
    names: list[str] = []
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            names.append(node.name)
        elif isinstance(node, ast.ClassDef):
            names += [
                f"{node.name}.{m.name}"
                for m in node.body
                if isinstance(m, (ast.FunctionDef, ast.AsyncFunctionDef))
                and not m.name.startswith("__")
            ]
    return names


def probe_module(
    module_path: pathlib.Path,
    test_args: Sequence[str],
    only: Sequence[str] = (),
    noise: float = 1e-6,
    extra_rules: Sequence[str] = (),
    budget_seconds: float | None = FUNCTION_BUDGET_SECONDS,
) -> list[AblationResult]:
    """Probe every public function of one module, each under its own sweep budget.

    The budget is PER FUNCTION, not per module, so a verdict never depends on which
    functions were probed before it. A module-wide clock would make the same construct
    read LIVE or OVER_BUDGET according to its position in the file, which is not a
    property of the code.
    """
    targets = list(only) or public_functions(module_path)
    if not targets:
        return []
    module_name = _module_name_for(module_path)
    module = importlib.import_module(module_name)
    # One driver run for the whole module rather than one per function.
    recorded = _record_many(module, targets, test_args)
    # One parse for the whole module too: the engine walks the tree once and returns
    # every function's ablations, so asking per function would reparse it per target.
    source = module_path.read_text()
    by_qualname: dict[str, list[NativeAblation]] = {}
    for ablation in native_ablations(source, extra=list(extra_rules)):
        by_qualname.setdefault(ablation.qualname, []).append(ablation)
    out: list[AblationResult] = []
    for qualname in targets:
        try:
            out += probe_function(
                module_path,
                qualname,
                test_args,
                module_name=module_name,
                noise=noise,
                extra_rules=extra_rules,
                calls=recorded.get(qualname, Recording()).calls,
                dropped=tuple(recorded.get(qualname, Recording()).dropped),
                ablations=by_qualname.get(qualname, []),
                source=source,
                budget_seconds=budget_seconds,
            )
        except LookupError:
            continue
    return out


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="conductor.equivalence_probe")
    parser.add_argument("module", type=pathlib.Path)
    parser.add_argument("tests", nargs="+", help="pytest targets that drive the module")
    parser.add_argument("--function", action="append", default=[])
    parser.add_argument(
        "--rule",
        action="append",
        default=[],
        dest="extra_rules",
        help="enable an opt-in rule, e.g. ablate_function_to_passthrough",
    )
    parser.add_argument(
        "--noise",
        type=float,
        default=1e-6,
        help="relative difference below which a change is round-off",
    )
    parser.add_argument(
        "--budget-seconds",
        type=float,
        default=FUNCTION_BUDGET_SECONDS,
        help="wall clock each function's sweeps may spend before the rest of its "
        "constructs are reported OVER_BUDGET; 0 sweeps every function to the end",
    )
    parser.add_argument("--json", type=pathlib.Path)
    parser.add_argument(
        "--fail-on", default="", help="comma-separated verdicts to fail on"
    )
    args = parser.parse_args(argv)

    results = probe_module(
        args.module,
        args.tests,
        args.function,
        args.noise,
        args.extra_rules,
        args.budget_seconds,
    )
    if args.json:
        args.json.write_text(
            json.dumps([r.as_dict() for r in results], indent=2) + "\n"
        )
    counts: dict[str, int] = {}
    for r in results:
        counts[r.verdict] = counts.get(r.verdict, 0) + 1
    for r in results:
        if r.verdict == Verdict.LIVE:
            continue
        amp = (
            f" amplified={r.max_diff_amplified:.3e} via {r.amplifier}"
            if r.amplifier
            else ""
        )
        print(f"{r.verdict:24s} {r.qualname}:{r.lineno} {r.description}{amp}")
    print(json.dumps(counts, sort_keys=True))
    fail_on = {v.strip() for v in args.fail_on.split(",") if v.strip()}
    return 1 if fail_on & {r.verdict for r in results} else 0


if __name__ == "__main__":
    raise SystemExit(main())
