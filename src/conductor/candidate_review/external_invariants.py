"""Per-nodeid admission for tests that pin an invariant outside the repository.

The value gate admits a new test only as CORE or INTENTIONAL_REDUNDANCY, and both are
earned by *killing a mutant*. A test whose subject lives upstream can earn neither. The
worked example:

    per_step = torch.stack([stacked[:, s].sum(dim=1) for s in range(length)], dim=1)
    assert torch.equal(per_step, stacked.sum(dim=2))

That executes no repository code. It pins an ATen property the native readout depends
on -- a fused ``[B,L,S,D].sum(dim=2)`` being bit-identical to the per-step sum it
replaced -- and ATen's accumulation order is size-dependent, so a torch upgrade that
changes it must fail loudly rather than silently break scan bit-exactness. It is not
CORE, because it kills nothing here. It is not INTENTIONAL_REDUNDANCY, because nothing
else carries that contract. Without a third path it blocks its campaign forever, and
the three ways to make it "pass" -- relabel, delete, or contrive a mutant against code
it never touches -- are all dishonest.

A waiver is declared in the owning campaign manifest:

    "external_invariants": [
      {
        "nodeid": "research/tests/test_x.py::test_aten_sum_is_order_stable",
        "justification": "why this property is depended on and what breaks if it moves",
        "pinned": {"torch": "2.13.0+cu130"}
      }
    ]

and is **verified on every gate run, never taken on trust**:

1. *It really is external.* Running the nodeid under coverage must execute no
   repository source outside test files. Test infrastructure -- the test module itself,
   its ``conftest``, helpers under a ``tests/`` directory -- is excluded, because a
   conftest fixture runs for every test and would otherwise make the criterion
   unsatisfiable. Production source is not.
2. *The pin still holds.* The declared torch version must equal the installed one. A
   bump re-REFUSES the campaign, which is precisely when the pin earns its keep.
3. *A reason was written down.* An empty justification is not a waiver.

Anything that fails, errors, or cannot be measured stays REFUSED. A declared waiver
that does not verify produces its own finding rather than falling through to a generic
"has no value classification", so the author is told which of the three conditions
failed.

This lives outside ``conductor/mutation_value.py`` deliberately. That module is a
pinned runner component -- 130 receipts bind its hash under ``runner_components_sha256``
-- so putting the admission there would invalidate every one of them and force a
repo-wide re-run. Here it costs the campaigns that pin ``verification.py``, which is a
far smaller set, and verifying at gate time rather than recording a measurement in a
receipt is the stronger guarantee anyway: a torch bump, or a test that starts touching
production code, re-REFUSES on the next run without anyone re-measuring.
"""

from __future__ import annotations

import json
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Mapping, Sequence

COVERAGE_TIMEOUT_SECONDS = 300


@dataclass(frozen=True, slots=True)
class WaiverOutcome:
    """One declared waiver, and whether it verified."""

    nodeid: str
    admitted: bool
    reason: str


def _installed_torch_version() -> str | None:
    """The full installed torch version, including the build tag.

    Read out of a subprocess rather than ``importlib.metadata`` or an import here:
    metadata reports ``2.13.0`` and drops the local segment, so a cu130 -> cu128 swap
    would leave the pin looking satisfied. That is precisely the change most likely to
    move ATen's accumulation order, which is what these waivers exist to notice.
    ``torch.__version__`` carries it (``2.13.0+cu130``), and a subprocess keeps the
    multi-second import out of the gate process.
    """
    try:
        probe = subprocess.run(
            [sys.executable, "-c", "import torch; print(torch.__version__)"],
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    version = probe.stdout.strip()
    return version if probe.returncode == 0 and version else None


def _is_test_infrastructure(relative: str) -> bool:
    """True for the test module, its conftest, and helpers that live beside them.

    A conftest fixture executes for every test in its directory, so counting it as
    repository source would make "executes no repository source" unsatisfiable for
    every test in the repo. Production source is what the criterion is about.
    """
    parts = Path(relative).parts
    name = Path(relative).name
    return "tests" in parts or name == "conftest.py" or name.startswith("test_")


def _measure(
    snapshot: Path, runtime_dir: Path, label: str, pytest_args: list[str]
) -> tuple[frozenset[tuple[str, int]], str | None]:
    """Repository source LINES executed by one pytest invocation.

    Line granularity, not file: the test module imports its subject at module scope, so
    the subject's file is in the import baseline either way. A test that reaches into
    an already-imported module executes NEW lines in it, and only a line-level
    comparison can tell that apart from the import itself.

    Returns ``(files, error)``. A non-None error means the measurement itself failed
    and the waiver must not be granted -- an unmeasurable claim is a refused claim.
    """
    data_file = runtime_dir / f"external_invariant_{label}.coverage"
    report = runtime_dir / f"external_invariant_{label}.json"
    for stale in (data_file, report):
        stale.unlink(missing_ok=True)
    run = subprocess.run(
        [
            sys.executable,
            "-m",
            "coverage",
            "run",
            "--branch",
            f"--data-file={data_file}",
            "-m",
            "pytest",
            "-q",
            "-o",
            "addopts=",
            "--rootdir=.",
            "-p",
            "no:cacheprovider",
            *pytest_args,
        ],
        cwd=snapshot,
        capture_output=True,
        text=True,
        timeout=COVERAGE_TIMEOUT_SECONDS,
        check=False,
    )
    if run.returncode != 0:
        tail = (run.stdout or run.stderr or "").strip()[-300:]
        return frozenset(), f"{label} run failed under coverage: {tail}"
    export = subprocess.run(
        [
            sys.executable,
            "-m",
            "coverage",
            "json",
            f"--data-file={data_file}",
            "-o",
            str(report),
        ],
        cwd=snapshot,
        capture_output=True,
        text=True,
        timeout=COVERAGE_TIMEOUT_SECONDS,
        check=False,
    )
    if export.returncode != 0 or not report.is_file():
        return frozenset(), "coverage produced no report"
    payload = json.loads(report.read_text("utf-8"))
    executed: set[tuple[str, int]] = set()
    for measured, entry in (payload.get("files") or {}).items():
        lines = entry.get("executed_lines") or []
        if not lines:
            continue
        try:
            relative = str(Path(measured).resolve().relative_to(snapshot.resolve()))
        except ValueError:
            continue  # outside the tree: site-packages, stdlib, torch itself
        if _is_test_infrastructure(relative):
            continue
        executed.update((relative, int(line)) for line in lines)
    return frozenset(executed), None


def evaluate(
    snapshot: Path,
    runtime_dir: Path,
    declarations: Sequence[Mapping[str, object]],
    gated_nodeids: Iterable[str],
) -> list[WaiverOutcome]:
    """Verify every declared waiver that covers a currently gated nodeid."""
    gated = set(gated_nodeids)
    outcomes: list[WaiverOutcome] = []
    installed = _installed_torch_version()
    for declaration in declarations:
        nodeid = declaration.get("nodeid")
        if not isinstance(nodeid, str) or nodeid not in gated:
            continue
        justification = declaration.get("justification")
        if not isinstance(justification, str) or not justification.strip():
            outcomes.append(WaiverOutcome(nodeid, False, "the justification is empty"))
            continue
        pinned = declaration.get("pinned")
        pinned_torch = pinned.get("torch") if isinstance(pinned, Mapping) else None
        if not isinstance(pinned_torch, str) or not pinned_torch:
            outcomes.append(
                WaiverOutcome(nodeid, False, "no pinned torch version is declared")
            )
            continue
        if installed is None:
            outcomes.append(
                WaiverOutcome(
                    nodeid,
                    False,
                    "torch is not installed, so the pin cannot be checked",
                )
            )
            continue
        if pinned_torch != installed:
            outcomes.append(
                WaiverOutcome(
                    nodeid,
                    False,
                    f"pinned torch {pinned_torch!r} but {installed!r} is installed; "
                    "re-verify the invariant and re-pin",
                )
            )
            continue
        module = nodeid.split("::", 1)[0]
        try:
            # Importing the test module legitimately executes production source at
            # module scope. What matters is whether the TEST BODY reaches any, so
            # subtract an import-only baseline (collect-only imports but runs nothing)
            # rather than demanding the module import nothing -- which no real test
            # module can satisfy.
            baseline, error = _measure(
                snapshot, runtime_dir, "import", ["--collect-only", module]
            )
            if error is None:
                executed, error = _measure(snapshot, runtime_dir, "call", [nodeid])
                executed = frozenset(executed - baseline)
        except (
            subprocess.TimeoutExpired,
            OSError,
            ValueError,
            json.JSONDecodeError,
        ) as exc:
            outcomes.append(
                WaiverOutcome(nodeid, False, f"coverage could not be measured: {exc}")
            )
            continue
        if error is not None:
            outcomes.append(WaiverOutcome(nodeid, False, error))
            continue
        if executed:
            outcomes.append(
                WaiverOutcome(
                    nodeid,
                    False,
                    "it executes repository source beyond importing its module, "
                    "so it is not an external invariant: "
                    f"{sorted({file for file, _ in executed})[:4]}",
                )
            )
            continue
        outcomes.append(
            WaiverOutcome(
                nodeid,
                True,
                f"executes no repository source; torch pinned at {pinned_torch}",
            )
        )
    return outcomes
