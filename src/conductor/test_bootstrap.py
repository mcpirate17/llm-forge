"""``conductor.bootstrap``: alias onto ``project_init``, plus its own new templates."""

from __future__ import annotations

import stat
import sys
import tomllib
from pathlib import Path
from unittest.mock import Mock

import pytest

from conductor import bootstrap
from conductor import project_init as pi


def _repo(tmp_path: Path) -> Path:
    project = tmp_path / "proj"
    project.mkdir()
    (project / ".git").mkdir()
    return project


def _config(project: Path, **kw: object) -> pi.InitConfig:
    return pi.InitConfig(project_dir=project, python=Path(sys.executable), **kw)


@pytest.fixture
def quiet_doctor(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(pi, "run_doctor", lambda config: 0)
    monkeypatch.setattr(pi, "_crg_importable", lambda python: True)


# ── the alias itself ─────────────────────────────────────────────────────────


def test_bootstrap_main_delegates_argv_to_project_init_main(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    stub = Mock(return_value=0)
    monkeypatch.setattr(bootstrap, "_init_main", stub)
    assert bootstrap.main(["/some/host", "--force"]) == 0
    stub.assert_called_once_with(["/some/host", "--force"])


def test_bootstrap_main_returns_project_init_exit_code(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(bootstrap, "_init_main", Mock(return_value=2))
    assert bootstrap.main([]) == 2


def test_bootstrap_subcommand_is_wired_in_conductor_main() -> None:
    from conductor.__main__ import SUBCOMMANDS

    assert SUBCOMMANDS["bootstrap"] == "conductor.bootstrap"


def test_bootstrap_cli_scaffolds_a_real_working_dispatcher(tmp_path: Path) -> None:
    """Runs the alias end to end (no stubbed doctor): the dispatch.py it writes must
    actually be executable and resolve ``tooling.hooks.dispatch`` -- the exact
    "shim is executable and points at an importable module" contract."""
    project = _repo(tmp_path)
    assert bootstrap.main([str(project)]) == 0
    launcher = project / pi.LAUNCHER
    assert launcher.stat().st_mode & stat.S_IXUSR
    assert launcher.read_text().startswith(f"#!{sys.executable}\n")
    assert "from tooling.hooks.dispatch.__main__ import main" in launcher.read_text()


# ── the two new create-once templates ───────────────────────────────────────


@pytest.mark.usefixtures("quiet_doctor")
def test_bootstrap_writes_workflow_and_env_stub_once(tmp_path: Path) -> None:
    project = _repo(tmp_path)
    assert bootstrap.main([str(project)]) == 0
    workflow = project / pi.WORKFLOW
    env_stub = project / pi.ENV_STUB
    assert workflow.read_text() == pi.WORKFLOW_TEXT
    assert env_stub.read_text() == pi.ENV_STUB_TEXT
    assert "conductor.guardrail_audit" in workflow.read_text()
    assert "conductor.radon_complexity" in workflow.read_text()
    assert "BASH_QUIET_SAVE_DIR" in env_stub.read_text()

    # Idempotent: a second run changes nothing.
    second = pi.plan(_config(project))
    assert second.changed == []

    # Project-owned: an owner edit survives a re-run, even with --force.
    workflow.write_text("owner edit\n", encoding="utf-8")
    env_stub.write_text("owner edit\n", encoding="utf-8")
    assert bootstrap.main([str(project), "--force"]) == 0
    assert workflow.read_text() == "owner edit\n"
    assert env_stub.read_text() == "owner edit\n"


# ── [tool.conductor] stanza warning ─────────────────────────────────────────


def test_pyproject_warning_when_manifest_absent(tmp_path: Path) -> None:
    project = _repo(tmp_path)
    warnings = pi._pyproject_conductor_warnings(project)
    assert len(warnings) == 1
    assert "pyproject.toml does not exist" in warnings[0]


def test_pyproject_warning_lists_missing_keys(tmp_path: Path) -> None:
    project = _repo(tmp_path)
    (project / "pyproject.toml").write_text(
        '[tool.conductor]\ncandidate_policy = "x.toml"\n', encoding="utf-8"
    )
    warnings = pi._pyproject_conductor_warnings(project)
    assert len(warnings) == 1
    assert "mutation_registry" in warnings[0]
    assert "package_root" in warnings[0]
    assert "candidate_policy" not in warnings[0].split("missing", 1)[1]


def test_pyproject_no_warning_when_stanza_is_complete(tmp_path: Path) -> None:
    project = _repo(tmp_path)
    (project / "pyproject.toml").write_text(
        "[tool.conductor]\n"
        'candidate_policy = "candidate_policy.toml"\n'
        'mutation_registry = "campaigns/registry.json"\n'
        'package_root = "src/conductor"\n',
        encoding="utf-8",
    )
    assert pi._pyproject_conductor_warnings(project) == []


def test_pyproject_warning_on_malformed_toml(tmp_path: Path) -> None:
    project = _repo(tmp_path)
    (project / "pyproject.toml").write_text("[tool.conductor\n", encoding="utf-8")
    warnings = pi._pyproject_conductor_warnings(project)
    assert len(warnings) == 1
    assert "could not be parsed" in warnings[0]


def test_pyproject_warning_reaches_the_plan_without_writing_pyproject(
    tmp_path: Path, quiet_doctor: None
) -> None:
    project = _repo(tmp_path)
    plan_ = pi.plan(_config(project))
    assert any("pyproject.toml does not exist" in w for w in plan_.warnings)
    assert not (project / "pyproject.toml").exists()


def test_conductor_stanza_keys_round_trip_tomllib() -> None:
    # Sanity: the constant this module warns about really is what tomllib parses
    # a [tool.conductor] table into, not a typo that would never match.
    parsed = tomllib.loads(
        '[tool.conductor]\ncandidate_policy = "a"\nmutation_registry = "b"\n'
        'package_root = "c"\n'
    )
    assert set(pi.CONDUCTOR_STANZA_KEYS) <= set(parsed["tool"]["conductor"])
