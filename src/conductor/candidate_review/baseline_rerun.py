"""Separate pre-existing test failures from the ones a candidate introduces.

``targeted-tests-full`` runs every selected test file, so a test already red on the
base tree blocks any candidate that touches a file selecting it. The failing pytest
node ids are rerun against the base tree: ids that also fail there are reported as
``inherited`` (visible, non-blocking); ids that pass there stay blocking. If the base
rerun cannot run, everything stays blocking and the finding says why.
"""

from __future__ import annotations

import dataclasses
import re
import subprocess
import sys
from collections.abc import Sequence

from conductor.candidate_review.checks import ReviewContext
from conductor.candidate_review.command_runner import _run_process, _tail
from conductor.candidate_review.git_source import GitSourceError, materialize_tree
from conductor.candidate_review.model import Finding, Severity
from conductor.candidate_review.policy import CheckPolicy

_SUMMARY_LINE = re.compile(r"^(?:FAILED|ERROR) (\S+?)(?: - .*)?$")
MAX_BASELINE_IDS = 2000
CHUNK_IDS = 200


def failed_node_ids(output: str) -> list[str]:
    """Node ids from pytest ``-rfE`` short-summary lines, in first-seen order."""
    seen: dict[str, None] = {}
    for line in output.splitlines():
        match = _SUMMARY_LINE.match(line.strip())
        if match:
            seen.setdefault(match.group(1))
    return list(seen)


def _base_failures(
    ctx: ReviewContext, check: CheckPolicy, node_ids: Sequence[str]
) -> set[str]:
    """Node ids among ``node_ids`` that still fail on the base tree."""
    still_failing: set[str] = set()
    with materialize_tree(ctx.repo, ctx.candidate.base_tree_oid) as (root, entries):
        base_ctx = dataclasses.replace(ctx, snapshot=root, entries=entries)
        # A file the candidate added has no base verdict, and pytest aborts the
        # whole run on a missing path argument: such ids stay new (blocking).
        node_ids = [
            node for node in node_ids if (root / node.split("::", 1)[0]).is_file()
        ]
        for start in range(0, len(node_ids), CHUNK_IDS):
            command = [
                sys.executable,
                "-m",
                "pytest",
                "-q",
                "-o",
                "addopts=",
                "-p",
                "no:cacheprovider",
                "-rfE",
                *node_ids[start : start + CHUNK_IDS],
            ]
            completed = _run_process(
                command,
                ctx=base_ctx,
                timeout_seconds=check.timeout_seconds,
                memory_mb=check.memory_mb,
                include_git_metadata=False,
                wall_timeout_seconds=check.wall_timeout_seconds,
            )
            if completed.returncode < 0:
                raise RuntimeError(
                    f"base rerun killed by signal {-completed.returncode}"
                )
            still_failing.update(
                failed_node_ids(completed.stdout + "\n" + completed.stderr)
            )
    return still_failing


def _blocking(check: CheckPolicy, message: str, evidence: dict[str, object]) -> Finding:
    return Finding(
        check_id=check.check_id,
        rule_id="targeted-test-failure",
        severity=Severity.HIGH,
        message=message,
        evidence=evidence,
    )


def baseline_failure_findings(
    ctx: ReviewContext,
    check: CheckPolicy,
    completed_all: Sequence[subprocess.CompletedProcess[str]],
    failed: Sequence[int],
    pytest_shard: Sequence[bool],
    fallback: Sequence[Finding],
) -> list[Finding]:
    """Replace blocking ``fallback`` findings with new-vs-inherited ones.

    ``pytest_shard[i]`` says whether shard ``i`` is a pytest shard (contract shards
    cannot be baselined). Any shard whose failures cannot be attributed to node ids
    keeps ``fallback`` blocking, unchanged, as does any finding other than a
    targeted-test failure.
    """
    if any(item.rule_id != "targeted-test-failure" for item in fallback):
        return list(fallback)
    per_shard = _attributable_failures(completed_all, failed, pytest_shard)
    if per_shard is None:
        return list(fallback)
    all_ids = sorted({node for ids in per_shard.values() for node in ids})
    try:
        on_base = _base_failures(ctx, check, all_ids)
    except (
        OSError,
        RuntimeError,
        ValueError,
        GitSourceError,
        subprocess.TimeoutExpired,
    ) as exc:
        reason = f"base rerun did not complete: {type(exc).__name__}: {exc}"
        return [
            dataclasses.replace(
                item, message=f"{item.message}\n[{reason}; all failures kept blocking]"
            )
            for item in fallback
        ]
    return _split_findings(check, completed_all, per_shard, all_ids, on_base)


def _attributable_failures(
    completed_all: Sequence[subprocess.CompletedProcess[str]],
    failed: Sequence[int],
    pytest_shard: Sequence[bool],
) -> dict[int, list[str]] | None:
    """Failing node ids per failed shard, or None when any cannot be baselined."""
    per_shard: dict[int, list[str]] = {}
    for index in failed:
        ids = failed_node_ids(completed_all[index].stdout)
        if not pytest_shard[index] or not ids:
            return None
        per_shard[index] = ids
    total = len({node for ids in per_shard.values() for node in ids})
    if not per_shard or total > MAX_BASELINE_IDS:
        return None
    return per_shard


def _split_findings(
    check: CheckPolicy,
    completed_all: Sequence[subprocess.CompletedProcess[str]],
    per_shard: dict[int, list[str]],
    all_ids: Sequence[str],
    on_base: set[str],
) -> list[Finding]:
    new = [node for node in all_ids if node not in on_base]
    inherited = [node for node in all_ids if node in on_base]
    findings: list[Finding] = []
    if new:
        blocking_shards = [i for i, ids in per_shard.items() if set(ids) - on_base]
        findings.append(
            _blocking(
                check,
                f"{len(new)} test(s) fail on the candidate but pass on base: "
                + ", ".join(new[:20])
                + "\n"
                + _tail(
                    "\n".join(completed_all[i].stdout for i in blocking_shards),
                    check.max_output_chars,
                ).strip(),
                {
                    "new_failures": new,
                    "failed_shards": [i + 1 for i in blocking_shards],
                },
            )
        )
    if inherited:
        findings.append(
            dataclasses.replace(
                _blocking(
                    check,
                    f"{len(inherited)} test(s) already fail on base (pre-existing, "
                    "not caused by this candidate): " + ", ".join(inherited[:50]),
                    {"base_failures": inherited},
                ),
                inherited=True,
            )
        )
    return findings
