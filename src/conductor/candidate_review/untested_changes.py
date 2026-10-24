"""Changed functions no recorded test executed.

A host that declares ``[tool.forge.test_record]`` records the lines every pytest run
executes, the land's targeted shards included. After the shards finish, each changed
function in a changed Python source file is looked up in that record across every
environment; one that no fixture or test executed in its current content is reported.
The finding is MEDIUM, so it names what a change left untested without blocking a host
that blocks at HIGH. A host without the table, or with ``lines = false``, gets nothing.
"""

from __future__ import annotations

from pathlib import Path

from conductor.candidate_review.checks import ReviewContext
from conductor.candidate_review.coverage_eval import changed_source_lines
from conductor.candidate_review.model import Finding, Severity

# Functions named in the message; the evidence lists every one.
_SHOWN = 20


def untested_changes(
    ctx: ReviewContext, check_id: str
) -> tuple[list[Finding], dict[str, object]]:
    from conductor._native import (
        test_record_config_native,
        test_record_untested_native,
    )

    config = test_record_config_native(str(ctx.snapshot))
    if config is None or not config[2]:
        return [], {}
    root, store, _, _ = config
    snapshot = Path(ctx.snapshot).resolve()
    changes = [
        ((snapshot / path).relative_to(root).as_posix(), sorted(lines))
        for path, lines in sorted(changed_source_lines(ctx).items())
        if lines
    ]
    count, untested = test_record_untested_native(root, store, changes)
    metrics: dict[str, object] = {
        "changed_functions": count,
        "untested_changed_functions": len(untested),
    }
    if not untested:
        return [], metrics
    names = [f"{path}:{line} {qualname}" for path, qualname, line in untested]
    more = len(names) - _SHOWN
    shown = ", ".join(names[:_SHOWN]) + (f", and {more} more" if more > 0 else "")
    finding = Finding(
        check_id=check_id,
        rule_id="untested-changed-function",
        severity=Severity.MEDIUM,
        message=f"{len(untested)} of {count} changed functions ran in no recorded test: {shown}",
        evidence={
            "untested": [
                {"path": path, "qualname": qualname, "line": line}
                for path, qualname, line in untested
            ]
        },
    )
    return [finding], metrics
