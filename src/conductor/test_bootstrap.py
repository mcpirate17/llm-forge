"""``conductor.bootstrap``: alias onto ``project_init``, plus its shim contract.

Coverage for the templates and warning logic ``project_init`` gained (WORKFLOW,
ENV_STUB, ``_pyproject_conductor_warnings``) lives in ``test_project_init.py``
beside the module that owns them -- the mutation campaign for ``project_init.py``
is paired to that file, not this one. This module owns only what ``bootstrap.py``
itself adds: the alias/delegation contract and the end-to-end shim it renders.
"""

from __future__ import annotations

import stat
import sys
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
