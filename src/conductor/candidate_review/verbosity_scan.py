"""Report Python functions whose comment lines outnumber their code lines.

The detector is `slop_core.verbosity_scan_files`. The CLI, line scoping and
printing are `style_scan.run_python_scan`: findings print to stderr as
`path:line: rule: message` and the exit status is 1 whenever one is kept; the
caller's severity makes it advisory.

A finding carries the function's span, and a function counts as changed when any
line of that span changed, because adding comments to the body leaves the `def`
line alone -- so the scan asks for span scoping.
"""

from __future__ import annotations

from conductor.candidate_review.style_scan import run_python_scan


def _scan(paths: list[str]) -> list[dict]:
    import slop_core

    return list(slop_core.verbosity_scan_files(paths))


def main(argv: list[str] | None = None) -> int:
    return run_python_scan(
        argv, version="verbosity-scan 1", description=__doc__, scan=_scan, span=True
    )


if __name__ == "__main__":
    raise SystemExit(main())
