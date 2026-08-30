"""pytest plugin: record every native artifact mapped into the test process.

Coverage traces Python lines only. A test that reaches repository logic through a
compiled artifact executes a call site -- which lives in the test file and is excluded
as test infrastructure -- and then nothing measurable. The line-level subtraction comes
back empty and the test reads as external while exercising repository code.

Reads ``/proc/self/maps`` rather than walking ``sys.modules``. That is a deliberate
change of instrument, not a wider filter: a module walk can only see loads that create a
module object, and the dominant native route in this tree does not.
``research/tests/conftest.py`` and ``research/_native_runtime.py`` both load the native
runtime with ``ctypes.CDLL``, which binds no module at all; ``torch.ops.load_library``
and cffi are the same shape. The process's own mappings catch every ``dlopen``
regardless of route, including JIT extensions in the torch cache, without special-casing
any path component.

Two things stay invisible even to that, because nothing is mapped from a file and no
Python lines are recorded: Triton compiles repository kernels to cubin and hands them to
the GPU context, and Numba ``@njit`` compiles a repository function in memory. Since
this codebase puts array ops on Numba or Triton by policy, their mere presence is
treated as disqualifying -- a test asserting an upstream accumulation invariant has no
business importing either, so the false-refusal cost is near zero while the
false-admission cost is the entire waiver.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

_NATIVE_SUFFIXES = (".so", ".pyd", ".dylib")
_OPAQUE_COMPILERS = ("triton", "numba")


def _mapped_native_artifacts(repo: Path) -> set[str]:
    """Every native file mapped into this process that is repository logic."""
    try:
        raw = Path("/proc/self/maps").read_text("utf-8")
    except OSError:
        # No procfs: we cannot establish what is loaded, and an unmeasurable claim is
        # a refused claim. Report a sentinel so the caller fails closed.
        return {"<proc-maps-unavailable>"}
    found: set[str] = set()
    for line in raw.splitlines():
        parts = line.split(maxsplit=5)
        if len(parts) < 6:
            continue
        path = parts[5].strip()
        if not path.startswith("/") or not path.endswith(_NATIVE_SUFFIXES):
            continue
        resolved_parts = Path(path).parts
        if "site-packages" in resolved_parts or "dist-packages" in resolved_parts:
            continue  # third-party wheels: numpy, torch itself
        if any(cache in path for cache in ("torch_extensions", "triton", "numba")):
            found.add(f"compiled-cache::{Path(path).name}")
            continue
        try:
            found.add(str(Path(path).resolve().relative_to(repo)))
        except (ValueError, OSError):
            continue  # genuinely outside the tree
    return found


def pytest_sessionfinish(session, exitstatus) -> None:
    del session, exitstatus
    report = os.environ.get("EXTERNAL_INVARIANT_NATIVE_REPORT")
    root = os.environ.get("EXTERNAL_INVARIANT_REPO_ROOT")
    if not report or not root:
        return
    found = _mapped_native_artifacts(Path(root).resolve())
    found.update(
        f"opaque-compiler::{name}" for name in _OPAQUE_COMPILERS if name in sys.modules
    )
    Path(report).write_text(json.dumps(sorted(found)), encoding="utf-8")
