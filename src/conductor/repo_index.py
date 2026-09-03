"""Python surface for the native test index.

The index itself is Rust (``tooling/native/slop-core``); everything here
is argument marshalling and a CLI. It replaces two O(repository) scans that the gate
was paying per unit of work:

* ``drivers_for`` re-walked and re-parsed every ``test_*.py`` once per module probed,
  at 1.12 s a module over 1,064 test files;
* ``refine_unexercised`` spawned one ``git grep`` per function the probe could not
  reach -- 405 subprocesses in the last sweep.

Both questions are answered from one pass. The index also resolves
``from pkg import module``, which the old matcher could not see at all; over this
repository that is 224 modules the gate reported as having no driver tests when they
have some.

There is no pure-Python fallback. A fallback that resolved a subset of imports would
report "no driver tests for this module" about modules that have them, which is the
exact defect this replaces.
"""

from __future__ import annotations

import argparse
import pathlib
from collections.abc import Sequence

from conductor._native import slop_core

# Decided once when conductor._native loaded: the extension, or SlopCoreUnavailable
# (an ImportError naming the build step). No per-call fallback.
_core = slop_core()
TestIndex = _core.TestIndex


def build(root: pathlib.Path | str) -> TestIndex:
    """Index every ``test_*.py`` under ``root``. One pass; query it many times."""
    return _core.build_test_index(str(pathlib.Path(root).resolve()))


def dotted_for(module: str) -> str:
    """``conductor/slop_gate.py`` -> ``conductor.slop_gate``."""
    return _core.dotted_for(module)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="conductor.repo_index")
    parser.add_argument("--root", type=pathlib.Path, default=pathlib.Path.cwd())
    parser.add_argument(
        "--drivers-for", metavar="MODULE", help="test files that import this module"
    )
    parser.add_argument(
        "--named-by",
        metavar="NAME",
        help="test files naming this identifier as a whole word",
    )
    args = parser.parse_args(argv)

    index = build(args.root)
    if args.drivers_for:
        for path in index.drivers_for(args.drivers_for):
            print(path)
    elif args.named_by:
        for path in index.named_by(args.named_by):
            print(path)
    else:
        # Print the resolved root, not the requested one: a tool that silently
        # indexes the wrong tree reports a clean answer about nothing.
        print(f"{pathlib.Path(args.root).resolve()}: {index!r}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
