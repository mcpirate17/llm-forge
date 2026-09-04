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
    assert "source.py" in verdict["detail"]


def test_the_check_runs_against_the_repo_root_not_the_process_directory(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A check run in the wrong tree reports every patch clean and audits nothing."""

    _git_repo(tmp_path)
    live = _mutation("live", _patch(tmp_path, "live", "VALUE = 1", "VALUE = 2"))
    campaign = _campaign(tmp_path, "corpus", (live,))
    elsewhere = tmp_path.parent / "elsewhere"
    elsewhere.mkdir()
    _git_repo(elsewhere)
    (elsewhere / "source.py").write_text("VALUE = 99\n", encoding="utf-8")
    monkeypatch.chdir(elsewhere)

    assert mutation_patch_audit._patch_verdict(campaign, live, tmp_path) is None
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

    absent = _argv_campaign(tmp_path, "absent", str(tmp_path / "gone" / "python"))
    verdict = mutation_patch_audit._interpreter_verdict(absent, tmp_path)
    assert verdict is not None
    assert verdict["reason"] == "INTERPRETER_ABSENT"


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

    assert (
        mutation_patch_audit._receipt_rejection(
            {"status": "PASS", "runner_components_sha256": dict(current)},
            current,
            tmp_path,
        )
        is None
    )
    assert (
        mutation_patch_audit._receipt_rejection(
            {"status": "BASELINE_FAILED", "runner_components_sha256": dict(current)},
            current,
            tmp_path,
        )
        == "status=BASELINE_FAILED"
    )
    assert (
        mutation_patch_audit._receipt_rejection({"status": "PASS"}, current, tmp_path)
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
        )
        == "runner components match neither this runner nor any lineage entry"
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

    assert (
        mutation_patch_audit._evidence_verdict(
            campaign, {"corpus": [stale, good]}, current, tmp_path
        )
        is None
    )

    missing = mutation_patch_audit._evidence_verdict(campaign, {}, current, tmp_path)
    assert missing is not None
    assert missing["reason"] == "NO_RECEIPT"
    assert missing["receipts"] == 0

    unusable = mutation_patch_audit._evidence_verdict(
        campaign, {"corpus": [stale, stale]}, current, tmp_path
    )
    assert unusable is not None
    assert unusable["reason"] == "NO_ACCEPTABLE_RECEIPT"
    assert unusable["receipts"] == 2


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
        },
    )

    assert found["stale_mutations"] == {"one::rotted", "two::rotted"}
    assert found["unloadable_manifests"] == {"broken.json"}
    assert found["host_pinned_interpreters"] == {"pinned"}
    assert found["uncovered_campaigns"] == {"uncovered"}


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


def test_the_exit_code_and_summary_carry_the_verdict(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    """CI reads the exit code; --summary may drop rows but never the counts.

    6 and 7 are distinct on purpose: a rotted patch is repaired by regenerating the
    mutant, an unreproducible campaign by re-running it, and one exit code for both
    hides which.
    """

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
        "interpreters": [{"campaign_id": "pinned"}],
        "evidence": [],
    }
    monkeypatch.setattr(
        mutation_patch_audit,
        "load_registered_campaigns",
        lambda *_args, **_kwargs: ({}, [], []),
    )
    monkeypatch.setattr(
        mutation_patch_audit, "audit_patches", lambda *_a, **_k: dict(patches)
    )
    monkeypatch.setattr(
        mutation_patch_audit, "audit_reproducibility", lambda *_a, **_k: dict(repro)
    )
    baseline = tmp_path / "baseline.json"

    def record(**kwargs: list[str]) -> None:
        baseline.write_text(
            json.dumps(
                {key: kwargs.get(key, []) for key in mutation_patch_audit.BASELINE_KEYS}
            ),
            encoding="utf-8",
        )

    args = ["--baseline", str(baseline)]

    record(stale_mutations=["corpus::rotted"], host_pinned_interpreters=["pinned"])
    assert mutation_patch_audit.main([*args, "--summary"]) == 0
    reported = json.loads(capsys.readouterr().out)
    assert "stale" not in reported["patches"]
    assert "interpreters" not in reported["reproducibility"]
    assert reported["patches"]["stale_mutations"] == 1
    assert reported["patches"]["stale_campaigns"] == {"corpus": 1}
    assert reported["reproducibility"]["host_pinned_interpreters"] == 1
    assert reported["status"] == "CLEAN"

    record(host_pinned_interpreters=["pinned"])
    assert mutation_patch_audit.main(args) == 6
    assert json.loads(capsys.readouterr().out)["status"] == "REGRESSED"

    record(stale_mutations=["corpus::rotted"])
    assert mutation_patch_audit.main(args) == 7
    reported = json.loads(capsys.readouterr().out)
    assert reported["status"] == "REGRESSED"
    assert reported["reproducibility"]["baseline"]["new_host_pinned_interpreters"] == [
        "pinned"
    ]


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
    assert recorded["stale_mutations"] == ["corpus::rotted"]
    assert recorded["host_pinned_interpreters"] == ["pinned"]
    assert recorded["uncovered_campaigns"] == ["uncovered"]
