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
import threading
import time

import pytest

from conductor import slop_gate


def _finding(
    verdict: str, rule: str = "drop_where", qualname: str = "Lane.forward"
) -> dict:
    return {
        "qualname": qualname,
        "rule": rule,
        "lineno": 12,
        "verdict": verdict,
        "description": "torch.where(...) collapsed",
        "amplifier": "params_x1e3",
        "max_diff_amplified": 0.9,
    }


@pytest.fixture
def repo(tmp_path: pathlib.Path) -> pathlib.Path:
    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    subprocess.run(
        [
            "git",
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "root",
        ],
        cwd=tmp_path,
        check=True,
    )
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
    monkeypatch.setattr(
        slop_gate, "build_index", lambda root: (builds.append(root), real(root))[1]
    )
    monkeypatch.setattr(slop_gate, "probe", lambda m, t, r, i=None: [])
    slop_gate.run("HEAD", repo, only=["lane.py", "other.py", "third.py"])
    assert len(builds) == 1, (
        f"the test tree was walked {len(builds)} times for 3 modules"
    )


def test_a_reachable_untested_branch_blocks(
    repo: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(slop_gate, "drivers_for", lambda m, r, i=None: ["test_lane.py"])
    monkeypatch.setattr(
        slop_gate, "probe", lambda m, t, r, i=None: [_finding("REACHABLE_BUT_UNTESTED")]
    )
    code, summary = slop_gate.run("HEAD", repo, only=["lane.py"])
    assert code == 1
    assert len(summary["blocking"]) == 1


def test_advisory_verdicts_never_block(
    repo: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Sampling cannot prove equivalence, so a clean sweep is a lead, not a verdict."""
    monkeypatch.setattr(slop_gate, "drivers_for", lambda m, r, i=None: ["test_lane.py"])
    monkeypatch.setattr(
        slop_gate,
        "probe",
        lambda m, t, r, i=None: [
            _finding("NO_DIFFERENCE_OBSERVED"),
            _finding("WITHIN_NUMERIC_NOISE"),
        ],
    )
    code, summary = slop_gate.run("HEAD", repo, only=["lane.py"])
    assert code == 0
    assert len(summary["advisory"]) == 2


def test_a_waiver_downgrades_only_the_rule_it_names(
    repo: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (repo / slop_gate.WAIVERS).write_text(
        json.dumps(
            {
                "waivers": [
                    {
                        "module": "lane.py",
                        "rule": "drop_where",
                        "qualname": "Lane.forward",
                        "reason": "covered by the integration probe, not the unit tests",
                    }
                ]
            }
        )
    )
    monkeypatch.setattr(slop_gate, "drivers_for", lambda m, r, i=None: ["test_lane.py"])
    monkeypatch.setattr(
        slop_gate,
        "probe",
        lambda m, t, r, i=None: [
            _finding("REACHABLE_BUT_UNTESTED", rule="drop_where"),
            _finding("REACHABLE_BUT_UNTESTED", rule="drop_clamp_min"),
        ],
    )
    code, summary = slop_gate.run("HEAD", repo, only=["lane.py"])
    assert code == 1
    assert [f["rule"] for f in summary["blocking"]] == ["drop_clamp_min"]
    assert [f["rule"] for f in summary["advisory"]] == ["drop_where"]


def test_unreached_is_separated_from_genuinely_untested(repo: pathlib.Path) -> None:
    """ "The probe never ran this" means two different things and must not be one bucket.

    A function no test file anywhere names is a coverage hole. One that some test does
    name was missed by driver selection, which picks test files that import the MODULE
    rather than the test that exercises the function. Measured over 59 such functions
    the split was 33 to 26, so collapsing them buries each behind the other.
    """
    (repo / "test_thing.py").write_text("from lane import mentioned\n")
    subprocess.run(["git", "add", "-A"], cwd=repo, check=True)
    findings = [
        {"qualname": "mentioned", "verdict": "NOT_EXERCISED", "rule": "r", "lineno": 1},
        {
            "qualname": "nowhere_at_all",
            "verdict": "NOT_EXERCISED",
            "rule": "r",
            "lineno": 2,
        },
        {"qualname": "other", "verdict": "LIVE", "rule": "r", "lineno": 3},
    ]
    refined = {
        f["qualname"]: f["verdict"]
        for f in slop_gate.refine_unexercised(findings, repo)
    }
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


def test_parallel_and_serial_report_the_same_findings(
    repo: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A parallel sweep is only useful if its report is the serial report.

    Findings arrive in completion order, which is timing-dependent, so `run` collects
    them and walks modules in order. Without that the report reshuffles between runs
    and stops being diffable evidence.
    """
    mods = [f"m{i}.py" for i in range(12)]
    for m in mods:
        (repo / m).write_text("VALUE = 1\n")
    monkeypatch.setattr(slop_gate, "drivers_for", lambda m, r, i=None: ["test_lane.py"])
    monkeypatch.setattr(slop_gate, "build_index", lambda root: None)

    def fake_probe(module, tests, root, index=None):
        # Later modules finish first, so completion order is the reverse of module order.
        time.sleep(0.02 * (len(mods) - mods.index(module)))
        f = _finding("NO_DIFFERENCE_OBSERVED")
        f["qualname"] = module
        return [f]

    monkeypatch.setattr(slop_gate, "probe", fake_probe)
    _, serial = slop_gate.run("HEAD", repo, only=mods, jobs=1)
    _, parallel = slop_gate.run("HEAD", repo, only=mods, jobs=6)
    order = [f["qualname"] for f in parallel["advisory"]]
    assert order == mods, "findings are not in module order"
    assert [f["qualname"] for f in serial["advisory"]] == order


def test_concurrent_probes_never_share_a_report_path(
    repo: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Two probes at once against one report path is cross-attribution, silently.

    Both files parse, so the only symptom is one module's findings appearing under
    another module's name.
    """
    seen: list[str] = []

    def capture(argv, **kwargs):
        seen.append(argv[argv.index("--json") + 1])
        return subprocess.CompletedProcess(argv, 0, "", "")

    monkeypatch.setattr(slop_gate.subprocess, "run", capture)
    for module in ("a.py", "b.py", "c.py"):
        slop_gate.probe(module, ["test_lane.py"], repo, None)
    assert len(set(seen)) == len(seen), f"report path reused across probes: {seen}"
    leftovers = list(repo.glob(".slop_gate*"))
    assert leftovers == [], f"probe left scratch behind: {leftovers}"


def test_more_than_one_module_is_probed_at_a_time(
    repo: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The point of the pool. A sweep is 1,100+ modules at tens of seconds each."""
    live = 0
    peak = 0
    lock = threading.Lock()

    def fake_probe(module, tests, root, index=None):
        nonlocal live, peak
        with lock:
            live += 1
            peak = max(peak, live)
        time.sleep(0.05)
        with lock:
            live -= 1
        return []

    mods = [f"m{i}.py" for i in range(8)]
    monkeypatch.setattr(slop_gate, "drivers_for", lambda m, r, i=None: ["test_lane.py"])
    monkeypatch.setattr(slop_gate, "build_index", lambda root: None)
    monkeypatch.setattr(slop_gate, "probe", fake_probe)
    slop_gate.run("HEAD", repo, only=mods, jobs=4)
    assert peak > 1, "modules were probed one at a time"


def test_a_nonsense_job_count_is_refused_not_clamped(
    repo: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Silently clamping --jobs 0 to 1 turns a typo into a sweep that takes hours."""
    monkeypatch.setattr(slop_gate, "drivers_for", lambda m, r, i=None: ["test_lane.py"])
    monkeypatch.setattr(slop_gate, "build_index", lambda root: None)
    monkeypatch.setattr(slop_gate, "probe", lambda m, t, r, i=None: [])
    with pytest.raises(ValueError):
        slop_gate.run("HEAD", repo, only=["lane.py"], jobs=0)


def test_the_default_worker_count_follows_the_machine(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Sizing the pool when --jobs is absent, asserted directly.

    Driving this through `run` would not test it: `run` is called with an explicit
    --jobs in every other test here, which takes the override branch and never reaches
    the sizing. It also cannot be asserted against the real machine -- a 2-core CI
    runner and a 32-core workstation are both correct and give different answers.
    """
    monkeypatch.setattr(slop_gate.os, "cpu_count", lambda: 32)
    assert slop_gate._worker_count(None, 100) == slop_gate.MAX_AUTO_JOBS
    monkeypatch.setattr(slop_gate.os, "cpu_count", lambda: 8)
    assert slop_gate._worker_count(None, 100) == 2
    assert slop_gate._worker_count(None, 1) == 1
    # Small machines stay serial rather than thrashing: each probe is a child
    # interpreter that imports torch.
    monkeypatch.setattr(slop_gate.os, "cpu_count", lambda: 2)
    assert slop_gate._worker_count(None, 100) == 1


def _fake_run(returncode: int, stderr: str, report: str | None):
    """A stand-in for the probe child that writes what a real one would."""

    def runner(argv, **kwargs):
        if report is not None:
            index = argv.index("--json")
            pathlib.Path(argv[index + 1]).write_text(report)
        return subprocess.CompletedProcess(argv, returncode, "", stderr)

    return runner


def test_a_timed_out_probe_is_a_finding_not_a_clean_sweep(
    repo: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The budget expiring means nothing was measured, and must not read as nothing found.

    This is the fail-open that cost a real gate run 181s of its 187s and reported
    zero findings from one module: `probe` returned a TIMEOUT finding, `run`
    bucketed only BLOCKING/ADVISORY/UNTESTED, and the verdict fell off the end of
    the loop -- no bucket, no metric, no rendered line.
    """

    def boom(argv, **kwargs):
        raise subprocess.TimeoutExpired(
            argv, slop_gate.PER_MODULE_TIMEOUT, stderr="killed mid-import"
        )

    monkeypatch.setattr(slop_gate.subprocess, "run", boom)
    findings = slop_gate.probe("lane.py", ["test_lane.py"], repo)
    assert [f["verdict"] for f in findings] == [slop_gate.TIMEOUT]
    assert "killed mid-import" in findings[0]["stderr_tail"]

    monkeypatch.setattr(slop_gate, "drivers_for", lambda m, r, i=None: ["test_lane.py"])
    monkeypatch.setattr(slop_gate, "build_index", lambda root: None)
    code, summary = slop_gate.run("HEAD", repo, only=["lane.py"])
    assert code == 0, "an unmeasured module is a coverage hole, not a defect"
    assert [f["verdict"] for f in summary["incomplete"]] == [slop_gate.TIMEOUT]
    assert summary["modules_probed"] == 0, "a module that timed out was not probed"


def test_a_crashed_probe_is_a_finding_not_a_clean_sweep(
    repo: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A non-zero exit used to return [] -- identical to a module with nothing wrong.

    The child is invoked without --fail-on, so every verdict it can reach exits 0.
    Non-zero is the child failing: an import error, an OOM kill, a segfault in a
    native extension. The traceback was captured and discarded.
    """
    monkeypatch.setattr(
        slop_gate.subprocess,
        "run",
        _fake_run(1, "ModuleNotFoundError: no module named 'lane'", None),
    )
    findings = slop_gate.probe("lane.py", ["test_lane.py"], repo)
    assert [f["verdict"] for f in findings] == [slop_gate.PROBE_FAILED]
    assert "ModuleNotFoundError" in findings[0]["stderr_tail"]
    # The rule, not just the verdict: an exit code and a missing report are two
    # different failures and the report has to say which one happened.
    assert findings[0]["rule"] == "probe-exit"


def test_a_probe_that_wrote_no_report_is_a_finding(
    repo: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Exit 0 with no report is the child dying after the work and before the write."""
    monkeypatch.setattr(slop_gate.subprocess, "run", _fake_run(0, "", None))
    findings = slop_gate.probe("lane.py", ["test_lane.py"], repo)
    assert [f["verdict"] for f in findings] == [slop_gate.PROBE_FAILED]


def test_an_unparseable_report_is_a_finding(
    repo: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A truncated report is a dead child, not an empty result."""
    monkeypatch.setattr(slop_gate.subprocess, "run", _fake_run(0, "", '[{"qual'))
    findings = slop_gate.probe("lane.py", ["test_lane.py"], repo)
    assert [f["verdict"] for f in findings] == [slop_gate.PROBE_FAILED]


def test_a_completed_probe_still_returns_its_findings(
    repo: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The guards above must not swallow the success path they wrap."""
    monkeypatch.setattr(slop_gate, "refine_unexercised", lambda f, r, i: f)
    monkeypatch.setattr(
        slop_gate.subprocess,
        "run",
        _fake_run(0, "", json.dumps([_finding("NO_DIFFERENCE_OBSERVED")])),
    )
    findings = slop_gate.probe("lane.py", ["test_lane.py"], repo)
    assert [f["verdict"] for f in findings] == ["NO_DIFFERENCE_OBSERVED"]


def test_the_probe_workdir_is_removed_on_every_exit(
    repo: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Each new early return is a new leak of a temp dir into the tree being reviewed."""
    monkeypatch.setattr(slop_gate, "refine_unexercised", lambda f, r, i: f)
    for runner in (
        _fake_run(1, "boom", None),
        _fake_run(0, "", None),
        _fake_run(0, "", '[{"qual'),
        _fake_run(0, "", "[]"),
    ):
        monkeypatch.setattr(slop_gate.subprocess, "run", runner)
        slop_gate.probe("lane.py", ["test_lane.py"], repo)
    assert list(repo.glob(".slop_gate-*")) == []
