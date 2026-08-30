"""Contract tests for the native ablation engine's Python surface.

Rule behaviour is tested in Rust (`cargo test -p slop-core`, 23 cases) where the
property that every ablation re-parses is checked directly. These cover the boundary:
marshalling, the fail-loud import, and the guarantees the probe depends on.
"""

import pytest

# exc_type is required, not cosmetic: the module imports fine and *raises*
# ImportError when the extension is missing. Older pytest skipped on that with a
# deprecation warning; newer pytest turns it into a collection ERROR, which is how
# this failed on CI while passing locally.
na = pytest.importorskip(
    "conductor.native_ablations",
    reason="slop_core not built; run `make -C research/runtime/native slop-core`",
    exc_type=ImportError,
)

SAMPLE = '''
import os  # noqa: F401


def helper(v, scale=1.0):
    if v is None:
        raise ValueError("v is required")
    return v * scale


def caller(x, flag=True):
    y = helper(x)
    return y.clamp(min=0) if flag else y
'''


def test_rules_split_into_default_and_measured_optout():
    default, optional = na.rule_names()
    assert len(default) > 15
    # These were measured and never discriminated; they must not be on by default.
    assert set(optional) >= {"drop_contiguous", "ablate_function_to_passthrough"}
    assert not set(default) & set(optional)


def test_every_ablation_compiles():
    # The whole framework rests on this: a mutant that does not compile is scored as
    # "the code mattered" when in fact the engine emitted garbage.
    for a in na.ablations(SAMPLE):
        compile(a.apply(SAMPLE), "<mutant>", "exec")


def test_ablation_changes_the_source_it_names():
    for a in na.ablations(SAMPLE):
        assert a.apply(SAMPLE) != SAMPLE, f"{a.rule} at line {a.line} changed nothing"
        assert a.line >= 1


def test_unknown_rule_is_refused_not_ignored():
    # Silently probing less than the caller asked for reports a clean sweep that
    # never ran.
    with pytest.raises(KeyError, match="nonexistent_rule"):
        na.ablations(SAMPLE, rules=["nonexistent_rule"])
    with pytest.raises(KeyError):
        na.ablations(SAMPLE, extra=["also_not_a_rule"])


def test_optional_rules_are_off_until_asked_for():
    base = {a.rule for a in na.ablations(SAMPLE)}
    assert "ablate_function_to_passthrough" not in base
    widened = {a.rule for a in na.ablations(SAMPLE, extra=["ablate_function_to_passthrough"])}
    assert "ablate_function_to_passthrough" in widened


def test_qualname_and_new_rule_families_are_reachable():
    found = na.ablations(SAMPLE)
    rules = {a.rule for a in found}
    # The four families this stage added, each on the sample above.
    assert "drop_import" in rules
    assert "pin_parameter_to_default" in rules
    assert "flip_boolean_default" in rules
    assert "ablate_function_to_none" in rules
    assert any(a.qualname == "helper" for a in found)


def test_multi_site_edits_apply_together():
    src = "def f(x, scale=2.0):\n    a = x * scale\n    return a + scale\n"
    pins = [a for a in na.ablations(src) if a.rule == "pin_parameter_to_default"]
    assert len(pins) == 1 and len(pins[0].edits) == 2
    out = pins[0].apply(src)
    assert "x * 2.0" in out and "a + 2.0" in out
