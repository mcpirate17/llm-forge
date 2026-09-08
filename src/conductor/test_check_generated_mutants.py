"""Contract tests for the hand-authored-campaign guard.

No parametrize: mutation campaigns pin bare node ids, and pytest's
``name[params]`` expansion breaks those pins.
"""

from __future__ import annotations

import json
import subprocess

import pytest

from conductor.check_generated_mutants import (
    declared_engine,
    generated_engines,
    main,
    parse_argv,
    paths_at_base,
)

MANIFEST = "conductor/mutation_campaigns/x.json"
HAND = "reviewed_unified_diff"


def _campaign(engine, **extra):
    payload = {"campaign_id": "x", "title": "t", **extra}
    if engine is not None:
        payload["mutation_engine"] = engine
    return json.dumps(payload)


def _git(repo, *args):
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)


def _repo(tmp_path):
    """A repo whose base commit holds one hand-written campaign.

    `old.json` is the grandfathered campaign; `new.json` and `gen.json` are
    written into the working tree afterwards and so are absent from the base.
    """

    root = tmp_path / "repo"
    campaigns = root / "conductor" / "mutation_campaigns"
    campaigns.mkdir(parents=True)
    (campaigns / "old.json").write_text(_campaign(HAND), encoding="utf-8")
    _git(root, "init", "-q")
    _git(root, "config", "user.email", "t@example.com")
    _git(root, "config", "user.name", "t")
    _git(root, "add", "-A")
    _git(root, "commit", "-qm", "base")
    base = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "HEAD"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    (campaigns / "new.json").write_text(_campaign(HAND), encoding="utf-8")
    (campaigns / "gen.json").write_text(
        _campaign(sorted(generated_engines())[0]), encoding="utf-8"
    )
    return root, base


def _run(root, base, paths, monkeypatch):
    monkeypatch.chdir(root)
    monkeypatch.setattr(
        "sys.argv", ["guard", "--repo", str(root), "--base", base, *paths]
    )
    return main()


def test_rejects_the_hand_authored_engines():
    hand = ("reviewed_unified_diff", "reviewed_patch", "diff", "patch")
    assert [e for e in hand if declared_engine(MANIFEST, _campaign(e)) == e] == list(
        hand
    )


def test_admits_every_engine_the_runner_can_execute():
    engines = sorted(generated_engines())
    assert engines, "the runner declares no generated engines"
    assert [
        e for e in engines if declared_engine(MANIFEST, _campaign(e)) is not None
    ] == []


def test_the_admitted_set_is_the_runners_own():
    from conductor.mutation_engine_generated import GENERATED_ENGINES

    assert generated_engines() == GENERATED_ENGINES


def test_a_manifest_declaring_no_engine_is_an_offence():
    assert declared_engine(MANIFEST, _campaign(None)) == "<none declared>"


def test_ignores_the_registry_and_the_baseline():
    registry = json.dumps({"campaigns": [], "schema_version": "v1"})
    baseline = json.dumps({"stale_mutations": [], "schema_version": "v1"})
    assert (
        declared_engine("conductor/mutation_campaigns/registry.json", registry) is None
    )
    assert (
        declared_engine(
            "conductor/mutation_campaigns/reproducibility_baseline.json", baseline
        )
        is None
    )


def test_ignores_receipts_and_patches_below_the_campaign_directory():
    hand = _campaign(HAND)
    assert declared_engine("conductor/mutation_campaigns/receipts/x.json", hand) is None
    assert (
        declared_engine("conductor/mutation_campaigns/registry.d/x.json", hand) is None
    )
    assert (
        declared_engine("conductor/mutation_campaigns/patches/x/1.patch.json", hand)
        is None
    )


def test_ignores_manifests_outside_the_campaign_directory():
    hand = _campaign(HAND)
    assert declared_engine("research/reports/x.json", hand) is None
    assert declared_engine("conductor/mutation_campaigns/x.md", hand) is None


def test_ignores_a_file_that_is_not_json():
    assert declared_engine(MANIFEST, "not json at all") is None


def test_paths_at_base_reports_only_what_the_base_commit_held(tmp_path):
    root, base = _repo(tmp_path)
    paths = [
        "conductor/mutation_campaigns/old.json",
        "conductor/mutation_campaigns/new.json",
    ]
    assert paths_at_base(str(root), base, paths) == {
        "conductor/mutation_campaigns/old.json"
    }
    assert paths_at_base(str(root), base, []) == frozenset()


def test_a_bad_base_raises_rather_than_calling_everything_new(tmp_path):
    root, _ = _repo(tmp_path)
    with pytest.raises(RuntimeError, match="ls-tree"):
        paths_at_base(str(root), "not-a-ref", ["conductor/mutation_campaigns/old.json"])


def test_parse_argv_takes_the_flags_and_leaves_the_paths():
    assert parse_argv(["--repo", "/r", "--base", "abc", "a.json", "b.json"]) == (
        "/r",
        "abc",
        ["a.json", "b.json"],
    )
    assert parse_argv(["--base", "abc", "--repo", "/r"]) == ("/r", "abc", [])


def test_parse_argv_refuses_to_guess_a_missing_base():
    with pytest.raises(SystemExit):
        parse_argv(["a.json"])
    with pytest.raises(SystemExit):
        parse_argv(["--repo", "/r", "a.json"])


def test_editing_a_campaign_that_existed_at_the_base_is_allowed(tmp_path, monkeypatch):
    root, base = _repo(tmp_path)
    edited = root / "conductor" / "mutation_campaigns" / "old.json"
    edited.write_text(_campaign(HAND, source_sha256={"a.py": "beef"}), encoding="utf-8")
    assert _run(root, base, ["conductor/mutation_campaigns/old.json"], monkeypatch) == 0


def test_a_new_hand_written_campaign_is_refused(tmp_path, monkeypatch, capsys):
    root, base = _repo(tmp_path)
    assert _run(root, base, ["conductor/mutation_campaigns/new.json"], monkeypatch) == 1
    assert "new.json" in capsys.readouterr().err


def test_a_new_generated_campaign_is_admitted(tmp_path, monkeypatch):
    root, base = _repo(tmp_path)
    assert _run(root, base, ["conductor/mutation_campaigns/gen.json"], monkeypatch) == 0


def test_names_only_the_offender(tmp_path, monkeypatch, capsys):
    root, base = _repo(tmp_path)
    paths = [
        "conductor/mutation_campaigns/gen.json",
        "conductor/mutation_campaigns/old.json",
        "conductor/mutation_campaigns/new.json",
    ]
    assert _run(root, base, paths, monkeypatch) == 1
    err = capsys.readouterr().err
    assert "new.json" in err
    assert "gen.json" not in err
    assert "old.json" not in err


def test_no_changed_files_is_not_an_offence(tmp_path, monkeypatch):
    root, base = _repo(tmp_path)
    assert _run(root, base, [], monkeypatch) == 0


def test_a_missing_path_does_not_stop_the_scan(tmp_path, monkeypatch, capsys):
    """A missing path must be skipped, not end the loop.

    Ordered so the offender comes last: if the OSError branch breaks out of the
    loop instead of continuing, the offender is never examined and main returns
    0. This is the only test that distinguishes the two.
    """

    root, base = _repo(tmp_path)
    paths = [
        "conductor/mutation_campaigns/gone.json",
        "conductor/mutation_campaigns/new.json",
    ]
    assert _run(root, base, paths, monkeypatch) == 1
    assert "new.json" in capsys.readouterr().err
