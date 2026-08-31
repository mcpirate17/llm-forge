"""Two governance-gate defects that produced false blocking findings.

1. ``_call_name`` returned the bare attribute name when a call's receiver could
   not be resolved, so ``model.to(device).eval()`` — the standard PyTorch
   eval-mode idiom — resolved to ``"eval"`` and raised a CRITICAL
   ``dynamic-execution`` finding.
2. ``_has_property_evidence`` was a plain regex over test text and never
   consulted mutation evidence, despite backing a rule named
   ``missing-property-or-mutation-evidence``.
"""

from __future__ import annotations

import ast
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import pytest

from conductor.candidate_review import verification
from conductor.candidate_review.checks import _call_name, _PythonVisitor


def _call_node(expression: str) -> ast.expr:
    parsed = ast.parse(expression).body[0]
    assert isinstance(parsed, ast.Expr)
    call = parsed.value
    assert isinstance(call, ast.Call)
    return call.func


@pytest.mark.parametrize(
    ("expression", "expected"),
    [
        # Bare builtins stay resolvable — these are the real detections.
        ("eval(payload)", "eval"),
        ("exec(payload)", "exec"),
        # Dotted from a Name root stays resolvable.
        ("os.system(cmd)", "os.system"),
        ("pickle.load(handle)", "pickle.load"),
        ("yaml.load(stream)", "yaml.load"),
        ("obj.eval()", "obj.eval"),
        ("self.eval()", "self.eval"),
        ("torch.nn.Module.eval(model)", "torch.nn.Module.eval"),
        # Unresolvable receivers must NOT collapse to the bare attribute.
        ("model.to(device).eval()", ""),
        ("build().exec()", ""),
        ("registry['key'].eval()", ""),
        ("(a + b).eval()", ""),
    ],
)
def test_call_name_never_guesses_a_builtin_from_an_unresolvable_receiver(
    expression: str, expected: str
) -> None:
    assert _call_name(_call_node(expression)) == expected


def _rule_ids(source: str) -> set[str]:
    visitor = _PythonVisitor("m.py", source.splitlines(), hot_path=False)
    visitor.visit(ast.parse(source))
    return {finding.rule_id for finding in visitor.findings}


@pytest.mark.parametrize(
    ("source", "flagged"),
    [
        ("def f(model, device):\n    return model.to(device).eval()\n", False),
        ("def f(model):\n    return model.eval()\n", False),
        ("def f(payload):\n    return eval(payload)\n", True),
        ("def f(payload):\n    return exec(payload)\n", True),
        ("import os\n\n\ndef f(cmd):\n    return os.system(cmd)\n", True),
    ],
)
def test_dynamic_execution_flags_real_calls_only(source: str, flagged: bool) -> None:
    assert ("dynamic-execution" in _rule_ids(source)) is flagged


@dataclass
class _Ctx:
    """Minimal stand-in for the fields ``_has_property_evidence`` reads."""

    snapshot: Path


def _write_registry(root: Path) -> None:
    registry = root / "conductor" / "mutation_campaigns" / "registry.json"
    registry.parent.mkdir(parents=True, exist_ok=True)
    registry.write_text('{"campaigns": []}', encoding="utf-8")


def test_property_regex_still_satisfies_the_gate(tmp_path: Path) -> None:
    test_path = tmp_path / "test_thing.py"
    test_path.write_text(
        "import pytest\n\n\n@pytest.mark.parametrize('n', [1])\ndef test_thing(n):\n"
        "    assert n\n",
        encoding="utf-8",
    )
    assert verification._has_property_evidence(_Ctx(tmp_path), {"test_thing.py"})


def test_mutation_evidence_satisfies_the_gate_without_property_text(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A campaign PASS is stronger than a decorator and must count."""
    (tmp_path / "test_thing.py").write_text(
        "def test_thing():\n    assert True\n", encoding="utf-8"
    )
    _write_registry(tmp_path)
    ctx = _Ctx(tmp_path)
    assert not verification._has_property_evidence(ctx, {"test_thing.py"})

    def _verify(_registry: Path, paths: Any, **_kw: Any) -> dict[str, Any]:
        return {"evidence": [{"path": path} for path in paths]}

    monkeypatch.setattr("conductor.mutation_testing.verify_evidence", _verify)
    assert verification._has_property_evidence(ctx, {"test_thing.py"})


@pytest.mark.parametrize(
    ("covered_path", "expected"),
    [
        ("test_thing.py", True),
        ("test_unrelated.py", False),
    ],
)
def test_mutation_evidence_must_cover_a_selected_test(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    covered_path: str,
    expected: bool,
) -> None:
    _write_registry(tmp_path)

    def _verify(_registry: Path, _paths: Any, **_kw: Any) -> dict[str, Any]:
        return {"evidence": [{"path": covered_path}]}

    monkeypatch.setattr("conductor.mutation_testing.verify_evidence", _verify)
    assert (
        verification._has_mutation_evidence(_Ctx(tmp_path), {"test_thing.py"})
        is expected
    )


def test_missing_registry_is_not_mutation_evidence(tmp_path: Path) -> None:
    assert not verification._has_mutation_evidence(_Ctx(tmp_path), {"test_thing.py"})


def test_broken_registry_does_not_claim_evidence(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A CampaignError belongs to the mutation-evidence check, not to this one."""
    from conductor.mutation_testing import CampaignError

    _write_registry(tmp_path)

    def _raise(_registry: Path, _paths: Any, **_kw: Any) -> dict[str, Any]:
        raise CampaignError("registry is broken")

    monkeypatch.setattr("conductor.mutation_testing.verify_evidence", _raise)
    assert not verification._has_mutation_evidence(_Ctx(tmp_path), {"test_thing.py"})


@pytest.fixture(autouse=True)
def _backlog_drop(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    """Keep the gate's backlog artifact out of the real repository.

    `check_equivalence_probe` writes a run artifact wherever GATE_FINDINGS points, so
    without this every test that calls it drops a file into
    research/reports/gate_findings with the FIXTURE module names in it -- and
    `make slop-backlog` then folds `lane.py` and `other.py` into the tracked ledger.
    That happened; the ledger picked up four fixture paths.
    """
    from conductor import slop_ledger

    drop = tmp_path / "autouse_gate_findings"
    monkeypatch.setattr(slop_ledger, "GATE_FINDINGS", drop)
    return drop


@dataclass(frozen=True)
class _StubChange:
    path: str
    classes: tuple[str, ...] = ("python",)


@dataclass(frozen=True)
class _StubContext:
    snapshot: Path
    live_changes: tuple[_StubChange, ...]


def _blocking(module: str = "lane.py") -> dict[str, Any]:
    return {
        "module": module, "qualname": "Lane.forward", "rule": "drop_where",
        "lineno": 12, "verdict": "REACHABLE_BUT_UNTESTED",
        "description": "torch.where(...) collapsed", "amplifier": "params_x1e3",
        "max_diff_amplified": 0.97,
    }


def test_equivalence_probe_reports_only_reachable_untested_branches(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Advisory verdicts must not become review findings.

    A clean differential sweep cannot prove equivalence, so surfacing it as a
    finding would block a change on a claim the probe never established.
    """
    from conductor import slop_gate
    from conductor.candidate_review.checks import check_equivalence_probe

    summary = {"modules_probed": 1, "modules_without_drivers": [],
               "blocking": [_blocking()], "advisory": [_blocking("other.py")]}
    monkeypatch.setattr(slop_gate, "run", lambda base, root, only=(): (1, summary))
    result = check_equivalence_probe(
        _StubContext(tmp_path, (_StubChange("lane.py"),))
    )
    assert [f.path for f in result.findings] == ["lane.py"]
    assert result.metrics["advisory"] == 1


def test_equivalence_probe_never_probes_a_test_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A test file is a driver, not a target; probing it would drive it with itself."""
    from conductor import slop_gate
    from conductor.candidate_review.checks import check_equivalence_probe

    seen: list[list[str]] = []

    def _run(base: str, root: Path, only: Any = ()) -> tuple[int, dict[str, Any]]:
        seen.append(list(only))
        return 0, {"modules_probed": 0, "modules_without_drivers": [],
                   "blocking": [], "advisory": []}

    monkeypatch.setattr(slop_gate, "run", _run)
    check_equivalence_probe(
        _StubContext(tmp_path, (_StubChange("lane.py"), _StubChange("test_lane.py")))
    )
    assert seen == [["lane.py"]]


def test_severity_follows_the_tier_so_only_shipped_code_blocks(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The decision that makes this check enforceable rather than advisory.

    The first real sweep put 86% of its findings in one-off research scripts. Blocking
    on those would make the gate unusable; blocking on none of them makes it
    decorative. So severity follows the tier, and the tier comes from the same prefix
    list the backlog uses -- the gate and the report cannot disagree about what ships.
    """
    from conductor import slop_gate
    from conductor.candidate_review.checks import check_equivalence_probe
    from conductor.candidate_review.model import Severity

    shipped = _blocking("conductor/lane.py")
    script = _blocking("research/tools/one_off.py")
    summary = {"modules_probed": 2, "modules_without_drivers": [],
               "blocking": [shipped, script], "advisory": []}
    monkeypatch.setattr(slop_gate, "run", lambda base, root, only=(): (1, summary))
    result = check_equivalence_probe(
        _StubContext(tmp_path, (_StubChange("conductor/lane.py"),
                                _StubChange("research/tools/one_off.py"))))
    by_path = {f.path: f.severity for f in result.findings}
    assert by_path["conductor/lane.py"] == Severity.HIGH
    assert by_path["research/tools/one_off.py"] == Severity.LOW


def test_the_gate_hands_its_findings_to_the_backlog(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, _backlog_drop: Path
) -> None:
    """The gate already paid for the measurement; discarding it was the whole gap.

    It writes a run ARTIFACT rather than the ledger: this check runs against a
    candidate snapshot and concurrently with other reviews, so a tracked-file write
    would dirty the tree under review.
    """
    import json as _json

    from conductor import slop_gate
    from conductor.candidate_review.checks import check_equivalence_probe

    drop = _backlog_drop
    summary = {"modules_probed": 1, "modules_without_drivers": [],
               "blocking": [_blocking("conductor/lane.py")], "advisory": []}
    monkeypatch.setattr(slop_gate, "run", lambda base, root, only=(): (1, summary))

    check_equivalence_probe(
        _StubContext(tmp_path, (_StubChange("conductor/lane.py"),)))

    written = list(drop.glob("*.json"))
    assert len(written) == 1, f"expected one run artifact, got {written}"
    assert _json.loads(written[0].read_text())["blocking"] == summary["blocking"]
    assert not list(drop.glob("*.part")), "a partial write must not be left behind"


def test_a_backlog_write_failure_does_not_fail_the_review(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Bookkeeping must never be able to fail a code review.

    A read-only checkout or a full disk is not a reason to reject a change, and the
    finding the reviewer needs has already been computed by this point.
    """
    from conductor import slop_gate, slop_ledger
    from conductor.candidate_review.checks import check_equivalence_probe

    blocked = tmp_path / "unwritable"
    blocked.write_text("not a directory")
    monkeypatch.setattr(slop_ledger, "GATE_FINDINGS", blocked / "nested")
    summary = {"modules_probed": 1, "modules_without_drivers": [],
               "blocking": [_blocking("conductor/lane.py")], "advisory": []}
    monkeypatch.setattr(slop_gate, "run", lambda base, root, only=(): (1, summary))

    result = check_equivalence_probe(
        _StubContext(tmp_path, (_StubChange("conductor/lane.py"),)))
    assert [f.path for f in result.findings] == ["conductor/lane.py"]
