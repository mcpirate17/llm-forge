from __future__ import annotations

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


def test_the_exit_code_and_summary_carry_the_verdict(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    """CI reads the exit code; --summary may drop rows but never the counts."""

    stale = {
        "status": "STALE",
        "stale_mutations": 3,
        "stale_campaigns": {"corpus": 3},
        "stale": [{"campaign_id": "corpus", "mutation_id": "rotted"}],
    }
    monkeypatch.setattr(
        mutation_patch_audit, "audit_patches", lambda *_args, **_kwargs: dict(stale)
    )

    assert mutation_patch_audit.main(["--summary"]) == 6
    reported = json.loads(capsys.readouterr().out)
    assert "stale" not in reported
    assert reported["stale_mutations"] == 3
    assert reported["stale_campaigns"] == {"corpus": 3}

    monkeypatch.setattr(
        mutation_patch_audit,
        "audit_patches",
        lambda *_args, **_kwargs: {"status": "CLEAN", "stale": []},
    )
    assert mutation_patch_audit.main([]) == 0
