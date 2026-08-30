"""Per-finding diff attribution: a candidate blocks on what it caused.

A PR adding one new module was blocked by an unused variable in a file it never
opened and a note file duplicating `http_transport.py`. Compliance was unachievable
by doing your own work well, so agents routed around the gate -- force-push, then a
13-branch fan-out, then a three-day merge to get back to one line.

Every boundary here has a fixture on both sides. The `attribution` field is the thing
standing between "the gate blocks what you broke" and "the gate blocks everyone", so
an operator flip in `mark_inherited` has to fail a test.
"""

from __future__ import annotations

from dataclasses import dataclass, field

import pytest

from conductor.candidate_review.engine import candidate_changed_paths, mark_inherited
from conductor.candidate_review.model import Finding, Severity
from conductor.candidate_review.policy import PolicyError, _attribution_value


@dataclass
class _Change:
    path: str | None = None
    old_path: str | None = None


@dataclass
class _Candidate:
    changes: list[_Change] = field(default_factory=list)


@dataclass
class _Check:
    check_id: str
    attribution: str = "candidate"


@dataclass
class _Policy:
    checks: tuple[_Check, ...] = ()


@dataclass
class _Ctx:
    candidate: _Candidate
    policy: _Policy


def _ctx(changes: list[_Change], checks: tuple[_Check, ...]) -> _Ctx:
    return _Ctx(candidate=_Candidate(changes=changes), policy=_Policy(checks=checks))


def _finding(check_id: str = "python-ast", path: str | None = "a.py") -> Finding:
    return Finding(
        check_id=check_id,
        rule_id="rule",
        severity=Severity.HIGH,
        message="m",
        path=path,
    )


# ---------------------------------------------------------------------------
# changed-path collection
# ---------------------------------------------------------------------------


def test_changed_paths_include_both_sides_of_a_rename() -> None:
    """A rename must count under both names or a real regression walks through."""
    ctx = _ctx([_Change(path="new.py", old_path="old.py")], ())
    assert candidate_changed_paths(ctx) == {"new.py", "old.py"}


def test_changed_paths_skip_missing_sides() -> None:
    ctx = _ctx([_Change(path="added.py", old_path=None)], ())
    assert candidate_changed_paths(ctx) == {"added.py"}


def test_changed_paths_of_an_empty_candidate_is_empty() -> None:
    assert candidate_changed_paths(_ctx([], ())) == set()


# ---------------------------------------------------------------------------
# attribution = "diff" -- both sides of the membership test
# ---------------------------------------------------------------------------


def test_diff_check_finding_on_a_changed_file_is_not_inherited() -> None:
    ctx = _ctx([_Change(path="a.py")], (_Check("python-ast", "diff"),))
    finding = _finding(path="a.py")
    mark_inherited(ctx, [finding], candidate_changed_paths(ctx))
    assert finding.inherited is False


def test_diff_check_finding_on_an_unchanged_file_is_inherited() -> None:
    """The measured failure: an unused variable in a file the diff never opened."""
    ctx = _ctx([_Change(path="a.py")], (_Check("python-ast", "diff"),))
    finding = _finding(path="somewhere/else.py")
    mark_inherited(ctx, [finding], candidate_changed_paths(ctx))
    assert finding.inherited is True


def test_diff_check_pathless_finding_is_inherited() -> None:
    """A pathless finding from a changed-file check is a whole-tree aggregate."""
    ctx = _ctx([_Change(path="a.py")], (_Check("python-ast", "diff"),))
    finding = _finding(path=None)
    mark_inherited(ctx, [finding], candidate_changed_paths(ctx))
    assert finding.inherited is True


def test_diff_check_attributes_a_renamed_file_to_the_candidate() -> None:
    ctx = _ctx([_Change(path="new.py", old_path="old.py")], (_Check("python-ast", "diff"),))
    finding = _finding(path="old.py")
    mark_inherited(ctx, [finding], candidate_changed_paths(ctx))
    assert finding.inherited is False


# ---------------------------------------------------------------------------
# attribution = "candidate" -- the default must narrow nothing
# ---------------------------------------------------------------------------


def test_candidate_check_never_marks_inherited_even_off_diff() -> None:
    """Default is fail-closed: undeclared checks keep blocking exactly as before."""
    ctx = _ctx([_Change(path="a.py")], (_Check("secret-scan", "candidate"),))
    finding = _finding(check_id="secret-scan", path="somewhere/else.py")
    mark_inherited(ctx, [finding], candidate_changed_paths(ctx))
    assert finding.inherited is False


def test_candidate_check_pathless_finding_still_blocks() -> None:
    ctx = _ctx([_Change(path="a.py")], (_Check("mutation-evidence", "candidate"),))
    finding = _finding(check_id="mutation-evidence", path=None)
    mark_inherited(ctx, [finding], candidate_changed_paths(ctx))
    assert finding.inherited is False


def test_a_check_absent_from_policy_defaults_to_candidate() -> None:
    ctx = _ctx([_Change(path="a.py")], ())
    finding = _finding(check_id="not-in-policy", path="elsewhere.py")
    mark_inherited(ctx, [finding], candidate_changed_paths(ctx))
    assert finding.inherited is False


def test_two_checks_are_attributed_independently() -> None:
    """One diff check and one candidate check, same off-diff path, opposite verdicts."""
    ctx = _ctx(
        [_Change(path="a.py")],
        (_Check("python-ast", "diff"), _Check("secret-scan", "candidate")),
    )
    diff_finding = _finding(check_id="python-ast", path="elsewhere.py")
    candidate_finding = _finding(check_id="secret-scan", path="elsewhere.py")
    mark_inherited(ctx, [diff_finding, candidate_finding], candidate_changed_paths(ctx))
    assert diff_finding.inherited is True
    assert candidate_finding.inherited is False


# ---------------------------------------------------------------------------
# fingerprint stability and policy validation
# ---------------------------------------------------------------------------


def test_inherited_does_not_change_a_finding_fingerprint() -> None:
    """The same defect keeps one identity, so exceptions and baselines stay stable."""
    caused = _finding(path="a.py").finalize()
    inherited = _finding(path="a.py")
    inherited.inherited = True
    assert inherited.finalize().fingerprint == caused.fingerprint


def test_attribution_accepts_both_declared_modes() -> None:
    assert _attribution_value("diff", field="f") == "diff"
    assert _attribution_value("candidate", field="f") == "candidate"


def test_attribution_refuses_an_unknown_mode() -> None:
    """A typo must not silently read as `candidate` and re-block inherited debt."""
    with pytest.raises(PolicyError):
        _attribution_value("dif", field="f")


def test_attribution_refuses_a_non_string() -> None:
    with pytest.raises(PolicyError):
        _attribution_value(True, field="f")
