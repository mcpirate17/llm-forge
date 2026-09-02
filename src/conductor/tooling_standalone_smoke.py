"""Standalone-install rehearsal for the extractable tooling.

Assembles the future ``conductor-tooling`` repo in a scratch dir from the *committed*
tree (``git archive``), installs it into a fresh ``uv`` venv (which builds the native
crate), proves the host project is not importable there, and runs the package's own
tests. The failures are the deliverable: every one is a coupling the boundary
contract cannot see -- a test that assumes ``research/`` next to the package, a
receipt path, a host tool. The report groups them by first failure line.

This is a rehearsal, not evidence: it is a Makefile target
(``make tooling-standalone-smoke``), never part of ``make gate``.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from collections import Counter
from collections.abc import Sequence
from dataclasses import asdict, dataclass, field
from pathlib import Path

from defusedxml import ElementTree

from conductor.tooling_boundary import PROJECT_PACKAGES

# (path in the repo tree, path in the assembled standalone tree)
LAYOUT: tuple[tuple[str, str], ...] = (
    ("conductor", "src/conductor"),
    ("tooling/native/conductor-native", "native/conductor-native"),
    (".claude/hooks", "hooks"),
    ("tooling/pyproject.toml", "pyproject.toml"),
    ("tooling/README.md", "README.md"),
)
EXCLUDED_HOOK_SUBDIR = "project"
PLUGIN_ENV = "CONDUCTOR_PROJECT_TEST_PLUGIN"
MAX_FAILING_NODEIDS = 50
MAX_GROUPS = 10


@dataclass(slots=True)
class Summary:
    passed: int = 0
    failed: int = 0
    errors: int = 0
    skipped: int = 0
    failing_nodeids: list[str] = field(default_factory=list)
    failure_groups: list[dict[str, object]] = field(default_factory=list)


def _run(
    cmd: Sequence[str], *, cwd: Path, timeout: int = 600, **kw: object
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        list(cmd),
        cwd=str(cwd),
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
        **kw,
    )


def archive_tree(repo: Path, ref: str, staging: Path) -> str:
    """Extract the committed ``LAYOUT`` sources of ``ref`` into ``staging``."""
    staging.mkdir(parents=True, exist_ok=True)
    tree = _run(["git", "rev-parse", f"{ref}^{{tree}}"], cwd=repo)
    if tree.returncode:
        raise RuntimeError(f"git rev-parse {ref} failed: {tree.stderr.strip()}")
    archive = subprocess.run(
        ["git", "archive", "--format=tar", ref, "--", *(src for src, _ in LAYOUT)],
        cwd=str(repo),
        capture_output=True,
        check=False,
    )
    if archive.returncode:
        raise RuntimeError(f"git archive failed: {archive.stderr.decode().strip()}")
    extract = subprocess.run(
        ["tar", "-x", "-C", str(staging)],
        input=archive.stdout,
        capture_output=True,
        check=False,
    )
    if extract.returncode:
        raise RuntimeError(f"tar extract failed: {extract.stderr.decode().strip()}")
    return tree.stdout.strip()


def lay_out(staging: Path, dest: Path) -> None:
    """Move the archived parts into the standalone layout; drop the project hooks."""
    for src, dst in LAYOUT:
        source = staging / src
        if not source.exists():
            raise FileNotFoundError(
                f"archive lacks {src}; the committed tree is incomplete"
            )
        target = dest / dst
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(source), str(target))
    project_hooks = dest / "hooks" / EXCLUDED_HOOK_SUBDIR
    if project_hooks.exists():
        shutil.rmtree(project_hooks)
    if project_hooks.exists():
        raise RuntimeError(f"{project_hooks} survived the layout")


def clean_env() -> dict[str, str]:
    """The rehearsal environment: the host's ``PYTHONPATH`` and project test plugin
    are dropped (the plugin variable is pinned empty so the package default cannot be
    reselected) and bytecode is not written into the layout."""
    env = {k: v for k, v in os.environ.items() if k not in {"PYTHONPATH", PLUGIN_ENV}}
    env[PLUGIN_ENV] = ""
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    return env


def assert_project_absent(python: str, cwd: Path) -> None:
    """Fail unless every host package is unimportable from the rehearsal venv.

    The probe runs in the same ``clean_env`` as ``run_pytest``: the gate runs tests
    with the snapshot on ``PYTHONPATH`` and the project plugin selected, and a probe
    that inherits either refuses falsely.
    """
    env = clean_env()
    for package in PROJECT_PACKAGES:
        probe = _run([python, "-c", f"import {package}"], cwd=cwd, timeout=60, env=env)
        if probe.returncode == 0:
            raise RuntimeError(
                f"{package} is importable from {python} (cwd {cwd}); the rehearsal "
                "would prove nothing"
            )


def install(dest: Path, uv: str) -> tuple[str, float]:
    """Create the venv and install the package with its test extra; returns (python, s)."""
    started = time.perf_counter()
    venv = _run([uv, "venv", "--python", sys.executable, str(dest / ".venv")], cwd=dest)
    if venv.returncode:
        raise RuntimeError(f"uv venv failed: {venv.stderr.strip()[-2000:]}")
    python = str(dest / ".venv" / "bin" / "python")
    pip = _run(
        [uv, "pip", "install", "--python", python, "-e", ".[test]"],
        cwd=dest,
        timeout=1500,
    )
    if pip.returncode:
        raise RuntimeError(f"uv pip install failed: {pip.stderr.strip()[-4000:]}")
    return python, time.perf_counter() - started


def run_pytest(dest: Path, python: str, timeout: int) -> tuple[int, Path]:
    junit = dest / "junit.xml"
    env = clean_env()
    try:
        completed = _run(
            [
                python,
                "-m",
                "pytest",
                "src/conductor",
                "-q",
                "-p",
                "no:cacheprovider",
                "--maxfail=200",
                "--continue-on-collection-errors",
                f"--junitxml={junit}",
                "-o",
                "junit_family=xunit2",
            ],
            cwd=dest,
            timeout=timeout,
            env=env,
        )
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(f"pytest exceeded {timeout}s") from exc
    (dest / "pytest.log").write_text(
        completed.stdout + completed.stderr, encoding="utf-8"
    )
    return completed.returncode, junit


COLLECTION_FAILURE = "collection failure"


def _first_line(element: ElementTree.Element, kind: str) -> str:
    """The message's first line; for a collection failure, the exception line."""
    message = (element.get("message") or "").strip().splitlines()
    if message and message[0] != COLLECTION_FAILURE:
        return message[0][:200]
    raised = [
        line[4:].strip()
        for line in (element.text or "").splitlines()
        if line.startswith("E   ")
    ]
    if raised:
        return f"{COLLECTION_FAILURE}: {raised[0]}"[:200]
    return message[0][:200] if message else f"<{kind} without message>"


def summarize_junit(xml_text: str) -> Summary:
    """Counts, the first failing nodeids and failure messages grouped by first line."""
    root = ElementTree.fromstring(xml_text)
    summary = Summary()
    groups: Counter[str] = Counter()
    for case in root.iter("testcase"):
        nodeid = f"{case.get('classname', '')}::{case.get('name', '')}"
        kind = next(
            (c.tag for c in case if c.tag in {"failure", "error", "skipped"}), None
        )
        if kind is None:
            summary.passed += 1
            continue
        if kind == "skipped":
            summary.skipped += 1
            continue
        if kind == "failure":
            summary.failed += 1
        else:
            summary.errors += 1
        if len(summary.failing_nodeids) < MAX_FAILING_NODEIDS:
            summary.failing_nodeids.append(nodeid)
        groups[_first_line(case.find(kind), kind)] += 1
    summary.failure_groups = [
        {"message": message, "count": count}
        for message, count in groups.most_common(MAX_GROUPS)
    ]
    return summary


def verdict(pytest_exit: int, summary: Summary) -> int:
    """Non-zero on any failure, error, or non-zero pytest exit; never optimistic."""
    if pytest_exit != 0 or summary.failed or summary.errors:
        return 1
    return 0


def write_report(path: Path, payload: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def rehearse(
    repo: Path, ref: str, workdir: Path, uv: str, timeout: int
) -> dict[str, object]:
    staging = workdir / "staging"
    dest = workdir / "conductor-tooling"
    tree = archive_tree(repo, ref, staging)
    lay_out(staging, dest)
    python, install_seconds = install(dest, uv)
    assert_project_absent(python, dest)
    pytest_exit, junit = run_pytest(dest, python, timeout)
    summary = summarize_junit(junit.read_text(encoding="utf-8"))
    return {
        "tree_sha": tree,
        "ref": ref,
        "workdir": str(dest),
        "install_seconds": round(install_seconds, 1),
        "pytest_exit": pytest_exit,
        "exit_code": verdict(pytest_exit, summary),
        **asdict(summary),
    }


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m conductor.tooling_standalone_smoke"
    )
    parser.add_argument("--repo", default=".")
    parser.add_argument("--ref", default="HEAD")
    parser.add_argument("--report", default="tooling_standalone_smoke.json")
    parser.add_argument(
        "--workdir", default=None, help="scratch dir (default: a temp dir)"
    )
    parser.add_argument("--keep", action="store_true", help="keep the scratch dir")
    parser.add_argument("--timeout", type=int, default=1500, help="pytest wall cap (s)")
    args = parser.parse_args(argv)
    uv = shutil.which("uv")
    if uv is None:
        raise RuntimeError("uv is not on PATH")
    repo = Path(args.repo).resolve()
    workdir = (
        Path(args.workdir)
        if args.workdir
        else Path(tempfile.mkdtemp(prefix="conductor-standalone-"))
    )
    workdir.mkdir(parents=True, exist_ok=True)
    started = time.perf_counter()
    try:
        payload = rehearse(repo, args.ref, workdir, uv, args.timeout)
    except Exception as exc:  # the report must record the refusal, then re-raise
        write_report(
            Path(args.report),
            {"ref": args.ref, "error": f"{type(exc).__name__}: {exc}", "exit_code": 2},
        )
        raise
    payload["total_seconds"] = round(time.perf_counter() - started, 1)
    write_report(Path(args.report), payload)
    print(
        json.dumps(
            {
                k: payload[k]
                for k in (
                    "tree_sha",
                    "install_seconds",
                    "passed",
                    "failed",
                    "errors",
                    "skipped",
                    "exit_code",
                )
            }
        )
    )
    for group in payload["failure_groups"]:
        print(f"  {group['count']:4d}  {group['message']}")
    if not args.keep:
        shutil.rmtree(workdir, ignore_errors=True)
    return int(payload["exit_code"])


if __name__ == "__main__":
    raise SystemExit(main())
