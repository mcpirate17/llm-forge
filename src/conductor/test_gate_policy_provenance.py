"""The gate reads its policy from the exported candidate, never the working tree.

Kept out of `test_gate.py` deliberately. That module arrived after the mutation
grandfather anchor and none of its tests carry value evidence yet, so adding to it
would drag all 28 of its definitions into the value gate as "new" -- debt this change
did not create. These three tests stand alone and each kills a mutant.

The defect they prevent: on 2026-08-30 a stale `candidate_policy.toml` left in the
shared checkout by an abandoned branch pinned waiver base 58da5608 while the constant
expects d3697f22, and `make gate` refused for every session -- against candidates that
never contained the offending file.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

import pytest

from conductor import gate
from conductor.candidate_review.policy import PolicyError, ToolPolicy

# Reuse the one-commit git fixture rather than restating it; duplicating the
# helper here is what jscpd flagged on the first pass.
from conductor.test_gate import repo

# Re-exported so pytest resolves it here; named explicitly so the dead-code
# audit can see the fixture is deliberate rather than an unused import.
__all__ = ["repo"]


@dataclass(frozen=True)
class _StubPolicy:
    """No tools and no waivers, so every phase after the load is inert."""

    tools: tuple[ToolPolicy, ...] = ()
    mutation_waivers: tuple[object, ...] = ()


@pytest.fixture(autouse=True)
def _inert_corpus_audit(monkeypatch: pytest.MonkeyPatch) -> None:
    """The `repo` fixture is a one-file candidate with no campaign registry.

    `mutation_corpus_audit` walks the registered corpus and refuses a candidate
    that declares none, which every fixture here is. Nothing in this module is
    about that phase, so it is made inert exactly as `_StubPolicy` makes the
    policy inert. Its own contracts live in `test_gate.py`.
    """

    monkeypatch.setattr(
        gate,
        "mutation_corpus_audit",
        lambda export_root, changed_files=None: gate.PhaseResult(
            name="mutation-corpus", ok=True, detail="stubbed for this module"
        ),
    )


def _run_gate(repo: Path, json_out: Path) -> None:
    gate.run_gate(
        repo,
        target_ref="HEAD",
        base_ref="HEAD",
        profile="full",
        python="python3",
        policy_path=Path("conductor/candidate_policy.toml"),
        json_out=json_out,
        skip_review=True,
    )


def test_run_gate_loads_the_policy_from_the_export_not_the_working_tree(
    repo: Path, tmp_path: Path, monkeypatch
) -> None:
    """A dirty checkout must not be able to change the verdict."""
    seen: list[Path] = []

    def _capture(path: Path) -> _StubPolicy:
        seen.append(Path(path))
        return _StubPolicy()

    monkeypatch.setattr(gate, "load_policy", _capture)
    _run_gate(repo, tmp_path / "out.json")

    assert seen, "run_gate never loaded a policy"
    loaded = seen[0]
    assert repo not in loaded.parents, (
        f"policy was read from the working tree ({loaded}); a stale or uncommitted "
        "policy there would decide the verdict"
    )
    assert loaded.name == "candidate_policy.toml"
    assert "gate-export-" in str(loaded), f"not read from the export: {loaded}"


def test_run_gate_exports_before_it_loads_the_policy(
    repo: Path, tmp_path: Path, monkeypatch
) -> None:
    """Ordering is the mechanism: the export must exist for the load to read it."""
    order: list[str] = []
    real_export = gate.export_tree

    def _export(repo_path: Path, ref: str, destination: Path) -> gate.PhaseResult:
        order.append("export")
        return real_export(repo_path, ref, destination)

    def _load(path: Path) -> _StubPolicy:
        order.append("load_policy")
        return _StubPolicy()

    monkeypatch.setattr(gate, "export_tree", _export)
    monkeypatch.setattr(gate, "load_policy", _load)
    _run_gate(repo, tmp_path / "out.json")

    assert order[:2] == ["export", "load_policy"]


def test_run_gate_still_refuses_when_the_candidate_policy_is_bad(
    repo: Path, tmp_path: Path, monkeypatch
) -> None:
    """The other side: a policy error in the *candidate* must still refuse.

    Guards the error path against a 'be forgiving' regression that falls back to
    the working-tree copy, which would reintroduce the defect by another route.
    """

    def _boom(path: Path) -> _StubPolicy:
        raise PolicyError("waiver pins an unexpected integration base")

    monkeypatch.setattr(gate, "load_policy", _boom)
    with pytest.raises(gate.GateRefusal, match="policy did not load"):
        _run_gate(repo, tmp_path / "out.json")


def test_run_gate_refuses_rather_than_fails_when_a_declared_tool_is_missing(
    repo: Path, tmp_path: Path, monkeypatch
) -> None:
    """A refusal is not a failure: the gate could not run, it did not find a defect.

    Also pins that tool preflight now happens after the export -- moving the policy
    load into the export block moved preflight with it.
    """
    missing = ToolPolicy(
        tool_id="definitely-absent",
        executable="definitely-absent-binary",
        version_command=("definitely-absent-binary", "--version"),
        expected_version="1.0.0",
        required_profiles=("fast", "full"),
        provided_by="test fixture",
        rationale="test fixture",
    )
    monkeypatch.setattr(gate, "load_policy", lambda path: _StubPolicy(tools=(missing,)))

    exit_code, phases, statuses = gate.run_gate(
        repo,
        target_ref="HEAD",
        base_ref="HEAD",
        profile="full",
        python="python3",
        policy_path=Path("conductor/candidate_policy.toml"),
        json_out=tmp_path / "out.json",
        skip_review=True,
    )

    assert exit_code == gate.EXIT_REFUSED
    assert exit_code != gate.EXIT_FAIL
    assert [phase.name for phase in phases] == ["export", "tool-preflight"]
    assert [status.tool_id for status in statuses] == ["definitely-absent"]


def test_run_gate_runs_the_review_unless_it_is_skipped(
    repo: Path, tmp_path: Path, monkeypatch
) -> None:
    """skip_review is the only thing standing between the gate and the review."""
    calls: list[str] = []

    def _review(repo_path: Path, **kwargs: object) -> tuple[gate.PhaseResult, dict]:
        calls.append(str(kwargs["target_ref"]))
        return gate.PhaseResult(name="review", ok=True, detail="stubbed"), {}

    monkeypatch.setattr(gate, "load_policy", lambda path: _StubPolicy())
    monkeypatch.setattr(gate, "run_review", _review)

    _, phases, _ = gate.run_gate(
        repo,
        target_ref="HEAD",
        base_ref="HEAD",
        profile="full",
        python="python3",
        policy_path=Path("conductor/candidate_policy.toml"),
        json_out=tmp_path / "out.json",
        skip_review=False,
    )

    assert calls == ["HEAD"]
    assert "review" in [phase.name for phase in phases]
