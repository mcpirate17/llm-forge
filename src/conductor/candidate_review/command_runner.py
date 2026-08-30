"""Hermetic, bounded execution for configured external analyzers."""

from __future__ import annotations

import os
import resource
import shutil
import subprocess
import sys
import time
import warnings
from pathlib import Path
from typing import Mapping, Sequence

from conductor.candidate_review.checks import ReviewContext, _result, files_for_policy
from conductor.candidate_review.model import (
    CheckResult,
    CheckStatus,
    Finding,
    Severity,
)
from conductor.candidate_review.policy import CheckPolicy


def _repository_objects(ctx: ReviewContext) -> str:
    completed = subprocess.run(
        ["git", "rev-parse", "--path-format=absolute", "--git-path", "objects"],
        cwd=ctx.repo,
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode:
        raise RuntimeError(
            "cannot resolve repository object directory for isolated analyzers: "
            + (completed.stderr.strip() or completed.stdout.strip())
        )
    return completed.stdout.strip()


def _tool_git_dir(ctx: ReviewContext) -> str:
    return str(ctx.runtime_dir / "tool-repo.git")


def _run_git_setup(
    command: list[str], *, cwd: str, environment: dict[str, str], input_text: str = ""
) -> str:
    completed = subprocess.run(
        command,
        cwd=cwd,
        env=environment,
        input=input_text or None,
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode:
        detail = completed.stderr.strip() or completed.stdout.strip()
        raise RuntimeError(
            f"isolated analyzer Git setup failed ({' '.join(command)}): {detail}"
        )
    return completed.stdout.strip()


def prepare_candidate_git_environment(ctx: ReviewContext) -> None:
    """Create an isolated Git repository backed by the candidate object database."""

    ctx.runtime_dir.mkdir(parents=True, exist_ok=True)
    git_dir = ctx.runtime_dir / "tool-repo.git"
    index = ctx.runtime_dir / "candidate-index"
    base_environment = {
        "PATH": os.environ.get("PATH", ""),
        "GIT_AUTHOR_NAME": "candidate-review",
        "GIT_AUTHOR_EMAIL": "candidate-review@example.invalid",
        "GIT_COMMITTER_NAME": "candidate-review",
        "GIT_COMMITTER_EMAIL": "candidate-review@example.invalid",
        "GIT_AUTHOR_DATE": "2000-01-01T00:00:00+00:00",
        "GIT_COMMITTER_DATE": "2000-01-01T00:00:00+00:00",
    }
    _run_git_setup(
        ["git", "init", "--bare", "--quiet", str(git_dir)],
        cwd=str(ctx.runtime_dir),
        environment=base_environment,
    )
    alternates = git_dir / "objects" / "info" / "alternates"
    alternates.write_text(_repository_objects(ctx) + "\n", encoding="utf-8")
    environment = {
        **base_environment,
        "GIT_DIR": str(git_dir),
        "GIT_INDEX_FILE": str(index),
        "GIT_OPTIONAL_LOCKS": "0",
        "GIT_WORK_TREE": str(ctx.snapshot),
    }
    commit = _run_git_setup(
        ["git", "commit-tree", ctx.candidate.tree_oid],
        cwd=str(ctx.snapshot),
        environment=environment,
        input_text="candidate review snapshot\n",
    )
    _run_git_setup(
        ["git", "update-ref", "refs/heads/candidate", commit],
        cwd=str(ctx.snapshot),
        environment=environment,
    )
    _run_git_setup(
        ["git", "symbolic-ref", "HEAD", "refs/heads/candidate"],
        cwd=str(ctx.snapshot),
        environment=environment,
    )
    _run_git_setup(
        ["git", "read-tree", ctx.candidate.tree_oid],
        cwd=str(ctx.snapshot),
        environment=environment,
    )


def _changed_files_file(
    ctx: ReviewContext, check_id: str, files: Sequence[str]
) -> Path:
    """Write the check's changed-file list to a scratch file and return its path.

    Some analyzers (jscpd, vulture) always scan the whole tree against a
    baseline rather than operating on ``{files}`` directly, so their
    ``--changed-file(s)`` attribution flag needs a path on disk, not an
    inline argument list that could run into thousands of entries.
    """
    ctx.runtime_dir.mkdir(parents=True, exist_ok=True)
    path = ctx.runtime_dir / f"changed-files-{check_id}.txt"
    path.write_text("\n".join(files) + ("\n" if files else ""), encoding="utf-8")
    return path


def _expand_command(
    template: Sequence[str],
    ctx: ReviewContext,
    files: Sequence[str],
    *,
    check_id: str = "command",
) -> list[str]:
    substitutions = {
        "{repo}": str(ctx.repo),
        "{snapshot}": str(ctx.snapshot),
        "{base}": ctx.candidate.base_commit_oid or ctx.candidate.base_tree_oid,
        "{tree}": ctx.candidate.tree_oid,
        "{python}": sys.executable,
    }
    if any("{changed_files_file}" in item for item in template):
        substitutions["{changed_files_file}"] = str(
            _changed_files_file(ctx, check_id, files)
        )
    output: list[str] = []
    for item in template:
        if item == "{files}":
            output.extend(files)
            continue
        for key, value in substitutions.items():
            item = item.replace(key, value)
        output.append(item)
    return output


def _environment(
    ctx: ReviewContext,
    *,
    include_git_metadata: bool = True,
    extra: Mapping[str, str] | None = None,
) -> dict[str, str]:
    allowed = {
        "CUDA_VISIBLE_DEVICES",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "LANG",
        "LC_ALL",
        "LD_LIBRARY_PATH",
        "NO_PROXY",
        "OMP_NUM_THREADS",
        "PATH",
        "REQUESTS_CA_BUNDLE",
        "ROCR_VISIBLE_DEVICES",
        "SSL_CERT_FILE",
        "TERM",
        "TZ",
    }
    environment = {key: value for key, value in os.environ.items() if key in allowed}
    isolated_home = ctx.runtime_dir / "home"
    isolated_home.mkdir(parents=True, exist_ok=True)
    environment.update(
        {
            "HOME": str(isolated_home),
            "PYTHONPATH": str(ctx.snapshot),
            "PYTHONDONTWRITEBYTECODE": "1",
            "PYTHONHASHSEED": "0",
            "RUFF_CACHE_DIR": str(ctx.runtime_dir / "ruff-cache"),
            "MYPY_CACHE_DIR": str(ctx.runtime_dir / "mypy-cache"),
            "COVERAGE_FILE": str(ctx.runtime_dir / ".coverage"),
            "GIT_OPTIONAL_LOCKS": "0",
            "XDG_CACHE_HOME": str(ctx.runtime_dir / "xdg-cache"),
            "XDG_CONFIG_HOME": str(ctx.runtime_dir / "xdg-config"),
            "XDG_DATA_HOME": str(ctx.runtime_dir / "xdg-data"),
        }
    )
    if include_git_metadata:
        environment.update(
            {
                "GIT_DIR": _tool_git_dir(ctx),
                "GIT_INDEX_FILE": str(ctx.runtime_dir / "candidate-index"),
                "GIT_WORK_TREE": str(ctx.snapshot),
            }
        )
    node_bin = ctx.repo / "node_modules" / ".bin"
    if node_bin.is_dir():
        environment["PATH"] = f"{node_bin}{os.pathsep}{environment.get('PATH', '')}"
    if extra:
        # Last, so a caller's pin beats the `allowed` passthrough of the same key.
        environment.update(extra)
    return environment


def _limited_command(
    command: list[str], *, memory_mb: int, timeout_seconds: int
) -> list[str]:
    prlimit = shutil.which("prlimit")
    if not prlimit:
        # Returning the bare command drops the CPU and address-space budget for every
        # analyzer and every test shard -- silently, with no finding and no log line.
        # That is exactly the "green locally, red in CI" shape the 2026-08-29 reset
        # exists to remove, so say it out loud. It is a warning rather than a raise
        # because the caller may be a developer probe on a box without util-linux;
        # `make gate` declares prlimit in [tools] and refuses to start without it.
        warnings.warn(
            "prlimit is unavailable: running analyzers WITHOUT a CPU or address-space "
            "budget. Timeouts still apply as wall clock only. Install util-linux, or "
            "use `make gate`, which refuses to run when a declared tool is missing.",
            RuntimeWarning,
            stacklevel=2,
        )
        return command
    address_space = memory_mb * 1024 * 1024
    cpu_seconds = max(timeout_seconds, 1)
    _soft_as, hard_as = resource.getrlimit(resource.RLIMIT_AS)
    if hard_as != resource.RLIM_INFINITY:
        address_space = min(address_space, hard_as)
    _soft_cpu, hard_cpu = resource.getrlimit(resource.RLIMIT_CPU)
    if hard_cpu != resource.RLIM_INFINITY:
        cpu_seconds = min(cpu_seconds, hard_cpu)
    return [prlimit, f"--as={address_space}", f"--cpu={cpu_seconds}", "--", *command]


def _run_process(
    command: list[str],
    *,
    ctx: ReviewContext,
    timeout_seconds: int,
    memory_mb: int,
    limit_resources: bool = True,
    include_git_metadata: bool = True,
    extra_env: Mapping[str, str] | None = None,
    wall_timeout_seconds: int | None = None,
) -> subprocess.CompletedProcess[str]:
    # `timeout_seconds` is the CPU budget (prlimit); the wall budget defaults to it but
    # is separable, because a process starved by concurrent siblings is slow in wall
    # time without burning the CPU that would prove it is actually stuck.
    wall = wall_timeout_seconds or timeout_seconds
    actual = (
        _limited_command(command, memory_mb=memory_mb, timeout_seconds=timeout_seconds)
        if limit_resources
        else command
    )
    return subprocess.run(
        actual,
        cwd=ctx.snapshot,
        env=_environment(
            ctx, include_git_metadata=include_git_metadata, extra=extra_env
        ),
        capture_output=True,
        text=True,
        errors="replace",
        timeout=wall,
        check=False,
    )


def _tail(value: str, maximum: int) -> str:
    return value if len(value) <= maximum else value[-maximum:]


def tool_version(
    ctx: ReviewContext, check: CheckPolicy
) -> tuple[str | None, str | None]:
    command = _expand_command(check.version_command, ctx, (), check_id=check.check_id)
    try:
        completed = _run_process(
            command,
            ctx=ctx,
            timeout_seconds=min(check.timeout_seconds, 30),
            memory_mb=check.memory_mb,
            limit_resources=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return None, f"{type(exc).__name__}: {exc}"
    if completed.returncode:
        detail = completed.stderr.strip() or completed.stdout.strip()
        return None, f"exit {completed.returncode}: {detail}"
    version = (completed.stdout.strip() or completed.stderr.strip()).splitlines()
    return (version[0][:500] if version else "unknown"), None


def run_command_check(
    ctx: ReviewContext,
    check: CheckPolicy,
    *,
    version: str | None = None,
    version_error: str | None = None,
) -> CheckResult:
    started = time.perf_counter()
    files = files_for_policy(ctx, check)
    if not check.always and not files:
        return CheckResult(
            check_id=check.check_id,
            status=CheckStatus.SKIPPED,
            duration_ms=0,
            skipped_reason="no matching candidate changes",
        )
    if version_error or not version:
        finding = Finding(
            check_id=check.check_id,
            rule_id="required-analyzer-unavailable",
            severity=Severity.CRITICAL,
            message=(
                "required analyzer is unavailable: "
                f"{version_error or 'no version evidence'}"
            ),
            help="Install the pinned analyzer; required checks never skip on missing tools.",
        )
        return _result(check.check_id, started, [finding], files=files)
    command = _expand_command(check.command, ctx, files, check_id=check.check_id)
    try:
        completed = _run_process(
            command,
            ctx=ctx,
            timeout_seconds=check.timeout_seconds,
            memory_mb=check.memory_mb,
        )
    except subprocess.TimeoutExpired as exc:
        finding = Finding(
            check_id=check.check_id,
            rule_id="analyzer-timeout",
            severity=Severity.CRITICAL,
            message=f"required analyzer exceeded {check.timeout_seconds}s: {exc}",
        )
        return _result(check.check_id, started, [finding], files=files)
    except OSError as exc:
        finding = Finding(
            check_id=check.check_id,
            rule_id="analyzer-crash",
            severity=Severity.CRITICAL,
            message=f"required analyzer could not execute: {exc}",
        )
        return _result(check.check_id, started, [finding], files=files)
    stdout_tail = _tail(completed.stdout, check.max_output_chars)
    stderr_tail = _tail(completed.stderr, check.max_output_chars)
    findings: list[Finding] = []
    if completed.returncode:
        crash = completed.returncode in {126, 127} or completed.returncode < 0
        analyzer_output = "\n".join(
            part for part in (stdout_tail.strip(), stderr_tail.strip()) if part
        )
        findings.append(
            Finding(
                check_id=check.check_id,
                rule_id="analyzer-crash" if crash else "analyzer-finding",
                severity=Severity.CRITICAL if crash else check.severity,
                message=analyzer_output or f"analyzer exited {completed.returncode}",
                evidence={"exit_code": completed.returncode},
            )
        )
    result = _result(check.check_id, started, findings, files=files)
    result.command = command
    result.tool_version = version
    result.exit_code = completed.returncode
    result.stdout_tail = stdout_tail
    result.stderr_tail = stderr_tail
    result.metrics = {
        "timeout_seconds": check.timeout_seconds,
        "memory_limit_mb": check.memory_mb,
    }
    return result


def command_cache_material(
    ctx: ReviewContext, check: CheckPolicy, version: str | None, files: Sequence[str]
) -> dict[str, object]:
    selected = set(files)
    changes = {
        change.path: change.new_oid
        for change in ctx.candidate.changes
        if change.path in selected
    }
    return {
        "check": check.check_id,
        "profile": ctx.profile,
        "policy": ctx.policy.digest,
        "tree": ctx.candidate.tree_oid,
        "base": ctx.candidate.base_tree_oid,
        "tool_version": version,
        "files": changes,
        "command": list(check.command),
    }
