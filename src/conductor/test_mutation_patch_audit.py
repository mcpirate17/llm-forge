from __future__ import annotations

import dataclasses
import json
from pathlib import Path
import subprocess

import pytest

from conductor import mutation_patch_audit, mutation_testing
from conductor.mutation_scope import CampaignError


def _git_repo(root: Path) -> None:
    subprocess.run(["git", "init", "-q", str(root)], check=True)
    (root / "source.py").write_text("VALUE = 1\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)


def _patch(root: Path, name: str, old: str, new: str) -> Path:
    path = root / f"{name}.patch"
    path.write_text(
        "diff --git a/source.py b/source.py\n"
        "--- a/source.py\n"
        "+++ b/source.py\n"
        "@@ -1 +1 @@\n"
        f"-{old}\n"
        f"+{new}\n",
        encoding="utf-8",
    )
    return path


def _campaign(
    root: Path, campaign_id: str, mutations: tuple[mutation_testing.Mutation, ...]
) -> mutation_testing.Campaign:
    manifest = root / f"{campaign_id}.json"
    manifest.write_text("{}\n", encoding="utf-8")
    return mutation_testing.Campaign(
        manifest_path=manifest,
        manifest_sha256=mutation_testing._sha256(manifest),  # noqa: SLF001
        campaign_id=campaign_id,
        title=campaign_id,
        language="python",
        mutation_engine="reviewed_unified_diff",
        expected_mutations=len(mutations),
        source_sha256={},
        ranked_tests=(),
        planned_mutations=(),
        mutations=mutations,
        test_argv=("python", "-m", "pytest"),
        timeout_seconds=10,
        blocked_process_substrings=(),
        poll_seconds=1,
        environment={},
        host_read_dependencies=(),
        test_scopes={},
    )


def _mutation(
    mutation_id: str, patch: Path, sha256: str | None = None
) -> mutation_testing.Mutation:
    return mutation_testing.Mutation(
        mutation_id,
        patch,
        sha256 if sha256 is not None else mutation_testing._sha256(patch),  # noqa: SLF001
        ("source.py",),
        (),
    )


def _tree(root: Path) -> mutation_patch_audit._TreeHasher:
    """The audited tree, hashed once per file -- what the stale-byte check reads."""

    return mutation_patch_audit._TreeHasher(root)


def test_a_rotted_anchor_is_reported_and_a_live_patch_is_not(tmp_path: Path) -> None:
    """The audit exists to separate mutants that still land from ones that cannot."""

    _git_repo(tmp_path)
    live = _mutation("live", _patch(tmp_path, "live", "VALUE = 1", "VALUE = 2"))
    rotted = _mutation("rotted", _patch(tmp_path, "rotted", "VALUE = 9", "VALUE = 2"))
    campaign = _campaign(tmp_path, "corpus", (live, rotted))

    assert mutation_patch_audit._patch_verdict(campaign, live, tmp_path) is None
    verdict = mutation_patch_audit._patch_verdict(campaign, rotted, tmp_path)
    assert verdict is not None
    assert verdict["reason"] == "DOES_NOT_APPLY"
    assert verdict["mutation_id"] == "rotted"
    # Two independent refusals are reported, and either one alone would let a
    # truncation of the other pass unnoticed -- so both halves are pinned.
    git_reason, separator, anchored_reason = verdict["detail"].partition(
        "; anchored retry: "
    )
    assert separator
    assert "source.py" in git_reason
    assert "source.py" in anchored_reason


def test_the_check_runs_against_the_repo_root_not_the_process_directory(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A check run in the wrong tree reports every patch clean and audits nothing."""

    _git_repo(tmp_path)
    live = _mutation("live", _patch(tmp_path, "live", "VALUE = 1", "VALUE = 2"))
    rotted = _mutation("rotted", _patch(tmp_path, "rotted", "VALUE = 9", "VALUE = 2"))
    campaign = _campaign(tmp_path, "corpus", (live, rotted))
    # The process directory is a tree the rotted patch applies to cleanly. A check
    # that reads it instead of the repo root returns early and calls a dead mutant
    # live -- the failure this test exists for.
    elsewhere = tmp_path.parent / "elsewhere"
    elsewhere.mkdir()
    _git_repo(elsewhere)
    (elsewhere / "source.py").write_text("VALUE = 9\n", encoding="utf-8")
    monkeypatch.chdir(elsewhere)

    assert mutation_patch_audit._patch_verdict(campaign, live, tmp_path) is None
    rotted_verdict = mutation_patch_audit._patch_verdict(campaign, rotted, tmp_path)
    assert rotted_verdict is not None
    assert rotted_verdict["reason"] == "DOES_NOT_APPLY"
    stale = mutation_patch_audit._patch_verdict(campaign, live, elsewhere)
    assert stale is not None and stale["reason"] == "DOES_NOT_APPLY"


def test_a_missing_or_drifted_patch_is_reported_without_being_applied(
    tmp_path: Path,
) -> None:
    """An unreviewed patch must never reach `git apply`, and must never read clean."""

    _git_repo(tmp_path)
    absent = _mutation("absent", _patch(tmp_path, "absent", "VALUE = 1", "VALUE = 2"))
    absent.patch_file.unlink()
    drifted_patch = _patch(tmp_path, "drifted", "VALUE = 1", "VALUE = 2")
    drifted = _mutation("drifted", drifted_patch, sha256="0" * 64)
    campaign = _campaign(tmp_path, "corpus", (absent, drifted))

    missing = mutation_patch_audit._patch_verdict(campaign, absent, tmp_path)
    assert missing is not None and missing["reason"] == "MISSING"
    assert "no patch file at" in missing["detail"]
    hash_drift = mutation_patch_audit._patch_verdict(campaign, drifted, tmp_path)
    assert hash_drift is not None and hash_drift["reason"] == "HASH_DRIFT"
    assert "0" * 64 in hash_drift["detail"]


def test_one_unloadable_manifest_does_not_hide_the_rest_of_the_corpus(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A half-written campaign must not mask, or be masked by, corpus health.

    The unloadable manifest is registered FIRST on purpose: the walk has to carry
    on past it, or every campaign behind a broken one silently leaves the report.
    """

    _git_repo(tmp_path)
    live = _mutation("live", _patch(tmp_path, "live", "VALUE = 1", "VALUE = 2"))
    rotted = _mutation("rotted", _patch(tmp_path, "rotted", "VALUE = 9", "VALUE = 2"))
    loadable = _campaign(tmp_path, "loadable", (live, rotted))
    registry = tmp_path / "registry.json"
    registry.write_text(
        json.dumps(
            {
                "campaigns": [
                    {"manifest": "broken.json"},
                    {"manifest": "loadable.json"},
                ]
            }
        ),
        encoding="utf-8",
    )
    monkeypatch.setattr(
        mutation_patch_audit,
        "_load_registry",
        lambda *_args: json.loads(registry.read_text(encoding="utf-8")),
    )

    def load(manifest: Path, *, repo_root: Path) -> mutation_testing.Campaign:
        if manifest.name == "broken.json":
            raise CampaignError("manifest is not a mapping")
        return loadable

    monkeypatch.setattr(mutation_patch_audit, "load_campaign", load)

    result = mutation_patch_audit.audit_patches(registry, repo_root=tmp_path)

    assert result["campaigns"] == 1
    assert result["mutations"] == 2
    assert result["stale_mutations"] == 1
    assert result["stale_campaigns"] == {"loadable": 1}
    assert [row["mutation_id"] for row in result["stale"]] == ["rotted"]
    assert result["unloadable"] == [
        {"manifest": "broken.json", "detail": "manifest is not a mapping"}
    ]
    assert result["status"] == "STALE"


def test_a_corpus_that_only_fails_to_load_is_not_reported_clean(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Zero stale mutants out of zero loadable campaigns is not a healthy corpus."""

    _git_repo(tmp_path)
    registry = tmp_path / "registry.json"
    registry.write_text(json.dumps({"campaigns": [{"manifest": "broken.json"}]}))
    monkeypatch.setattr(
        mutation_patch_audit,
        "_load_registry",
        lambda *_args: json.loads(registry.read_text(encoding="utf-8")),
    )
    monkeypatch.setattr(
        mutation_patch_audit,
        "load_campaign",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(CampaignError("unreadable")),
    )

    result = mutation_patch_audit.audit_patches(registry, repo_root=tmp_path)

    assert result["stale_mutations"] == 0
    assert result["status"] == "STALE"


def _argv_campaign(
    root: Path, campaign_id: str, argv0: str
) -> mutation_testing.Campaign:
    campaign = _campaign(root, campaign_id, ())
    return dataclasses.replace(campaign, test_argv=(argv0, "-m", "pytest"))


def test_an_absolute_interpreter_is_reported_and_a_bare_one_is_not(
    tmp_path: Path,
) -> None:
    """A bare `python` is rewritten to the runner's own; an absolute path is not.

    The failure this predicts is silent and misattributed: the campaign dies at
    `unmutated baseline failed` naming the tests, never the interpreter.
    """

    _git_repo(tmp_path)
    bare = _argv_campaign(tmp_path, "bare", "python")
    assert mutation_patch_audit._interpreter_verdict(bare, tmp_path) is None

    present = tmp_path / "venv" / "bin" / "python"
    present.parent.mkdir(parents=True)
    present.write_text("", encoding="utf-8")
    pinned = _argv_campaign(tmp_path, "pinned", str(present))
    verdict = mutation_patch_audit._interpreter_verdict(pinned, tmp_path)
    assert verdict is not None
    assert verdict["reason"] == "INTERPRETER_HOST_PINNED"
    assert verdict["interpreter"] == str(present)
    assert verdict["campaign_id"] == "pinned"
    assert "runner's own interpreter" in verdict["detail"]
    assert "absolute path" in verdict["detail"]
    assert "checkout" in verdict["detail"]
    assert "use a bare `python`" in verdict["detail"]

    absent = _argv_campaign(tmp_path, "absent", str(tmp_path / "gone" / "python"))
    verdict = mutation_patch_audit._interpreter_verdict(absent, tmp_path)
    assert verdict is not None
    assert verdict["reason"] == "INTERPRETER_ABSENT"
    assert "does not exist on this host" in verdict["detail"]


def test_receipts_are_indexed_by_their_declared_id_not_their_filename(
    tmp_path: Path,
) -> None:
    """The gate indexes on the `campaign_id` field, and a campaign may carry many.

    Reading `<id>.json` instead is not a near miss: on this corpus it reported 209
    uncovered campaigns where the true number was 71. An audit that overstates the
    gap by 3x gets ignored, which is the same outcome as not running it.
    """

    receipts = tmp_path / "receipts"
    receipts.mkdir()
    (receipts / "run-a.json").write_text(
        json.dumps({"campaign_id": "corpus", "status": "PASS"}), encoding="utf-8"
    )
    (receipts / "run-b.json").write_text(
        json.dumps({"campaign_id": "corpus", "status": "FAIL"}), encoding="utf-8"
    )
    (receipts / "other.json").write_text(
        json.dumps({"campaign_id": "elsewhere", "status": "PASS"}), encoding="utf-8"
    )
    (receipts / "junk.json").write_text("{not json", encoding="utf-8")
    (receipts / "wrong-shape.json").write_text("[]", encoding="utf-8")

    index = mutation_patch_audit._receipts_by_campaign(tmp_path, ["receipts"])

    assert sorted(index) == ["corpus", "elsewhere"]
    assert len(index["corpus"]) == 2
    assert {row["status"] for row in index["corpus"]} == {"PASS", "FAIL"}


def test_a_receipt_is_evidence_only_when_a_known_runner_produced_it(
    tmp_path: Path,
) -> None:
    """PASS is not evidence by itself; the runner that produced it has to be known.

    A receipt with no runner map, or one matching neither this runner nor any
    lineage entry, describes a tree nobody can reproduce -- and says PASS anyway.
    """

    current = {"conductor/mutation_testing.py": "a" * 64}
    tree = _tree(tmp_path)
    campaign = _campaign(tmp_path, "corpus", ())

    assert (
        mutation_patch_audit._receipt_rejection(
            {"status": "PASS", "runner_components_sha256": dict(current)},
            current,
            tmp_path,
            tree,
            campaign,
        )
        is None
    )
    assert (
        mutation_patch_audit._receipt_rejection(
            {"status": "BASELINE_FAILED", "runner_components_sha256": dict(current)},
            current,
            tmp_path,
            tree,
            campaign,
        )
        == "status=BASELINE_FAILED"
    )
    assert (
        mutation_patch_audit._receipt_rejection(
            {"status": "PASS"}, current, tmp_path, tree, campaign
        )
        == "no runner component map"
    )
    assert (
        mutation_patch_audit._receipt_rejection(
            {
                "status": "PASS",
                "runner_components_sha256": {"conductor/mutation_testing.py": "b" * 64},
            },
            current,
            tmp_path,
            tree,
            campaign,
        )
        == "runner components match neither this runner nor any lineage entry"
    )
    # A generated campaign passes with RATCHET_HELD rather than PASS -- its verdict
    # is the survivor set, not the score -- and `mutation_engine_generated` exits 0
    # on either. While this accepted only PASS, every generated campaign was
    # permanently NO_ACCEPTABLE_RECEIPT: the audit could not see one as covered
    # however well it ran, so the engines that exist to remove hand-authored
    # mutants could not produce evidence the audit would take. The runner-component
    # check still applies, which is what makes this narrower than widening the set.
    assert (
        mutation_patch_audit._receipt_rejection(
            {"status": "RATCHET_HELD", "runner_components_sha256": dict(current)},
            current,
            tmp_path,
            tree,
            campaign,
        )
        is None
    )
    assert (
        mutation_patch_audit._receipt_rejection(
            {
                "status": "RATCHET_HELD",
                "runner_components_sha256": {"conductor/mutation_testing.py": "b" * 64},
            },
            current,
            tmp_path,
            tree,
            campaign,
        )
        == "runner components match neither this runner nor any lineage entry"
    )
    assert mutation_patch_audit.PASSING_RECEIPT_STATUSES == frozenset(
        {"PASS", "RATCHET_HELD"}
    )


def test_one_acceptable_receipt_covers_a_campaign_and_none_leaves_it_uncovered(
    tmp_path: Path,
) -> None:
    """Coverage is `any`, not `all` -- and an unusable receipt is not coverage."""

    current = {"conductor/mutation_testing.py": "a" * 64}
    campaign = _campaign(tmp_path, "corpus", ())
    good = {"status": "PASS", "runner_components_sha256": dict(current)}
    stale = {
        "status": "PASS",
        "runner_components_sha256": {"conductor/mutation_testing.py": "b" * 64},
    }
    tree = mutation_patch_audit._TreeHasher(tmp_path)

    assert (
        mutation_patch_audit._evidence_verdict(
            campaign, {"corpus": [stale, good]}, current, tmp_path, tree
        )
        is None
    )

    missing = mutation_patch_audit._evidence_verdict(
        campaign, {}, current, tmp_path, tree
    )
    assert missing is not None
    assert missing["reason"] == "NO_RECEIPT"
    assert missing["campaign_id"] == "corpus"
    assert missing["receipts"] == 0
    assert missing["detail"] == "no receipt declares this campaign_id"

    unusable = mutation_patch_audit._evidence_verdict(
        campaign, {"corpus": [stale, stale]}, current, tmp_path, tree
    )
    assert unusable is not None
    assert unusable["reason"] == "NO_ACCEPTABLE_RECEIPT"
    assert unusable["campaign_id"] == "corpus"
    assert "runner components" in unusable["detail"]
    assert unusable["receipts"] == 2


def _measured(campaign: mutation_testing.Campaign) -> mutation_testing.Campaign:
    """The same campaign, with the question `which of my tests detect anything`."""

    return dataclasses.replace(
        campaign,
        value_analysis=mutation_testing.ValueAnalysisSpec(
            adapter="pytest",
            baseline_repetitions=1,
            contracts=(),
            tests=(),
            mutation_contracts={},
        ),
    )


def _value_receipt(
    runner: dict[str, str], value: object, stamp: str
) -> dict[str, object]:
    return {
        "status": "PASS",
        "runner_components_sha256": dict(runner),
        "generated_at": stamp,
        "test_value": value,
    }


def test_a_campaign_is_unmeasured_or_its_inert_tests_are_named(tmp_path: Path) -> None:
    """Legacy value findings distinguish missing measurement from inert tests."""

    current = {"conductor/mutation_testing.py": "a" * 64}
    campaign = _campaign(tmp_path, "unmeasured", ())
    measured = _measured(_campaign(tmp_path, "measured", ()))
    value = {
        "tests": [
            {"nodeid": "t.py::test_kills", "classification": "CORE"},
            {"nodeid": "t.py::test_inert", "classification": "DELETE_CANDIDATE"},
        ]
    }
    unmeasured, inert = mutation_patch_audit._value_verdicts(
        [campaign, measured],
        {"measured": [_value_receipt(current, value, "20260905T000000Z")]},
        current,
        tmp_path,
        _tree(tmp_path),
    )
    assert [row["campaign_id"] for row in unmeasured] == ["unmeasured"]
    assert unmeasured[0]["reason"] == "NO_VALUE_ANALYSIS"
    assert "nothing measures" in unmeasured[0]["detail"]
    assert [row["nodeid"] for row in inert] == ["t.py::test_inert"]
    assert inert[0]["reason"] == "KILLS_NOTHING"

    stale = _value_receipt(
        {"conductor/mutation_testing.py": "b" * 64}, value, "20260905T000000Z"
    )
    assert mutation_patch_audit._value_verdicts(
        [measured], {"measured": [stale]}, current, tmp_path, _tree(tmp_path)
    ) == ([], [])

    generated, inert = mutation_patch_audit._value_verdicts(
        [campaign],
        {"unmeasured": [_value_receipt(current, value, "20260908T000000Z")]},
        current,
        tmp_path,
        _tree(tmp_path),
    )
    assert generated == []
    assert [row["nodeid"] for row in inert] == ["t.py::test_inert"]

    generated_campaign = dataclasses.replace(campaign, generated=True)
    missing, inert = mutation_patch_audit._value_verdicts(
        [generated_campaign],
        {"unmeasured": [_value_receipt(current, None, "20260908T000000Z")]},
        current,
        tmp_path,
        _tree(tmp_path),
    )
    assert missing == [] and inert == []

    invalid, inert = mutation_patch_audit._value_verdicts(
        [generated_campaign],
        {
            "unmeasured": [
                _value_receipt(
                    current, {"tests": [{"nodeid": "t.py::bad"}]}, "20260908T000000Z"
                )
            ]
        },
        current,
        tmp_path,
        _tree(tmp_path),
    )
    assert invalid[0]["reason"] == "INVALID_VALUE_ANALYSIS"
    assert "nodeid and classification" in invalid[0]["detail"]
    assert inert == []


def test_the_newest_acceptable_receipt_is_the_one_that_speaks(tmp_path: Path) -> None:
    """Older receipts describe tests that have since changed; the newest wins."""

    current = {"conductor/mutation_testing.py": "a" * 64}
    campaign = _campaign(tmp_path, "corpus", ())
    old = {
        "status": "PASS",
        "runner_components_sha256": dict(current),
        "generated_at": "20260901T000000Z",
        "test_value": {"tests": [{"nodeid": "t.py::a", "classification": "CORE"}]},
    }
    new = {
        "status": "PASS",
        "runner_components_sha256": dict(current),
        "generated_at": "20260905T000000Z",
        "test_value": {"tests": [{"nodeid": "t.py::a", "classification": "CORE"}]},
    }

    chosen = mutation_patch_audit._acceptable_receipt(
        campaign, {"corpus": [new, old]}, current, tmp_path, _tree(tmp_path)
    )
    assert chosen is not None and chosen["generated_at"] == "20260905T000000Z"
    assert (
        mutation_patch_audit._acceptable_receipt(campaign, {}, current, tmp_path, _tree(tmp_path))
        is None
    )


def _found(**kwargs: set[str]) -> dict[str, set[str]]:
    return {key: kwargs.get(key, set()) for key in mutation_patch_audit.BASELINE_KEYS}


def test_every_dimension_reduces_to_comparable_ids(tmp_path: Path) -> None:
    """A rotted mutant is identified by campaign AND id; a campaign by its id.

    Keying stale mutants on the mutation id alone would let one campaign's recorded
    debt silence the same-named mutant in every other campaign.
    """

    found = mutation_patch_audit._findings(
        {
            "stale": [
                {"campaign_id": "one", "mutation_id": "rotted"},
                {"campaign_id": "two", "mutation_id": "rotted"},
            ],
            "unloadable": [{"manifest": "broken.json"}],
        },
        {
            "interpreters": [{"campaign_id": "pinned"}],
            "evidence": [{"campaign_id": "uncovered"}],
            "unmeasured": [{"campaign_id": "unmeasured"}],
            "inert_tests": [
                {"campaign_id": "one", "nodeid": "t.py::test_x"},
                {"campaign_id": "two", "nodeid": "t.py::test_x"},
            ],
            "uncovered_files": [{"file": "src/new.py"}],
        },
    )

    assert found["stale_mutations"] == {"one::rotted", "two::rotted"}
    assert found["unloadable_manifests"] == {"broken.json"}
    assert found["host_pinned_interpreters"] == {"pinned"}
    assert found["uncovered_campaigns"] == {"uncovered"}
    assert found["campaigns_without_value_analysis"] == {"unmeasured"}
    assert found["uncovered_changed_files"] == {"src/new.py"}
    # An inert test is keyed by campaign AND nodeid for the same reason a stale
    # mutant is: two campaigns can name the same test.
    assert found["tests_that_kill_nothing"] == {
        "one::t.py::test_x",
        "two::t.py::test_x",
    }
    assert set(found) == set(mutation_patch_audit.BASELINE_KEYS)


def test_the_baseline_ratchet_bites_in_both_directions(tmp_path: Path) -> None:
    """A new finding fails, and so does a recorded one that is now fixed.

    Only reporting regressions leaves the recorded debt frozen forever, which is
    how a gate becomes a check-box: permanently amber, permanently ignored.
    """

    found = _found(
        stale_mutations={"one::rotted"},
        host_pinned_interpreters={"pinned"},
        uncovered_campaigns={"uncovered"},
    )

    clean = mutation_patch_audit._baseline_delta(found, dict(found))
    assert clean["status"] == "CLEAN"
    assert clean["new_host_pinned_interpreters"] == []
    assert clean["resolved_uncovered_campaigns"] == []

    regressed = mutation_patch_audit._baseline_delta(
        found,
        _found(stale_mutations={"one::rotted"}, uncovered_campaigns={"uncovered"}),
    )
    assert regressed["status"] == "REGRESSED"
    assert regressed["new_host_pinned_interpreters"] == ["pinned"]

    stale = mutation_patch_audit._baseline_delta(
        found,
        _found(
            stale_mutations={"one::rotted"},
            host_pinned_interpreters={"pinned"},
            uncovered_campaigns={"uncovered", "already_fixed"},
        ),
    )
    assert stale["status"] == "BASELINE_STALE"
    assert stale["resolved_uncovered_campaigns"] == ["already_fixed"]


def test_a_missing_baseline_makes_every_finding_new(tmp_path: Path) -> None:
    """A deleted baseline must read as `everything is new`, and must not abort.

    Both directions matter: leniency makes deleting the file the cheapest route to
    a green audit, and refusing to run makes the audit unadoptable on any tree that
    has not recorded its debt yet.
    """

    recorded = mutation_patch_audit._load_baseline(Path("absent.json"), tmp_path)
    delta = mutation_patch_audit._baseline_delta(
        _found(host_pinned_interpreters={"pinned"}), recorded
    )
    assert delta["status"] == "REGRESSED"


def _audit_fixture(tmp_path: Path) -> tuple[dict[str, object], dict[str, object]]:
    patches = {
        "status": "STALE",
        "repo_root": str(tmp_path),
        "campaigns": 1,
        "mutations": 1,
        "stale_mutations": 1,
        "stale_campaigns": {"corpus": 1},
        "stale": [{"campaign_id": "corpus", "mutation_id": "rotted"}],
        "unloadable": [],
    }
    repro = {
        "campaigns": 1,
        "host_pinned_interpreters": 1,
        "uncovered_campaigns": 0,
        "campaigns_without_value_analysis": 1,
        "tests_that_kill_nothing": 1,
        "uncovered_changed_files": 0,
        "interpreters": [{"campaign_id": "pinned"}],
        "evidence": [],
        "unmeasured": [{"campaign_id": "unmeasured"}],
        "inert_tests": [{"campaign_id": "corpus", "nodeid": "t.py::test_x"}],
        "uncovered_files": [],
    }
    return patches, repro


def _record_baseline(path: Path, **kwargs: list[str]) -> None:
    path.write_text(
        json.dumps(
            {key: kwargs.get(key, []) for key in mutation_patch_audit.BASELINE_KEYS}
        ),
        encoding="utf-8",
    )


def _run_audit(
    baseline: Path,
    expected: int,
    capsys: pytest.CaptureFixture[str],
    *extra: str,
) -> dict[str, object]:
    assert mutation_patch_audit.main(["--baseline", str(baseline), *extra]) == expected
    return json.loads(capsys.readouterr().out)


def _assert_clean_summary(reported: dict[str, object], tmp_path: Path) -> None:
    assert "stale" not in reported["patches"]
    assert "interpreters" not in reported["reproducibility"]
    assert "unmeasured" not in reported["reproducibility"]
    assert "inert_tests" not in reported["reproducibility"]
    assert "uncovered_files" not in reported["reproducibility"]
    assert reported["patches"]["stale_mutations"] == 1
    assert reported["patches"]["stale_campaigns"] == {"corpus": 1}
    assert reported["patches"]["repo_root"] == str(tmp_path)
    assert reported["patches"]["campaigns"] == 1
    assert reported["patches"]["mutations"] == 1
    assert reported["reproducibility"]["host_pinned_interpreters"] == 1
    assert reported["reproducibility"]["campaigns_without_value_analysis"] == 1
    assert reported["reproducibility"]["tests_that_kill_nothing"] == 1
    assert reported["reproducibility"]["uncovered_changed_files"] == 0
    assert reported["status"] == "CLEAN"
    assert reported["repo_root"] == str(tmp_path)
    assert reported["campaigns"] == 1


def _assert_regressed_summary(
    reported: dict[str, object], patches: dict[str, object]
) -> None:
    assert reported["status"] == "REGRESSED"
    assert reported["patches"] == patches
    assert reported["reproducibility"]["baseline"]["new_host_pinned_interpreters"] == [
        "pinned"
    ]


def test_the_exit_code_and_summary_carry_the_verdict(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    """CI reads the exit code; --summary may drop rows but never the counts."""

    patches, repro = _audit_fixture(tmp_path)
    monkeypatch.setattr(
        mutation_patch_audit,
        "load_registered_campaigns",
        lambda *_a, **_k: ({}, [], []),
    )
    monkeypatch.setattr(
        mutation_patch_audit, "audit_patches", lambda *_a, **_k: dict(patches)
    )
    monkeypatch.setattr(
        mutation_patch_audit, "audit_reproducibility", lambda *_a, **_k: dict(repro)
    )
    baseline = tmp_path / "baseline.json"

    _record_baseline(
        baseline,
        stale_mutations=["corpus::rotted"],
        host_pinned_interpreters=["pinned"],
        campaigns_without_value_analysis=["unmeasured"],
        tests_that_kill_nothing=["corpus::t.py::test_x"],
    )
    reported = _run_audit(baseline, 0, capsys, "--summary")
    _assert_clean_summary(reported, tmp_path)

    # A test that stops detecting anything is a regression in its own right, and
    # is not a rotted patch -- so it exits 7, not 6.
    _record_baseline(
        baseline,
        stale_mutations=["corpus::rotted"],
        host_pinned_interpreters=["pinned"],
        campaigns_without_value_analysis=["unmeasured"],
    )
    reported = _run_audit(baseline, 7, capsys)
    assert reported["reproducibility"]["baseline"]["new_tests_that_kill_nothing"] == [
        "corpus::t.py::test_x"
    ]

    _record_baseline(baseline, host_pinned_interpreters=["pinned"])
    assert _run_audit(baseline, 6, capsys)["status"] == "REGRESSED"

    _record_baseline(baseline, stale_mutations=["corpus::rotted"])
    reported = _run_audit(baseline, 7, capsys)
    _assert_regressed_summary(reported, patches)


def test_write_baseline_records_instead_of_judging(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    """Recording the debt is how it is paid down; it must not also pass judgement."""

    monkeypatch.setattr(
        mutation_patch_audit,
        "load_registered_campaigns",
        lambda *_args, **_kwargs: ({}, [], []),
    )
    monkeypatch.setattr(
        mutation_patch_audit,
        "audit_patches",
        lambda *_a, **_k: {
            "status": "CLEAN",
            "repo_root": str(tmp_path),
            "campaigns": 0,
            "stale": [{"campaign_id": "corpus", "mutation_id": "rotted"}],
            "unloadable": [],
        },
    )
    monkeypatch.setattr(
        mutation_patch_audit,
        "audit_reproducibility",
        lambda *_a, **_k: {
            "interpreters": [{"campaign_id": "pinned"}],
            "evidence": [{"campaign_id": "uncovered"}],
            "unmeasured": [{"campaign_id": "unmeasured"}],
            "inert_tests": [{"campaign_id": "corpus", "nodeid": "t.py::test_x"}],
            "uncovered_files": [],
        },
    )
    baseline = tmp_path / "baseline.json"

    assert (
        mutation_patch_audit.main(
            ["--baseline", str(baseline), "--write-baseline", "--summary"]
        )
        == 0
    )
    capsys.readouterr()
    recorded = json.loads(baseline.read_text(encoding="utf-8"))
    assert baseline.read_bytes().endswith(b"\n")
    assert recorded["schema_version"] == 1
    assert "ratchet" in recorded["note"]
    assert recorded["stale_mutations"] == ["corpus::rotted"]
    assert recorded["host_pinned_interpreters"] == ["pinned"]
    assert recorded["uncovered_campaigns"] == ["uncovered"]
    assert recorded["campaigns_without_value_analysis"] == ["unmeasured"]
    assert recorded["tests_that_kill_nothing"] == ["corpus::t.py::test_x"]


def test_optional_attribution_rejects_each_malformed_shape(tmp_path: Path) -> None:
    campaign = dataclasses.replace(_campaign(tmp_path, "generated", ()), generated=True)
    current = {"runner": "digest"}
    inert_row = {"nodeid": "t.py::inert", "classification": "DELETE_CANDIDATE"}
    for value in [
        [],
        {"tests": "not a list"},
        {"tests": [None]},
        {"tests": [{"nodeid": 1, "classification": "CORE"}]},
        {"tests": [{"nodeid": "t.py::bad", "classification": None}, inert_row]},
    ]:
        unmeasured, inert = mutation_patch_audit._value_verdicts(
            [campaign],
            {
                "generated": [
                    {
                        "status": "PASS",
                        "runner_components_sha256": current,
                        "test_value": value,
                    }
                ]
            },
            current,
            tmp_path,
            _tree(tmp_path),
        )
        assert len(unmeasured) == 1
        assert unmeasured[0]["campaign_id"] == "generated"
        assert unmeasured[0]["reason"] == "INVALID_VALUE_ANALYSIS"
        assert inert == []
    unmeasured, inert = mutation_patch_audit._value_verdicts(
        [campaign],
        {
            "generated": [
                {
                    "status": "PASS",
                    "runner_components_sha256": current,
                    "test_value": {"tests": [inert_row]},
                }
            ]
        },
        current,
        tmp_path,
        _tree(tmp_path),
    )
    assert unmeasured == []
    assert inert == [
        {"campaign_id": "generated", "nodeid": "t.py::inert", "reason": "KILLS_NOTHING"}
    ]

    second = dataclasses.replace(campaign, campaign_id="second")
    unmeasured, inert = mutation_patch_audit._value_verdicts(
        [campaign, second],
        {
            "generated": [
                {
                    "status": "PASS",
                    "runner_components_sha256": current,
                    "test_value": [],
                }
            ],
            "second": [
                {
                    "status": "PASS",
                    "runner_components_sha256": current,
                    "test_value": {"tests": [inert_row]},
                }
            ],
        },
        current,
        tmp_path,
        _tree(tmp_path),
    )
    assert unmeasured == [
        {
            "campaign_id": "generated",
            "reason": "INVALID_VALUE_ANALYSIS",
            "detail": "receipt test_value must contain a tests list",
        }
    ]
    assert inert == [
        {"campaign_id": "second", "nodeid": "t.py::inert", "reason": "KILLS_NOTHING"}
    ]


def test_receipt_and_baseline_io_validate_shapes_and_preserve_later_rows(
    tmp_path: Path,
) -> None:
    receipts = tmp_path / "receipts"
    receipts.mkdir()
    (receipts / "00-malformed.json").write_text("[]", encoding="utf-8")
    (receipts / "01-valid.json").write_text('{"campaign_id":"later"}', encoding="utf-8")
    assert mutation_patch_audit._receipts_by_campaign(tmp_path, ["receipts"]) == {
        "later": [{"campaign_id": "later"}]
    }
    with pytest.raises(CampaignError, match="registry.receipt_directories"):
        mutation_patch_audit._receipts_by_campaign(tmp_path, ["../outside"])
    baseline = tmp_path / "baseline.json"
    key = mutation_patch_audit.BASELINE_KEYS[0]
    for invalid in ["not a list", [1]]:
        baseline.write_text(json.dumps({key: invalid}), encoding="utf-8")
        with pytest.raises(CampaignError, match="must be an array of ids"):
            mutation_patch_audit._load_baseline(baseline, tmp_path)
    mutation_patch_audit._write_baseline(Path("relative.json"), tmp_path, _found())
    assert (tmp_path / "relative.json").read_bytes().endswith(b"\n")


def test_empty_patch_corpus_is_clean_and_external_interpreter_is_identified(
    tmp_path: Path,
) -> None:
    import sys

    result = mutation_patch_audit.audit_patches(
        Path("registry.json"), repo_root=tmp_path, loaded=({}, [], [])
    )
    assert result["status"] == "CLEAN"
    assert result["repo_root"] == str(tmp_path)
    assert result["campaigns"] == result["mutations"] == 0
    verdict = mutation_patch_audit._interpreter_verdict(
        _argv_campaign(tmp_path, "external", sys.executable), tmp_path
    )
    assert verdict is not None
    assert "host's filesystem" in verdict["detail"]


def test_audit_reproducibility_resolves_lineage_from_runner_component_root(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """``audit_reproducibility`` must pin evidence acceptance to
    ``runner_component_root()``, never to ``repo_root`` -- the same distinction
    that makes a src layout's bare ``conductor/...`` literals resolve correctly.

    The receipt's ``runner_components_sha256`` matches neither the current runner
    nor a lineage entry recorded at ``repo_root`` -- only one recorded under a
    *separate* ``package_root`` tree. If the audit ever regresses to consulting
    ``repo_root`` for lineage (or a hardcoded location) instead of calling
    ``runner_component_root()``, this receipt reads as uncovered.
    """

    repo_root = tmp_path / "repo"
    package_root = tmp_path / "elsewhere" / "src"
    (repo_root / "receipts").mkdir(parents=True)
    (package_root / "conductor").mkdir(parents=True)

    recorded = {"conductor/mutation_testing.py": "b" * 64}
    current = {"conductor/mutation_testing.py": "a" * 64}
    lineage = {
        "schema_version": 1,
        "entries": [{"runner_components_sha256": dict(recorded)}],
    }
    (package_root / "conductor" / "mutation_runner_lineage.json").write_text(
        json.dumps(lineage), encoding="utf-8"
    )
    # repo_root deliberately carries no lineage file at all -- if the audit ever
    # looked there instead, this receipt would be rejected.
    assert not (repo_root / "conductor" / "mutation_runner_lineage.json").exists()

    (repo_root / "receipts" / "corpus.json").write_text(
        json.dumps(
            {
                "campaign_id": "corpus",
                "status": "PASS",
                "runner_components_sha256": dict(recorded),
            }
        ),
        encoding="utf-8",
    )

    campaign = _campaign(repo_root, "corpus", ())
    registry = {"receipt_directories": ["receipts"]}

    monkeypatch.setattr(
        mutation_patch_audit, "runner_component_root", lambda: package_root
    )
    monkeypatch.setattr(
        mutation_patch_audit, "_runner_components_sha256", lambda: dict(current)
    )

    result = mutation_patch_audit.audit_reproducibility(
        [campaign], registry, repo_root=repo_root
    )
    assert result["uncovered_campaigns"] == 0
    assert result["evidence"] == []

    # Point the audit at repo_root instead -- the same tree, minus the lineage
    # file -- to prove the prior pass depended on consulting package_root.
    monkeypatch.setattr(
        mutation_patch_audit, "runner_component_root", lambda: repo_root
    )
    regressed = mutation_patch_audit.audit_reproducibility(
        [campaign], registry, repo_root=repo_root
    )
    assert regressed["uncovered_campaigns"] == 1
    assert regressed["evidence"][0]["reason"] == "NO_ACCEPTABLE_RECEIPT"


def _evidence_env(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> dict[str, str]:
    """Pin the runner-component facts so only the stale-byte question varies."""

    current = {"conductor/mutation_testing.py": "a" * 64}
    monkeypatch.setattr(
        mutation_patch_audit, "runner_component_root", lambda: tmp_path
    )
    monkeypatch.setattr(
        mutation_patch_audit, "_runner_components_sha256", lambda: dict(current)
    )
    return current


def _receipt_file(root: Path, name: str, receipt: dict[str, object]) -> None:
    (root / "receipts").mkdir(exist_ok=True)
    (root / "receipts" / f"{name}.json").write_text(
        json.dumps(receipt), encoding="utf-8"
    )


def test_a_receipt_pinning_other_bytes_is_not_evidence(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """PR #28 passed with a receipt hashing files the PR tip had rewritten.

    A PASS over bytes this tree does not contain is a pass over code nobody
    shipped, and must read as a rejection. The one exemption is symbol-pinned
    files: their whole-file digest is expected to move when the edit sits
    outside the pinned symbols, and the symbol pins say so precisely.
    """

    _git_repo(tmp_path)
    current = _evidence_env(tmp_path, monkeypatch)
    digest = mutation_testing._sha256(tmp_path / "source.py")  # noqa: SLF001
    campaign = _campaign(tmp_path, "corpus", ())
    registry = {"receipt_directories": ["receipts"]}
    fresh = {
        "campaign_id": "corpus",
        "status": "PASS",
        "runner_components_sha256": dict(current),
        "source_sha256": {"source.py": digest},
    }
    stale = {**fresh, "source_sha256": {"source.py": "b" * 64}}

    rejection = mutation_patch_audit._receipt_rejection(
        stale, current, tmp_path, _tree(tmp_path), campaign
    )
    assert rejection == "source hashes differ from this tree: ['source.py']"
    assert (
        mutation_patch_audit._receipt_rejection(
            fresh, current, tmp_path, _tree(tmp_path), campaign
        )
        is None
    )
    # A pinned file the audited tree lacks is the loudest form of stale.
    absent = {**fresh, "source_sha256": {"gone.py": digest}}
    assert "gone.py" in (
        mutation_patch_audit._receipt_rejection(
            absent, current, tmp_path, _tree(tmp_path), campaign
        )
        or ""
    )

    _receipt_file(tmp_path, "stale", stale)
    result = mutation_patch_audit.audit_reproducibility(
        [campaign], registry, repo_root=tmp_path
    )
    assert result["uncovered_campaigns"] == 1
    assert result["evidence"][0]["reason"] == "NO_ACCEPTABLE_RECEIPT"
    assert "source hashes differ" in result["evidence"][0]["detail"]

    # The same stale receipt under a campaign that pins source.py symbol-by-
    # symbol stays acceptable: out-of-symbol edits move whole-file digests.
    symbols = dataclasses.replace(
        campaign, source_symbols={"source.py": {"VALUE": digest}}
    )
    assert (
        mutation_patch_audit._receipt_rejection(
            stale, current, tmp_path, _tree(tmp_path), symbols
        )
        is None
    )


def test_one_stale_hash_fails_the_whole_audit(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    """The fixture PR from the brief: one stale hash, audit exits 7, not 0."""

    _git_repo(tmp_path)
    current = _evidence_env(tmp_path, monkeypatch)
    campaign = _campaign(tmp_path, "corpus", ())
    _receipt_file(
        tmp_path,
        "run",
        {
            "campaign_id": "corpus",
            "status": "PASS",
            "runner_components_sha256": dict(current),
            "source_sha256": {"source.py": "b" * 64},
        },
    )
    monkeypatch.setattr(
        mutation_patch_audit,
        "load_registered_campaigns",
        lambda *_a, **_k: ({"receipt_directories": ["receipts"]}, [campaign], []),
    )
    monkeypatch.setattr(
        mutation_patch_audit,
        "audit_patches",
        lambda *_a, **_k: {
            "status": "CLEAN",
            "repo_root": str(tmp_path),
            "campaigns": 1,
            "stale": [],
            "unloadable": [],
        },
    )
    baseline = tmp_path / "baseline.json"

    assert (
        mutation_patch_audit.main(["--baseline", str(baseline), "--summary"]) == 7
    )
    reported = json.loads(capsys.readouterr().out)
    assert reported["status"] == "REGRESSED"
    delta = reported["reproducibility"]["baseline"]
    assert delta["new_uncovered_campaigns"] == ["corpus"]


def test_a_new_file_beside_a_covered_file_is_reported_uncovered(
    tmp_path: Path,
) -> None:
    """The mirror half of PR #28: in-scope-looking changes nobody pins.

    A file beside pinned files, or deeper inside a directory the campaign's
    pins occupy in its own language, is inside territory the corpus claims to
    speak for; a doc file or an unrelated crate's source is not, and must not
    flood the finding.
    """

    covered = dataclasses.replace(
        _campaign(tmp_path, "covered", ()),
        source_sha256={"src/one.py": "d" * 64},
        test_sha256={"tests/test_one.py": "e" * 64},
    )
    rows = mutation_patch_audit._uncovered_changed_files(
        {
            "src/one.py",  # pinned: covered
            "tests/test_one.py",  # pinned: covered
            "src/two.py",  # beside a pinned file
            "src/nested/three.py",  # deeper inside the campaign's territory
            "docs/readme.md",  # outside every territory, wrong suffix
            "native/other/lib.rs",  # right suffix, no rust campaign anywhere
        },
        [covered],
    )
    assert [row["file"] for row in rows] == ["src/nested/three.py", "src/two.py"]
    assert all(row["reason"] == "NO_PINNING_CAMPAIGN" for row in rows)
    assert "plan one" in rows[0]["detail"]

    # Legacy whole-tree mode: no changed-file set, no per-PR dimension.
    assert mutation_patch_audit._uncovered_changed_files(set(), [covered]) == []


def test_unpinned_changed_files_fail_the_whole_audit(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    """The dimension is not advisory: an unpinned beside-file exits 7."""

    _git_repo(tmp_path)
    current = _evidence_env(tmp_path, monkeypatch)
    covered = dataclasses.replace(
        _campaign(tmp_path, "covered", ()),
        source_sha256={"source.py": "d" * 64},
        test_sha256={},
    )
    # An acceptable receipt, so the ONLY failing dimension is the new one: the
    # campaign itself is covered while the file beside its pin is not.
    _receipt_file(
        tmp_path,
        "covered",
        {
            "campaign_id": "covered",
            "status": "PASS",
            "runner_components_sha256": dict(current),
            "source_sha256": {
                "source.py": mutation_testing._sha256(tmp_path / "source.py")  # noqa: SLF001
            },
        },
    )
    monkeypatch.setattr(
        mutation_patch_audit,
        "load_registered_campaigns",
        lambda *_a, **_k: ({"receipt_directories": ["receipts"]}, [covered], []),
    )
    monkeypatch.setattr(
        mutation_patch_audit,
        "audit_patches",
        lambda *_a, **_k: {
            "status": "CLEAN",
            "repo_root": str(tmp_path),
            "campaigns": 1,
            "stale": [],
            "unloadable": [],
        },
    )
    baseline = tmp_path / "baseline.json"
    # The campaign's own recorded debt (it has no value analysis) is paid for
    # up front, so the audit fails on the new dimension and nothing else.
    _record_baseline(baseline, campaigns_without_value_analysis=["covered"])

    assert (
        mutation_patch_audit.main(
            [
                "--baseline",
                str(baseline),
                "--summary",
                "--changed-file",
                "source.py",
                "--changed-file",
                "sibling.py",
            ]
        )
        == 7
    )
    reported = json.loads(capsys.readouterr().out)
    delta = reported["reproducibility"]["baseline"]
    assert delta["new_uncovered_changed_files"] == ["sibling.py"]
    assert delta["status"] == "REGRESSED"
    assert reported["reproducibility"]["uncovered_changed_files"] == 1
