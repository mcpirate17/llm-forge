"""Tests for the tier-1 gate wrapper.

The probe decides what is true; this module decides what blocks a commit. The two
failure modes worth pinning are opposite: a gate that blocks nothing is theatre, and
a gate that blocks on a waived or advisory finding will be turned off by the first
person it inconveniences.
"""

from __future__ import annotations

import json
import pathlib
import subprocess

import pytest

from conductor import slop_gate


def _finding(verdict: str, rule: str = "drop_where", qualname: str = "Lane.forward") -> dict:
    return {"qualname": qualname, "rule": rule, "lineno": 12, "verdict": verdict,
            "description": "torch.where(...) collapsed", "amplifier": "params_x1e3",
            "max_diff_amplified": 0.9}


@pytest.fixture
def repo(tmp_path: pathlib.Path) -> pathlib.Path:
    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t",
                    "commit", "-q", "--allow-empty", "-m", "root"],
                   cwd=tmp_path, check=True)
    (tmp_path / "conductor").mkdir()
    (tmp_path / slop_gate.WAIVERS).write_text(json.dumps({"waivers": []}))
    return tmp_path


def test_changed_set_excludes_test_files(repo: pathlib.Path) -> None:
    """A test file is a driver, not a probe target; including it would probe itself."""
    (repo / "lane.py").write_text("x = 1\n")
    (repo / "test_lane.py").write_text("x = 1\n")
    subprocess.run(["git", "add", "-A"], cwd=repo, check=True)
    assert slop_gate.changed_modules("HEAD", repo) == ["lane.py"]


def test_drivers_are_the_tests_that_import_the_module(repo: pathlib.Path) -> None:
    (repo / "lane.py").write_text("VALUE = 1\n")
    (repo / "test_lane.py").write_text("from lane import VALUE\n")
    (repo / "test_other.py").write_text("import json\n")
    assert slop_gate.drivers_for("lane.py", repo) == ["test_lane.py"]


def test_the_run_builds_one_index_and_shares_it(
    repo: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The index is built once per run, not once per module.

    Both spellings return the same answers, so nothing about the report distinguishes
    them -- the only observable is how many times the tree was walked. Rebuilding per
    module is the O(repository)-per-module cost the index exists to remove, and it
    would come back silently.
    """
    (repo / "lane.py").write_text("VALUE = 1\n")
    (repo / "other.py").write_text("VALUE = 2\n")
    (repo / "third.py").write_text("VALUE = 3\n")
    (repo / "test_lane.py").write_text("from lane import VALUE\n")

    builds = []
    real = slop_gate.build_index
    monkeypatch.setattr(slop_gate, "build_index",
                        lambda root: (builds.append(root), real(root))[1])
    monkeypatch.setattr(slop_gate, "probe", lambda m, t, r, i=None: [])
    slop_gate.run("HEAD", repo, only=["lane.py", "other.py", "third.py"])
    assert len(builds) == 1, f"the test tree was walked {len(builds)} times for 3 modules"


def test_a_reachable_untested_branch_blocks(
    repo: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(slop_gate, "drivers_for", lambda m, r, i=None: ["test_lane.py"])
    monkeypatch.setattr(slop_gate, "probe",
                        lambda m, t, r, i=None: [_finding("REACHABLE_BUT_UNTESTED")])
    code, summary = slop_gate.run("HEAD", repo, only=["lane.py"])
    assert code == 1
    assert len(summary["blocking"]) == 1


def test_advisory_verdicts_never_block(
    repo: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Sampling cannot prove equivalence, so a clean sweep is a lead, not a verdict."""
    monkeypatch.setattr(slop_gate, "drivers_for", lambda m, r, i=None: ["test_lane.py"])
    monkeypatch.setattr(slop_gate, "probe", lambda m, t, r, i=None: [
        _finding("NO_DIFFERENCE_OBSERVED"), _finding("WITHIN_NUMERIC_NOISE")])
    code, summary = slop_gate.run("HEAD", repo, only=["lane.py"])
    assert code == 0
    assert len(summary["advisory"]) == 2


def test_a_waiver_downgrades_only_the_rule_it_names(
    repo: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (repo / slop_gate.WAIVERS).write_text(json.dumps({"waivers": [
        {"module": "lane.py", "rule": "drop_where", "qualname": "Lane.forward",
         "reason": "covered by the integration probe, not the unit tests"}]}))
    monkeypatch.setattr(slop_gate, "drivers_for", lambda m, r, i=None: ["test_lane.py"])
    monkeypatch.setattr(slop_gate, "probe", lambda m, t, r, i=None: [
        _finding("REACHABLE_BUT_UNTESTED", rule="drop_where"),
        _finding("REACHABLE_BUT_UNTESTED", rule="drop_clamp_min")])
    code, summary = slop_gate.run("HEAD", repo, only=["lane.py"])
    assert code == 1
    assert [f["rule"] for f in summary["blocking"]] == ["drop_clamp_min"]
    assert [f["rule"] for f in summary["advisory"]] == ["drop_where"]


def test_unreached_is_separated_from_genuinely_untested(
    repo: pathlib.Path
) -> None:
    """"The probe never ran this" means two different things and must not be one bucket.

    A function no test file anywhere names is a coverage hole. One that some test does
    name was missed by driver selection, which picks test files that import the MODULE
    rather than the test that exercises the function. Measured over 59 such functions
    the split was 33 to 26, so collapsing them buries each behind the other.
    """
    (repo / "test_thing.py").write_text("from lane import mentioned\n")
    subprocess.run(["git", "add", "-A"], cwd=repo, check=True)
    findings = [
        {"qualname": "mentioned", "verdict": "NOT_EXERCISED", "rule": "r", "lineno": 1},
        {"qualname": "nowhere_at_all", "verdict": "NOT_EXERCISED", "rule": "r", "lineno": 2},
        {"qualname": "other", "verdict": "LIVE", "rule": "r", "lineno": 3},
    ]
    refined = {f["qualname"]: f["verdict"] for f in slop_gate.refine_unexercised(findings, repo)}
    assert refined["mentioned"] == slop_gate.UNREACHED
    assert refined["nowhere_at_all"] == slop_gate.UNTESTED
    assert refined["other"] == "LIVE"


def test_a_module_with_no_driver_is_reported_not_silently_passed(
    repo: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(slop_gate, "drivers_for", lambda m, r, i=None: [])
    code, summary = slop_gate.run("HEAD", repo, only=["lane.py"])
    assert code == 0
    assert summary["modules_without_drivers"] == ["lane.py"]
    assert summary["modules_probed"] == 0
