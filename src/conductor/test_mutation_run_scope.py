"""Mutation commands must not spend work outside the current agent's files."""

from __future__ import annotations

import json
import re
from pathlib import Path
from types import SimpleNamespace

import pytest

from conductor import mutation_run_scope as scope
from conductor.mutation_scope import CampaignError


def campaign(root: Path, *sources: str, engine: str = "fest") -> SimpleNamespace:
    for source in sources:
        path = root / source
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("x = 1\n")
    return SimpleNamespace(
        mutation_engine=engine,
        source=sources,
        source_sha256=dict.fromkeys(sources, "pin"),
        test_sha256={},
        options={},
        operators=(),
    )


def changes(monkeypatch: pytest.MonkeyPatch, *paths: str) -> None:
    monkeypatch.setattr(scope, "changed_sources", lambda *a, **kw: set(paths))


def test_agent_scope_and_explicit_selection_cannot_admit_a_neighbour(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    manifest = campaign(tmp_path, "mine.py", "other.py")
    changes(monkeypatch, "mine.py")
    with pytest.raises(CampaignError, match="outside this agent's changes"):
        scope.validate_run_scope(manifest, repo_root=tmp_path)
    with pytest.raises(CampaignError, match="--only names files outside"):
        scope.validate_run_scope(manifest, repo_root=tmp_path, only=["other.py"])
    manifest.source = ("mine.py",)
    assert scope.validate_run_scope(manifest, repo_root=tmp_path, only=["mine.py"]) == [
        "mine.py"
    ]
    manifest.source = ("mine.py", "other.py")
    changes(monkeypatch, "mine.py", "other.py")
    with pytest.raises(CampaignError, match="other.py"):
        scope.validate_run_scope(manifest, repo_root=tmp_path, only=["mine.py"])

    calls = []

    def changed(base: str, *, repo_root: Path, owner: str | None) -> set[str]:
        calls.append((base, repo_root, owner))
        return {"mine.py"}

    monkeypatch.setattr(scope, "changed_sources", changed)
    scope.validate_run_scope(
        campaign(tmp_path, "mine.py"),
        repo_root=tmp_path,
        owner="agent",
        base="base-ref",
    )
    assert calls == [("base-ref", tmp_path, "agent")]

    from conductor import mutation_engine_generated as runner

    broad = campaign(tmp_path, "mine.py", "other.py")
    monkeypatch.setattr(runner, "REPO_ROOT", tmp_path)
    monkeypatch.setattr(runner, "load_generated_campaign", lambda path: broad)
    engine_calls = []

    def adapter(engine):
        engine_calls.append(engine)
        raise RuntimeError("engine reached")

    monkeypatch.setattr(runner, "adapter_for", adapter)
    args = ["run", "campaign.json", "--allow-mutations", "--owner", "agent"]
    assert runner.main(args) == 4
    assert json.loads(capsys.readouterr().out)["status"] == "REFUSED"
    assert engine_calls == []


def test_changed_test_admits_only_its_named_source(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    manifest = campaign(tmp_path, "subject.py", "other.py")
    changes(monkeypatch, "tests/test_subject.py")
    manifest.test_sha256 = {"tests/test_subject.py": "pin"}
    with pytest.raises(CampaignError, match="other.py"):
        scope.validate_run_scope(manifest, repo_root=tmp_path)
    manifest.source = ("subject.py",)
    assert scope.validate_run_scope(manifest, repo_root=tmp_path) == ["subject.py"]
    manifest.test_sha256 = {}
    with pytest.raises(CampaignError, match="subject.py"):
        scope.validate_run_scope(manifest, repo_root=tmp_path)

    manifest = campaign(tmp_path, "a/subject.py", "b/subject.py")
    manifest.test_sha256 = {"a/test_subject.py": "pin"}
    changes(monkeypatch, "a/test_subject.py")
    with pytest.raises(CampaignError, match="b/subject.py"):
        scope.validate_run_scope(manifest, repo_root=tmp_path)
    manifest.source = ("a/subject.py",)
    assert scope.validate_run_scope(manifest, repo_root=tmp_path) == ["a/subject.py"]


def test_rust_package_relative_path_is_checked_against_repo_relative_diff(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    manifest = campaign(tmp_path, "crate/src/lib.rs", engine="cargo-mutants")
    manifest.source = ("src/lib.rs",)
    manifest.options = {"package_root": "crate"}
    changes(monkeypatch, "crate/src/lib.rs")
    assert scope.validate_run_scope(manifest, repo_root=tmp_path) == [
        "crate/src/lib.rs"
    ]
    manifest.source = ("src/**/*.rs",)
    with pytest.raises(CampaignError, match="exact repository files"):
        scope.validate_run_scope(manifest, repo_root=tmp_path)
    manifest = campaign(tmp_path, "src/lib.rs", engine="cargo-mutants")
    changes(monkeypatch, "src/lib.rs")
    assert scope.validate_run_scope(manifest, repo_root=tmp_path) == ["src/lib.rs"]


def test_symlink_escape_and_unpinned_source_refuse(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = tmp_path / "repo"
    root.mkdir()
    manifest = campaign(root, "mine.py")
    (root / "nested").mkdir()
    manifest.source = ("nested/../mine.py",)
    with pytest.raises(CampaignError, match="exact repository files"):
        scope.validate_run_scope(manifest, repo_root=root)
    manifest.source = ("mine.py",)
    changes(monkeypatch, "mine.py")
    manifest.source_sha256 = {}
    with pytest.raises(CampaignError, match="pinned"):
        scope.validate_run_scope(manifest, repo_root=root)
    manifest.source = ()
    with pytest.raises(CampaignError, match="pinned"):
        scope.validate_run_scope(manifest, repo_root=root)
    outside = tmp_path / "outside.py"
    outside.write_text("x = 2\n")
    (root / "link.py").symlink_to(outside)
    manifest.source = ("link.py",)
    with pytest.raises(CampaignError, match="outside the checkout"):
        scope.validate_run_scope(manifest, repo_root=root)

    manifest = campaign(root, "mine.py")
    changes(monkeypatch, "mine.py")
    for bad in (
        "*.py",
        "?.py",
        "[a].py",
        "../escape.py",
        "/tmp/escape.py",
        "missing.py",
    ):
        manifest.source = (bad,)
        with pytest.raises(CampaignError, match="mutation scope"):
            scope.validate_run_scope(manifest, repo_root=root)


def test_mull_config_filters_before_execution_and_escapes_regex(tmp_path: Path) -> None:
    manifest = campaign(tmp_path, "src/a+.cc", engine="mull")
    manifest.operators = ("cxx_add_to_sub",)
    config = scope.mull_scope_config(manifest, tmp_path)
    lines = config.read_text().splitlines()
    assert lines[0] == "includePaths:"
    pattern = json.loads(lines[1].strip()[2:])
    assert re.fullmatch(pattern, str(tmp_path / "src/a+.cc"))
    assert re.search(pattern, str(tmp_path / "src/aa.cc")) is None
    assert re.search(pattern, str(tmp_path / "src/a+.cc.extra")) is None
    assert re.search(pattern, "prefix" + str(tmp_path / "src/a+.cc")) is None
    assert lines[2:] == ["mutators:", '  - "cxx_add_to_sub"']
    manifest.operators = ()
    assert "mutators" not in scope.mull_scope_config(manifest, tmp_path).read_text()
    manifest.source = ()
    with pytest.raises(CampaignError, match="at least one"):
        scope.mull_scope_config(manifest, tmp_path)


def test_mull_globs_are_expanded_to_pins_before_ownership_check(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    manifest = campaign(tmp_path, "src/one.cc", "src/two.cc", engine="mull")
    manifest.source = ("src/**",)
    changes(monkeypatch, "src/one.cc")
    with pytest.raises(CampaignError, match="src/two.cc"):
        scope.validate_run_scope(manifest, repo_root=tmp_path)
    changes(monkeypatch, "src/one.cc", "src/two.cc")
    assert scope.validate_run_scope(manifest, repo_root=tmp_path) == [
        "src/one.cc",
        "src/two.cc",
    ]
    lines = scope.mull_scope_config(manifest, tmp_path).read_text().splitlines()
    assert len(lines) == 3
    assert (
        json.loads(lines[1].strip()[2:])
        == "^" + re.escape(str(tmp_path / "src/one.cc")) + "$"
    )
    manifest.source = ("missing/**",)
    with pytest.raises(CampaignError, match="no pinned target"):
        scope.mull_scope_config(manifest, tmp_path)
