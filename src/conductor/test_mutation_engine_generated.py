"""Contracts for the engine-agnostic half of the generated-mutant runner.

The point of a generated engine is that nobody chooses the mutants, so the tests
that matter here are the ones that stop a run from looking clean when it
measured nothing, and the ones that keep a mutant's name stable enough for a
survivor baseline to mean something across edits.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from conductor.mutation_engine_generated import (
    adapter_for,
    identify,
    load_generated_campaign,
    manifest_engine,
    mutant_id,
    pinned,
    require_executed,
    score,
)
from conductor.mutation_scope import CampaignError

REPO_ROOT = Path(__file__).resolve().parents[1]


def manifest(tmp_path: Path, **overrides: object) -> Path:
    """A minimal generated-engine manifest on disk."""

    payload: dict[str, object] = {
        "campaign_id": "probe",
        "title": "probe",
        "language": "python",
        "mutation_engine": "fest",
        "generator": {
            "source": ["conductor/gate_rollout.py"],
            "run_timeout_seconds": 60,
        },
        "test_argv": ["python", "-m", "pytest", "-q", "conductor/test_gate_rollout.py"],
        "source_sha256": {"conductor/gate_rollout.py": "0" * 64},
        "test_sha256": {"conductor/test_gate_rollout.py": "1" * 64},
    }
    payload.update(overrides)
    path = tmp_path / "campaign.json"
    path.write_text(json.dumps(payload), encoding="utf-8")
    return path


def scored(rows: list[tuple[str, str]], baseline: list[str], tmp_path: Path) -> dict:
    """Score a receipt carrying exactly ``rows`` of (id, outcome)."""

    campaign = load_generated_campaign(manifest(tmp_path, survivor_baseline=baseline))
    receipt: dict = {"mutants": [{"id": i, "outcome": o} for i, o in rows]}
    score(campaign, receipt)
    return receipt


def survivors(names: list[str], baseline: list[str], tmp_path: Path) -> dict:
    """Score a receipt whose only survivors are ``names``, plus one kill."""

    rows = [(name, "SURVIVED") for name in names] + [("k", "KILLED")]
    return scored(rows, baseline, tmp_path)


def test_a_mutant_is_named_by_what_it_does_not_by_where_it_sits() -> None:
    """The same rewrite in the same file keeps its name wherever it moves.

    Line and byte offset both move when anything above a mutant changes, so
    naming a mutant by position would churn the survivor baseline on every
    unrelated edit and make the ratchet unreadable. Nothing positional is an
    input to the name, so this is a property of the identifier, not of a run.
    """

    here = mutant_id("a.py", "constant_replace", '"gh"', '""', 0)
    assert here == mutant_id("a.py", "constant_replace", '"gh"', '""', 0)
    assert here != mutant_id("b.py", "constant_replace", '"gh"', '""', 0)
    assert here != mutant_id("a.py", "constant_replace", '"gh"', '"gg"', 0)
    assert here != mutant_id("a.py", "operator_swap", '"gh"', '""', 0)
    assert here != mutant_id("a.py", "constant_replace", '"gh"', '""', 1)


def test_identical_rewrites_in_one_file_get_distinct_names() -> None:
    """Two `"gh"` -> `""` mutants in one file are two mutants, not one row."""

    same = ("a.py", "constant_replace", '"gh"', '""')
    names = identify([same, same, ("b.py", *same[1:])])
    assert len(set(names)) == 3
    # Asserted by exact value, not by suffix: a name that ends "-1" can still be
    # malformed. Both a mis-split base and a counter running backwards produce
    # names an endswith() check accepts and a survivor baseline cannot match.
    assert names == [
        mutant_id(*same, 0),
        mutant_id(*same, 1),
        mutant_id("b.py", *same[1:], 0),
    ]


def test_a_run_that_executed_nothing_is_refused() -> None:
    """Zero executed mutants means zero survivors, which would score clean."""

    with pytest.raises(CampaignError, match="matched nothing"):
        require_executed(0, 0, ["x/*.py"])
    with pytest.raises(CampaignError, match="no mutant was executed"):
        require_executed(174, 0, ["x/*.py"])
    require_executed(174, 1, ["x/*.py"])


def test_a_new_survivor_fails_and_a_known_one_only_holds(tmp_path: Path) -> None:
    """The campaign fails on movement, and never reports PASS while gaps remain."""

    fails = survivors(["a", "b"], ["a"], tmp_path)
    assert fails["status"] == "FAIL"
    assert fails["new_survivors"] == ["b"]
    # One kill against two survivors. Asserting the verdict alone leaves the
    # arithmetic free to produce a negative or greater-than-one score.
    assert fails["mutation_score"] == pytest.approx(1 / 3)
    assert fails["outcome_counts"]["SURVIVED"] == 2

    holds = survivors(["a"], ["a", "z"], tmp_path)
    assert holds["status"] == "RATCHET_HELD"
    assert holds["new_survivors"] == []
    assert holds["resolved_survivors"] == ["z"]

    clean = survivors([], [], tmp_path)
    assert clean["status"] == "PASS"
    assert clean["mutation_score"] == 1.0


def test_a_timeout_is_never_scored_as_a_kill(tmp_path: Path) -> None:
    """A mutant the tests merely outran was not detected."""

    receipt = scored([("t", "TIMED_OUT"), ("k", "KILLED")], [], tmp_path)
    assert receipt["status"] == "ERROR"


def test_uncovered_mutants_are_counted_but_never_scored(tmp_path: Path) -> None:
    """Code no test reaches is the finding, not a free point in the score."""

    rows = [(f"n{i}", "NO_COVERAGE") for i in range(3)] + [("k", "KILLED")]
    receipt = scored(rows, [], tmp_path)
    assert receipt["no_coverage"] == 3
    assert receipt["mutation_score"] == 1.0
    assert receipt["status"] == "PASS"


def test_unviable_mutants_are_counted_but_never_scored(tmp_path: Path) -> None:
    """A mutant that does not compile was never a test of anything.

    Reading it as a kill -- which any kill-fraction over the whole corpus does --
    inflates the score with mutants no suite could have caught.
    """

    rows = [(f"u{i}", "UNVIABLE") for i in range(24)] + [("k", "KILLED")]
    receipt = scored(rows, [], tmp_path)
    assert receipt["unviable"] == 24
    assert receipt["mutation_score"] == 1.0
    assert receipt["status"] == "PASS"


def test_an_unknown_outcome_is_refused_not_averaged(tmp_path: Path) -> None:
    """A tool release that adds an outcome must stop the run, not be ignored."""

    with pytest.raises(CampaignError, match="unknown mutant outcome"):
        scored([("x", "PROBABLY_FINE")], [], tmp_path)


def test_a_hand_written_campaign_is_refused_by_this_runner(tmp_path: Path) -> None:
    """The 476 reviewed-patch campaigns belong to the other runner."""

    path = manifest(tmp_path, mutation_engine="reviewed_unified_diff")
    assert manifest_engine(path) == "reviewed_unified_diff"
    with pytest.raises(CampaignError, match="is not a generated engine"):
        load_generated_campaign(path)
    with pytest.raises(CampaignError, match="is not a generated engine"):
        adapter_for("reviewed_unified_diff")


def test_every_declared_engine_resolves_to_an_adapter() -> None:
    """A name in the registry with no module behind it fails only at run time."""

    for engine in ("fest", "cargo-mutants"):
        adapter = adapter_for(engine)
        assert adapter.ENGINE == engine
        assert callable(adapter.binary)
        assert callable(adapter.execute)


def test_a_manifest_missing_a_required_key_is_refused(tmp_path: Path) -> None:
    """Refuse the manifest rather than default the subject to nothing."""

    path = tmp_path / "bad.json"
    path.write_text(json.dumps({"campaign_id": "x"}), encoding="utf-8")
    with pytest.raises(CampaignError, match="missing required key 'title'"):
        load_generated_campaign(path)


def test_the_test_command_is_pinned_to_this_interpreter() -> None:
    """A bare `python` in a manifest resolves to whatever PATH holds."""

    assert pinned(["python", "-m", "pytest"], "/v/bin/python") == [
        "/v/bin/python",
        "-m",
        "pytest",
    ]
    # `python3` is exactly as unpinned as `python`, and is what a manifest
    # copied from a shell command most often says.
    assert pinned(["python3", "-m", "pytest"], "/v/bin/python") == [
        "/v/bin/python",
        "-m",
        "pytest",
    ]
    # Anything else is a real command and is left alone.
    assert pinned(["cargo", "test"], "/v/bin/python") == ["cargo", "test"]
    assert pinned([], "/v/bin/python") == []


def test_the_source_globs_must_name_something(tmp_path: Path) -> None:
    """An empty subject would generate no mutants and score clean."""

    path = manifest(tmp_path, generator={"source": [], "run_timeout_seconds": 60})
    with pytest.raises(CampaignError, match="at least one glob"):
        load_generated_campaign(path)


def test_the_campaign_pins_the_bytes_it_measured(tmp_path: Path) -> None:
    """Without a pin the receipt cannot say which source it mutated."""

    path = manifest(tmp_path, source_sha256={})
    with pytest.raises(CampaignError, match="must pin every mutated file"):
        load_generated_campaign(path)


def test_the_optional_generator_keys_reach_the_campaign(tmp_path: Path) -> None:
    """A misread key name silently substitutes the default for the manifest.

    `exclude`, `operators` and `mutant_timeout_seconds` are all optional, so
    reading the wrong name raises nothing -- the campaign just runs with no
    exclusions and the default timeout while the manifest says otherwise. That
    is invisible in the receipt, which records the campaign object rather than
    the file.
    """

    path = manifest(
        tmp_path,
        generator={
            "source": ["conductor/gate_rollout.py"],
            "exclude": ["**/test_*.py"],
            "operators": ["constant_*"],
            "seed": 7,
            "jobs": 4,
            "mutant_timeout_seconds": 45,
            "run_timeout_seconds": 60,
        },
    )
    campaign = load_generated_campaign(path)
    assert campaign.exclude == ("**/test_*.py",)
    assert campaign.operators == ("constant_*",)
    assert campaign.seed == 7
    assert campaign.jobs == 4
    assert campaign.mutant_timeout_seconds == 45


def test_the_environment_a_manifest_declares_reaches_the_run(tmp_path: Path) -> None:
    """A dropped environment runs the engine under a different one than recorded.

    The receipt reports the campaign object, so a manifest whose environment
    never arrives looks identical to one that has none.
    """

    campaign = load_generated_campaign(
        manifest(tmp_path, environment={"CARGO_TERM_COLOR": "never"})
    )
    assert campaign.environment == {"CARGO_TERM_COLOR": "never"}


def test_the_defaults_are_the_ones_the_manifests_were_written_against(
    tmp_path: Path,
) -> None:
    """Every committed manifest omitting these relies on these exact values."""

    campaign = load_generated_campaign(manifest(tmp_path))
    assert campaign.exclude == ()
    assert campaign.operators == ()
    assert campaign.options == {}
    assert campaign.seed == 0
    assert campaign.jobs == 1
    assert campaign.mutant_timeout_seconds == 30
