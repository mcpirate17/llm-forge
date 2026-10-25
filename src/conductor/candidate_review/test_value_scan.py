"""Refuse tests that cannot fail: no assertion, or an assert that is a constant.

The detector is `slop_core.test_value_scan_files`, tree-sitter in Rust. The CLI,
line scoping and printing are `style_scan.run_python_scan`.

A finding spans the whole test function, so a candidate that edits any line of
an assert-less test is held to it -- editing the body of a test is the moment to
make it check something. Scoping by the first line alone would let a body edit
slip past a finding that sits on the `def`, so the scan asks for span scoping.
"""

from __future__ import annotations

from conductor.candidate_review.style_scan import run_python_scan

RULES = ("test/always-true-assert", "test/no-assertion")


def _scan(paths: list[str]) -> list[dict]:
    import slop_core

    return list(slop_core.test_value_scan_files(paths))


def main(argv: list[str] | None = None) -> int:
    return run_python_scan(
        argv, version="test-value-scan 1", description=__doc__, scan=_scan, span=True
    )


if __name__ == "__main__":
    raise SystemExit(main())
