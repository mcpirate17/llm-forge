"""Contracts for the engine-agnostic half of the generated-mutant runner.

The point of a generated engine is that nobody chooses the mutants, so the tests
that matter here are the ones that stop a run from looking clean when it
measured nothing, and the ones that keep a mutant's name stable enough for a
survivor baseline to mean something across edits.
"""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from conductor.mutation_engine_generated import (
    adapter_for,
    identify,
    load_generated_campaign,
    manifest_engine,
    mutant_id,
    pinned,
    record_survivor_baseline,
    require_executed,
    resolve_mutant_timeout,
    resolve_receipt_path,
    score,
)
from conductor.mutation_scope import CampaignError

REPO_ROOT = Path(__file__).resolve().parents[1]


def test_direct_run_refuses_unowned_sources_before_resolving_an_engine(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from conductor import mutation_engine_generated as runner
    from conductor import mutation_run_scope

    source = tmp_path / "conductor/gate_rollout.py"
    source.parent.mkdir()
    source.write_text("value = 1\n")
    subject = load_generated_campaign(manifest(tmp_path))
    seen = []

    def engine(name):
        seen.append(name)
        raise RuntimeError("engine reached")

    monkeypatch.setattr(runner, "adapter_for", engine)
    monkeypatch.setattr(mutation_run_scope, "changed_sources", lambda *a, **kw: set())
    with pytest.raises(CampaignError, match="outside this agent's changes"):
        runner.run_generated_campaign(subject, allow_mutations=True, repo_root=tmp_path)
    assert seen == []
    monkeypatch.setattr(
        mutation_run_scope,
        "changed_sources",
        lambda *a, **kw: {"conductor/gate_rollout.py"},
    )
    with pytest.raises(RuntimeError, match="engine reached"):
        runner.run_generated_campaign(subject, allow_mutations=True, repo_root=tmp_path)
    assert seen == ["fest"]


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


def test_the_per_mutant_bound_comes_from_the_manifest_or_the_baseline(
    tmp_path: Path,
) -> None:
    """A pinned bound wins; an unpinned one is 3x the baseline, floored at 60 s.

    The bound is resolved against the baseline suite's own wall time because
    that is the one number every campaign already pays to know: a mutant that
    runs the whole suite honestly needs at least the suite's cost, and a floor
    keeps a two-second suite from handing every mutant two seconds.
    """

    pinned_campaign = load_generated_campaign(
        manifest(tmp_path, generator={"source": ["conductor/gate_rollout.py"],
                                      "mutant_timeout_seconds": 300,
                                      "run_timeout_seconds": 60})
    )
    assert pinned_campaign.mutant_timeout_seconds == 300
    assert resolve_mutant_timeout(pinned_campaign, 5.0) == 300
    # A pinned bound is not re-derived, however slow or fast the baseline was.
    assert pinned_campaign.mutant_timeout_seconds == 300

    derived = load_generated_campaign(manifest(tmp_path))
    assert derived.mutant_timeout_seconds is None
    assert resolve_mutant_timeout(derived, 0.5) == 60  # the floor
    assert derived.mutant_timeout_seconds == 60

    slow = load_generated_campaign(manifest(tmp_path))
    assert resolve_mutant_timeout(slow, 41.2) == 124  # ceil(3 x 41.2)
    assert slow.mutant_timeout_seconds == 124

    negative = load_generated_campaign(manifest(tmp_path))
    assert resolve_mutant_timeout(negative, -7.0) == 60  # a nonsense clock still floors


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
    """A mutant the bound cut off is unknown -- not a kill, not a survivor.

    The engine stopped it at exactly the per-mutant limit the campaign asked
    for, so the run measured nothing about it. That used to read as a campaign
    ERROR, which blocked the ratchet on a healthy run and forced debt notes in
    every PR that carried one; it is a count beside `no_coverage` instead.
    """

    receipt = scored([("t", "TIMED_OUT"), ("k", "KILLED")], [], tmp_path)
    assert receipt["status"] == "PASS"
    assert receipt["timed_out"] == 1
    # Excluded from the denominator as well as the numerator: a slow kill is
    # not a kill, and a slow survivor has not been shown to survive.
    assert receipt["mutation_score"] == 1.0
    assert "t" not in receipt["survivors"]

    held = scored(
        [("t", "TIMED_OUT"), ("s", "SURVIVED"), ("k", "KILLED")], ["s"], tmp_path
    )
    assert held["status"] == "RATCHET_HELD"
    assert held["timed_out"] == 1

    regressed = scored(
        [("t", "TIMED_OUT"), ("s", "SURVIVED"), ("k", "KILLED")], [], tmp_path
    )
    assert regressed["status"] == "FAIL"
    assert regressed["new_survivors"] == ["s"]

    errored = scored([("e", "ERROR"), ("k", "KILLED")], [], tmp_path)
    assert errored["status"] == "ERROR"


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
    exclusions and a derived timeout while the manifest says otherwise. That
    is invisible in the receipt, which records the campaign object rather than
    the file.
    """

    path = manifest(
        tmp_path,
        generator={
            "source": ["conductor/gate_rollout.py"],
            "exclude": ["**/test_*.py"],
            "operators": ["constant_*"],
            "options": {"package_root": "native"},
            "seed": 7,
            "jobs": 4,
            "mutant_timeout_seconds": 45,
            "run_timeout_seconds": 60,
        },
    )
    campaign = load_generated_campaign(path)
    assert campaign.exclude == ("**/test_*.py",)
    assert campaign.operators == ("constant_*",)
    assert campaign.options == {"package_root": "native"}
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
    # No per-mutant bound until a run derives one from its baseline; the old
    # silent 30 is gone, because a suite that needs 90 s read six honest kills
    # as TIMED_OUT every run.
    assert campaign.mutant_timeout_seconds is None


def test_the_first_run_records_its_own_survivor_baseline(tmp_path: Path) -> None:
    """A fresh campaign is otherwise permanently red, and nobody may hand-fix it.

    Without a baseline every survivor is a new survivor, so the first honest run
    of a generated campaign FAILs and the only way out used to be an agent
    typing the survivor set into the manifest -- exactly the hand-authoring the
    automated-only rule forbids. The tool writes it instead, once, from what the
    engine found, and re-scores in place so the receipt the run publishes is the
    ratcheted one rather than the red one it started as.
    """

    path = manifest(tmp_path, survivor_baseline_note="Awaiting first measured run")
    campaign = load_generated_campaign(path)
    assert campaign.survivor_baseline_recorded is False

    receipt: dict = {
        "mutants": [
            {"id": "a", "outcome": "SURVIVED"},
            {"id": "k", "outcome": "KILLED"},
        ]
    }
    score(campaign, receipt)
    assert receipt["status"] == "FAIL"

    assert record_survivor_baseline(campaign, receipt) is True
    assert campaign.survivor_baseline_recorded is True
    assert receipt["status"] == "RATCHET_HELD"
    assert receipt["new_survivors"] == []
    assert receipt["survivor_baseline_recorded_by_this_run"] is True

    written = json.loads(path.read_text(encoding="utf-8"))
    assert written["survivor_baseline"] == ["a"]
    assert written["survivor_baseline_recorded"] is True
    assert "survivor_baseline_recorded_at" in written
    assert "survivor_baseline_note" not in written
    # The receipt has to name the manifest it now describes, not the one it read.
    assert receipt["manifest_sha256"] == campaign.manifest_sha256

    # Exactly once: a second run inherits the live ratchet instead of moving it.
    again = load_generated_campaign(path)
    second: dict = {"mutants": [{"id": "b", "outcome": "SURVIVED"}]}
    score(again, second)
    assert record_survivor_baseline(again, second) is False
    assert second["status"] == "FAIL"
    assert second["new_survivors"] == ["b"]


def test_recorded_flag_and_legacy_survivors_preserve_the_ratchet(
    tmp_path: Path,
) -> None:
    recorded_empty = load_generated_campaign(
        manifest(tmp_path, survivor_baseline_recorded=True)
    )
    assert recorded_empty.survivor_baseline_recorded is True
    legacy = load_generated_campaign(manifest(tmp_path, survivor_baseline=["legacy"]))
    assert legacy.survivor_baseline_recorded is True
    explicitly_unrecorded = load_generated_campaign(
        manifest(
            tmp_path, survivor_baseline=["legacy"], survivor_baseline_recorded=False
        )
    )
    assert explicitly_unrecorded.survivor_baseline_recorded is False


def test_an_errored_run_never_becomes_a_baseline(tmp_path: Path) -> None:
    """An ERROR measured nothing; its survivors are absence of evidence.

    Recording them would bless every gap the run failed to probe as a known,
    accepted survivor -- a permanently green campaign built out of a crash.
    """

    path = manifest(tmp_path)
    campaign = load_generated_campaign(path)
    receipt = {"status": "ERROR", "survivors": ["a", "b"]}

    assert record_survivor_baseline(campaign, receipt) is False
    assert campaign.survivor_baseline_recorded is False
    assert "survivor_baseline" not in json.loads(path.read_text(encoding="utf-8"))


def _campaign(campaign_id: str = "generated_fixture") -> SimpleNamespace:
    """A duck-typed stand-in: ``resolve_receipt_path`` reads only ``campaign_id``."""
    return SimpleNamespace(campaign_id=campaign_id)


def test_resolve_receipt_path_defaults_to_the_configured_receipt_root(
    tmp_path: Path,
) -> None:
    (tmp_path / "pyproject.toml").write_text(
        '[tool.conductor]\nmutation_receipt_root = "campaigns/receipts"\n',
        encoding="utf-8",
    )
    output, relative = resolve_receipt_path(_campaign("gen_campaign"), None, tmp_path)
    assert output.parent == tmp_path / "campaigns" / "receipts"
    assert (tmp_path / "campaigns" / "receipts").is_dir()
    assert relative.startswith("campaigns/receipts/gen_campaign_")


def test_resolve_receipt_path_falls_back_to_the_monorepo_literal_unconfigured(
    tmp_path: Path,
) -> None:
    output, relative = resolve_receipt_path(_campaign(), None, tmp_path)
    assert output.parent == tmp_path / "research" / "reports" / "mutation_testing"
    assert relative.startswith("research/reports/mutation_testing/generated_fixture_")


def test_resolve_receipt_path_fails_loud_when_the_directory_cannot_be_created(
    tmp_path: Path,
) -> None:
    (tmp_path / "pyproject.toml").write_text(
        '[tool.conductor]\nmutation_receipt_root = "blocked"\n', encoding="utf-8"
    )
    (tmp_path / "blocked").write_text("not a directory\n", encoding="utf-8")
    with pytest.raises(CampaignError, match="cannot create mutation receipt directory"):
        resolve_receipt_path(_campaign(), None, tmp_path)


def test_resolve_receipt_path_still_honours_an_explicit_path(tmp_path: Path) -> None:
    explicit = tmp_path / "somewhere" / "receipt.json"
    output, relative = resolve_receipt_path(_campaign(), explicit, tmp_path)
    assert output == explicit.resolve()
    assert relative == "somewhere/receipt.json"


def test_a_baseline_that_died_for_want_of_python_says_so(
    tmp_path: Path,
) -> None:
    """Gap 6's other half: the failure that reads as a mystery must not be one.

    `unmutated baseline failed` alone sent lanes hunting a broken test while
    the same suite passed on the host -- the snapshot simply had no interpreter
    that could import conductor. The tails say which it was; the refusal now
    repeats them as a reason.
    """

    from conductor.mutation_campaign_model import CommandResult

    from conductor.mutation_engine_generated import note_baseline

    campaign = load_generated_campaign(manifest(tmp_path))
    receipt: dict = {}
    output = tmp_path / "receipt.json"

    def result(stderr: str) -> CommandResult:
        return CommandResult(
            returncode=1,
            timed_out=False,
            duration_seconds=1.0,
            stdout_tail="",
            stderr_tail=stderr,
        )

    for tail in (
        "python3: not found",
        "ModuleNotFoundError: No module named 'conductor'",
        "ImportError: No module named 'x'",
    ):
        with pytest.raises(CampaignError, match="could not drive Python"):
            note_baseline(campaign, receipt, result(tail), ["cargo", "test"], output)
        assert receipt["status"] == "BASELINE_FAILED"

    # A tail carrying EVERY marker at once is the one input that tells `in`
    # from `not in`: any partial tail leaves some marker absent, so an
    # inverted membership test still finds a "missing" marker and returns
    # the hint anyway. All three present, and only the honest test fires.
    with pytest.raises(CampaignError, match="could not drive Python"):
        note_baseline(
            campaign,
            receipt,
            result("python3: not found: ModuleNotFoundError: No module named 'x'"),
            ["cargo", "test"],
            output,
        )

    # Any other failure stays a plain refusal -- the hint must not smudge a
    # genuine red suite into an interpreter problem.
    with pytest.raises(CampaignError, match="^unmutated baseline failed"):
        note_baseline(
            campaign, receipt, result("test result: FAILED. 3 passed; 2 failed"),
            ["cargo", "test"], output,
        )


def test_the_disk_copy_is_slim_while_the_returned_receipt_stays_full(
    tmp_path: Path,
) -> None:
    """Slice L: every receipt the engine leaves on disk is slim.

    The in-memory dict keeps its full rows (attribution and the CLI summary
    read them before the write ever happens); the file on disk folds the
    detail under one key -- inline under 50 mutants, one zstd+base64 blob
    above -- with the summary byte-identical.
    """

    import conductor.mutation_engine_generated as runner
    from conductor.mutation_receipt_slim import expand_receipt

    receipt = {
        "campaign_id": "gen_slim",
        "status": "RATCHET_HELD",
        "generated_at": "2026-09-13T00:00:00+00:00",
        "mutants": [{"id": f"m{i}", "outcome": "KILLED"} for i in range(80)],
    }
    out = tmp_path / "receipt.json"
    runner.write_receipt(out, receipt)
    text = out.read_text(encoding="utf-8")
    disk = json.loads(text)
    assert "mutants" not in disk
    assert disk["detail"]["encoding"] == "zstd+base64"
    assert disk["campaign_id"] == receipt["campaign_id"]
    assert expand_receipt(disk) == receipt
    assert text == json.dumps(disk, indent=2, sort_keys=True) + "\n"

    small = dict(receipt, mutants=receipt["mutants"][:5])
    runner.write_receipt(out, small)
    disk = json.loads(out.read_text(encoding="utf-8"))
    assert disk["detail"]["encoding"] == "json"
    assert expand_receipt(disk) == small
