"""Merging a declared host-read dependency into a snapshot.

Split out of ``test_mutation_testing`` so the merge rules have a module the
campaign generator can pair with the code that implements them.  Every case
here runs through ``mutation_testing._link_host_dependencies``, which is the
seam production uses: it supplies the hard-link ``materialize`` and the
``CampaignError`` type, and delegates the tree walk to
``conductor.mutation_testing_support``.
"""

from __future__ import annotations

import dataclasses
import os
from pathlib import Path
import signal
import subprocess
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
    return dataclasses.replace(
        mutation_testing.load_campaign(
            mutation_testing.REPO_ROOT
            / "conductor/mutation_campaigns/claude_bash_quiet.json"
        ),
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
    result = mutation_testing_support.run_command(
        ["sh", "-c", f"sleep 15 & echo $! > {marker}; sleep 15"],
        cwd=tmp_path,
        timeout_seconds=2,
        environment={},
        pin_argv=list,
        result_factory=_Result,
        output_tail_chars=200,
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


def test_run_command_completion_is_unaffected_by_the_new_session() -> None:
    """The ordinary path still reports the command's own code and output."""

    result = mutation_testing_support.run_command(
        ["sh", "-c", "printf out; printf err >&2; exit 3"],
        cwd=Path.cwd(),
        timeout_seconds=30,
        environment={},
        pin_argv=list,
        result_factory=_Result,
        output_tail_chars=200,
    )

    assert (result.returncode, result.timed_out) == (3, False)
    assert (result.stdout_tail, result.stderr_tail) == ("out", "err")
    assert 0.0 < result.duration_seconds < 30.0


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
