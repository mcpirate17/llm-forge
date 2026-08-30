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

Advisory verdicts are deliberately not blocking: sampling cannot prove equivalence,
so a clean sweep is a lead for a human, never a licence to delete.
"""

from __future__ import annotations

import argparse
import ast
import json
import pathlib
import subprocess
import sys
from dataclasses import dataclass
from typing import Iterable, Sequence

BLOCKING = ("REACHABLE_BUT_UNTESTED",)
ADVISORY = ("NO_DIFFERENCE_OBSERVED", "WITHIN_NUMERIC_NOISE")
UNTESTED = "NOT_EXERCISED"
UNREACHED = "NOT_REACHED_BY_DRIVERS"
WAIVERS = pathlib.Path("conductor/slop_waivers.json")
PER_MODULE_TIMEOUT = 180


@dataclass(frozen=True)
class Waiver:
    module: str
    rule: str
    qualname: str
    reason: str

    def covers(self, module: str, rule: str, qualname: str) -> bool:
        return (self.module == module and self.rule == rule
                and self.qualname in ("*", qualname))


def load_waivers(path: pathlib.Path = WAIVERS) -> list[Waiver]:
    if not path.is_file():
        return []
    raw = json.loads(path.read_text())
    return [Waiver(w["module"], w["rule"], w.get("qualname", "*"), w["reason"])
            for w in raw.get("waivers", [])]


def changed_modules(base: str, root: pathlib.Path) -> list[str]:
    """Changed, still-present, non-test Python sources."""
    out = subprocess.run(["git", "diff", "--name-only", f"{base}...HEAD"],
                         cwd=root, capture_output=True, text=True, check=True).stdout
    staged = subprocess.run(["git", "diff", "--name-only", "--cached"],
                            cwd=root, capture_output=True, text=True, check=True).stdout
    names = {n for n in (out + staged).splitlines() if n.endswith(".py")}
    return sorted(
        n for n in names
        if (root / n).is_file() and not pathlib.Path(n).name.startswith("test_")
    )


def drivers_for(module: str, root: pathlib.Path) -> list[str]:
    """Test files that import ``module``, which are what can drive it with real data."""
    dotted = module[:-3].replace("/", ".")
    hits: list[str] = []
    for test in root.rglob("test_*.py"):
        if ".git" in test.parts:
            continue
        try:
            tree = ast.parse(test.read_text())
        except (OSError, SyntaxError):
            continue
        for node in ast.walk(tree):
            if isinstance(node, ast.ImportFrom) and node.module == dotted:
                hits.append(str(test.relative_to(root)))
                break
            if isinstance(node, ast.Import) and any(a.name == dotted for a in node.names):
                hits.append(str(test.relative_to(root)))
                break
    return sorted(hits)


def refine_unexercised(findings: list[dict], root: pathlib.Path) -> list[dict]:
    """Split "the probe never ran this" into the two things it can mean.

    A function the driver tests never call is either a real coverage hole or a miss in
    how drivers were chosen -- ``drivers_for`` selects test files that import the
    MODULE, which is not the same as the test that exercises one function in it.
    Measured over 59 such functions the split was 33 to 26, so reporting them as one
    bucket buries a genuine hole under a harness limitation and vice versa.
    """
    for finding in findings:
        if finding.get("verdict") != UNTESTED:
            continue
        name = finding.get("qualname", "").split(".")[-1]
        if not name:
            continue
        named_by = subprocess.run(
            ["git", "grep", "-l", "-w", "-F", name, "--", "*/test_*.py", "test_*.py"],
            cwd=root, capture_output=True, text=True,
        ).stdout.split()
        if named_by:
            finding["verdict"] = UNREACHED
            finding["named_by"] = named_by[:4]
    return findings


def probe(module: str, tests: Sequence[str], root: pathlib.Path) -> list[dict]:
    report = root / ".slop_gate_report.json"
    try:
        subprocess.run(
            [sys.executable, "-m", "conductor.equivalence_probe", module, *tests,
             "--json", str(report)],
            cwd=root, capture_output=True, text=True, timeout=PER_MODULE_TIMEOUT,
        )
    except subprocess.TimeoutExpired:
        return [{"qualname": "<module>", "rule": "timeout", "lineno": 0,
                 "verdict": "TIMEOUT", "description": "probe exceeded its budget"}]
    if not report.is_file():
        return []
    try:
        return refine_unexercised(json.loads(report.read_text()), root)
    finally:
        report.unlink(missing_ok=True)


def run(base: str, root: pathlib.Path, only: Iterable[str] = ()) -> tuple[int, dict]:
    waivers = load_waivers(root / WAIVERS)
    modules = list(only) or changed_modules(base, root)
    blocking: list[dict] = []
    advisory: list[dict] = []
    skipped: list[str] = []
    untested: list[dict] = []
    for module in modules:
        tests = drivers_for(module, root)
        if not tests:
            skipped.append(module)
            continue
        for finding in probe(module, tests, root):
            finding["module"] = module
            if finding["verdict"] in BLOCKING:
                if any(w.covers(module, finding["rule"], finding["qualname"])
                       for w in waivers):
                    finding["waived"] = True
                    advisory.append(finding)
                else:
                    blocking.append(finding)
            elif finding["verdict"] in ADVISORY:
                advisory.append(finding)
            elif finding["verdict"] == UNTESTED:
                untested.append(finding)
    summary = {
        "base": base, "modules_probed": len(modules) - len(skipped),
        "modules_without_drivers": skipped,
        "blocking": blocking, "advisory": advisory, "untested": untested,
    }
    return (1 if blocking else 0), summary


def _render(summary: dict) -> None:
    for f in summary["blocking"]:
        print(f"BLOCKING  {f['module']}::{f['qualname']}:{f['lineno']}  {f['description']}")
        amp = f.get("amplifier")
        if amp:
            print(f"          reachable via {amp} "
                  f"(relative change {f.get('max_diff_amplified'):.3e}) -- add a test "
                  f"that drives that regime, or remove the construct")
    for f in summary["advisory"]:
        mark = "waived" if f.get("waived") else f["verdict"].lower()
        print(f"advisory  [{mark}] {f['module']}::{f['qualname']}:{f['lineno']}  {f['description']}")
    if summary.get("untested"):
        print(f"{len(summary['untested'])} function(s) no test file anywhere names -- "
              "a coverage hole, not a driver-selection miss:")
        for f in summary["untested"][:8]:
            print(f"            {f['module']}::{f['qualname']}")
    if summary["modules_without_drivers"]:
        print(f"no driver tests for {len(summary['modules_without_drivers'])} changed module(s); "
              "the probe cannot speak for them")
    print(f"blocking={len(summary['blocking'])} advisory={len(summary['advisory'])} "
          f"probed={summary['modules_probed']}")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="conductor.slop_gate")
    parser.add_argument("--base", default="origin/master")
    parser.add_argument("--root", type=pathlib.Path, default=pathlib.Path.cwd())
    parser.add_argument("--module", action="append", default=[],
                        help="probe these modules instead of the changed set")
    parser.add_argument("--json", type=pathlib.Path)
    parser.add_argument("--enforce", action="store_true",
                        help="fail on REACHABLE_BUT_UNTESTED. Off by default while the "
                             "probe is alpha: its ablation set is still growing, so a "
                             "blocking verdict today may be an artifact of a rule "
                             "written yesterday rather than a defect in the code.")
    args = parser.parse_args(argv)

    code, summary = run(args.base, args.root, args.module)
    if args.json:
        args.json.write_text(json.dumps(summary, indent=2) + "\n")
    _render(summary)
    return code if args.enforce else 0


if __name__ == "__main__":
    raise SystemExit(main())
