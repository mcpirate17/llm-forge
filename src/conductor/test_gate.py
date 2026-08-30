"""Tests for the single governance gate.

Every comparison and boolean in `conductor.gate` gets a fixture on each side of its
boundary. A gate whose tests only exercise the happy path is worth nothing: the
failure this module exists to prevent was a check that had never once run red.
"""

from __future__ import annotations

import subprocess
from dataclasses import dataclass
from pathlib import Path

import pytest

from conductor import gate
from conductor.candidate_review.policy import ToolPolicy


def _tool(
    tool_id: str = "sample",
    *,
    executable: str = "sample",
    expected_version: str = "1.2.3",
    required_profiles: tuple[str, ...] = ("fast", "full"),
) -> ToolPolicy:
    return ToolPolicy(
        tool_id=tool_id,
        executable=executable,
        version_command=(executable, "--version"),
        expected_version=expected_version,
        required_profiles=required_profiles,
        provided_by="test fixture",
        rationale="test fixture",
    )


def _fake_executable(directory: Path, name: str, output: str, *, exit_code: int = 0) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / name
    path.write_text(f"#!/bin/sh\necho '{output}'\nexit {exit_code}\n", encoding="utf-8")
    path.chmod(0o755)
    return path


def _git(repo: Path, *args: str) -> str:
    completed = subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=True
    )
    return completed.stdout.strip()


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    root = tmp_path / "repo"
    root.mkdir()
    _git(root, "init", "--quiet", "-b", "main")
    _git(root, "config", "user.email", "test@example.invalid")
    _git(root, "config", "user.name", "test")
    (root / "tracked.py").write_text("VALUE = 1\n", encoding="utf-8")
    _git(root, "add", "tracked.py")
    _git(root, "commit", "--quiet", "-m", "first")
    return root


# --------------------------------------------------------------------------
# runner_search_path -- both sides of "node_modules/.bin exists"
# --------------------------------------------------------------------------


def test_search_path_prepends_node_bin_when_present(tmp_path: Path) -> None:
    (tmp_path / "node_modules" / ".bin").mkdir(parents=True)
    assert gate.runner_search_path(tmp_path).startswith(str(tmp_path / "node_modules" / ".bin"))


def test_search_path_is_unchanged_when_node_bin_absent(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setenv("PATH", "/usr/bin")
    assert gate.runner_search_path(tmp_path) == "/usr/bin"


# --------------------------------------------------------------------------
# probe_tool -- found/missing, and version matching on both sides
# --------------------------------------------------------------------------


def test_probe_tool_reports_a_matching_version(tmp_path: Path) -> None:
    _fake_executable(tmp_path / "bin", "sample", "sample 1.2.3")
    status = gate.probe_tool(_tool(), str(tmp_path / "bin"))
    assert status.found is True
    assert status.matches_expected is True


def test_probe_tool_reports_a_drifted_version(tmp_path: Path) -> None:
    """The other side of the version comparison: present, but not CI's pin."""
    _fake_executable(tmp_path / "bin", "sample", "sample 9.9.9")
    status = gate.probe_tool(_tool(), str(tmp_path / "bin"))
    assert status.found is True
    assert status.matches_expected is False
    assert status.version == "sample 9.9.9"


def test_probe_tool_reports_a_missing_tool(tmp_path: Path) -> None:
    status = gate.probe_tool(_tool(), str(tmp_path / "empty"))
    assert status.found is False
    assert status.resolved_path is None


def test_probe_tool_treats_a_failing_version_command_as_versionless(tmp_path: Path) -> None:
    """Exit-code boundary: the binary exists, but its version probe fails."""
    _fake_executable(tmp_path / "bin", "sample", "boom", exit_code=1)
    status = gate.probe_tool(_tool(), str(tmp_path / "bin"))
    assert status.found is True
    assert status.version is None
    assert status.matches_expected is False


# --------------------------------------------------------------------------
# preflight_tools -- refusal, pass, and profile filtering on both sides
# --------------------------------------------------------------------------


def test_preflight_refuses_when_a_required_tool_is_missing(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setenv("PATH", str(tmp_path / "empty"))
    phase, statuses = gate.preflight_tools((_tool(),), "full", tmp_path)
    assert phase.ok is False
    assert phase.evidence["missing"] == ["sample"]
    assert len(statuses) == 1


def test_preflight_passes_when_every_required_tool_is_present(tmp_path: Path, monkeypatch) -> None:
    _fake_executable(tmp_path / "bin", "sample", "sample 1.2.3")
    monkeypatch.setenv("PATH", str(tmp_path / "bin"))
    phase, _statuses = gate.preflight_tools((_tool(),), "full", tmp_path)
    assert phase.ok is True
    assert phase.evidence["drifted"] == []


def test_preflight_reports_drift_without_refusing(tmp_path: Path, monkeypatch) -> None:
    """A version skew is information, not a refusal -- the other side of `missing`."""
    _fake_executable(tmp_path / "bin", "sample", "sample 9.9.9")
    monkeypatch.setenv("PATH", str(tmp_path / "bin"))
    phase, _statuses = gate.preflight_tools((_tool(),), "full", tmp_path)
    assert phase.ok is True
    assert phase.evidence["drifted"] == ["sample"]


def test_preflight_skips_a_tool_the_profile_does_not_require(tmp_path: Path, monkeypatch) -> None:
    """Profile filtering, absent side: a full-only tool must not refuse a fast run."""
    monkeypatch.setenv("PATH", str(tmp_path / "empty"))
    phase, statuses = gate.preflight_tools((_tool(required_profiles=("full",)),), "fast", tmp_path)
    assert phase.ok is True
    assert statuses == []


def test_preflight_probes_a_tool_the_profile_does_require(tmp_path: Path, monkeypatch) -> None:
    """Profile filtering, present side: the same tool refuses a full run."""
    monkeypatch.setenv("PATH", str(tmp_path / "empty"))
    phase, statuses = gate.preflight_tools((_tool(required_profiles=("full",)),), "full", tmp_path)
    assert phase.ok is False
    assert len(statuses) == 1


def test_preflight_names_npm_ci_when_node_modules_is_absent(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setenv("PATH", str(tmp_path / "empty"))
    tool = ToolPolicy(
        tool_id="biome",
        executable="biome",
        version_command=("biome", "--version"),
        expected_version="2.4.15",
        required_profiles=("full",),
        provided_by="npm install --global @biomejs/biome@2.4.15",
        rationale="test",
    )
    phase, _statuses = gate.preflight_tools((tool,), "full", tmp_path)
    assert phase.ok is False
    assert "npm ci" in phase.detail
    assert phase.evidence["node_modules"] is False


# --------------------------------------------------------------------------
# export_tree -- the clean-clone surface
# --------------------------------------------------------------------------


def test_export_contains_tracked_files_and_no_git_directory(repo: Path, tmp_path: Path) -> None:
    destination = tmp_path / "export" / "tree"
    phase = gate.export_tree(repo, "HEAD", destination)
    assert phase.ok is True
    assert (destination / "tracked.py").is_file()
    assert not (destination / ".git").exists()
    assert phase.evidence["exported_files"] == phase.evidence["tracked_files"]


def test_export_omits_untracked_files(repo: Path, tmp_path: Path) -> None:
    """The whole point: an untracked file does not exist for CI."""
    (repo / "untracked.py").write_text("VALUE = 2\n", encoding="utf-8")
    destination = tmp_path / "export" / "tree"
    gate.export_tree(repo, "HEAD", destination)
    assert not (destination / "untracked.py").exists()


def test_export_refuses_an_unknown_ref(repo: Path, tmp_path: Path) -> None:
    with pytest.raises(gate.GateRefusal):
        gate.export_tree(repo, "refs/heads/does-not-exist", tmp_path / "export" / "tree")


# --------------------------------------------------------------------------
# pytest config discovery and sampling
# --------------------------------------------------------------------------


def test_discover_skips_vendored_configs(tmp_path: Path) -> None:
    (tmp_path / "pytest.ini").write_text("[pytest]\n", encoding="utf-8")
    vendored = tmp_path / "node_modules" / "pkg"
    vendored.mkdir(parents=True)
    (vendored / "pytest.ini").write_text("[pytest]\n", encoding="utf-8")
    found = gate.discover_pytest_configs(tmp_path)
    assert found == [tmp_path / "pytest.ini"]


def test_sample_test_file_picks_the_smallest(tmp_path: Path) -> None:
    (tmp_path / "test_big.py").write_text("x = 1\n" * 500, encoding="utf-8")
    (tmp_path / "test_small.py").write_text("x = 1\n", encoding="utf-8")
    assert gate._sample_test_file(tmp_path) == tmp_path / "test_small.py"


def test_sample_test_file_is_none_without_tests(tmp_path: Path) -> None:
    assert gate._sample_test_file(tmp_path) is None


def test_pytest_config_check_fails_on_an_unparseable_addopts(tmp_path: Path) -> None:
    """The `--dist loadgroup` regression: an addopts naming an absent plugin."""
    (tmp_path / "pytest.ini").write_text(
        "[pytest]\naddopts = --this-flag-does-not-exist\n", encoding="utf-8"
    )
    phase = gate.preflight_pytest_config(tmp_path, "python3")
    assert phase.ok is False
    assert "do not parse" in phase.detail


def test_pytest_config_check_passes_on_a_valid_addopts(tmp_path: Path) -> None:
    """The other side: a config whose options really do parse."""
    (tmp_path / "pytest.ini").write_text("[pytest]\naddopts = --tb=short\n", encoding="utf-8")
    phase = gate.preflight_pytest_config(tmp_path, "python3")
    assert phase.ok is True


# --------------------------------------------------------------------------
# waiver activation -- both sides of the integration_base comparison
# --------------------------------------------------------------------------


@dataclass(frozen=True)
class _Waiver:
    integration_base: str


def test_waivers_are_active_on_their_pinned_base() -> None:
    phase = gate.waiver_activation((_Waiver("abc123"), _Waiver("abc123")), "abc123")
    assert phase.evidence["active"] == 2
    assert phase.evidence["inert"] == 0
    assert "inert" not in phase.detail


def test_waivers_are_inert_on_any_other_base() -> None:
    phase = gate.waiver_activation((_Waiver("abc123"), _Waiver("abc123")), "def456")
    assert phase.evidence["active"] == 0
    assert phase.evidence["inert"] == 2
    assert "inert" in phase.detail


def test_waiver_activation_is_reported_per_waiver_not_all_or_nothing() -> None:
    phase = gate.waiver_activation((_Waiver("abc123"), _Waiver("def456")), "abc123")
    assert phase.evidence["active"] == 1
    assert phase.evidence["inert"] == 1


# --------------------------------------------------------------------------
# rendering and exit codes
# --------------------------------------------------------------------------


def test_render_distinguishes_pass_fail_and_refused() -> None:
    ok = [gate.PhaseResult(name="p", ok=True, detail="fine")]
    bad = [gate.PhaseResult(name="p", ok=False, detail="broken")]
    assert gate.render(ok, [], gate.EXIT_PASS).startswith("gate | PASS")
    assert gate.render(bad, [], gate.EXIT_FAIL).startswith("gate | FAIL")
    assert gate.render(bad, [], gate.EXIT_REFUSED).startswith("gate | REFUSED")


def test_exit_codes_are_distinct() -> None:
    """A refusal is not a pass and is not a fail; conflating them is the bug."""
    assert len({gate.EXIT_PASS, gate.EXIT_FAIL, gate.EXIT_REFUSED}) == 3
