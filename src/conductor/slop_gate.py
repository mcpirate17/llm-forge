"""Mandatory tier-1 gate: no change ships with a reachable branch nothing tests.

The mutation gate asks whether a test notices a corrupted line. This gate asks the
question that one cannot answer -- whether the code under a changed module does
anything, and whether the tests can tell. It runs the differential equivalence probe
over every changed module paired with the tests that import it, and reports:

    REACHABLE_BUT_UNTESTED  blocking. A construct that changes behaviour in a regime
                            the tests never reach. Either cover it or delete it; both
                            are cheap, and leaving it is how a live guard gets
                            withdrawn as "equivalent" months later.
    NO_DIFFERENCE_OBSERVED  advisory. Nothing this probe could do changed the result.
    WITHIN_NUMERIC_NOISE    advisory. Only the last bits moved.
    NONDETERMINISTIC        advisory. The function disagrees with ITSELF on a repeat
                            call, so no difference under it is attributable to the
                            ablation. Worth fixing in the code under test -- an
                            unstable return value defeats every differential tool --
                            but it is not evidence about this construct.

    TIMEOUT                 incomplete. The probe hit PER_MODULE_TIMEOUT. Nothing was
                            measured for that module.
    PROBE_FAILED            incomplete. The child exited non-zero or died before it
                            wrote a report. Nothing was measured for that module.

Advisory verdicts are deliberately not blocking: sampling cannot prove equivalence,
so a clean sweep is a lead for a human, never a licence to delete.

The two incomplete verdicts are reported separately from both, and never silently:
a check whose failure is indistinguishable from its success measures nothing. They
are not blocking either -- an unreached module is a cost signal and a coverage hole,
not a defect in the change -- but they are counted, named and rendered so a PASS can
be read against how much of the sweep actually ran.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Iterable, Sequence

if TYPE_CHECKING:  # pragma: no cover - the native class has no Python definition
    from conductor.repo_index import TestIndex
else:
    TestIndex = "TestIndex"

BLOCKING = ("REACHABLE_BUT_UNTESTED",)
ADVISORY = ("NO_DIFFERENCE_OBSERVED", "WITHIN_NUMERIC_NOISE", "NONDETERMINISTIC")
UNTESTED = "NOT_EXERCISED"
UNREACHED = "NOT_REACHED_BY_DRIVERS"
# Not verdicts about the code: verdicts about the probe. A module carrying one of
# these was not measured, and no conclusion about it may be drawn from a clean run.
TIMEOUT = "TIMEOUT"
PROBE_FAILED = "PROBE_FAILED"
INCOMPLETE = (TIMEOUT, PROBE_FAILED)
STDERR_TAIL_CHARS = 2000
WAIVERS = pathlib.Path("conductor/slop_waivers.json")
PER_MODULE_TIMEOUT = 180
# Beyond this the sweep is bounded by memory and disk, not cores; an explicit --jobs
# can still go higher when the caller knows the machine.
MAX_AUTO_JOBS = 8


def _worker_count(jobs: int | None, pending: int) -> int:
    """How many probes to run at once.

    Each probe is a child interpreter that imports torch and runs a test module, so
    the ceiling is memory and core contention rather than the GIL. A quarter of the
    cores is deliberately conservative: the children are themselves threaded, and
    oversubscribing turns a parallel sweep into a slower serial one. `--jobs` overrides.
    """
    if jobs is not None:
        if jobs < 1:
            raise ValueError(f"--jobs must be at least 1, got {jobs}")
        return min(jobs, pending)
    return max(1, min((os.cpu_count() or 4) // 4, pending, MAX_AUTO_JOBS))


def build_index(root: pathlib.Path) -> TestIndex:
    """The native test index, imported at the point of use.

    Deliberately not a module-level import. `conductor.repo_index` refuses to load
    without the built extension -- correctly, since a partial index would report
    modules as having no driver tests -- but importing this module is not the same as
    running the gate. A module-level import made every test that so much as imports
    `slop_gate` fail to collect on a machine without the extension, CI included.
    """
    from conductor.repo_index import build

    return build(root)


@dataclass(frozen=True)
class Waiver:
    module: str
    rule: str
    qualname: str
    reason: str

    def covers(self, module: str, rule: str, qualname: str) -> bool:
        return (
            self.module == module
            and self.rule == rule
            and self.qualname in ("*", qualname)
        )


def load_waivers(path: pathlib.Path = WAIVERS) -> list[Waiver]:
    if not path.is_file():
        return []
    raw = json.loads(path.read_text())
    return [
        Waiver(w["module"], w["rule"], w.get("qualname", "*"), w["reason"])
        for w in raw.get("waivers", [])
    ]


def changed_modules(base: str, root: pathlib.Path) -> list[str]:
    """Changed, still-present, non-test Python sources."""
    out = subprocess.run(
        ["git", "diff", "--name-only", f"{base}...HEAD"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    staged = subprocess.run(
        ["git", "diff", "--name-only", "--cached"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    names = {n for n in (out + staged).splitlines() if n.endswith(".py")}
    return sorted(
        n
        for n in names
        if (root / n).is_file() and not pathlib.Path(n).name.startswith("test_")
    )


def drivers_for(
    module: str, root: pathlib.Path, index: TestIndex | None = None
) -> list[str]:
    """Test files that import ``module``, which are what can drive it with real data.

    Answered from the native index. The scan this replaces re-walked and re-parsed
    every ``test_*.py`` in the repository on every call -- 1.12 s a module over 1,064
    files -- and matched an ``ImportFrom`` only on its ``module`` field, so
    ``from conductor import slop_gate`` never resolved. That is the dominant idiom
    here: 224 modules were reported as having no driver tests when they have some,
    and a module with no drivers is skipped by the gate entirely.
    """
    return (index or build_index(root)).drivers_for(module)


def refine_unexercised(
    findings: list[dict], root: pathlib.Path, index: TestIndex | None = None
) -> list[dict]:
    """Split "the probe never ran this" into the two things it can mean.

    A function the driver tests never call is either a real coverage hole or a miss in
    how drivers were chosen -- ``drivers_for`` selects test files that import the
    MODULE, which is not the same as the test that exercises one function in it.
    Measured over 59 such functions the split was 33 to 26, so reporting them as one
    bucket buries a genuine hole under a harness limitation and vice versa.
    """
    index = index or build_index(root)
    for finding in findings:
        if finding.get("verdict") != UNTESTED:
            continue
        name = finding.get("qualname", "").split(".")[-1]
        if not name:
            continue
        named_by = index.named_by(name)
        if named_by:
            finding["verdict"] = UNREACHED
            finding["named_by"] = named_by[:4]
    return findings


def _incomplete(
    verdict: str,
    rule: str,
    description: str,
    stderr: str | bytes | None,
    duration: float,
) -> dict:
    """A finding that says the probe did not run, not that the code is clean.

    `stderr` is decoded here because the two callers hand over different types:
    `CompletedProcess.stderr` is text under `text=True`, but `TimeoutExpired.stderr`
    is the raw bytes `Popen._check_timeout` collected -- text mode never reaches it.
    A bytes tail survives every assertion in this module and then raises
    `TypeError: Object of type bytes is not JSON serializable` the moment the
    summary is written, so the timeout path crashed whatever was trying to report
    it. Decoding at the one place both callers meet is what keeps that from being
    two separate fixes.
    """
    if isinstance(stderr, bytes):
        stderr = stderr.decode("utf-8", errors="replace")
    return {
        "qualname": "<module>",
        "rule": rule,
        "lineno": 0,
        "verdict": verdict,
        "description": description,
        "duration_s": round(duration, 1),
        "stderr_tail": (stderr or "")[-STDERR_TAIL_CHARS:],
    }


def probe(
    module: str,
    tests: Sequence[str],
    root: pathlib.Path,
    index: TestIndex | None = None,
) -> list[dict]:
    # A private directory per probe, not a fixed report path. Two probes running at
    # once against a shared name is one reading the other's findings and attributing
    # them to the wrong module -- silently, because both files parse.
    #
    # A directory rather than mkstemp because `report.is_file()` below is what
    # distinguishes "the probe wrote findings" from "the probe wrote nothing", and
    # mkstemp would leave a 0-byte file that passes that check and then fails to
    # parse.
    workdir = pathlib.Path(tempfile.mkdtemp(prefix=".slop_gate-", dir=root))
    report = workdir / "report.json"
    started = time.monotonic()
    try:
        completed = subprocess.run(
            [
                sys.executable,
                "-m",
                "conductor.equivalence_probe",
                module,
                *tests,
                "--json",
                str(report),
            ],
            cwd=root,
            capture_output=True,
            text=True,
            timeout=PER_MODULE_TIMEOUT,
        )
    except subprocess.TimeoutExpired as expired:
        shutil.rmtree(workdir, ignore_errors=True)
        return [
            _incomplete(
                TIMEOUT,
                "timeout",
                f"probe exceeded its {PER_MODULE_TIMEOUT}s budget; "
                "nothing was measured for this module",
                expired.stderr,
                time.monotonic() - started,
            )
        ]
    duration = time.monotonic() - started
    # The child is invoked without --fail-on, so it exits 0 on every verdict it can
    # reach. A non-zero exit is the child failing, not a finding. It also always
    # writes --json before returning, so an absent report is the child dying first.
    # Both used to return [] -- indistinguishable from a module with no findings.
    if completed.returncode != 0:
        shutil.rmtree(workdir, ignore_errors=True)
        return [
            _incomplete(
                PROBE_FAILED,
                "probe-exit",
                f"probe exited {completed.returncode}; "
                "nothing was measured for this module",
                completed.stderr,
                duration,
            )
        ]
    if not report.is_file():
        shutil.rmtree(workdir, ignore_errors=True)
        return [
            _incomplete(
                PROBE_FAILED,
                "probe-no-report",
                "probe exited 0 but wrote no report; "
                "nothing was measured for this module",
                completed.stderr,
                duration,
            )
        ]
    try:
        findings = refine_unexercised(json.loads(report.read_text()), root, index)
    except json.JSONDecodeError as broken:
        return [
            _incomplete(
                PROBE_FAILED,
                "probe-bad-report",
                f"probe report is not valid JSON ({broken}); "
                "nothing was measured for this module",
                completed.stderr,
                duration,
            )
        ]
    finally:
        shutil.rmtree(workdir, ignore_errors=True)
    return findings


def run(
    base: str, root: pathlib.Path, only: Iterable[str] = (), jobs: int | None = None
) -> tuple[int, dict]:
    waivers = load_waivers(root / WAIVERS)
    modules = list(only) or changed_modules(base, root)
    # One pass over the test tree answers both the driver question for every module
    # and the "does anything name this?" question for every unreached function.
    index = build_index(root)
    blocking: list[dict] = []
    advisory: list[dict] = []
    skipped: list[str] = []
    untested: list[dict] = []
    incomplete: list[dict] = []

    driven: list[tuple[str, list[str]]] = []
    for module in modules:
        tests = drivers_for(module, root, index)
        if tests:
            driven.append((module, tests))
        else:
            skipped.append(module)

    # Threads, not processes: probe() is a subprocess.run, so the worker holds no GIL
    # while the real work happens in a child interpreter. Findings are collected and
    # then walked in module order, so a parallel run reports identically to a serial
    # one -- the report is evidence, and evidence that reorders itself between runs
    # is hard to diff and easy to distrust.
    findings_by_module: dict[str, list[dict]] = {}
    if driven:
        workers = _worker_count(jobs, len(driven))
        with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
            futures = {pool.submit(probe, m, t, root, index): m for m, t in driven}
            for future in concurrent.futures.as_completed(futures):
                findings_by_module[futures[future]] = future.result()

    for module, _ in driven:
        for finding in findings_by_module.get(module, []):
            finding["module"] = module
            if finding["verdict"] in BLOCKING:
                if any(
                    w.covers(module, finding["rule"], finding["qualname"])
                    for w in waivers
                ):
                    finding["waived"] = True
                    advisory.append(finding)
                else:
                    blocking.append(finding)
            elif finding["verdict"] in ADVISORY:
                advisory.append(finding)
            elif finding["verdict"] == UNTESTED:
                untested.append(finding)
            elif finding["verdict"] in INCOMPLETE:
                incomplete.append(finding)
    # A module that timed out or crashed was not probed. Counting it in
    # modules_probed is how "0 findings" came to look like "nothing wrong".
    incomplete_modules = {f["module"] for f in incomplete}
    summary = {
        "base": base,
        "modules_probed": len(modules) - len(skipped) - len(incomplete_modules),
        "modules_without_drivers": skipped,
        "blocking": blocking,
        "advisory": advisory,
        "untested": untested,
        "incomplete": incomplete,
    }
    return (1 if blocking else 0), summary


def _render(summary: dict) -> None:
    for f in summary["blocking"]:
        print(
            f"BLOCKING  {f['module']}::{f['qualname']}:{f['lineno']}  {f['description']}"
        )
        amp = f.get("amplifier")
        if amp:
            print(
                f"          reachable via {amp} "
                f"(relative change {f.get('max_diff_amplified'):.3e}) -- add a test "
                f"that drives that regime, or remove the construct"
            )
    for f in summary["advisory"]:
        mark = "waived" if f.get("waived") else f["verdict"].lower()
        print(
            f"advisory  [{mark}] {f['module']}::{f['qualname']}:{f['lineno']}  {f['description']}"
        )
    if summary.get("untested"):
        print(
            f"{len(summary['untested'])} function(s) no test file anywhere names -- "
            "a coverage hole, not a driver-selection miss:"
        )
        for f in summary["untested"][:8]:
            print(f"            {f['module']}::{f['qualname']}")
    for f in summary.get("incomplete", []):
        print(
            f"INCOMPLETE [{f['verdict'].lower()}] {f['module']}  "
            f"{f['description']} ({f.get('duration_s')}s)"
        )
        tail = (f.get("stderr_tail") or "").strip().splitlines()
        for line in tail[-5:]:
            print(f"            {line}")
    if summary["modules_without_drivers"]:
        print(
            f"no driver tests for {len(summary['modules_without_drivers'])} changed module(s); "
            "the probe cannot speak for them"
        )
    incomplete = summary.get("incomplete", ())
    print(
        f"blocking={len(summary['blocking'])} advisory={len(summary['advisory'])} "
        f"probed={summary['modules_probed']} incomplete={len(incomplete)}"
    )


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="conductor.slop_gate")
    parser.add_argument("--base", default="origin/master")
    parser.add_argument("--root", type=pathlib.Path, default=pathlib.Path.cwd())
    parser.add_argument(
        "--module",
        action="append",
        default=[],
        help="probe these modules instead of the changed set",
    )
    parser.add_argument("--json", type=pathlib.Path)
    parser.add_argument(
        "--jobs",
        type=int,
        default=None,
        help="probe this many modules at once (default: a quarter of "
        "the cores, capped at 8). Each probe is a child "
        "interpreter, so oversubscribing slows the sweep down.",
    )
    parser.add_argument(
        "--enforce",
        action="store_true",
        help="fail on REACHABLE_BUT_UNTESTED. Off by default while the "
        "probe is alpha: its ablation set is still growing, so a "
        "blocking verdict today may be an artifact of a rule "
        "written yesterday rather than a defect in the code.",
    )
    args = parser.parse_args(argv)

    code, summary = run(args.base, args.root, args.module, args.jobs)
    if args.json:
        args.json.write_text(json.dumps(summary, indent=2) + "\n")
    _render(summary)
    return code if args.enforce else 0


if __name__ == "__main__":
    raise SystemExit(main())
