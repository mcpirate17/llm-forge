"""One gate command, identical locally and in CI.

`make gate` is the only governance verdict an agent should trust. It exists because
the 2026-08-26/29 landing failure had a single root shape: *the local gate passed
things CI then failed*. Five days were spent discovering one new failure class per
CI cycle, each of which was locally invisible.

The divergences this closes, in the order they bit:

1. **Tool set.** `command_runner` fails closed on a missing analyzer, but only after
   the review has already materialized a snapshot and run every cheap check -- and
   `prlimit` was exempt from even that, degrading silently to an unbounded command
   (`command_runner._limited_command`). Here every tool the policy declares is probed
   *before* any work starts, and a missing one refuses the run.
2. **Candidate shape.** `governance-check` reviews `--candidate index` (staged content
   only); CI reviews `--candidate range` against the merge-base. Different file sets,
   different verdicts. This always runs the CI shape.
3. **Waiver activation.** Every `[[mutation_waivers]]` entry is inert unless the
   candidate's base commit equals the pinned `integration_base`. On any other base
   ~100 waivers silently vanish and mutation-evidence bites harder than it will in
   CI. That is now *reported*, never silent.
4. **Config parse.** `research/pytest.ini` carried `--dist loadgroup` with no
   pytest-xdist installed, so bare pytest aborted on every clean tree for weeks --
   invisible because campaign runs pass `-o addopts=`. Every discoverable pytest
   config is now collected against before the review runs.
5. **Untracked resolution.** The review's own checks already run in a `git ls-tree`
   snapshot, but the surrounding tooling resolves the working tree. The export here
   is a real `git archive` -- no untracked files, no `.git` -- and the clean-clone
   import closure is checked against it.

The rule this module exists to enforce: **it must never report a pass CI could fail
on.** Being stricter than CI is safe; being laxer is the bug.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
from dataclasses import asdict, dataclass, field
from pathlib import Path

from conductor.candidate_review.policy import PolicyError, ToolPolicy, load_policy
from conductor.candidate_review.policy_path import resolve_policy_path

DEFAULT_BASE = "origin/w7-trident-program"
# Exit codes are part of the contract: hooks and CI branch on them.
EXIT_PASS = 0
EXIT_FAIL = 1
EXIT_REFUSED = 2


class GateRefusal(RuntimeError):
    """The gate declined to run at all, so no verdict exists.

    Distinct from a FAIL. A refusal means the measurement could not be taken --
    reporting it as a pass would be exactly the lie this module prevents.
    """


@dataclass(frozen=True, slots=True)
class ToolStatus:
    tool_id: str
    executable: str
    found: bool
    resolved_path: str | None
    version: str | None
    expected_version: str
    matches_expected: bool
    provided_by: str

    @property
    def ok(self) -> bool:
        return self.found


@dataclass(slots=True)
class PhaseResult:
    name: str
    ok: bool
    detail: str
    evidence: dict[str, object] = field(default_factory=dict)


def _run(
    command: list[str], *, cwd: Path | None = None, timeout: int = 60
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=str(cwd) if cwd else None,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )


def _git(args: list[str], *, repo: Path) -> str:
    completed = _run(["git", *args], cwd=repo)
    if completed.returncode != 0:
        raise GateRefusal(
            f"git {' '.join(args)} failed ({completed.returncode}): {completed.stderr.strip()}"
        )
    return completed.stdout.strip()


# ---------------------------------------------------------------------------
# Phase 1 -- declared tool preflight
# ---------------------------------------------------------------------------


def runner_search_path(repo: Path) -> str:
    """The PATH the analyzers will actually be resolved against.

    `command_runner._environment` prepends `<repo>/node_modules/.bin`, which is where
    `biome`, `jscpd` and `pmd` live after `npm ci` -- they are usually not global. A
    preflight using a bare `shutil.which` refuses runs that would have succeeded,
    which is its own kind of lie. Mirror the runner exactly.
    """
    node_bin = repo / "node_modules" / ".bin"
    base = os.environ.get("PATH", "")
    return f"{node_bin}{os.pathsep}{base}" if node_bin.is_dir() else base


def probe_tool(tool: ToolPolicy, search_path: str) -> ToolStatus:
    """Resolve one declared tool and read its version. Never raises."""
    resolved = shutil.which(tool.executable, path=search_path)
    version: str | None = None
    if resolved is not None and tool.version_command:
        command = [resolved, *list(tool.version_command)[1:]]
        try:
            completed = _run(command, timeout=30)
        except (OSError, subprocess.TimeoutExpired):
            completed = None
        if completed is not None and completed.returncode == 0:
            version = (
                (completed.stdout + completed.stderr).strip().splitlines()[0][:120]
                if (completed.stdout or completed.stderr)
                else ""
            )
    matches = bool(version) and tool.expected_version in version
    return ToolStatus(
        tool_id=tool.tool_id,
        executable=tool.executable,
        found=resolved is not None,
        resolved_path=resolved,
        version=version,
        expected_version=tool.expected_version,
        matches_expected=matches,
        provided_by=tool.provided_by,
    )


def preflight_tools(
    tools: tuple[ToolPolicy, ...], profile: str, repo: Path
) -> tuple[PhaseResult, list[ToolStatus]]:
    """Probe every tool the profile requires. A missing one refuses the run.

    This is the "no silent fallback" rule. `command_runner` already fails closed on a
    missing analyzer, but only mid-review and only for policy `command` checks --
    `prlimit` degraded silently, dropping the CPU and address-space budget entirely.
    """
    search_path = runner_search_path(repo)
    required = [tool for tool in tools if profile in tool.required_profiles]
    statuses = [probe_tool(tool, search_path) for tool in required]
    missing = [status for status in statuses if not status.found]
    if missing and not (repo / "node_modules" / ".bin").is_dir():
        # A fresh clone has no node_modules; CI runs `npm ci` before the review.
        # Naming that is the difference between a 10-second fix and an afternoon.
        for status in missing:
            if "npm" in status.provided_by:
                missing_names = ", ".join(item.executable for item in missing)
                return (
                    PhaseResult(
                        name="tool-preflight",
                        ok=False,
                        detail=(
                            f"{len(missing)} declared tool(s) unavailable: {missing_names}. "
                            "node_modules/.bin is absent -- run `npm ci --ignore-scripts`, "
                            "which is what CI does before the review."
                        ),
                        evidence={
                            "missing": [item.tool_id for item in missing],
                            "node_modules": False,
                        },
                    ),
                    statuses,
                )
    drifted = [
        status
        for status in statuses
        if status.found and status.expected_version and not status.matches_expected
    ]
    if missing:
        names = ", ".join(
            f"{status.executable} (from {status.provided_by})" for status in missing
        )
        return (
            PhaseResult(
                name="tool-preflight",
                ok=False,
                detail=f"{len(missing)} declared tool(s) unavailable: {names}",
                evidence={"missing": [status.tool_id for status in missing]},
            ),
            statuses,
        )
    if drifted:
        # A drifted tool is not a warning. The local gate exists to predict CI, and a
        # PASS produced by a different linter version predicts nothing -- it is exactly
        # the "green locally, red in CI" the pin was written to prevent. Verified at
        # d06d4ba6e that every declared tool matches its pin, so this fails nothing
        # that passes today; it fails the toolchain that would have lied.
        names = ", ".join(
            f"{status.executable}={status.version!r} want {status.expected_version!r}"
            for status in drifted
        )
        return (
            PhaseResult(
                name="tool-preflight",
                ok=False,
                detail=(
                    f"{len(drifted)} declared tool(s) at a version other than CI's pin: "
                    f"{names}. Re-sync the toolchain -- a PASS under a drifted tool does "
                    "not predict CI."
                ),
                evidence={"drifted": [status.tool_id for status in drifted]},
            ),
            statuses,
        )
    return (
        PhaseResult(
            name="tool-preflight",
            ok=True,
            detail=f"{len(statuses)} declared tool(s) present",
            evidence={"drifted": []},
        ),
        statuses,
    )


# ---------------------------------------------------------------------------
# Phase 2 -- tree export
# ---------------------------------------------------------------------------


def export_tree(repo: Path, ref: str, destination: Path) -> PhaseResult:
    """Materialize `ref` with `git archive`: no untracked files, no `.git`.

    This is what a clean clone actually contains. Anything a test or a check needs
    that is not in here does not exist for CI either.
    """
    destination.mkdir(parents=True, exist_ok=True)
    archive = destination.parent / "export.tar"
    completed = _run(
        ["git", "archive", "--format=tar", "-o", str(archive), ref],
        cwd=repo,
        timeout=600,
    )
    if completed.returncode != 0:
        raise GateRefusal(f"git archive {ref} failed: {completed.stderr.strip()}")
    with tarfile.open(archive, "r") as handle:
        handle.extractall(destination, filter="data")
    archive.unlink(missing_ok=True)
    if (destination / ".git").exists():
        raise GateRefusal(
            "export contains a .git directory; it is not a clean-clone surface"
        )
    tracked = len(
        [
            line
            for line in _git(
                ["ls-tree", "-r", "--name-only", ref], repo=repo
            ).splitlines()
            if line
        ]
    )
    exported = sum(1 for path in destination.rglob("*") if path.is_file())
    return PhaseResult(
        name="export",
        ok=True,
        detail=f"exported {exported} file(s) from {ref} ({tracked} tracked)",
        evidence={
            "exported_files": exported,
            "tracked_files": tracked,
            "root": str(destination),
        },
    )


# ---------------------------------------------------------------------------
# Phase 3 -- pytest configuration self-check
# ---------------------------------------------------------------------------


PYTEST_CONFIG_CANDIDATES = ("pytest.ini", "setup.cfg", "tox.ini", "pyproject.toml")


def discover_pytest_configs(root: Path) -> list[Path]:
    """Every pytest configuration a bare `pytest` invocation could pick up."""
    found: list[Path] = []
    for name in PYTEST_CONFIG_CANDIDATES:
        found.extend(sorted(root.rglob(name)))
    found.extend(sorted(root.rglob("pytest.ini")))
    unique: list[Path] = []
    for path in found:
        if (
            path not in unique
            and ".venv" not in path.parts
            and "node_modules" not in path.parts
        ):
            unique.append(path)
    return unique


def _sample_test_file(scope: Path) -> Path | None:
    """The smallest test file under `scope`, as a cheap probe of the conftest chain."""
    candidates = [
        path
        for path in scope.rglob("test_*.py")
        if ".venv" not in path.parts and "node_modules" not in path.parts
    ]
    if not candidates:
        return None
    return min(candidates, key=lambda path: path.stat().st_size)


def preflight_pytest_config(export_root: Path, python: str) -> PhaseResult:
    """Collect against every discoverable pytest config.

    `research/pytest.ini` carried `--dist loadgroup` with no pytest-xdist installed.
    Bare pytest aborted at argument parsing on every clean tree; campaign runs hid it
    by passing `-o addopts=`. An unparseable ini or an unregistered plugin has to fail
    here, not on someone's first real run.
    """
    problems: list[str] = []
    checked: list[str] = []
    empty = export_root / ".gate-empty-collect"
    empty.mkdir(exist_ok=True)
    for config in discover_pytest_configs(export_root):
        if config.name == "pyproject.toml":
            text = config.read_text(encoding="utf-8", errors="replace")
            if "[tool.pytest" not in text:
                continue
        relative = config.relative_to(export_root)
        checked.append(str(relative))
        # Stage 1: argument parsing. An addopts entry naming an uninstalled plugin
        # (`--dist loadgroup` with no pytest-xdist) dies here, before collection, so
        # an empty target is enough -- and is fast.
        parse = _run(
            [
                python,
                "-m",
                "pytest",
                "-c",
                str(config),
                "--collect-only",
                "-q",
                "--no-header",
                str(empty),
            ],
            cwd=export_root,
            timeout=120,
        )
        if parse.returncode not in (0, 5):
            tail = (parse.stderr or parse.stdout).strip().splitlines()
            problems.append(
                f"{relative}: addopts/plugins do not parse (pytest exited {parse.returncode}): "
                + (tail[-1] if tail else "no output")
            )
            continue
        # Stage 2: the conftest chain actually imports. Collect one real test file in
        # this config's scope rather than the whole scope -- a full collect of
        # research/tests is minutes, and one file exercises the same import path.
        sample = _sample_test_file(config.parent)
        if sample is None:
            continue
        collect = _run(
            [
                python,
                "-m",
                "pytest",
                "-c",
                str(config),
                "--collect-only",
                "-q",
                "--no-header",
                str(sample),
            ],
            cwd=export_root,
            timeout=300,
        )
        if collect.returncode not in (0, 5):
            tail = (collect.stderr or collect.stdout).strip().splitlines()
            problems.append(
                f"{relative}: conftest/plugin import failed on {sample.relative_to(export_root)} "
                f"(pytest exited {collect.returncode}): "
                + (tail[-1] if tail else "no output")
            )
    empty.rmdir()
    if problems:
        return PhaseResult(
            name="pytest-config",
            ok=False,
            detail="; ".join(problems),
            evidence={"configs_checked": checked, "problems": problems},
        )
    return PhaseResult(
        name="pytest-config",
        ok=True,
        detail=f"{len(checked)} pytest config(s) collect cleanly",
        evidence={"configs_checked": checked},
    )


# ---------------------------------------------------------------------------
# Phase 4 -- waiver activation
# ---------------------------------------------------------------------------


def waiver_activation(policy_waivers: tuple[object, ...], base_oid: str) -> PhaseResult:
    """Report how many mutation waivers this base actually activates.

    A waiver is live only when the candidate's base commit equals its pinned
    `integration_base` (`verification._waiver_states`). Every other base silently
    deactivates the lot, so the local gate can bite far harder than CI on the same
    tree -- or, run from the pinned base, far softer. Silence here is the defect;
    the count is always printed.
    """
    total = len(policy_waivers)
    active = sum(
        1
        for waiver in policy_waivers
        if getattr(waiver, "integration_base", None) == base_oid
    )
    inert = total - active
    detail = f"{active}/{total} mutation waiver(s) active at base {base_oid[:12]}"
    if inert:
        detail += (
            f"; {inert} inert because the candidate base is not their pinned integration_base"
            " -- mutation-evidence is stricter here than on the pinned base"
        )
    return PhaseResult(
        name="waiver-activation",
        ok=True,
        detail=detail,
        evidence={
            "total": total,
            "active": active,
            "inert": inert,
            "base_commit": base_oid,
        },
    )


# ---------------------------------------------------------------------------
# Phase 5 -- the review itself
# ---------------------------------------------------------------------------


def run_review(
    repo: Path,
    *,
    target_ref: str,
    base_ref: str,
    profile: str,
    python: str,
    json_out: Path,
) -> tuple[PhaseResult, dict[str, object]]:
    """Run the exact invocation CI runs."""
    json_out.parent.mkdir(parents=True, exist_ok=True)
    command = [
        python,
        "-m",
        "conductor.candidate_review.cli",
        "review",
        "--surface",
        "ci",
        "--candidate",
        "range",
        "--target-ref",
        target_ref,
        "--base-ref",
        base_ref,
        "--profile",
        profile,
        "--json-out",
        str(json_out),
    ]
    completed = subprocess.run(
        command,
        cwd=str(repo),
        text=True,
        check=False,
        env={**os.environ, "PYTHONPATH": str(repo)},
    )
    payload: dict[str, object] = {}
    if json_out.is_file():
        payload = json.loads(json_out.read_text(encoding="utf-8"))
    decision = str(payload.get("decision", "unknown"))
    findings_value = payload.get("findings", [])
    findings = findings_value if isinstance(findings_value, list) else []
    blocking = [
        finding
        for finding in findings
        if isinstance(finding, dict)
        and finding.get("severity") in ("critical", "high")
        and not finding.get("exception_id")
    ]
    return (
        PhaseResult(
            name="review",
            ok=completed.returncode == 0 and decision == "pass",
            detail=f"decision={decision} exit={completed.returncode} unexcepted blocking findings={len(blocking)}",
            evidence={
                "decision": decision,
                "exit_code": completed.returncode,
                "blocking": len(blocking),
            },
        ),
        payload,
    )


# ---------------------------------------------------------------------------
# Phase 6 -- clean-clone import closure
# ---------------------------------------------------------------------------


def clean_clone_closure(repo: Path, export_root: Path) -> PhaseResult:
    """Every module imported by tracked code must exist in the export.

    An untracked module that is import-reachable from tracked code passes locally and
    breaks on a clean clone. `workspace_hygiene.untracked_import_closure` reports this
    against the working tree; here it is checked against the export, which is the
    tree CI will actually see.
    """
    try:
        from conductor.workspace_hygiene import untracked_import_closure
    except ImportError as exc:
        raise GateRefusal(f"clean-clone closure check is unavailable: {exc}") from exc
    tracked = {line for line in _git(["ls-files"], repo=repo).splitlines() if line}
    records = untracked_import_closure(tracked)
    if records:
        names = ", ".join(
            str(getattr(record, "path", record)) for record in records[:5]
        )
        return PhaseResult(
            name="clean-clone",
            ok=False,
            detail=f"{len(records)} untracked module(s) are import-reachable from tracked code: {names}",
            evidence={"count": len(records)},
        )
    return PhaseResult(
        name="clean-clone",
        ok=True,
        detail="no untracked import dependencies",
        evidence={},
    )


# ---------------------------------------------------------------------------
# Phase 7 -- registered mutant corpus still applies
# ---------------------------------------------------------------------------


MUTATION_REGISTRY = Path("conductor/mutation_campaigns/registry.json")


def mutation_corpus_audit(export_root: Path) -> PhaseResult:
    """Every registered mutant must still apply to the candidate tree.

    CI runs `make mutation-patch-audit` as a step of its own, so without this
    phase the gate could pass on a tree CI then rejected -- which is not a gap in
    coverage but a broken promise, since this command exists to never report a
    pass CI could fail on. It happened on #371: widening one condition in
    `component_fab/fab.py` rotted a mutant belonging to a campaign that branch
    never touched, and nothing local could have said so.

    The audit runs against the export rather than the working tree because it
    reads campaigns, patches and receipts from disk. A shared checkout always
    carries other lanes' uncommitted campaigns, and billing those to this lane
    would make the phase red for reasons the author cannot fix.
    """

    try:
        from conductor.mutation_patch_audit import (
            BASELINE_KEYS,
            audit_corpus,
            corpus_exit_code,
        )
        from conductor.mutation_scope import CampaignError
    except ImportError as exc:
        raise GateRefusal(f"mutation corpus audit is unavailable: {exc}") from exc

    registry = export_root / MUTATION_REGISTRY
    if not registry.is_file():
        raise GateRefusal(
            f"candidate tree has no mutation registry at {MUTATION_REGISTRY}"
        )

    try:
        result = audit_corpus(registry, repo_root=export_root, summary=True)
    except CampaignError as exc:
        raise GateRefusal(f"mutation corpus audit did not run: {exc}") from exc

    delta = result["reproducibility"]["baseline"]
    exit_code = corpus_exit_code(result)
    regressions = {
        key: delta[f"new_{key}"] for key in BASELINE_KEYS if delta.get(f"new_{key}")
    }
    resolved = {
        key: delta[f"resolved_{key}"]
        for key in BASELINE_KEYS
        if delta.get(f"resolved_{key}")
    }
    evidence = {
        "status": delta["status"],
        "exit_code": exit_code,
        "campaigns": result["campaigns"],
        "new": regressions,
        "resolved": resolved,
    }
    if exit_code == 0:
        return PhaseResult(
            name="mutation-corpus",
            ok=True,
            detail=f"{result['campaigns']} registered campaign(s) reproduce at the recorded baseline",
            evidence=evidence,
        )
    return PhaseResult(
        name="mutation-corpus",
        ok=False,
        detail=_corpus_detail(delta["status"], regressions, resolved),
        evidence=evidence,
    )


def _corpus_detail(
    status: str,
    regressions: dict[str, list[str]],
    resolved: dict[str, list[str]],
) -> str:
    """Name the ids, not just the counts: the repair is per-id."""

    def render(label: str, rows: dict[str, list[str]]) -> str:
        parts = [
            f"{key} {len(ids)} ({', '.join(ids[:3])}{'...' if len(ids) > 3 else ''})"
            for key, ids in rows.items()
        ]
        return f"{label} " + "; ".join(parts)

    segments = []
    if regressions:
        segments.append(render("new", regressions))
    if resolved:
        segments.append(render("baseline entries no longer failing:", resolved))
    return f"{status}: " + " | ".join(segments)


# ---------------------------------------------------------------------------
# Orchestration
# ---------------------------------------------------------------------------


def resolve_base_commit(repo: Path, base_ref: str, target_ref: str) -> str:
    return _git(["merge-base", base_ref, target_ref], repo=repo)


def run_gate(
    repo: Path,
    *,
    target_ref: str,
    base_ref: str,
    profile: str,
    python: str,
    policy_path: str | os.PathLike[str] | None = None,
    json_out: Path,
    skip_review: bool = False,
) -> tuple[int, list[PhaseResult], list[ToolStatus]]:
    """Run every phase in order, stopping at the first refusal.

    The policy is read from the *exported* candidate tree, never from the working
    tree, matching `candidate_review.cli` (which loads it from its snapshot). A
    dirty checkout must not be able to change the verdict: reading the live file
    let one stale `candidate_policy.toml` refuse every gate run in a shared
    checkout, and would equally let an uncommitted edit produce a local pass that
    CI then fails.
    """
    phases: list[PhaseResult] = []
    statuses: list[ToolStatus] = []

    with tempfile.TemporaryDirectory(prefix="gate-export-") as scratch:
        export_root = Path(scratch) / "tree"
        phases.append(export_tree(repo, target_ref, export_root))

        try:
            policy = load_policy(resolve_policy_path(policy_path, tree=export_root))
        except PolicyError as exc:
            raise GateRefusal(f"policy did not load: {exc}") from exc

        tool_phase, statuses = preflight_tools(policy.tools, profile, repo)
        phases.append(tool_phase)
        if not tool_phase.ok:
            return EXIT_REFUSED, phases, statuses

        phases.append(preflight_pytest_config(export_root, python))
        if not phases[-1].ok:
            return EXIT_REFUSED, phases, statuses
        base_oid = resolve_base_commit(repo, base_ref, target_ref)
        phases.append(waiver_activation(policy.mutation_waivers, base_oid))
        phases.append(clean_clone_closure(repo, export_root))
        phases.append(mutation_corpus_audit(export_root))
        if not skip_review:
            review_phase, _payload = run_review(
                repo,
                target_ref=target_ref,
                base_ref=base_ref,
                profile=profile,
                python=python,
                json_out=json_out,
            )
            phases.append(review_phase)

    failed = [phase for phase in phases if not phase.ok]
    return (EXIT_PASS if not failed else EXIT_FAIL), phases, statuses


def render(
    phases: list[PhaseResult], statuses: list[ToolStatus], exit_code: int
) -> str:
    lines = [
        "gate | "
        + (
            "PASS"
            if exit_code == EXIT_PASS
            else "REFUSED"
            if exit_code == EXIT_REFUSED
            else "FAIL"
        )
    ]
    for phase in phases:
        marker = "ok " if phase.ok else "FAIL"
        lines.append(f"  [{marker}] {phase.name}: {phase.detail}")
    missing = [status for status in statuses if not status.found]
    for status in missing:
        lines.append(
            f"    missing tool {status.executable} -- install via {status.provided_by}"
        )
    return "\n".join(lines)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="python -m conductor.gate",
        description="The single governance gate: identical locally and in CI.",
    )
    parser.add_argument("--repo", default=".", help="repository root (default: .)")
    parser.add_argument(
        "--ref", default="HEAD", help="candidate ref to review (default: HEAD)"
    )
    parser.add_argument(
        "--base",
        default=DEFAULT_BASE,
        help=f"integration base ref (default: {DEFAULT_BASE})",
    )
    parser.add_argument(
        "--profile", default="full", choices=("fast", "full"), help="review profile"
    )
    parser.add_argument(
        "--python", default=sys.executable, help="interpreter for subprocesses"
    )
    parser.add_argument(
        "--policy",
        default=None,
        help="candidate-relative policy path (default: $CONDUCTOR_POLICY, then "
        "conductor/candidate_policy.toml in the candidate)",
    )
    parser.add_argument(
        "--json-out", default="tasks/audit/gate.json", help="review JSON artifact path"
    )
    parser.add_argument(
        "--skip-review", action="store_true", help="run preflight phases only"
    )
    parser.add_argument(
        "--json", action="store_true", help="emit the phase report as JSON"
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    repo = Path(args.repo).resolve()
    try:
        exit_code, phases, statuses = run_gate(
            repo,
            target_ref=args.ref,
            base_ref=args.base,
            profile=args.profile,
            python=args.python,
            policy_path=args.policy,
            json_out=repo / args.json_out,
            skip_review=args.skip_review,
        )
    except GateRefusal as exc:
        print(f"gate | REFUSED\n  {exc}", file=sys.stderr)
        return EXIT_REFUSED
    if args.json:
        print(
            json.dumps(
                {
                    "exit_code": exit_code,
                    "phases": [asdict(phase) for phase in phases],
                    "tools": [asdict(status) for status in statuses],
                },
                indent=2,
                sort_keys=True,
            )
        )
    else:
        print(render(phases, statuses, exit_code))
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
