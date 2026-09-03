"""Standalone-install rehearsal for the extractable tooling.

Assembles the future ``conductor-tooling`` repo in a scratch dir from the *committed*
tree (``git archive``), installs it into a fresh ``uv`` venv (which builds the native
crate), proves the host project is not importable there, and runs the package's own
tests. The failures are the deliverable: every one is a coupling the boundary
contract cannot see -- a test that assumes ``research/`` next to the package, a
receipt path, a host tool. The report groups them by first failure line.

The second half is the foreign install: the wheel is built from that tree and
installed into another fresh venv, ``conductor init`` scaffolds a throwaway git
repository there (running the hook doctor), a force-push is denied through the
scaffolded dispatcher, and the dispatcher and ``conductor`` are proven to import from
that venv alone. Any of those failing is a blocker, raised, never summarized.

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
    ("tooling/native/slop-core", "native/slop-core"),
    ("tooling/hooks", "src/tooling/hooks"),
    (".claude/hooks", "hooks"),
    ("tooling/pyproject.toml", "pyproject.toml"),
    ("tooling/README.md", "README.md"),
)
EXCLUDED_HOOK_SUBDIR = "project"
PLUGIN_ENV = "CONDUCTOR_PROJECT_TEST_PLUGIN"
MAX_FAILING_NODEIDS = 50
MAX_GROUPS = 10
NATIVE_CRATES = ("native/conductor-native", "native/slop-core")
FOREIGN_MODULES = ("tooling.hooks.dispatch", "conductor.project_init")
FORCE_PUSH = {
    "session_id": "standalone-smoke",
    "hook_event_name": "PreToolUse",
    "tool_name": "Bash",
    "tool_input": {"command": "git push --force origin master"},
}


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


def build_wheel(dest: Path, uv: str) -> Path:
    """``uv build --wheel`` of the assembled tree; the one wheel it produces."""
    built = _run([uv, "build", "--wheel", "--out-dir", "dist"], cwd=dest, timeout=600)
    if built.returncode:
        raise RuntimeError(f"uv build failed: {built.stderr.strip()[-4000:]}")
    wheels = sorted((dest / "dist").glob("*.whl"))
    if len(wheels) != 1:
        raise RuntimeError(f"expected one wheel in {dest / 'dist'}, found {wheels}")
    return wheels[0]


def install_wheel(dest: Path, uv: str, wheel: Path) -> tuple[str, float]:
    """A second fresh venv holding only the crates and the built wheel; (python, s)."""
    started = time.perf_counter()
    venv_dir = dest / ".venv-foreign"
    venv = _run([uv, "venv", "--python", sys.executable, str(venv_dir)], cwd=dest)
    if venv.returncode:
        raise RuntimeError(f"uv venv (foreign) failed: {venv.stderr.strip()[-2000:]}")
    python = str(venv_dir / "bin" / "python")
    pip = _run(
        [uv, "pip", "install", "--python", python, *NATIVE_CRATES, str(wheel)],
        cwd=dest,
        timeout=1500,
    )
    if pip.returncode:
        raise RuntimeError(
            f"uv pip install (wheel) failed: {pip.stderr.strip()[-4000:]}"
        )
    return python, time.perf_counter() - started


def init_foreign_project(dest: Path, python: str) -> tuple[Path, str]:
    """``conductor init`` on a throwaway git repository; the doctor runs inside it."""
    project = dest / "foreign-project"
    project.mkdir()
    git = _run(["git", "init", "-q", str(project)], cwd=dest, timeout=60)
    if git.returncode:
        raise RuntimeError(f"git init failed: {git.stderr.strip()}")
    env = clean_env()
    env.pop("CLAUDE_PROJECT_DIR", None)
    init = _run(
        [python, "-m", "conductor", "init", str(project)],
        cwd=dest,
        env=env,
        timeout=600,
    )
    if init.returncode:
        raise RuntimeError(
            f"conductor init failed (exit {init.returncode}):\n"
            f"{init.stdout[-3000:]}\n{init.stderr[-3000:]}"
        )
    return project, init.stdout


def deny_force_push(project: Path) -> None:
    """The scaffolded launcher must deny a force-push with only the venv's shebang."""
    env = clean_env()
    env["CLAUDE_PROJECT_DIR"] = str(project)
    env["CRG_SKIP_EMBED"] = "1"
    proc = _run(
        [str(project / ".claude" / "hooks" / "dispatch.py"), "PreToolUse"],
        cwd=project,
        env=env,
        timeout=60,
        input=json.dumps({**FORCE_PUSH, "cwd": str(project)}),
    )
    if proc.returncode:
        raise RuntimeError(
            f"dispatcher exited {proc.returncode}: {proc.stderr.strip()[-2000:]}"
        )
    try:
        decision = json.loads(proc.stdout)["hookSpecificOutput"]["permissionDecision"]
    except (ValueError, KeyError, TypeError) as exc:
        raise RuntimeError(
            f"dispatcher output is not a decision: {proc.stdout[:500]!r}"
        ) from exc
    if decision != "deny":
        raise RuntimeError(f"force-push was {decision!r}, not denied")


def assert_imports_from_venv(python: str, project: Path) -> dict[str, str]:
    """Every foreign module resolves inside the venv, never the monorepo or the tree."""
    venv_root = str(Path(python).absolute().parents[1])  # never resolve a venv python
    roots: dict[str, str] = {}
    for module in FOREIGN_MODULES:
        probe = _run(
            [python, "-c", f"import {module} as m; print(m.__file__)"],
            cwd=project,
            env=clean_env(),
            timeout=60,
        )
        if probe.returncode:
            raise RuntimeError(
                f"{module} does not import from the foreign venv: "
                f"{probe.stderr.strip()[-500:]}"
            )
        location = probe.stdout.strip()
        if not location.startswith(venv_root):
            raise RuntimeError(f"{module} imports from {location}, outside {venv_root}")
        roots[module] = location
    return roots


def rehearse_foreign(dest: Path, uv: str) -> dict[str, object]:
    wheel = build_wheel(dest, uv)
    python, install_seconds = install_wheel(dest, uv, wheel)
    project, doctor_output = init_foreign_project(dest, python)
    assert_project_absent(python, project)
    deny_force_push(project)
    roots = assert_imports_from_venv(python, project)
    return {
        "foreign_wheel": wheel.name,
        "foreign_install_seconds": round(install_seconds, 1),
        "foreign_doctor": doctor_output.strip().splitlines()[-1],
        "foreign_force_push": "deny",
        "foreign_import_roots": roots,
    }


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
                "src/tooling",
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
    foreign = rehearse_foreign(dest, uv)
    return {
        "tree_sha": tree,
        "ref": ref,
        "workdir": str(dest),
        "install_seconds": round(install_seconds, 1),
        "pytest_exit": pytest_exit,
        "exit_code": verdict(pytest_exit, summary),
        **asdict(summary),
        **foreign,
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
                    "foreign_install_seconds",
                    "foreign_doctor",
                    "foreign_force_push",
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
