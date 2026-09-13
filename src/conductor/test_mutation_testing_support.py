"""Merging a declared host-read dependency into a snapshot; process lifetime.

Split out of ``test_mutation_testing`` so the merge rules have a module the
campaign generator can pair with the code that implements them.  Every case
here runs through ``mutation_testing._link_host_dependencies``, which is the
seam production uses: it supplies the hard-link ``materialize`` and the
``CampaignError`` type, and delegates the tree walk to
``conductor.mutation_testing_support``.

The same module owns ``run_command``'s lifetime guarantees: a spawned command
dies with its engine (timeout, parent death) and never leaves a group the
host has to hunt by hand.  Those tests spawn real processes -- sleeps that
outlive their own runs -- and clean them up in ``finally`` blocks.
"""

from __future__ import annotations

import dataclasses
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
from typing import ClassVar

import pytest

from conductor import mutation_testing
from conductor import mutation_testing_support


def _host_dependency_campaign(
    host: Path, relatives: tuple[str, ...]
) -> mutation_testing.Campaign:
    (host / "reports" / "screen").mkdir(parents=True)
    (host / "reports" / "screen" / "receipt.json").write_text("{}", encoding="utf-8")
    (host / "reports" / "screen" / "ignored.json").write_text("[]", encoding="utf-8")
    campaign_path = (
        mutation_testing.REPO_ROOT / "src/conductor/testdata/mutation_testing/campaign.json"
    )
    return dataclasses.replace(
        mutation_testing.load_campaign(campaign_path),
        host_read_dependencies=relatives,
    )


def test_host_read_dependency_merge_fills_in_what_is_missing_and_refuses_the_rest(
    tmp_path: Path,
) -> None:
    """The four merge outcomes, in one test because no mutant separates them.

    Twenty-four campaigns declare paths under ``research/reports`` that were
    gitignored scratch when they were authored.  Those directories are only
    *partially* tracked now -- one measured 408 entries on disk against 109
    committed -- so the snapshot holds the committed files and is still missing
    every gitignored one.  Deciding once for the whole directory, either way, is
    wrong: refusing makes the campaign unrunnable, and skipping leaves it reading
    files that are not there.

    The three refusals live here rather than in tests of their own because the
    generated campaign kills every mutant in the merge walk through the fill-in
    case alone: split out, each refusal is dominated and classifies MERGE while
    asserting something the fill-in case does not.  Folded in, the assertions
    survive under one classified nodeid.
    """

    host = tmp_path / "filled" / "host"
    campaign = _host_dependency_campaign(host, ("reports/screen",))
    snapshot = tmp_path / "filled" / "snapshot"
    (snapshot / "reports" / "screen").mkdir(parents=True)
    (snapshot / "reports" / "screen" / "receipt.json").write_text(
        "{}", encoding="utf-8"
    )
    (snapshot / "reports" / "screen" / "tracked_only.json").write_text(
        "0", encoding="utf-8"
    )

    mutation_testing._link_host_dependencies(campaign, snapshot, host)  # noqa: SLF001

    screen = snapshot / "reports" / "screen"
    assert screen.joinpath("ignored.json").read_text(encoding="utf-8") == "[]"
    assert screen.joinpath("receipt.json").read_text(encoding="utf-8") == "{}"
    assert screen.joinpath("tracked_only.json").read_text(encoding="utf-8") == "0"

    # A snapshot copy whose bytes differ from the host is refused, and the
    # refusal names the diverging file rather than the declared directory.
    host = tmp_path / "diverged" / "host"
    campaign = _host_dependency_campaign(host, ("reports/screen",))
    snapshot = tmp_path / "diverged" / "snapshot"
    (snapshot / "reports" / "screen").mkdir(parents=True)
    (snapshot / "reports" / "screen" / "receipt.json").write_text(
        '{"uncommitted": true}', encoding="utf-8"
    )

    with pytest.raises(
        mutation_testing.CampaignError,
        match=r"differs from the host: reports/screen/receipt\.json$",
    ):
        mutation_testing._link_host_dependencies(campaign, snapshot, host)  # noqa: SLF001

    # A directory on the host against a file in the snapshot cannot be merged.
    host = tmp_path / "kind" / "host"
    campaign = _host_dependency_campaign(host, ("reports/screen",))
    snapshot = tmp_path / "kind" / "snapshot"
    (snapshot / "reports").mkdir(parents=True)
    (snapshot / "reports" / "screen").write_text("not a directory", encoding="utf-8")

    with pytest.raises(
        mutation_testing.CampaignError,
        match=r"differs from the host: reports/screen$",
    ):
        mutation_testing._link_host_dependencies(campaign, snapshot, host)  # noqa: SLF001

    # A symlink is never compared: it can point outside the snapshot entirely.
    host = tmp_path / "symlink" / "host"
    campaign = _host_dependency_campaign(host, ("reports/screen",))
    snapshot = tmp_path / "symlink" / "snapshot"
    (snapshot / "reports").mkdir(parents=True)
    (snapshot / "reports" / "screen").symlink_to(host / "reports" / "screen")

    with pytest.raises(
        mutation_testing.CampaignError,
        match=r"already contains host read dependency path: reports/screen$",
    ):
        mutation_testing._link_host_dependencies(campaign, snapshot, host)  # noqa: SLF001


@dataclasses.dataclass(frozen=True)
class _Result:
    """The minimal shape ``run_command``'s ``result_factory`` has to build."""

    returncode: int | None
    timed_out: bool
    duration_seconds: float
    stdout_tail: str
    stderr_tail: str


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def test_run_command_timeout_kills_the_whole_process_group(tmp_path: Path) -> None:
    """A timed-out command must not leave the processes it spawned running.

    Every engine runs mutants out of process, so killing only the direct child
    orphans one test process per in-flight mutant.  On 2026-09-08 four of them
    ran 18 hours at 100% CPU that way.  The grandchild here stands in for a
    mutant: it is spawned by the command, it outlives its own parent, and it
    holds the command's stdout pipe open, which is what made the old
    ``subprocess.run(..., timeout=)`` path unable to reap it.
    """

    # The sleeps outlive the timeout but not the mutant budget: a mutant that
    # drops the kill has to be caught failing, not caught hanging, or the
    # campaign scores ERROR instead of killing it.
    marker = tmp_path / "grandchild.pid"
    registry = tmp_path / "live_pgids.json"
    result = mutation_testing_support.run_command(
        ["sh", "-c", f"sleep 15 & echo $! > {marker}; sleep 15"],
        cwd=tmp_path,
        timeout_seconds=2,
        environment={},
        pin_argv=list,
        result_factory=_Result,
        output_tail_chars=200,
        pgid_registry=registry,
    )

    assert result.timed_out is True
    assert result.returncode is None
    # Elapsed, not summed: `monotonic() + started` is two uptimes and would
    # report a run that has not finished as days long.
    assert 0.0 < result.duration_seconds < 15.0
    grandchild = int(marker.read_text(encoding="utf-8").strip())
    deadline = time.monotonic() + 10
    while _alive(grandchild) and time.monotonic() < deadline:
        time.sleep(0.05)
    assert not _alive(grandchild), (
        f"pid {grandchild} survived the timeout; the kill reached only the "
        "direct child, so mutant processes still orphan"
    )


def test_run_command_completion_is_unaffected_by_the_new_session(
    tmp_path: Path,
) -> None:
    """The ordinary path still reports the command's own code and output."""

    registry = tmp_path / "live_pgids.json"
    result = mutation_testing_support.run_command(
        ["sh", "-c", "printf out; printf err >&2; exit 3"],
        cwd=Path.cwd(),
        timeout_seconds=30,
        environment={},
        pin_argv=list,
        result_factory=_Result,
        output_tail_chars=200,
        pgid_registry=registry,
    )

    assert (result.returncode, result.timed_out) == (3, False)
    assert (result.stdout_tail, result.stderr_tail) == ("out", "err")
    assert 0.0 < result.duration_seconds < 30.0
    # A finished run is not live: the registry it appended is emptied again,
    # or the reaper would one day kill a recycled pid on stale evidence. The
    # bytes are checked, not just the JSON, because the registry is read back
    # by a human-adjacent tool and written through atomic_json's canonical
    # shape -- a trailing newline included.
    assert registry.read_text(encoding="utf-8") == (
        json.dumps(
            {mutation_testing_support.LIVE_PGIDS_KEY: []}, indent=2, sort_keys=True
        )
        + "\n"
    )


def test_kill_process_group_reports_both_ends_of_the_race(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A dead group returns its output; a live one is raised, never returned.

    Returning on a surviving group would record the run as a clean timeout
    while its descendants kept the CPU, which is the failure the group kill
    exists to end.  Raising on a group that merely exited first would refuse
    every run that finishes microseconds after its own timeout.  `killpg` is
    stubbed in both halves because the only group this test could truthfully
    signal is the one running it.
    """

    class _Stub:
        args: ClassVar[list[str]] = ["pretend-engine"]

        def __init__(self, pid: int, surviving: bool) -> None:
            self.pid = pid
            self._surviving = surviving

        def communicate(self, timeout: int | None = None) -> tuple[str, str]:
            if self._surviving:
                raise subprocess.TimeoutExpired(self.args, timeout or 0)
            return ("tail", "")

    signalled: list[tuple[int, int]] = []
    monkeypatch.setattr(
        mutation_testing_support.os,
        "killpg",
        lambda pgid, sig: signalled.append((pgid, sig)),
    )
    with pytest.raises(
        mutation_testing_support.OrphanedProcessGroupError,
        match=r"pgid 4242 still held its pipes",
    ):
        mutation_testing_support._kill_process_group(  # noqa: SLF001
            _Stub(4242, surviving=True),  # type: ignore[arg-type]
            drain_seconds=0,
        )
    assert signalled == [(4242, signal.SIGKILL)]

    def _gone(pgid: int, sig: int) -> None:
        raise ProcessLookupError(pgid)

    monkeypatch.setattr(mutation_testing_support.os, "killpg", _gone)
    assert mutation_testing_support._kill_process_group(  # noqa: SLF001
        _Stub(4243, surviving=False),  # type: ignore[arg-type]
        drain_seconds=0,
    ) == ("tail", "")


# ── Dying with the engine: PDEATHSIG binding and the orphan reaper ────────

_ENGINE_CHILD_SCRIPT = """
import sys
from pathlib import Path

from conductor import mutation_testing_support

mutation_testing_support.run_command(
    ["sleep", "300"],
    cwd=Path(sys.argv[3]),
    timeout_seconds=300,
    environment={},
    pin_argv=list,
    result_factory=lambda **kwargs: None,
    output_tail_chars=10,
    pgid_registry=Path(sys.argv[1]),
)
Path(sys.argv[2]).write_text("done", encoding="utf-8")
"""


def _command_name(pid: int) -> str:
    return Path(f"/proc/{pid}/comm").read_text(encoding="utf-8").strip()


def test_a_command_dies_with_the_process_that_spawned_it(tmp_path: Path) -> None:
    """PR_SET_PDEATHSIG: SIGKILL the engine and its command goes with it.

    The timeout path only fires when the engine lives to see the timeout;
    Ctrl-C, session end and OOM kill the engine outright, and on 2026-09-13
    that left ten pytest mutant runs at 100 % CPU for 8-10 h.  The child
    spawned here stands in for the engine binary: killed engine, gone
    command, within two seconds.
    """

    registry = tmp_path / "live_pgids.json"
    finished = tmp_path / "child-finished"
    engine = subprocess.Popen(
        [
            sys.executable,
            "-c",
            _ENGINE_CHILD_SCRIPT,
            str(registry),
            str(finished),
            str(tmp_path),
        ],
        cwd=tmp_path,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    pgid: int | None = None
    try:
        deadline = time.monotonic() + 30
        while not registry.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        entries = mutation_testing_support._live_pgid_entries(  # noqa: SLF001
            registry
        )
        assert len(entries) == 1
        assert entries[0]["engine_pid"] == engine.pid
        pgid = entries[0]["pgid"]
        # `comm` names the exec'd binary only once the child has run its
        # preexec_fn and exec'd -- before that the binding may not be set yet.
        while _command_name(pgid) != "sleep" and time.monotonic() < deadline:
            time.sleep(0.05)
        assert _command_name(pgid) == "sleep"

        killed_at = time.monotonic()
        os.kill(engine.pid, signal.SIGKILL)
        engine.wait()

        while _alive(pgid) and time.monotonic() - killed_at < 2.0:
            time.sleep(0.02)
        elapsed = time.monotonic() - killed_at
        assert not _alive(pgid), (
            f"pid {pgid} outlived its engine by {elapsed:.2f}s; PDEATHSIG"
            " never fired, so an engine death still orphans its runs"
        )
    finally:
        engine.kill()
        engine.wait()
        if pgid is not None:
            try:
                os.killpg(pgid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass


def test_a_run_that_cannot_record_its_pgid_kills_what_it_spawned(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A refused registry write must not become the next orphan's origin."""

    marker = tmp_path / "spawned.pid"

    def _refuse_after_marker(*args: object, **kwargs: object) -> None:
        # The refusal must land after the command has started (and written
        # its pid), or the test would race the very kill it is checking.
        deadline = time.monotonic() + 10
        while not marker.exists() and time.monotonic() < deadline:
            time.sleep(0.02)
        raise OSError(28, f"No space left on device for {args} {kwargs}")

    monkeypatch.setattr(
        mutation_testing_support, "record_live_pgid", _refuse_after_marker
    )
    with pytest.raises(OSError, match="No space left"):
        mutation_testing_support.run_command(
            ["sh", "-c", f"echo $$ > {marker}; sleep 30"],
            cwd=tmp_path,
            timeout_seconds=30,
            environment={},
            pin_argv=list,
            result_factory=_Result,
            output_tail_chars=10,
            pgid_registry=tmp_path / "live_pgids.json",
        )
    spawned = int(marker.read_text(encoding="utf-8").strip())
    deadline = time.monotonic() + 10
    # The killed command is this process's own child, so it stays a zombie
    # until reaped -- signal 0 keeps succeeding on a zombie. Reap, then look.
    while _alive(spawned) and time.monotonic() < deadline:
        try:
            if os.waitpid(spawned, os.WNOHANG)[0] != 0:
                break
        except ChildProcessError:
            break
        time.sleep(0.05)
    assert not _alive(spawned), (
        f"pid {spawned} survived the refusal that could never reap it"
    )


def test_parent_death_binding_refuses_non_linux_loudly() -> None:
    """A silent skip would leave every non-Linux run unbound again."""

    for platform in ("darwin", "win32"):
        with pytest.raises(NotImplementedError, match="PR_SET_PDEATHSIG"):
            mutation_testing_support._parent_death_preexec(  # noqa: SLF001
                platform=platform
            )


def test_the_binding_is_actually_set_on_the_spawned_command() -> None:
    """PR_GET_PDEATHSIG reads SIGKILL back out of a bound child.

    The binding is only observable cross-process, which is exactly why this
    stays here: the campaign's covering set is measured in-process, so a
    preexec_fn that silently binds nothing would survive on the out-of-process
    test above alone.
    """

    probe = subprocess.run(
        [
            sys.executable,
            "-c",
            "import ctypes; value = ctypes.c_int(0);"
            "ctypes.CDLL(None).prctl(2, ctypes.byref(value));"
            "print(value.value)",
        ],
        capture_output=True,
        text=True,
        timeout=30,
        check=True,
        start_new_session=True,
        preexec_fn=mutation_testing_support._parent_death_preexec(),  # noqa: SLF001
    )
    assert probe.stdout.strip() == str(signal.SIGKILL)


def test_live_pgids_path_sits_in_the_iterations_dir_beside_receipts(
    tmp_path: Path,
) -> None:
    from conductor.project_paths import receipts_relative

    assert mutation_testing_support.live_pgids_path(tmp_path) == (
        tmp_path / receipts_relative(tmp_path) / ".iterations"
        / mutation_testing_support.LIVE_PGIDS_FILENAME
    )


def test_reap_reports_a_missing_registry_and_touches_nothing(tmp_path: Path) -> None:
    lines, found = mutation_testing_support.reap_orphaned_runs(
        tmp_path / "absent.json", apply=False
    )
    assert found == 0
    assert lines == [f"no live pgid registry at {tmp_path / 'absent.json'}"]


def test_a_malformed_registry_refuses_rather_than_guessing(tmp_path: Path) -> None:
    registry = tmp_path / "live_pgids.json"
    registry.write_text('{"pgid": 1}', encoding="utf-8")
    with pytest.raises(ValueError, match="not a JSON object with a list"):
        mutation_testing_support.reap_orphaned_runs(registry, apply=False)


class _OrphanScene:
    """A registry holding every truth the reaper can meet, plus live groups.

    The in-flight entry's engine is another live sleep -- deliberately not
    this test process, so a mutant that turns the liveness probe into a
    lethal signal can only ever reach a sleep.
    """

    def __init__(self, tmp_path: Path) -> None:
        self.registry = tmp_path / "live_pgids.json"
        exited = subprocess.run(
            [sys.executable, "-c", "import os; print(os.getpid())"],
            capture_output=True,
            text=True,
            check=True,
        )
        self.dead_engine = int(exited.stdout)
        assert not _alive(self.dead_engine)
        self.orphan = subprocess.Popen(["sleep", "300"], start_new_session=True)
        self.bystander = subprocess.Popen(["sleep", "300"], start_new_session=True)
        self.in_flight = subprocess.Popen(["sleep", "300"], start_new_session=True)
        self.live_engine = subprocess.Popen(["sleep", "300"], start_new_session=True)
        mutation_testing_support.record_live_pgid(
            self.registry, pgid=self.orphan.pid, engine_pid=self.dead_engine,
            argv0="sleep",
        )
        mutation_testing_support.record_live_pgid(
            self.registry,
            pgid=self.in_flight.pid,
            engine_pid=self.live_engine.pid,
            argv0="sleep",
        )
        # A stale entry with no argv0: everyone it names is already gone.
        self.registry.write_text(
            json.dumps(
                {
                    mutation_testing_support.LIVE_PGIDS_KEY: [
                        *mutation_testing_support._live_pgid_entries(  # noqa: SLF001
                            self.registry
                        ),
                        {"engine_pid": self.dead_engine, "pgid": self.dead_engine},
                    ]
                }
            )
            + "\n",
            encoding="utf-8",
        )

    def recorded_pgids(self) -> set[int]:
        return {
            entry["pgid"]
            for entry in mutation_testing_support._live_pgid_entries(  # noqa: SLF001
                self.registry
            )
        }

    def close(self) -> None:
        for proc in (self.orphan, self.bystander, self.in_flight, self.live_engine):
            proc.kill()
            proc.wait()


def test_reap_dry_run_lists_findings_and_touches_nothing(tmp_path: Path) -> None:
    scene = _OrphanScene(tmp_path)
    try:
        lines, found = mutation_testing_support.reap_orphaned_runs(
            scene.registry, apply=False
        )
        assert found == 1
        assert any(
            f"pgid {scene.orphan.pid} (sleep)" in line and "would SIGKILL" in line
            for line in lines
        )
        assert all(
            f"pgid {scene.in_flight.pid}" not in line or "in flight" in line
            for line in lines
        )
        assert any("(?)" in line and "stale" in line for line in lines), lines
        # The unrecorded bystander appears in no line at all.
        assert not any(f"pgid {scene.bystander.pid}" in line for line in lines)
        assert _alive(scene.orphan.pid), "the dry run must not kill"
        assert _alive(scene.bystander.pid), "the dry run must not kill"
        assert _alive(scene.in_flight.pid), "the dry run must not kill"

        # The CLI reports findings as a failed check while it only lists.
        assert (
            mutation_testing_support.main(["reap", "--registry", str(scene.registry)])
            == 1
        )
        assert _alive(scene.orphan.pid), "listing must stay a dry run"
    finally:
        scene.close()


def test_reap_apply_kills_only_the_recorded_orphan(tmp_path: Path) -> None:
    scene = _OrphanScene(tmp_path)
    try:
        lines, found = mutation_testing_support.reap_orphaned_runs(
            scene.registry, apply=True
        )
        assert found == 1
        # wait() rather than signal 0: a killed child of this process is a
        # zombie until reaped, and zombies answer signal 0.
        assert scene.orphan.wait(timeout=10) == -signal.SIGKILL, (
            "--apply must kill the recorded orphan"
        )
        assert _alive(scene.bystander.pid), "nothing unrecorded is ever signalled"
        assert _alive(scene.in_flight.pid), "a live engine's run is never touched"
        assert scene.recorded_pgids() == {scene.in_flight.pid, scene.dead_engine}

        # Nothing left to find: the check goes green under --apply too.
        assert (
            mutation_testing_support.main(
                ["reap", "--registry", str(scene.registry), "--apply"]
            )
            == 0
        )
    finally:
        scene.close()


def test_reap_on_an_empty_registry_records_no_runs(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """An existing registry with no entries is a clean, quiet check."""

    registry = tmp_path / "live_pgids.json"
    registry.write_text(
        json.dumps({mutation_testing_support.LIVE_PGIDS_KEY: []}) + "\n",
        encoding="utf-8",
    )
    assert mutation_testing_support.main(["reap", "--registry", str(registry)]) == 0
    assert "records no runs" in capsys.readouterr().out
