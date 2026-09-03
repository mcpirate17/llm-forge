#!/usr/bin/env python3
from __future__ import annotations

import argparse
import ast
import json
import shutil
import subprocess
import sys
from collections.abc import Iterable, Sequence
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

from conductor._native import guardrail_ast_metrics_native
from conductor.audit_root import (
    AuditRootError,
    print_audit_provenance,
    resolve_audit_root,
)
from conductor.run_duplicate_audit import should_skip_python

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_TARGETS = ("research", "aria_core", "aria_designer", "component_fab")
ALLOWLIST_PATH = Path(__file__).resolve().parent / "guardrail_allowlist.json"


def _load_allowlist() -> dict[str, set[str]]:
    if not ALLOWLIST_PATH.exists():
        return {"god_files": set(), "god_functions": set(), "complexity": set()}
    try:
        raw = json.loads(ALLOWLIST_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {"god_files": set(), "god_functions": set(), "complexity": set()}
    return {
        "god_files": set(raw.get("god_files", [])),
        "god_functions": set(raw.get("god_functions", [])),
        "complexity": set(raw.get("complexity", [])),
    }


_ALLOWLIST = _load_allowlist()


def _has_marker(text: str, marker: str) -> bool:
    return f"# guardrail: {marker}" in text


CODE_EXTS = {
    ".py",
    ".js",
    ".jsx",
    ".ts",
    ".tsx",
    ".c",
    ".cc",
    ".cpp",
    ".cu",
    ".h",
    ".hpp",
}
SKIP_PARTS = {
    "node_modules",
    ".venv",
    "__pycache__",
    ".git",
    "archive",
    ".pytest_cache",
    ".mypy_cache",
    "dist",
    "build",
    "conductor",
}


@dataclass
class Issue:
    kind: str
    severity: str
    path: str
    symbol: str | None
    message: str
    recommendation: str
    metric: dict[str, Any]


def _should_skip(path: Path) -> bool:
    return any(part in SKIP_PARTS for part in path.parts)


def _git_changed_paths(
    targets: tuple[str, ...], *, staged_only: bool, from_ref: str | None
) -> list[str]:
    if staged_only == (from_ref is not None):
        raise ValueError("select exactly one of staged_only or from_ref")
    diff_args = ["git", "diff"]
    if staged_only:
        diff_args.append("--cached")
    else:
        diff_args.append(f"{from_ref}...HEAD")
    diff_args.extend(["--name-only", "--diff-filter=ACMR", "-z", "--", *targets])
    proc = subprocess.run(
        diff_args,
        cwd=ROOT,
        capture_output=True,
        check=True,
    )
    return [path for path in proc.stdout.decode("utf-8", "replace").split("\0") if path]


def _iter_files(
    targets: Iterable[str],
    staged_only: bool = False,
    from_ref: str | None = None,
) -> list[Path]:
    target_list = tuple(targets)
    if staged_only or from_ref is not None:
        candidates = [
            ROOT / path
            for path in _git_changed_paths(
                target_list, staged_only=staged_only, from_ref=from_ref
            )
        ]
    else:
        candidates = []
        for target in target_list:
            base = ROOT / target
            if not base.exists():
                continue
            if base.is_file():
                candidates.append(base)
            else:
                candidates.extend(p for p in base.rglob("*") if p.is_file())
    out: list[Path] = []
    for path in candidates:
        if (not staged_only and from_ref is None and not path.exists()) or _should_skip(
            path
        ):
            continue
        if path.suffix.lower() in CODE_EXTS:
            out.append(path)
    return sorted(set(out))


def _read_candidate_text(path: Path, *, staged_only: bool, from_ref: str | None) -> str:
    rel = path.relative_to(ROOT).as_posix()
    if staged_only or from_ref is not None:
        revision = f":{rel}" if staged_only else f"HEAD:{rel}"
        proc = subprocess.run(
            ["git", "show", revision],
            cwd=ROOT,
            capture_output=True,
            check=True,
        )
        raw = proc.stdout
        try:
            return raw.decode("utf-8")
        except UnicodeDecodeError:
            return raw.decode("latin-1")
    try:
        return path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return path.read_text(encoding="latin-1")


def _resolve_tool_command(tool: str, *args: str) -> list[str]:
    """Prefer the executable installed beside the running Python interpreter."""

    # Keep the environment's bin directory even when ``python`` is a symlink to
    # the system interpreter; resolving it would escape the active environment.
    sibling = Path(sys.executable).with_name(tool)
    resolved = shutil.which(str(sibling)) or shutil.which(tool)
    return [resolved or tool, *args]


def _run_tool(
    command: list[str],
    *,
    timeout_seconds: int = 120,
) -> tuple[int, str]:
    try:
        proc = subprocess.run(
            command,
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
            timeout=timeout_seconds,
        )
    except FileNotFoundError:
        return 127, f"missing tool: {command[0]}"
    except subprocess.TimeoutExpired as exc:
        stdout = exc.stdout or ""
        stderr = exc.stderr or ""
        if isinstance(stdout, bytes):
            stdout = stdout.decode("utf-8", errors="replace")
        if isinstance(stderr, bytes):
            stderr = stderr.decode("utf-8", errors="replace")
        return 124, f"timed out: {' '.join(command)}\n{stdout}\n{stderr}".strip()
    return proc.returncode, (proc.stdout + proc.stderr).strip()


def _record_incomplete_tool(
    issues: list[Issue],
    tool_failures: list[str],
    *,
    tool: str,
    returncode: int,
    output: str,
) -> None:
    detail = output.splitlines()[0] if output else f"unexpected exit {returncode}"
    tool_failures.append(f"{tool}: {detail}")
    issues.append(
        Issue(
            kind="audit_incomplete",
            severity="critical",
            path="tooling",
            symbol=tool,
            message=f"{tool} audit did not complete: {detail}",
            recommendation=(
                "Restore the pinned audit tool or fix its execution before trusting "
                "this report."
            ),
            metric={"exit_code": returncode},
        )
    )


def _structural_issues(
    files: list[Path], *, staged_only: bool, from_ref: str | None
) -> tuple[list[Issue], int]:
    issues: list[Issue] = []
    inputs: list[tuple[Path, str, str, list[str]]] = []
    python_records: list[tuple[str, str]] = []
    for path in files:
        rel = path.relative_to(ROOT).as_posix()
        text = _read_candidate_text(path, staged_only=staged_only, from_ref=from_ref)
        lines = text.splitlines()
        inputs.append((path, rel, text, lines))
        if path.suffix == ".py":
            python_records.append((rel, text))

    function_policy = json.dumps(
        {
            "god_functions": sorted(_ALLOWLIST["god_functions"]),
            "complexity": sorted(_ALLOWLIST["complexity"]),
        },
        separators=(",", ":"),
    )
    native_files = json.loads(
        guardrail_ast_metrics_native(python_records, function_policy)
    )
    native_by_path = {record["path"]: record for record in native_files}
    for path, rel, text, lines in inputs:
        allow_god_file = rel in _ALLOWLIST["god_files"] or _has_marker(
            text, "allow-god-file"
        )
        if len(lines) > 1250 and not allow_god_file:
            issues.append(
                Issue(
                    kind="god_file",
                    severity="critical",
                    path=rel,
                    symbol=None,
                    message=f"File is {len(lines)} lines (>1250).",
                    recommendation=(
                        "Split by responsibility boundaries and isolate "
                        "orchestration from pure logic."
                    ),
                    metric={"lines": len(lines)},
                )
            )
        if path.suffix != ".py":
            continue
        native = native_by_path[rel]
        if native["parse_error"]:
            try:
                ast.parse(text, filename=rel)
            except SyntaxError as exc:
                issues.append(
                    Issue(
                        kind="syntax_error",
                        severity="critical",
                        path=rel,
                        symbol=None,
                        message=f"Syntax error: {exc.msg}",
                        recommendation="Fix parse errors before merge.",
                        metric={"lineno": exc.lineno},
                    )
                )
                continue
            raise RuntimeError(f"native parser rejected CPython-valid source: {rel}")
        issues.extend(Issue(**row) for row in native["issues"])
    return issues, len(python_records)


def _vulture_issues(
    target_list: tuple[str, ...],
    issues: list[Issue],
    tool_failures: list[str],
) -> tuple[int, list[str]]:
    command = _resolve_tool_command(
        "vulture",
        *target_list,
        "research/tools/vulture_whitelist.py",
        "--min-confidence",
        "80",
        "--exclude",
        "*/.venv/*,*/node_modules/*,*/__pycache__/*,*/.run/*,*/tests/*,*/migrations/*",
    )
    returncode, output = _run_tool(command, timeout_seconds=300)
    if returncode not in {0, 3}:
        _record_incomplete_tool(
            issues,
            tool_failures,
            tool="vulture",
            returncode=returncode,
            output=output,
        )
        return returncode, []
    hits = [line for line in output.splitlines() if line.strip()]
    for line in hits[:25]:
        issues.append(
            Issue(
                kind="dead_code",
                severity="high",
                path=line.split(":", 1)[0],
                symbol=None,
                message=line,
                recommendation=(
                    "Delete, wire in, or explicitly whitelist if intentionally dynamic."
                ),
                metric={},
            )
        )
    return returncode, hits


def _pylint_duplicate_issues(
    python_targets: list[str],
    issues: list[Issue],
    tool_failures: list[str],
) -> tuple[int, list[str]]:
    command = _resolve_tool_command(
        "pylint",
        *python_targets,
        "--disable=all",
        "--enable=duplicate-code",
        "--min-similarity-lines=10",
        "--jobs=0",
    )
    returncode, output = _run_tool(command, timeout_seconds=600)
    if returncode not in {0, 8}:
        _record_incomplete_tool(
            issues,
            tool_failures,
            tool="pylint",
            returncode=returncode,
            output=output,
        )
        return returncode, []
    hits = [line for line in output.splitlines() if "duplicate-code" in line]
    for line in hits[:25]:
        issues.append(
            Issue(
                kind="duplicate_code",
                severity="medium",
                path="multiple",
                symbol=None,
                message=line.strip(),
                recommendation=(
                    "Collapse repeated logic into one implementation or delete stale variants."
                ),
                metric={},
            )
        )
    return returncode, hits


def _external_issues(
    target_list: tuple[str, ...], files: list[Path], issues: list[Issue]
) -> dict[str, Any]:
    tool_failures: list[str] = []
    python_targets = [
        path.relative_to(ROOT).as_posix()
        for path in files
        if path.suffix == ".py" and not should_skip_python(path, root=ROOT)
    ]
    vulture_rc, dead_code_hits = _vulture_issues(target_list, issues, tool_failures)
    pylint_rc, duplicate_hits = _pylint_duplicate_issues(
        python_targets, issues, tool_failures
    )
    return {
        "vulture_exit_code": vulture_rc,
        "pylint_exit_code": pylint_rc,
        "dead_code_hits": len(dead_code_hits),
        "duplicate_hits": len(duplicate_hits),
        "audit_complete": not tool_failures,
        "tool_failures": tool_failures,
    }


def collect_issues(
    targets: Iterable[str],
    staged_only: bool = False,
    from_ref: str | None = None,
) -> tuple[list[Issue], dict[str, Any]]:
    if staged_only and from_ref is not None:
        raise ValueError("staged_only and from_ref are mutually exclusive")
    target_list = tuple(targets)
    files = _iter_files(target_list, staged_only=staged_only, from_ref=from_ref)
    issues, py_count = _structural_issues(
        files, staged_only=staged_only, from_ref=from_ref
    )
    metrics: dict[str, Any] = {
        "vulture_exit_code": 0,
        "pylint_exit_code": 0,
        "dead_code_hits": 0,
        "duplicate_hits": 0,
        "audit_complete": True,
        "tool_failures": [],
    }
    if not staged_only and from_ref is None:
        metrics.update(_external_issues(target_list, files, issues))
    return issues, {
        "files_scanned": len(files),
        "python_files_scanned": py_count,
        **metrics,
    }


def _group(issues: list[Issue], *kinds: str) -> list[Issue]:
    order = {"critical": 0, "high": 1, "medium": 2, "low": 3}
    return sorted(
        [issue for issue in issues if issue.kind in kinds],
        key=lambda item: (order.get(item.severity, 9), item.path, item.symbol or ""),
    )


def build_markdown_report(issues: list[Issue], summary: dict[str, Any]) -> str:
    critical = _critical_issues(issues)
    exact_targets = critical[:20]
    fast_wins = _group(issues, "dead_code", "duplicate_code", "complexity")[:10]
    structural = _group(issues, "god_file", "god_function")[:10]
    perf = _group(issues, "native_hotspot_candidate", "complexity")[:10]

    lines = [
        "# Audit Report",
        "",
        f"Scanned `{summary['files_scanned']}` code files across `{summary['python_files_scanned']}` Python files.",
        "",
        "### A. Critical problems",
    ]
    if critical:
        for issue in critical[:15]:
            symbol = f"::{issue.symbol}" if issue.symbol else ""
            lines.append(f"- `{issue.path}{symbol}` [{issue.severity}] {issue.message}")
    else:
        lines.append("- No critical guardrail violations detected.")

    lines.extend(["", "### B. Exact targets"])
    if exact_targets:
        for issue in exact_targets:
            lines.append(f"- file path: `{issue.path}`")
            lines.append(f"  symbol/function/class name: `{issue.symbol or '-'} `")
            lines.append(f"  estimated severity: `{issue.severity}`")
            lines.append(f"  why it is bad: {issue.message}")
            lines.append(f"  exact recommendation: {issue.recommendation}")
    else:
        lines.append("- No exact targets identified.")

    lines.extend(["", "### C. Fast wins"])
    if fast_wins:
        for issue in fast_wins:
            lines.append(f"- `{issue.path}`: {issue.recommendation}")
    else:
        lines.append("- No low-risk fast wins identified.")

    lines.extend(["", "### D. Structural rewrites"])
    if structural:
        for issue in structural:
            lines.append(f"- `{issue.path}`: {issue.message} {issue.recommendation}")
    else:
        lines.append("- No structural rewrites required by current thresholds.")

    lines.extend(
        [
            "",
            "### E. Performance upgrades by language",
            "- Python",
        ]
    )
    if perf:
        for issue in perf:
            lines.append(
                f"  - `{issue.path}` `{issue.symbol or ''}`: {issue.recommendation}"
            )
    else:
        lines.append(
            "  - No obvious Python hotspots were flagged by the current heuristic scan."
        )
    lines.extend(
        [
            "- JavaScript/TypeScript",
            "  - Add ESLint/unused-export enforcement next; this pass does not yet scan JS/TS symbol usage deeply.",
            "- Database/SQL",
            "  - No SQL-specific automated audit added in this pass; add query-plan/index checks separately.",
            "- Rust/C/C++/Cython opportunities",
            "  - Prioritize files flagged as `native_hotspot_candidate` after benchmark confirmation.",
            "",
            "### F. Proposed patch plan",
            "1. delete dead code",
            "2. split god files",
            "3. split god functions",
            "4. optimize hot paths",
            "5. optimize database access",
            "6. reduce dependency and bundle bloat",
            "7. move justified hotspots to compiled/native code",
            "8. benchmark before/after",
            "",
            "### G. Proof",
            f"- files scanned: {summary['files_scanned']}",
            f"- dead code hits reported by vulture: {summary['dead_code_hits']}",
            f"- duplicate-code hits reported by pylint: {summary['duplicate_hits']}",
            f"- external tool audit complete: {summary['audit_complete']}",
            *(
                [f"- tool failure: {failure}" for failure in summary["tool_failures"]]
                or ["- tool failures: none"]
            ),
            f"- critical findings: {sum(1 for issue in issues if issue.severity == 'critical')}",
            f"- high findings: {sum(1 for issue in issues if issue.severity == 'high')}",
        ]
    )
    return "\n".join(lines) + "\n"


def _critical_issues(issues: list[Issue]) -> list[Issue]:
    return _group(
        issues,
        "god_file",
        "god_function",
        "syntax_error",
        "native_hotspot_candidate",
        "dead_code",
        "complexity",
        "audit_incomplete",
    )


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Guardrail audit and blocking checks")
    parser.add_argument("--targets", nargs="*", default=list(DEFAULT_TARGETS))
    source = parser.add_mutually_exclusive_group()
    source.add_argument("--staged-only", action="store_true")
    source.add_argument(
        "--from-ref",
        help="Audit files changed from the merge base with REF to HEAD",
    )
    parser.add_argument(
        "--check", action="store_true", help="Exit non-zero on critical/high findings"
    )
    parser.add_argument("--markdown-out", type=str, default="")
    parser.add_argument("--json-out", type=str, default="")
    parser.add_argument(
        "--root",
        help=(
            "Repository tree to audit. Defaults to the Git worktree containing "
            "the current working directory, never the checkout that supplied "
            "the imported conductor module."
        ),
    )
    args = parser.parse_args(argv)

    global ROOT
    try:
        ROOT = resolve_audit_root(args.root)
    except AuditRootError as exc:
        print(f"ERROR: guardrail-audit: {exc}", file=sys.stderr)
        return 2
    print_audit_provenance("guardrail-audit", ROOT)

    issues, summary = collect_issues(
        args.targets,
        staged_only=args.staged_only,
        from_ref=args.from_ref,
    )
    payload = {"summary": summary, "issues": [asdict(issue) for issue in issues]}
    report = build_markdown_report(issues, summary)

    if args.markdown_out:
        out = ROOT / args.markdown_out
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(report, encoding="utf-8")
    else:
        sys.stdout.write(report)

    if args.json_out:
        out = ROOT / args.json_out
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(payload, indent=2), encoding="utf-8")

    if args.check:
        blockers = [i for i in issues if i.severity in {"critical", "high"}]
        return 1 if blockers else 0
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
