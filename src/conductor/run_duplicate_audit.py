#!/usr/bin/env python3
"""Run clone detectors through stable repo entrypoints."""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
AUDIT_DIR = ROOT / "tasks" / "audit"

DEFAULT_SOURCE_DIRS = (
    "research",
    "aria_core",
    "aria_designer",
    "component_fab",
    "conductor",
)

GENERATED_ARTIFACT_GLOBS = ("aria_designer/workflows/generated/**",)

PMD_EXCLUDES = (
    "**/.venv/**",
    "**/node_modules/**",
    "**/__pycache__/**",
    "**/build/**",
    "**/dist/**",
    "**/.run/**",
    "**/tests/**",
    "research/dashboard/**",
    "research/runtime/**",
    "research/runtime_events/**",
    "research/reports/**",
    "research/data/**",
    "research/perf_artifacts/**",
)


def should_skip_python(path: Path) -> bool:
    rel = path.relative_to(ROOT).as_posix()
    parts = set(path.relative_to(ROOT).parts)
    if path.name.startswith("."):
        return True
    if {"tests", "__pycache__", "node_modules", "build", "dist"} & parts:
        return True
    skip_prefixes = (
        "research/dashboard/",
        "research/runtime/",
        "research/runtime_events/",
        "research/reports/",
        "research/data/",
        "research/perf_artifacts/",
        "research/corpus/",
        "tasks/audit/",
    )
    return rel.startswith(skip_prefixes)


def python_file_list(name: str, paths: tuple[str, ...] = DEFAULT_SOURCE_DIRS) -> Path:
    AUDIT_DIR.mkdir(parents=True, exist_ok=True)
    file_list = AUDIT_DIR / name
    files: list[str] = []
    for source in existing(paths):
        for path in (ROOT / source).rglob("*.py"):
            if not should_skip_python(path):
                files.append(str(path))
    file_list.write_text("\n".join(sorted(files)) + "\n", encoding="utf-8")
    return file_list


def existing(paths: tuple[str, ...]) -> list[str]:
    return [path for path in paths if (ROOT / path).exists()]


def command_path(name: str) -> str | None:
    resolved = shutil.which(name)
    if resolved:
        return resolved
    local = Path.home() / ".local" / "bin" / name
    if local.exists():
        return str(local)
    return None


def run(cmd: list[str], *, allow_findings: bool = False, cwd: Path = ROOT) -> int:
    print("+ " + " ".join(cmd), flush=True)
    completed = subprocess.run(cmd, cwd=cwd, check=False)
    if completed.returncode and not allow_findings:
        return completed.returncode
    return 0


def run_jscpd(check: bool) -> int:
    cmd = [
        "npx",
        "--no-install",
        "jscpd",
        "--config",
        ".jscpd.json",
        "--noTips",
    ]
    if not check:
        cmd.extend(
            [
                "--reporters",
                "json",
                "--output",
                str(AUDIT_DIR / "duplication-jscpd"),
                "--silent",
                "--threshold",
                "100",
            ]
        )
    cmd.extend(existing(DEFAULT_SOURCE_DIRS))
    code = run(cmd, allow_findings=not check)
    if code or check:
        return code
    return run_jscpd_generated()


def run_jscpd_generated() -> int:
    generated_paths = [
        pattern.rsplit("/**", 1)[0]
        for pattern in GENERATED_ARTIFACT_GLOBS
        if (ROOT / pattern.rsplit("/**", 1)[0]).exists()
    ]
    if not generated_paths:
        return 0
    AUDIT_DIR.mkdir(parents=True, exist_ok=True)
    generated_config = AUDIT_DIR / "duplication-jscpd-generated-config.json"
    generated_config.write_text(
        json.dumps(
            {
                "threshold": 100,
                "minLines": 10,
                "minTokens": 80,
                "reporters": ["json"],
                "gitignore": True,
                "noSymlinks": True,
                "ignore": [
                    "**/node_modules/**",
                    "**/build/**",
                    "**/dist/**",
                    "**/__pycache__/**",
                ],
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    cmd = [
        "npx",
        "--no-install",
        "jscpd",
        "--config",
        str(generated_config),
        "--reporters",
        "json",
        "--output",
        str(AUDIT_DIR / "duplication-jscpd-generated"),
        "--silent",
        "--threshold",
        "100",
        "--noTips",
        *generated_paths,
    ]
    print("== jscpd-generated ==", flush=True)
    return run(cmd, allow_findings=True)


def run_pylint(check: bool) -> int:
    report = AUDIT_DIR / "duplication-pylint.txt"
    file_list = python_file_list(
        "duplication-python-files.txt", ("research", "aria_core", "aria_designer")
    )
    files = [
        line for line in file_list.read_text(encoding="utf-8").splitlines() if line
    ]
    cmd = [
        "uv",
        "run",
        "pylint",
        *files,
        "--disable=all",
        "--enable=duplicate-code",
        "--min-similarity-lines=10",
        "--jobs=0",
        "--output-format=text",
    ]
    print(f"Writing {report.relative_to(ROOT)}", flush=True)
    AUDIT_DIR.mkdir(parents=True, exist_ok=True)
    with report.open("w", encoding="utf-8") as handle:
        completed = subprocess.run(
            cmd, cwd=ROOT, stdout=handle, stderr=subprocess.STDOUT, check=False
        )
    if completed.returncode and check:
        return completed.returncode
    return 0


def run_pmd_python(check: bool) -> int:
    report = AUDIT_DIR / "duplication-pmd-python.txt"
    file_list = python_file_list("duplication-python-files.txt")
    cmd = [
        "npx",
        "--no-install",
        "pmd",
        "cpd",
        "--file-list",
        str(file_list),
        "--language",
        "python",
        "--minimum-tokens",
        "80",
        "--skip-duplicate-files",
        "--relativize-paths-with",
        str(ROOT),
        "--format",
        "text",
        "--report-file",
        str(report),
    ]
    if not check:
        cmd.append("--no-fail-on-violation")
    for pattern in PMD_EXCLUDES:
        cmd.extend(["--exclude", pattern])
    AUDIT_DIR.mkdir(parents=True, exist_ok=True)
    return run(cmd)


def run_nicad_python(check: bool) -> int:
    nicad = command_path("nicad")
    if not nicad:
        print(
            "nicad not found. Install NiCad/OpenTxl, or put nicad on PATH.",
            file=sys.stderr,
        )
        return 127 if check else 0

    nicad_dir = AUDIT_DIR / "nicad"
    nicad_dir.mkdir(parents=True, exist_ok=True)
    cmd = [nicad, "functions", "py", str(ROOT / "research"), "notests-report"]
    return run(cmd, allow_findings=not check, cwd=nicad_dir)


TOOLS = {
    "jscpd": run_jscpd,
    "pylint": run_pylint,
    "pmd-python": run_pmd_python,
    "nicad-python": run_nicad_python,
}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--tool",
        action="append",
        choices=sorted(TOOLS),
        default=[],
        help="Tool to run. Can be repeated. Defaults to jscpd and pmd-python.",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Return nonzero when a detector reports duplicates.",
    )
    args = parser.parse_args()

    selected = args.tool or ["jscpd", "pmd-python"]
    for name in selected:
        print(f"== {name} ==", flush=True)
        code = TOOLS[name](args.check)
        if code:
            return code
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
