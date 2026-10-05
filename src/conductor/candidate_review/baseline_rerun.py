"""Separate pre-existing test failures from the ones a candidate introduces.

``targeted-tests-full`` runs every selected test file, so a test already red on the
base tree blocks any candidate that touches a file selecting it. Each failed pytest
shard is replayed on the base tree with the same test files in the same order, since
many failures depend on what ran earlier in the process (RNG state, files a previous
test wrote). A failing node id that also fails in its base replay is reported as
``inherited`` (visible, non-blocking); one that passes there stays blocking. If the
base replay cannot run, everything stays blocking and the finding says why.
"""

from __future__ import annotations

import dataclasses
import re
import subprocess
import sys
from collections.abc import Mapping, Sequence

from conductor.candidate_review.checks import ReviewContext
from conductor.candidate_review.command_runner import _tail
from conductor.candidate_review.git_source import GitSourceError, materialize_tree
from conductor.candidate_review.model import Finding, Severity
from conductor.candidate_review.policy import CheckPolicy
from conductor.candidate_review.sharding import execute_shards

_SUMMARY_LINE = re.compile(r"^(?:FAILED|ERROR) (\S+?)(?: - .*)?$")
MAX_BASELINE_IDS = 2000


def failed_node_ids(output: str) -> list[str]:
    """Node ids from pytest ``-rfE`` short-summary lines, in first-seen order."""
    seen: dict[str, None] = {}
    for line in output.splitlines():
        match = _SUMMARY_LINE.match(line.strip())
        if match:
            seen.setdefault(match.group(1))
    return list(seen)


def _base_failures(
    ctx: ReviewContext, check: CheckPolicy, shard_tests: Mapping[int, Sequence[str]]
) -> dict[int, set[str]]:
    """Per shard, the node ids that fail when that shard is replayed on base."""
    with materialize_tree(ctx.repo, ctx.candidate.base_tree_oid) as (root, entries):
        base_ctx = dataclasses.replace(ctx, snapshot=root, entries=entries)
        # A file the candidate added has no base verdict, and pytest aborts the
        # whole run on a missing path argument: its ids stay new (blocking).
        kept = {
            index: [t for t in tests if (root / t.split("::", 1)[0]).is_file()]
            for index, tests in shard_tests.items()
        }
        order = [index for index, tests in kept.items() if tests]
        commands = [
            [
                sys.executable,
                "-m",
                "pytest",
                "-q",
                "-o",
                "addopts=",
                "-p",
                "no:cacheprovider",
                "-rfE",
                *kept[index],
            ]
            for index in order
        ]
        completed, timed_out = execute_shards(base_ctx, commands, check)
    if timed_out:
        raise RuntimeError(f"{len(timed_out)} base replay shard(s) timed out")
    result: dict[int, set[str]] = {index: set() for index in shard_tests}
    for index, done in zip(order, completed, strict=True):
        if done is None or done.returncode < 0:
            raise RuntimeError(f"base replay of shard {index + 1} was killed")
        result[index] = set(failed_node_ids(done.stdout + "\n" + done.stderr))
    return result


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
    shards: Sequence[Sequence[str]],
    completed_all: Sequence[subprocess.CompletedProcess[str]],
    failed: Sequence[int],
    pytest_shard: Sequence[bool],
    fallback: Sequence[Finding],
) -> list[Finding]:
    """Replace blocking ``fallback`` findings with new-vs-inherited ones.

    ``shards[i]`` lists shard ``i``'s test paths and ``pytest_shard[i]`` says whether
    it is a pytest shard (contract shards cannot be baselined). Any shard whose
    failures cannot be attributed to node ids keeps ``fallback`` blocking, unchanged,
    as does any finding other than a targeted-test failure.
    """
    if any(item.rule_id != "targeted-test-failure" for item in fallback):
        return list(fallback)
    per_shard = _attributable_failures(completed_all, failed, pytest_shard)
    if per_shard is None:
        return list(fallback)
    try:
        on_base = _base_failures(ctx, check, {i: shards[i] for i in per_shard})
    except (
        OSError,
        RuntimeError,
        ValueError,
        GitSourceError,
        subprocess.TimeoutExpired,
    ) as exc:
        reason = f"base replay did not complete: {type(exc).__name__}: {exc}"
        return [
            dataclasses.replace(
                item, message=f"{item.message}\n[{reason}; all failures kept blocking]"
            )
            for item in fallback
        ]
    return _split_findings(check, completed_all, per_shard, on_base)


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
    total = sum(len(ids) for ids in per_shard.values())
    if not per_shard or total > MAX_BASELINE_IDS:
        return None
    return per_shard


def _split_findings(
    check: CheckPolicy,
    completed_all: Sequence[subprocess.CompletedProcess[str]],
    per_shard: dict[int, list[str]],
    on_base: dict[int, set[str]],
) -> list[Finding]:
    new_by_shard = {
        i: [node for node in ids if node not in on_base[i]]
        for i, ids in per_shard.items()
    }
    new = sorted({node for ids in new_by_shard.values() for node in ids})
    inherited = sorted(
        {node for i, ids in per_shard.items() for node in ids if node in on_base[i]}
    )
    findings: list[Finding] = []
    if new:
        blocking_shards = [i for i, ids in new_by_shard.items() if ids]
        findings.append(
            _blocking(
                check,
                f"{len(new)} test(s) fail on the candidate but pass when their shard "
                "is replayed on base: "
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
                    f"{len(inherited)} test(s) also fail when their shard is replayed "
                    "on base (pre-existing, not caused by this candidate): "
                    + ", ".join(inherited[:50]),
                    {"base_failures": inherited},
                ),
                inherited=True,
            )
        )
    return findings
