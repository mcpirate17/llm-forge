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
import signal
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
_ANSI_ESCAPE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
_SUMMARY_ITEM = re.compile(
    r"(?i)(\d+)\s+(passed|failed|errors?|skipped|xfailed|xpassed|warnings?|deselected)"
)
_TEST_SUMMARY = re.compile(
    r"(?i)^(?P<counts>\d+\s+(?:passed|failed|errors?|skipped|xfailed|xpassed|warnings?|deselected)"
    r"(?:,\s*\d+\s+(?:passed|failed|errors?|skipped|xfailed|xpassed|warnings?|deselected))*)"
    r"\s+in\s+\d+(?:\.\d+)?s(?:\s+\([^)]*\))?$"
)
MAX_BASELINE_IDS = 2000
MAX_BASE_REPLAY_TAIL_CHARS = 1000


class BaseReplayInconclusive(RuntimeError):
    """The base replay did not produce a trustworthy pytest test verdict."""

    def __init__(
        self,
        reason: str,
        *,
        shard_index: int | None = None,
        completed: subprocess.CompletedProcess[str] | None = None,
        affected_shards: Sequence[int] = (),
    ) -> None:
        self.reason = reason
        indexes = list(affected_shards) or (
            [shard_index] if shard_index is not None else [None]
        )
        self.shards = [
            {
                "shard_index": index,
                "returncode": completed.returncode if completed is not None else None,
                "stdout_tail": _diagnostic_tail(
                    completed.stdout if completed is not None else None
                ),
                "stderr_tail": _diagnostic_tail(
                    completed.stderr if completed is not None else None
                ),
            }
            for index in indexes
        ]
        self.evidence = {
            "status": "inconclusive",
            "reason": reason,
            "shards": self.shards,
        }
        shard_label = (
            f"shard {shard_index} " if shard_index is not None else "base replay "
        )
        super().__init__(f"{shard_label}{reason}")


def _diagnostic_tail(output: str | None) -> str:
    """Keep a bounded, nonempty diagnostic even when a stream was empty."""
    if output is None:
        return "(no process output was available)"
    tail = _tail(output.strip(), MAX_BASE_REPLAY_TAIL_CHARS)
    return tail or "(process stream was empty)"


def _summary_counts(output: str) -> dict[str, int] | None:
    """Read only pytest's final terminal summary line, never traceback/captured text."""
    lines = [line.strip() for line in output.splitlines() if line.strip()]
    if not lines:
        return None
    summary = _ANSI_ESCAPE.sub("", lines[-1]).strip()
    # Pytest uses a separator around this exact summary when verbosity is nonnegative.
    summary = summary.strip("= ").strip()
    match = _TEST_SUMMARY.fullmatch(summary)
    if match is None:
        return None
    counts: dict[str, int] = {}
    for item in _SUMMARY_ITEM.finditer(match.group("counts")):
        name = item.group(2).lower()
        if name.endswith("s"):
            name = name[:-1]
        counts[name] = counts.get(name, 0) + int(item.group(1))
    return counts


def _ran_test_count(counts: Mapping[str, int]) -> int:
    """Tests that reached a test outcome; warnings and deselected items are not tests."""
    return sum(
        counts.get(name, 0)
        for name in ("passed", "failed", "error", "skipped", "xfailed", "xpassed")
    )


def _timed_out_shards(
    order: Sequence[int], timed_out_commands: Sequence[int]
) -> list[int]:
    """Map timed-out execute_shards ordinals back to original 1-based shard IDs."""
    return [order[index] + 1 for index in timed_out_commands]


def failed_node_ids(output: str) -> list[str]:
    """Node ids from pytest ``-rfE`` short-summary lines, in first-seen order."""
    seen: dict[str, None] = {}
    for line in output.splitlines():
        match = _SUMMARY_LINE.match(line.strip())
        if match:
            seen.setdefault(match.group(1))
    return list(seen)


def _failure_node_ids_by_kind(output: str) -> tuple[set[str], set[str]]:
    """Separate pytest failure and error node IDs so verdict counts must agree."""
    failed: set[str] = set()
    errors: set[str] = set()
    for line in output.splitlines():
        match = _SUMMARY_LINE.match(line.strip())
        if match is None:
            continue
        node_id = match.group(1)
        if line.lstrip().startswith("FAILED "):
            failed.add(node_id)
        else:
            errors.add(node_id)
    return failed, errors


def _base_replay_node_ids(
    completed: subprocess.CompletedProcess[str], shard_index: int
) -> set[str]:
    """Return failures only when pytest produced an unambiguous test verdict."""
    code = completed.returncode
    if code < 0:
        number = -code
        try:
            name = signal.Signals(number).name
        except ValueError:
            name = f"signal {number}"
        raise BaseReplayInconclusive(
            f"terminated by {name} (signal {number}, return code {code})",
            shard_index=shard_index,
            completed=completed,
        )
    if code not in {0, 1}:
        raise BaseReplayInconclusive(
            f"exited with non-verdict pytest code {code} (expected 0 or 1)",
            shard_index=shard_index,
            completed=completed,
        )

    # Pytest's terminal summary is the final nonblank stdout line. Searching all
    # output for diagnostic words misclassifies ordinary traceback/captured data.
    counts = _summary_counts(completed.stdout)
    failed_ids, error_ids = _failure_node_ids_by_kind(completed.stdout)
    ids = failed_ids | error_ids
    if counts is None or _ran_test_count(counts) == 0:
        raise BaseReplayInconclusive(
            "did not produce a pytest terminal test summary for any collected tests",
            shard_index=shard_index,
            completed=completed,
        )
    if code == 0 and (ids or counts.get("failed", 0) or counts.get("error", 0)):
        raise BaseReplayInconclusive(
            "exited 0 with failure summaries",
            shard_index=shard_index,
            completed=completed,
        )
    has_failed = counts.get("failed", 0) > 0
    has_errors = counts.get("error", 0) > 0
    if code == 1 and (
        not ids or has_failed != bool(failed_ids) or has_errors != bool(error_ids)
    ):
        raise BaseReplayInconclusive(
            "exited 1 without a consistent failed-test or setup-error summary",
            shard_index=shard_index,
            completed=completed,
        )
    return ids


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
        raise BaseReplayInconclusive(
            f"{len(timed_out)} base replay shard(s) exceeded their wall budget",
            affected_shards=_timed_out_shards(order, timed_out),
        )
    result: dict[int, set[str]] = {index: set() for index in shard_tests}
    for index, done in zip(order, completed, strict=True):
        if done is None:
            raise BaseReplayInconclusive(
                "produced no process result",
                shard_index=index + 1,
            )
        result[index] = _base_replay_node_ids(done, index + 1)
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
        reason = "candidate shard output did not provide attributable pytest node ids"
        return [
            dataclasses.replace(
                item,
                message=(
                    f"{item.message}\n[base attribution inconclusive: {reason}; "
                    "no base attribution was made; the candidate finding remains blocking]"
                ),
                evidence={
                    **item.evidence,
                    "base_replay": {"status": "inconclusive", "reason": reason},
                },
            )
            for item in fallback
        ]
    try:
        on_base = _base_failures(ctx, check, {i: shards[i] for i in per_shard})
    except BaseReplayInconclusive as exc:
        reason = f"base replay inconclusive: {exc}"
        replay_evidence = exc.evidence
        return [
            dataclasses.replace(
                item,
                message=(
                    f"{item.message}\n[{reason}; no base attribution was made; "
                    "the candidate finding remains blocking]"
                ),
                evidence={**item.evidence, "base_replay": replay_evidence},
            )
            for item in fallback
        ]
    except (
        OSError,
        RuntimeError,
        ValueError,
        GitSourceError,
        subprocess.TimeoutExpired,
    ) as exc:
        reason = f"base replay inconclusive: {type(exc).__name__}: {exc}"
        replay_evidence = BaseReplayInconclusive(
            str(exc), affected_shards=[index + 1 for index in per_shard]
        ).evidence
        return [
            dataclasses.replace(
                item,
                message=(
                    f"{item.message}\n[{reason}; no base attribution was made; "
                    "the candidate finding remains blocking]"
                ),
                evidence={
                    **item.evidence,
                    "base_replay": {**replay_evidence, "reason": reason},
                },
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
