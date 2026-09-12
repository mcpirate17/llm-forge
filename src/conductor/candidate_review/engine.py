"""Bounded candidate review orchestration, cache, attestation, and receipts."""

from __future__ import annotations

import fcntl
import hmac
import json
import os
import re
import secrets
import subprocess
import sys
import time
import traceback
from collections.abc import Iterator, Sequence
from concurrent.futures import ThreadPoolExecutor, as_completed
from contextlib import contextmanager
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, cast

from conductor.candidate_review import SCHEMA_VERSION
from conductor.candidate_review.checks import (
    ReviewContext,
    TestSelection,
    check_performance_evidence,
    check_research_evidence,
    files_for_policy,
    run_builtin,
)
from conductor.candidate_review.command_runner import (
    command_cache_material,
    prepare_candidate_git_environment,
    run_command_check,
    tool_version,
)
from conductor.candidate_review.git_source import git_common_dir, run_git
from conductor.candidate_review.model import (
    SEVERITY_RANK,
    Candidate,
    CheckResult,
    CheckStatus,
    Finding,
    ReviewReceipt,
    Severity,
    seal_receipt,
    sha256_file,
    sha256_json,
    write_json_atomic,
)
from conductor.candidate_review.policy import (
    CheckPolicy,
    apply_exceptions,
    baseline_receipts,
    unmatched_exceptions,
)
from conductor.candidate_review.verification import (
    check_test_evidence,
    run_targeted_tests,
)
from conductor.project_paths import package_path, package_tree_root

SPECIAL_CHECKS = {
    "performance-evidence",
    "research-integrity",
    "targeted-tests",
    "targeted-tests-full",
    "test-evidence",
}
TRAILER_TREE = "Governance-Tree"
TRAILER_POLICY = "Governance-Policy"
TRAILER_RECEIPT = "Governance-Receipt"
# Squash-merge rewrites the author of every landed commit to the account that
# merges, so `git log` cannot say which session did the work. GitHub concatenates
# the commit messages, so a trailer is the one identifier that survives -- which
# is why CLAUDE.md requires it and why this is the place it is enforced.
AGENT_TRAILER = re.compile(r"^Agent:[ \t]*([A-Za-z0-9][A-Za-z0-9._-]*)[ \t]*$", re.M)
# Enforced from five minutes after 06f725eb0 put the rule in CLAUDE.md, not
# retroactively: commits that predate it were written under no such obligation,
# and failing them would strand branches already in flight. A rebase rewrites the
# committer date, so a rewritten old commit is a new commit and is gated like one.
AGENT_TRAILER_REQUIRED_FROM = datetime(2026, 9, 6, 15, 0, tzinfo=UTC)
INHERITED_LOCK_FD_ENV = "LLM_GOVERNANCE_COMMIT_LOCK_FD"
INHERITED_LOCK_TOKEN_ENV = "LLM_GOVERNANCE_COMMIT_LOCK_TOKEN"
# A crash receipt carries the tail of the traceback, not the whole thing: the last
# frames name the defect, and an unbounded string here lands in every stored receipt.
CRASH_TRACEBACK_CHARS = 4000


@dataclass(frozen=True, slots=True)
class ReviewOutcome:
    receipt: ReviewReceipt
    results: tuple[CheckResult, ...]
    receipt_path: Path


class ResultCache:
    def __init__(self, root: Path, ttl_days: int) -> None:
        self.root = root
        self.ttl_seconds = ttl_days * 86400

    def load(self, key: str) -> CheckResult | None:
        path = self.root / f"{key}.json"
        try:
            if time.time() - path.stat().st_mtime > self.ttl_seconds:
                return None
            payload = json.loads(path.read_text(encoding="utf-8"))
            if (
                payload.get("schema_version") != SCHEMA_VERSION
                or payload.get("cache_key") != key
            ):
                return None
            result = _result_from_dict(payload["result"])
        except (OSError, ValueError, KeyError, TypeError):
            return None
        result.status = CheckStatus.CACHED
        result.cache_hit = True
        result.cache_key = key
        result.duration_ms = 0
        return result

    def store(self, key: str, result: CheckResult) -> None:
        if result.status in {CheckStatus.ERROR, CheckStatus.SKIPPED}:
            return
        payload = {
            "schema_version": SCHEMA_VERSION,
            "cache_key": key,
            "result": asdict(result),
        }
        write_json_atomic(self.root / f"{key}.json", payload)


def _result_from_dict(payload: dict[str, object]) -> CheckResult:
    findings = [
        Finding(
            check_id=str(item["check_id"]),
            rule_id=str(item["rule_id"]),
            severity=Severity(str(item["severity"])),
            message=str(item["message"]),
            path=str(item["path"]) if item.get("path") is not None else None,
            line=int(item["line"]) if item.get("line") is not None else None,
            column=int(item["column"]) if item.get("column") is not None else None,
            help=str(item["help"]) if item.get("help") is not None else None,
            evidence=dict(item.get("evidence", {})),
            fingerprint=str(item.get("fingerprint", "")),
            exception_id=(
                str(item["exception_id"])
                if item.get("exception_id") is not None
                else None
            ),
        )
        for item in cast(list[dict[str, Any]], payload.get("findings", []))
    ]
    return CheckResult(
        check_id=str(payload["check_id"]),
        status=CheckStatus(str(payload["status"])),
        duration_ms=int(cast(int | str, payload.get("duration_ms", 0))),
        findings=findings,
        files=[str(item) for item in payload.get("files", [])],  # type: ignore[union-attr]
        command=[str(item) for item in payload.get("command", [])],  # type: ignore[union-attr]
        tool_version=(
            str(payload["tool_version"])
            if payload.get("tool_version") is not None
            else None
        ),
        cache_key=str(payload["cache_key"]) if payload.get("cache_key") else None,
        cache_hit=bool(payload.get("cache_hit", False)),
        exit_code=int(cast(int | str, payload["exit_code"]))
        if payload.get("exit_code") is not None
        else None,
        skipped_reason=(
            str(payload["skipped_reason"])
            if payload.get("skipped_reason") is not None
            else None
        ),
        stdout_tail=str(payload.get("stdout_tail", "")),
        stderr_tail=str(payload.get("stderr_tail", "")),
        metrics=dict(cast(dict[str, Any], payload.get("metrics", {}))),
    )


def _package_hash(root: Path) -> tuple[str, dict[str, str]]:
    """The review engine's own sources under ``root``, keyed root-relative.

    Where the package sits is the host's to declare: the monorepo keeps it at the
    repo root, a src layout two levels down. Reading it back through
    ``project_paths`` means the candidate snapshot answers for its own layout, so a
    candidate that moves the package reports a hash difference rather than an
    absent engine.
    """
    package = package_path(root) / "candidate_review"
    if not package.is_dir():
        return "", {}
    files = {
        path.relative_to(root).as_posix(): sha256_file(path)
        for path in sorted(package.glob("*.py"))
        if path.is_file()
    }
    return sha256_json(files), files


def _engine_integrity(ctx: ReviewContext) -> tuple[dict[str, object], CheckResult]:
    started = time.perf_counter()
    candidate_hash, candidate_files = _package_hash(ctx.snapshot)
    runtime_root = package_tree_root(Path(__file__).resolve().parents[1])
    runtime_hash, runtime_files = _package_hash(runtime_root)
    findings: list[Finding] = []
    if not candidate_hash:
        findings.append(
            Finding(
                check_id="engine-integrity",
                rule_id="engine-absent-from-candidate",
                severity=Severity.CRITICAL,
                message="candidate tree does not contain the governance engine that is reviewing it",
            )
        )
    elif runtime_hash != candidate_hash:
        findings.append(
            Finding(
                check_id="engine-integrity",
                rule_id="dirty-engine-source",
                severity=Severity.CRITICAL,
                message="executing governance engine differs from the exact candidate-tree engine",
                evidence={
                    "runtime_sha256": runtime_hash,
                    "candidate_sha256": candidate_hash,
                    "runtime_files": runtime_files,
                    "candidate_files": candidate_files,
                },
            )
        )
    result = CheckResult(
        check_id="engine-integrity",
        status=CheckStatus.FAILED if findings else CheckStatus.PASSED,
        duration_ms=round((time.perf_counter() - started) * 1000),
        findings=[finding.finalize() for finding in findings],
        metrics={"candidate_sha256": candidate_hash, "runtime_sha256": runtime_hash},
    )
    return {
        "schema_version": SCHEMA_VERSION,
        "candidate_source_sha256": candidate_hash,
        "runtime_source_sha256": runtime_hash,
        "files": candidate_files,
    }, result


def _governance_lock_path(repo: Path) -> Path:
    return git_common_dir(repo) / "governance" / "commit-review.lock"


def _inherited_lock_fd(lock_path: Path) -> int | None:
    raw = os.environ.get(INHERITED_LOCK_FD_ENV)
    if raw is None:
        return None
    try:
        file_descriptor = int(raw)
        if file_descriptor <= 2:
            return None
        inherited = os.fstat(file_descriptor)
        expected = lock_path.stat()
        if (inherited.st_dev, inherited.st_ino) != (expected.st_dev, expected.st_ino):
            return None
        fcntl.flock(file_descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except (OSError, ValueError):
        return None
    return file_descriptor


def _is_ancestor_process(process_id: int) -> bool:
    current = os.getpid()
    visited: set[int] = set()
    while current > 1 and current not in visited:
        if current == process_id:
            return True
        visited.add(current)
        try:
            status = Path(f"/proc/{current}/status").read_text(encoding="utf-8")
            parent_line = next(
                line for line in status.splitlines() if line.startswith("PPid:")
            )
            current = int(parent_line.split(":", 1)[1].strip())
        except (OSError, StopIteration, ValueError):
            return False
    return current == process_id


def _inherited_lock_token_valid(lock_path: Path) -> bool:
    token = os.environ.get(INHERITED_LOCK_TOKEN_ENV, "")
    if len(token) != 64 or any(
        character not in "0123456789abcdef" for character in token
    ):
        return False
    try:
        payload = json.loads(lock_path.read_text(encoding="utf-8"))
        owner = payload.get("pid")
        recorded = payload.get("token")
    except (OSError, AttributeError, json.JSONDecodeError):
        return False
    if (
        not isinstance(owner, int)
        or owner <= 1
        or not isinstance(recorded, str)
        or not hmac.compare_digest(recorded, token)
        or not _is_ancestor_process(owner)
    ):
        return False
    try:
        with lock_path.open("a+", encoding="utf-8") as verifier:
            try:
                fcntl.flock(verifier.fileno(), fcntl.LOCK_SH | fcntl.LOCK_NB)
            except BlockingIOError:
                return True
            fcntl.flock(verifier.fileno(), fcntl.LOCK_UN)
    except OSError:
        return False
    return False


@contextmanager
def _held_governance_lock(
    repo: Path, *, exclusive: bool, timeout_seconds: float, lease_token: str = ""
) -> Iterator[tuple[Path, int]]:
    lock_path = _governance_lock_path(repo)
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with lock_path.open("a+", encoding="utf-8") as handle:
        operation = fcntl.LOCK_EX if exclusive else fcntl.LOCK_SH
        deadline = time.monotonic() + timeout_seconds
        while True:
            try:
                fcntl.flock(handle.fileno(), operation | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise TimeoutError(
                        f"governance commit mutex remained busy for {timeout_seconds:.1f}s: {lock_path}"
                    )
                time.sleep(0.05)
        handle.seek(0)
        handle.truncate()
        handle.write(
            json.dumps(
                {
                    "pid": os.getpid(),
                    "token": lease_token,
                    "acquired": datetime.now(UTC).isoformat(),
                },
                sort_keys=True,
            )
            + "\n"
        )
        handle.flush()
        os.fsync(handle.fileno())
        try:
            yield lock_path, handle.fileno()
        finally:
            # Erase our own record before dropping the lock, not after: the file is
            # read by _inherited_lock_token_valid, and a released lock that still
            # names a pid and a token is a lease claim with no holder behind it.
            # Only our own record -- a shared holder must not erase a peer's.
            try:
                handle.seek(0)
                payload = json.loads(handle.read() or "{}")
                if isinstance(payload, dict) and payload.get("pid") == os.getpid():
                    handle.seek(0)
                    handle.truncate()
                    handle.flush()
            except (OSError, ValueError):
                # Deliberately absorbed, and the only absorbed exception here: this
                # `finally` runs before the flock is dropped, so raising out of it
                # would hold the governance mutex for the life of the process and
                # wedge every later commit. An unerasable record costs a stale
                # diagnostic; a raise costs the lock. Covered by
                # test_an_unreadable_record_does_not_break_release.
                pass
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


@contextmanager
def governance_lock(
    repo: Path, *, exclusive: bool, timeout_seconds: float = 30.0
) -> Iterator[Path]:
    """Hold the shared governance lock, reusing a verified commit-wrapper lease."""

    lock_path = _governance_lock_path(repo)
    if not exclusive and (
        _inherited_lock_fd(lock_path) is not None
        or _inherited_lock_token_valid(lock_path)
    ):
        yield lock_path
        return
    with _held_governance_lock(
        repo, exclusive=exclusive, timeout_seconds=timeout_seconds
    ) as (held_path, _file_descriptor):
        yield held_path


def _cache_key(
    ctx: ReviewContext,
    check: CheckPolicy,
    engine_digest: str,
    baseline_data: Sequence[dict[str, object]],
    version: str | None,
) -> str:
    files = files_for_policy(ctx, check)
    material = command_cache_material(ctx, check, version, files)
    material.update(
        {
            "engine": engine_digest,
            "baselines": list(baseline_data),
            "surface_policy": ctx.surface,
        }
    )
    return sha256_json(material)


def _run_regular_check(
    ctx: ReviewContext,
    check: CheckPolicy,
    cache: ResultCache,
    engine_digest: str,
    baselines: Sequence[dict[str, object]],
) -> CheckResult:
    files = files_for_policy(ctx, check)
    if not check.always and not files:
        return CheckResult(
            check_id=check.check_id,
            status=CheckStatus.SKIPPED,
            duration_ms=0,
            skipped_reason="no matching candidate changes",
        )
    version: str | None = (
        f"builtin/{SCHEMA_VERSION}" if check.kind == "builtin" else None
    )
    version_error: str | None = None
    if check.kind == "command":
        version, version_error = tool_version(ctx, check)
    key = _cache_key(ctx, check, engine_digest, baselines, version)
    if check.cache:
        cached = cache.load(key)
        if cached is not None:
            return cached
    if check.kind == "builtin":
        result = run_builtin(ctx, check)
        result.tool_version = version
    else:
        result = run_command_check(
            ctx,
            check,
            version=version,
            version_error=version_error,
        )
    result.cache_key = key
    if check.cache:
        cache.store(key, result)
    return result


def _receipt_store(repo: Path) -> Path:
    return git_common_dir(repo) / "governance" / "receipts"


def receipt_path(repo: Path, surface: str, candidate: Candidate, profile: str) -> Path:
    identity = (
        candidate.tree_oid
        if surface == "pre-commit"
        else candidate.commit_oid or candidate.tree_oid
    )
    return _receipt_store(repo) / surface / f"{identity}-{profile}.json"


def verify_receipt_payload(payload: dict[str, object]) -> tuple[bool, str]:
    supplied = payload.get("receipt_digest")
    if not isinstance(supplied, str) or not supplied:
        return False, "receipt_digest is absent"
    candidate = dict(payload)
    candidate["receipt_id"] = ""
    candidate["receipt_digest"] = ""
    calculated = sha256_json(candidate)
    if calculated != supplied:
        return (
            False,
            f"receipt digest mismatch: expected {supplied}, calculated {calculated}",
        )
    return True, "ok"


def _matching_precommit_receipt(
    ctx: ReviewContext,
) -> tuple[dict[str, object] | None, str]:
    path = receipt_path(ctx.repo, "pre-commit", ctx.candidate, "fast")
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        return None, f"missing or unreadable pre-commit receipt: {exc}"
    valid, detail = verify_receipt_payload(payload)
    if not valid:
        return None, detail
    candidate = payload.get("candidate")
    policy = payload.get("policy")
    if (
        not isinstance(candidate, dict)
        or candidate.get("tree_oid") != ctx.candidate.tree_oid
    ):
        return None, "pre-commit receipt is bound to a different tree"
    if not isinstance(policy, dict) or policy.get("sha256") != ctx.policy.digest:
        return None, "pre-commit receipt is bound to a different policy"
    if payload.get("decision") != "pass":
        return None, "pre-commit receipt did not pass"
    return payload, "ok"


def _commit_message(repo: Path, commit_oid: str) -> str:
    return run_git(repo, ["show", "-s", "--format=%B", commit_oid]).stdout.decode(
        "utf-8", "replace"
    )


def _trailers(message: str) -> dict[str, str]:
    trailers: dict[str, str] = {}
    for line in message.splitlines():
        key, separator, value = line.partition(":")
        if separator and key in {TRAILER_TREE, TRAILER_POLICY, TRAILER_RECEIPT}:
            trailers[key] = value.strip()
    return trailers


def _agent_trailer_required(repo: Path, commit_oid: str) -> bool:
    """Whether `commit_oid` was written after the `Agent:` trailer became a rule.

    Fail-closed on an unreadable date: a commit whose committer date cannot be
    parsed is treated as current, so a malformed date cannot buy an exemption.
    """

    raw = (
        run_git(repo, ["show", "-s", "--format=%cI", commit_oid])
        .stdout.decode("utf-8", "replace")
        .strip()
    )
    try:
        committed = datetime.fromisoformat(raw)
    except ValueError:
        return True
    if committed.tzinfo is None:
        committed = committed.replace(tzinfo=UTC)
    return committed >= AGENT_TRAILER_REQUIRED_FROM


def _ci_commit_attestations(
    ctx: ReviewContext,
) -> tuple[list[str], list[str], list[str], list[str]]:
    """``(commits, missing, mismatched, unattributed)`` over the candidate range.

    Each commit is read once and charged against every rule that reads a commit
    message, so the range costs one `git show` per commit rather than one per
    rule.
    """

    raw = run_git(
        ctx.repo,
        [
            "rev-list",
            "--reverse",
            f"{ctx.candidate.base_commit_oid}..{ctx.candidate.commit_oid}",
        ],
    ).stdout.decode()
    commits = [line for line in raw.splitlines() if line]
    missing: list[str] = []
    mismatched: list[str] = []
    unattributed: list[str] = []
    for commit_oid in commits:
        message = _commit_message(ctx.repo, commit_oid)
        trailers = _trailers(message)
        tree_oid = (
            run_git(ctx.repo, ["rev-parse", f"{commit_oid}^{{tree}}"])
            .stdout.decode()
            .strip()
        )
        if not trailers:
            missing.append(commit_oid)
        elif trailers.get(TRAILER_TREE) != tree_oid:
            mismatched.append(commit_oid)
        # Regex first: the date lookup is a subprocess, and an attributed commit
        # -- the normal case -- never needs one.
        if not AGENT_TRAILER.search(message) and _agent_trailer_required(
            ctx.repo, commit_oid
        ):
            unattributed.append(commit_oid)
    return commits, missing, mismatched, unattributed


def _bypass_evidence(ctx: ReviewContext) -> tuple[dict[str, object], CheckResult]:
    started = time.perf_counter()
    findings: list[Finding] = []
    evidence: dict[str, object] = {"surface": ctx.surface}
    if ctx.surface == "post-commit":
        receipt, detail = _matching_precommit_receipt(ctx)
        evidence["precommit_receipt"] = "valid" if receipt else "missing_or_invalid"
        evidence["detail"] = detail
        if not receipt:
            findings.append(
                Finding(
                    check_id="attestation",
                    rule_id="precommit-bypass-recovered",
                    severity=Severity.MEDIUM,
                    message=(
                        "no valid pre-commit receipt matched this commit tree; "
                        "post-commit review is producing equivalent recovery evidence"
                    ),
                    evidence={"detail": detail},
                )
            )
    if (
        ctx.surface == "ci"
        and ctx.candidate.commit_oid
        and ctx.candidate.base_commit_oid
    ):
        commits, missing, mismatched, unattributed = _ci_commit_attestations(ctx)
        evidence.update(
            {
                "commits_examined": len(commits),
                "missing_attestations": missing,
                "mismatched_attestations": mismatched,
                "unattributed_commits": unattributed,
                "equivalent_ci_review": True,
            }
        )
        if unattributed:
            findings.append(
                Finding(
                    check_id="attestation",
                    rule_id="commit-agent-unattributed",
                    severity=Severity.HIGH,
                    message=(
                        f"{len(unattributed)} candidate commit(s) carry no `Agent:` "
                        "trailer naming the session that wrote them; squash-merge "
                        "rewrites the author, so the trailer is the only record "
                        "that survives. Add `Agent: <session-name>` beside "
                        "`Co-Authored-By:` and amend or rebase"
                    ),
                    evidence={"commits": unattributed},
                )
            )
        if missing or mismatched:
            findings.append(
                Finding(
                    check_id="attestation",
                    rule_id="commit-hook-bypass-detected",
                    severity=Severity.MEDIUM,
                    message=(
                        "one or more candidate commits lack a matching local attestation; "
                        "the mandatory CI review is the replacement promotion evidence"
                    ),
                    evidence={"missing": missing, "mismatched": mismatched},
                )
            )
    result = CheckResult(
        check_id="attestation",
        status=CheckStatus.FAILED if findings else CheckStatus.PASSED,
        duration_ms=round((time.perf_counter() - started) * 1000),
        findings=[finding.finalize() for finding in findings],
        metrics=evidence,
    )
    return evidence, result


def _special_results(
    ctx: ReviewContext,
    checks: dict[str, CheckPolicy],
    selection: TestSelection,
) -> list[CheckResult]:
    results: list[CheckResult] = []
    if "performance-evidence" in checks:
        results.append(check_performance_evidence(ctx, selection))
    if "research-integrity" in checks:
        results.append(check_research_evidence(ctx, selection))
    for check_id in ("targeted-tests", "targeted-tests-full"):
        if check_id in checks:
            results.append(
                run_targeted_tests(
                    ctx,
                    selection,
                    checks[check_id],
                    coverage=check_id == "targeted-tests-full",
                )
            )
    return results


def _crash_result(check_id: str, exc: Exception) -> CheckResult:
    """Turn a check that raised into a blocking receipt that says where it raised.

    Until 2026-09-05 this rendered `type(exc).__name__: exc` and dropped the
    traceback, so a `TypeError: Object of type bytes is not JSON serializable`
    arrived with no file, no line and no frames -- three rounds of manual
    bisection to find the one call site that produced it. The exception crosses
    the `ThreadPoolExecutor` boundary through `future.result()`, which preserves
    `__traceback__`, so the frames were there the whole time.

    The raise site goes in the message because that is the line an operator sees
    in the terminal; the frames go in `evidence` because that is what the receipt
    is for. `path` and `line` are deliberately left unset: a check declared
    `attribution = "diff"` treats a finding naming a path outside the candidate's
    changed files as inherited debt, and inherited findings do not block. A crash
    must always block, so it must not carry a path.
    """
    frames = traceback.extract_tb(exc.__traceback__)
    site = (
        f" (raised at {Path(frames[-1].filename).name}:{frames[-1].lineno}"
        f" in {frames[-1].name})"
        if frames
        else ""
    )
    return CheckResult(
        check_id=check_id,
        status=CheckStatus.ERROR,
        duration_ms=0,
        findings=[
            Finding(
                check_id=check_id,
                rule_id="policy-engine-crash",
                severity=Severity.CRITICAL,
                message=(
                    "check crashed inside the policy engine: "
                    f"{type(exc).__name__}: {exc}{site}"
                ),
                evidence={
                    "traceback": "".join(
                        traceback.format_exception(type(exc), exc, exc.__traceback__)
                    )[-CRASH_TRACEBACK_CHARS:]
                },
            ).finalize()
        ],
    )


def _parallel_results(
    ctx: ReviewContext,
    active: Sequence[CheckPolicy],
    engine_digest: str,
    baselines: Sequence[dict[str, object]],
) -> list[CheckResult]:
    cache_root = git_common_dir(ctx.repo) / "governance" / "cache"
    cache = ResultCache(cache_root, ctx.policy.cache_ttl_days)
    regular = [check for check in active if check.check_id not in SPECIAL_CHECKS]
    command_checks = [
        check
        for check in regular
        if check.kind == "command" and (check.always or files_for_policy(ctx, check))
    ]
    requires_test_git = any(
        check.check_id in {"targeted-tests", "targeted-tests-full"} for check in active
    )
    if command_checks or requires_test_git:
        prepare_candidate_git_environment(ctx)
    results: list[CheckResult] = []
    with ThreadPoolExecutor(max_workers=ctx.policy.max_workers) as executor:
        futures = {
            executor.submit(
                _run_regular_check,
                ctx,
                check,
                cache,
                engine_digest,
                baselines,
            ): check.check_id
            for check in regular
        }
        for future in as_completed(futures):
            check_id = futures[future]
            try:
                results.append(future.result())
            except Exception as exc:  # noqa: BLE001 - convert every check crash to a receipt
                results.append(_crash_result(check_id, exc))
    return results


def _test_and_special_results(
    ctx: ReviewContext,
    active_by_id: dict[str, CheckPolicy],
    results: list[CheckResult],
) -> TestSelection:
    test_result, selection = check_test_evidence(ctx)
    if "test-evidence" in active_by_id:
        results.append(test_result)
    critical_static = any(
        finding.severity == Severity.CRITICAL
        for result in results
        for finding in result.findings
    )
    special_checks = active_by_id
    if critical_static:
        for check_id in ("targeted-tests", "targeted-tests-full"):
            if check_id in active_by_id:
                results.append(
                    CheckResult(
                        check_id=check_id,
                        status=CheckStatus.SKIPPED,
                        duration_ms=0,
                        skipped_reason=(
                            "critical static evidence failed before test execution"
                        ),
                    )
                )
        special_checks = {
            key: value
            for key, value in active_by_id.items()
            if key not in {"targeted-tests", "targeted-tests-full"}
        }
    results.extend(_special_results(ctx, special_checks, selection))
    return selection


def candidate_changed_paths(ctx: ReviewContext) -> set[str]:
    """Every path this candidate touched, old names included.

    A rename must count under both names, or the finding on the renamed-from path
    reads as inherited and a real regression walks through.
    """
    paths: set[str] = set()
    for change in ctx.candidate.changes:
        for path in (change.path, change.old_path):
            if path:
                paths.add(path)
    return paths


def mark_inherited(
    ctx: ReviewContext, findings: Sequence[Finding], changed: set[str]
) -> None:
    """Flag findings that are pre-existing tree debt rather than this candidate's doing.

    Only checks declaring `attribution = "diff"` participate; everything else keeps
    blocking exactly as before, so this narrows nothing by default. For a "diff"
    check, a finding blocks when it names a changed path, and is inherited when it
    names an unchanged path or no path at all -- a pathless finding from a check whose
    job is judging changed files is, by construction, a whole-tree aggregate.

    Why this exists: a PR adding one new module was blocked by an unused variable in
    a file it never opened and a note duplicating http_transport.py. Compliance was
    unachievable by doing your own work well, so agents routed around the gate --
    force-push, then a 13-branch fan-out, then a three-day merge. A gate that blocks
    on debt you did not create does not get obeyed; it gets bypassed.
    """
    attribution = {check.check_id: check.attribution for check in ctx.policy.checks}
    for finding in findings:
        if attribution.get(finding.check_id, "candidate") != "diff":
            continue
        finding.inherited = finding.path is None or finding.path not in changed


def _review_findings(
    ctx: ReviewContext, results: Sequence[CheckResult]
) -> tuple[list[Finding], str]:
    findings = [finding for result in results for finding in result.findings]
    apply_exceptions(ctx.policy, findings)
    mark_inherited(ctx, findings, candidate_changed_paths(ctx))
    blocking = any(
        finding.exception_id is None
        and not finding.inherited
        and SEVERITY_RANK[finding.severity] >= SEVERITY_RANK[ctx.policy.block_at]
        for finding in findings
    )
    return findings, "fail" if blocking else "pass"


def _review_binding(
    ctx: ReviewContext,
    engine: dict[str, object],
    baselines: Sequence[dict[str, object]],
    results: Sequence[CheckResult],
) -> str:
    return sha256_json(
        {
            "tree_oid": ctx.candidate.tree_oid,
            "base_tree_oid": ctx.candidate.base_tree_oid,
            "commit_oid": ctx.candidate.commit_oid,
            "policy_sha256": ctx.policy.digest,
            "engine_sha256": engine["candidate_source_sha256"],
            "profile": ctx.profile,
            "checks": [result.check_id for result in results],
            "baselines": list(baselines),
        }
    )


def examined_paths(results: Sequence[CheckResult]) -> dict[str, set[str]]:
    """Every path each check actually read, by check id.

    A check can be reported in more than one result -- a sharded run, or a
    special-cased second pass -- so the sets are unioned rather than assigned.
    Taking the last result instead would say a check never opened files it did,
    which reads downstream as "nothing examined that path".
    """
    examined: dict[str, set[str]] = {}
    for result in results:
        examined.setdefault(result.check_id, set()).update(result.files)
    return examined


def _build_receipt(
    ctx: ReviewContext,
    *,
    wall_started: datetime,
    monotonic_started: float,
    engine: dict[str, object],
    selection: TestSelection,
    bypass: dict[str, object],
    baselines: list[dict[str, object]],
    results: list[CheckResult],
) -> ReviewReceipt:
    findings, decision = _review_findings(ctx, results)
    receipt = ReviewReceipt(
        schema_version=SCHEMA_VERSION,
        receipt_id="",
        receipt_digest="",
        surface=ctx.surface,
        profile=ctx.profile,
        decision=decision,
        candidate={
            "kind": ctx.candidate.kind,
            "tree_oid": ctx.candidate.tree_oid,
            "base_tree_oid": ctx.candidate.base_tree_oid,
            "base_commit_oid": ctx.candidate.base_commit_oid,
            "commit_oid": ctx.candidate.commit_oid,
            "target_ref": ctx.candidate.target_ref,
            "changes": [asdict(change) for change in ctx.candidate.changes],
        },
        policy={
            "path": ctx.policy.path.relative_to(ctx.snapshot).as_posix(),
            "sha256": ctx.policy.digest,
            "schema_version": ctx.policy.schema_version,
            "block_at": ctx.policy.block_at.value,
            "baseline_expires": ctx.policy.baseline_expires.isoformat(),
            "unmatched_exceptions": list(
                unmatched_exceptions(ctx.policy, examined_paths(results), findings)
            ),
        },
        engine=engine,
        graph=selection.graph,
        bypass=bypass,
        timings={
            "started_at": wall_started.isoformat(),
            "finished_at": datetime.now(UTC).isoformat(),
            "duration_ms": round((time.perf_counter() - monotonic_started) * 1000),
        },
        cache={
            "hits": sum(result.cache_hit for result in results),
            "misses": sum(
                bool(result.cache_key) and not result.cache_hit for result in results
            ),
        },
        baselines=baselines,
        checks=[asdict(result.finalize()) for result in results],
        findings=[asdict(finding) for finding in findings],
        binding=_review_binding(ctx, engine, baselines, results),
    )
    return seal_receipt(receipt)


def run_review(ctx: ReviewContext) -> ReviewOutcome:
    wall_started = datetime.now(UTC)
    monotonic_started = time.perf_counter()
    engine, engine_result = _engine_integrity(ctx)
    baselines = baseline_receipts(ctx.policy, ctx.snapshot, ctx.profile, ctx.classes)
    active = ctx.policy.active_checks(ctx.profile)
    active_by_id = {check.check_id: check for check in active}
    results = [
        engine_result,
        *_parallel_results(
            ctx,
            active,
            str(engine["candidate_source_sha256"]),
            baselines,
        ),
    ]
    selection = _test_and_special_results(ctx, active_by_id, results)
    bypass, attestation_result = _bypass_evidence(ctx)
    results.append(attestation_result)
    order = {check.check_id: index for index, check in enumerate(active)}
    order.update({"engine-integrity": -2, "attestation": len(order) + 10})
    results.sort(
        key=lambda result: (order.get(result.check_id, len(order)), result.check_id)
    )
    receipt = _build_receipt(
        ctx,
        wall_started=wall_started,
        monotonic_started=monotonic_started,
        engine=engine,
        selection=selection,
        bypass=bypass,
        baselines=baselines,
        results=results,
    )
    path = receipt_path(ctx.repo, ctx.surface, ctx.candidate, ctx.profile)
    write_json_atomic(path, receipt.to_dict())
    return ReviewOutcome(receipt=receipt, results=tuple(results), receipt_path=path)


def append_attestation(message_file: Path, repo: Path) -> dict[str, str]:
    candidate_tree = run_git(repo, ["write-tree"]).stdout.decode().strip()
    receipt_candidate = Candidate(
        kind="index",
        tree_oid=candidate_tree,
        base_tree_oid="",
        base_commit_oid=None,
        commit_oid=None,
        target_ref=None,
        changes=(),
    )
    path = receipt_path(repo, "pre-commit", receipt_candidate, "fast")
    payload = json.loads(path.read_text(encoding="utf-8"))
    valid, detail = verify_receipt_payload(payload)
    if not valid or payload.get("decision") != "pass":
        raise RuntimeError(
            f"cannot attest commit without a valid passing receipt: {detail}"
        )
    candidate = payload.get("candidate")
    policy = payload.get("policy")
    if not isinstance(candidate, dict) or candidate.get("tree_oid") != candidate_tree:
        raise RuntimeError(
            "pre-commit receipt tree no longer matches the candidate index"
        )
    if not isinstance(policy, dict) or not isinstance(policy.get("sha256"), str):
        raise RuntimeError("pre-commit receipt has no bound policy digest")  # noqa: TRY004 - protocol error, not a type error
    message = message_file.read_text(encoding="utf-8")
    existing = _trailers(message)
    desired = {
        TRAILER_TREE: candidate_tree,
        TRAILER_POLICY: str(policy["sha256"]),
        TRAILER_RECEIPT: str(payload["receipt_digest"]),
    }
    if existing:
        if any(existing.get(key) != value for key, value in desired.items()):
            raise RuntimeError(
                "existing governance trailers do not match the current candidate"
            )
        return desired
    suffix = (
        "" if message.endswith("\n\n") else "\n" if message.endswith("\n") else "\n\n"
    )
    trailer_text = "\n".join(f"{key}: {value}" for key, value in desired.items()) + "\n"
    message_file.write_text(message + suffix + trailer_text, encoding="utf-8")
    return desired


def snapshot_working_tree(repo: Path, owner: str) -> str | None:
    """Commit the entire working tree to a ref before anything can discard it.

    `git commit` fires pre-commit, whose `staged_files_only` writes a patch and then
    runs `git checkout -- .` across the WHOLE worktree to isolate the staged content.
    That wipes every unstaged tracked modification, including files belonging to other
    agents working in the same checkout, and it never captures untracked files at all.
    If the process dies between the checkout and the restore -- OOM, prlimit kill,
    Ctrl-C during a multi-minute review -- the only copy left is a patch file under
    ~/.cache/pre-commit/.

    So take a real snapshot first. A private GIT_INDEX_FILE keeps the repository's
    shared index untouched (writing to it is itself a way to destroy a peer's staged
    work), and the result is an ordinary commit object reachable from
    refs/snapshots/<owner>/<timestamp>, recoverable with git checkout long after the
    patch cache has been pruned.

    Returns the ref, or None if the snapshot could not be taken -- the caller decides
    whether that is fatal. It never raises: failing to snapshot must not be a new way
    to fail a commit.
    """
    stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    ref = f"refs/snapshots/{owner}/{stamp}"
    # In a linked worktree `.git` is a FILE pointing at the real gitdir, so the index
    # path has to be resolved by git rather than assembled from the repo root.
    try:
        git_dir = Path(
            subprocess.run(
                ["git", "rev-parse", "--absolute-git-dir"],
                cwd=repo,
                capture_output=True,
                text=True,
                check=True,
            ).stdout.strip()
        )
    except (subprocess.CalledProcessError, OSError):
        return None
    index_file = git_dir / f"governance-snapshot-index-{os.getpid()}"
    environment = {**os.environ, "GIT_INDEX_FILE": str(index_file)}

    def run(*args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["git", *args],
            cwd=repo,
            env=environment,
            capture_output=True,
            text=True,
            check=True,
        )

    try:
        head = run("rev-parse", "HEAD").stdout.strip()
        run("read-tree", head)
        # -A picks up untracked files too; pre-commit's isolation never captures them.
        run("add", "-A")
        tree = run("write-tree").stdout.strip()
        message = f"governance-commit snapshot for {owner} at {stamp}"
        commit = run("commit-tree", tree, "-p", head, "-m", message).stdout.strip()
        run("update-ref", ref, commit)
        return ref
    except (subprocess.CalledProcessError, OSError):
        return None
    finally:
        index_file.unlink(missing_ok=True)


def run_locked_git_commit(repo: Path, args: Sequence[str]) -> int:
    if not args or args[0] != "commit":
        raise ValueError(
            "commit mutex wrapper accepts only arguments beginning with 'commit'"
        )
    owner = os.environ.get("GOVERNANCE_OWNER")
    if owner is None:
        owner = "unknown"
    snapshot_ref = snapshot_working_tree(repo, owner)
    if snapshot_ref:
        print(
            f"governance-commit: working tree snapshotted to {snapshot_ref}",
            file=sys.stderr,
        )
    else:
        print(
            "governance-commit: WARNING -- could not snapshot the working tree; "
            "uncommitted work is not recoverable if this commit's hooks discard it",
            file=sys.stderr,
        )
    lease_token = secrets.token_hex(32)
    with _held_governance_lock(
        repo,
        exclusive=True,
        timeout_seconds=60.0,
        lease_token=lease_token,
    ) as (_lock_path, file_descriptor):
        environment = os.environ.copy()
        environment[INHERITED_LOCK_FD_ENV] = str(file_descriptor)
        environment[INHERITED_LOCK_TOKEN_ENV] = lease_token
        return subprocess.run(
            ["git", *args],
            cwd=repo,
            env=environment,
            pass_fds=(file_descriptor,),
            check=False,
        ).returncode
