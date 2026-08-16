#!/usr/bin/env python3
"""Run repository-wide source analyzers through stable entrypoints."""

from __future__ import annotations

import argparse
from collections.abc import Callable, Iterator
from contextlib import contextmanager
import defusedxml.ElementTree as ET
import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from pathlib import PurePosixPath


ROOT = Path(__file__).resolve().parents[1]
AUDIT_DIR = ROOT / "tasks" / "audit"

# Duplication baselines: grandfather pre-existing clone pairs so the staged
# gate only fails on NEW duplication introduced by a commit, not on repo-wide
# debt that predates it (same "add to the whitelist with reviewer approval"
# pattern as conductor/guardrail_allowlist.json and
# conductor/radon_complexity_baseline.json). Refresh with --save-baseline
# after a deliberate refactor changes the known clone set.
JSCPD_BASELINE_PATH = ROOT / "conductor" / "jscpd_duplication_baseline.json"
PMD_CPD_BASELINE_PATH = ROOT / "conductor" / "pmd_cpd_duplication_baseline.json"

DEFAULT_SOURCE_DIRS = (
    "research",
    "aria_core",
    "aria_designer",
    "component_fab",
    "conductor",
)

GENERATED_ARTIFACT_GLOBS = ("aria_designer/workflows/generated/**",)
JSCPD_INDEX_CONFIG_PATHS = (".gitignore", "package.json")

JSCPD_SOURCE_SUFFIXES = frozenset(
    {
        ".c",
        ".cc",
        ".cpp",
        ".cu",
        ".h",
        ".hpp",
        ".js",
        ".jsx",
        ".json",
        ".py",
        ".rs",
        ".sh",
        ".ts",
        ".tsx",
        ".yaml",
        ".yml",
    }
)
VULTURE_SOURCE_DIRS = ("research", "aria_core", "aria_designer")
VULTURE_SOURCE_SUFFIXES = frozenset({".py"})

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


def existing(paths: tuple[str, ...], *, root: Path = ROOT) -> list[str]:
    return [path for path in paths if (root / path).exists()]


def _existing_absolute(paths: tuple[str, ...], *, root: Path) -> list[str]:
    return [str(root / path) for path in paths if (root / path).exists()]


def _tracked_index_sources(
    paths: tuple[str, ...],
    suffixes: frozenset[str],
    *,
    root: Path,
    skip: Callable[[PurePosixPath], bool] | None = None,
    exact_paths: tuple[str, ...] = (),
) -> tuple[str, ...]:
    """Return source paths present in the candidate commit (the Git index)."""
    completed = subprocess.run(
        ["git", "ls-files", "--cached", "-z", "--", *paths, *exact_paths],
        cwd=root,
        check=True,
        capture_output=True,
    )
    candidates = completed.stdout.decode("utf-8", "surrogateescape").split("\0")
    selected: list[str] = []
    exact_path_set = set(exact_paths)
    for candidate in candidates:
        if not candidate:
            continue
        path = PurePosixPath(candidate)
        if candidate not in exact_path_set and path.suffix.lower() not in suffixes:
            continue
        if skip is not None and skip(path):
            continue
        selected.append(candidate)
    return tuple(selected)


@contextmanager
def materialized_index_sources(
    paths: tuple[str, ...],
    suffixes: frozenset[str],
    *,
    root: Path = ROOT,
    skip: Callable[[PurePosixPath], bool] | None = None,
    exact_paths: tuple[str, ...] = (),
) -> Iterator[Path]:
    """Materialize only selected source blobs from the Git index."""
    sources = _tracked_index_sources(
        paths, suffixes, root=root, skip=skip, exact_paths=exact_paths
    )
    with tempfile.TemporaryDirectory(prefix="llm-index-sources-") as temporary:
        snapshot = Path(temporary)
        if sources:
            payload = (
                b"\0".join(
                    source.encode("utf-8", "surrogateescape") for source in sources
                )
                + b"\0"
            )
            subprocess.run(
                [
                    "git",
                    "checkout-index",
                    "--stdin",
                    "-z",
                    f"--prefix={snapshot.as_posix()}/",
                ],
                cwd=root,
                input=payload,
                check=True,
            )
        yield snapshot


def _skip_vulture_source(path: PurePosixPath) -> bool:
    return bool(
        {".venv", "node_modules", "__pycache__", ".run", "tests", "migrations"}
        & set(path.parts)
    )


def _stable_dup_key(first_path: str, second_path: str, fragment: str) -> str:
    """Content-hash identity for a clone pair, stable across unrelated line drift.

    Keying on the duplicated text itself (not line numbers) means an edit
    elsewhere in either file doesn't spuriously "un-baseline" an existing,
    already-reviewed clone pair.
    """
    normalized = "\n".join(line.rstrip() for line in fragment.strip("\n").splitlines())
    digest = hashlib.sha256(normalized.encode("utf-8", "surrogateescape")).hexdigest()[
        :16
    ]
    a, b = sorted((first_path, second_path))
    return f"{a}::{b}::{digest}"


def _write_baseline(path: Path, entries: list[dict]) -> None:
    keyed: dict[str, dict] = {}
    for entry in entries:
        keyed[entry["key"]] = {
            "files": sorted((entry["firstFile"], entry["secondFile"])),
            "lines": entry["lines"],
        }
    payload = {
        "_comment": (
            "Baseline of known duplicate-code pairs, keyed by a content hash "
            "of the duplicated fragment (not line numbers). The staged gate "
            "only fails on pairs NOT in this file. Regenerate with "
            "`python conductor/run_duplicate_audit.py --tool <name> "
            "--save-baseline` after a deliberate refactor changes the known "
            "clone set; never hand-edit to hide a new duplicate."
        ),
        "count": len(keyed),
        "entries": keyed,
    }
    path.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    # Match the repo's biome JSON style (e.g. short arrays inline) so
    # regenerating a baseline doesn't leave a formatting-only diff behind.
    subprocess.run(
        ["npx", "--no-install", "biome", "check", "--write", str(path)],
        cwd=ROOT,
        check=False,
        capture_output=True,
    )
    print(f"Wrote {len(keyed)} baseline entries to {path.relative_to(ROOT)}")


def _check_against_baseline(path: Path, entries: list[dict], *, tool_name: str) -> int:
    baseline: dict[str, dict] = {}
    if path.exists():
        baseline = json.loads(path.read_text(encoding="utf-8")).get("entries", {})
    current = {entry["key"]: entry for entry in entries}
    new_keys = sorted(set(current) - set(baseline))
    print(
        f"{tool_name}: {len(current)} duplicate pair(s) found, "
        f"{len(baseline)} in baseline, {len(new_keys)} new."
    )
    if not new_keys:
        return 0
    print(f"ERROR: {tool_name} found {len(new_keys)} new duplicate pair(s):")
    for key in new_keys[:20]:
        entry = current[key]
        print(
            f"  {entry['firstFile']}  <->  {entry['secondFile']}"
            f"  ({entry['lines']} lines)"
        )
    if len(new_keys) > 20:
        print(f"  ... and {len(new_keys) - 20} more")
    print(
        "Refactor to remove the duplication, or if it's a deliberate/"
        "pre-existing pattern being adopted with reviewer approval, rerun "
        f"with --save-baseline to record it in {path.relative_to(ROOT)}."
    )
    return 1


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


def _run_jscpd_paths(
    check: bool,
    paths: list[str],
    *,
    cwd: Path,
    executable: str | None = None,
) -> int:
    cmd = (
        [executable, "--noTips"]
        if executable
        else ["npx", "--no-install", "jscpd", "--noTips"]
    )
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
    cmd.extend(paths)
    if not paths:
        return 0
    return run(cmd, allow_findings=not check, cwd=cwd)


def _jscpd_collect_duplicates(
    paths: list[str], *, cwd: Path, executable: str | None = None
) -> list[dict]:
    """Run jscpd with the JSON reporter and return normalized clone entries."""
    if not paths:
        return []
    with tempfile.TemporaryDirectory(prefix="llm-jscpd-json-") as tmp:
        out_dir = Path(tmp)
        cmd = (
            [executable, "--noTips"]
            if executable
            else ["npx", "--no-install", "jscpd", "--noTips"]
        )
        cmd.extend(
            [
                "--reporters",
                "json",
                "--output",
                str(out_dir),
                "--silent",
                "--threshold",
                "100",
            ]
        )
        cmd.extend(paths)
        print("+ " + " ".join(cmd), flush=True)
        subprocess.run(cmd, cwd=cwd, check=False)
        report = out_dir / "jscpd-report.json"
        if not report.exists():
            return []
        data = json.loads(report.read_text(encoding="utf-8"))
        entries = []
        for dup in data.get("duplicates", []):
            first = dup["firstFile"]["name"]
            second = dup["secondFile"]["name"]
            fragment = dup.get("fragment", "")
            entries.append(
                {
                    "key": _stable_dup_key(first, second, fragment),
                    "firstFile": first,
                    "secondFile": second,
                    "lines": dup.get("lines"),
                }
            )
        return entries


def run_jscpd(
    check: bool,
    index_snapshot: bool = False,
    save_baseline: bool = False,
    *,
    root: Path = ROOT,
) -> int:
    if index_snapshot:
        executable = root / "node_modules" / ".bin" / "jscpd"
        if not executable.exists():
            print(
                f"jscpd not found at {executable}. Run npm install in {root}.",
                file=sys.stderr,
            )
            return 127
        with materialized_index_sources(
            DEFAULT_SOURCE_DIRS,
            JSCPD_SOURCE_SUFFIXES,
            root=root,
            exact_paths=JSCPD_INDEX_CONFIG_PATHS,
        ) as snapshot:
            paths = existing(DEFAULT_SOURCE_DIRS, root=snapshot)
            entries = _jscpd_collect_duplicates(
                paths, cwd=snapshot, executable=str(executable.resolve())
            )
        if save_baseline:
            _write_baseline(JSCPD_BASELINE_PATH, entries)
            return 0
        return _check_against_baseline(JSCPD_BASELINE_PATH, entries, tool_name="jscpd")

    if save_baseline:
        entries = _jscpd_collect_duplicates(existing(DEFAULT_SOURCE_DIRS), cwd=root)
        _write_baseline(JSCPD_BASELINE_PATH, entries)
        return 0

    if check:
        entries = _jscpd_collect_duplicates(existing(DEFAULT_SOURCE_DIRS), cwd=root)
        return _check_against_baseline(JSCPD_BASELINE_PATH, entries, tool_name="jscpd")

    code = _run_jscpd_paths(check, existing(DEFAULT_SOURCE_DIRS), cwd=ROOT)
    if code:
        return code
    return run_jscpd_generated()


def run_vulture(
    check: bool,
    index_snapshot: bool = False,
    save_baseline: bool = False,
    *,
    root: Path = ROOT,
) -> int:
    del save_baseline

    def analyze(paths: list[str]) -> int:
        if not paths:
            return 0
        cmd = [
            "uv",
            "run",
            "vulture",
            *paths,
            "--min-confidence",
            "80",
        ]
        return run(cmd, allow_findings=not check, cwd=root)

    if index_snapshot:
        with materialized_index_sources(
            VULTURE_SOURCE_DIRS,
            VULTURE_SOURCE_SUFFIXES,
            root=root,
            skip=_skip_vulture_source,
        ) as snapshot:
            return analyze(_existing_absolute(VULTURE_SOURCE_DIRS, root=snapshot))

    return analyze(existing(VULTURE_SOURCE_DIRS, root=root))


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


def run_pylint(
    check: bool, index_snapshot: bool = False, save_baseline: bool = False
) -> int:
    del index_snapshot, save_baseline
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


def _skip_pmd_source(path: PurePosixPath) -> bool:
    parts = set(path.parts)
    if {
        ".venv",
        "node_modules",
        "__pycache__",
        "build",
        "dist",
        ".run",
        "tests",
    } & parts:
        return True
    rel = path.as_posix()
    skip_prefixes = (
        "research/dashboard/",
        "research/runtime/",
        "research/runtime_events/",
        "research/reports/",
        "research/data/",
        "research/perf_artifacts/",
    )
    return rel.startswith(skip_prefixes)


def _pmd_snapshot_file_list(root: Path, snapshot: Path) -> list[str]:
    sources = _tracked_index_sources(
        DEFAULT_SOURCE_DIRS, frozenset({".py"}), root=root, skip=_skip_pmd_source
    )
    return [str(snapshot / source) for source in sources]


def _relativize(path_str: str, root: Path) -> str:
    try:
        return Path(path_str).resolve().relative_to(root.resolve()).as_posix()
    except ValueError:
        return path_str


def _pmd_collect_duplicates(files: list[str], *, relativize_root: Path) -> list[dict]:
    """Run PMD CPD with the XML reporter and return normalized clone entries.

    PMD's own ``--relativize-paths-with`` flag is a no-op with the XML
    reporter in the pinned PMD version (paths come back exactly as given in
    the file-list), so paths are relativized here instead.
    """
    if not files:
        return []
    with tempfile.TemporaryDirectory(prefix="llm-pmd-xml-") as tmp:
        file_list = Path(tmp) / "files.txt"
        file_list.write_text("\n".join(files) + "\n", encoding="utf-8")
        report = Path(tmp) / "cpd-report.xml"
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
            str(relativize_root),
            "--format",
            "xml",
            "--report-file",
            str(report),
            "--no-fail-on-violation",
        ]
        for pattern in PMD_EXCLUDES:
            cmd.extend(["--exclude", pattern])
        print("+ " + " ".join(cmd), flush=True)
        subprocess.run(cmd, cwd=ROOT, check=False)
        if not report.exists():
            return []
        try:
            tree = ET.parse(report)
        except ET.ParseError:
            return []
        root = tree.getroot()
        if root is None:
            return []
        ns = {"cpd": "https://pmd-code.org/schema/cpd-report"}
        entries = []
        for dup in root.findall("cpd:duplication", ns):
            file_elements = dup.findall("cpd:file", ns)
            if len(file_elements) < 2:
                continue
            first = _relativize(file_elements[0].get("path", ""), relativize_root)
            second = _relativize(file_elements[1].get("path", ""), relativize_root)
            fragment_el = dup.find("cpd:codefragment", ns)
            fragment = (fragment_el.text or "") if fragment_el is not None else ""
            entries.append(
                {
                    "key": _stable_dup_key(first, second, fragment),
                    "firstFile": first,
                    "secondFile": second,
                    "lines": int(dup.get("lines", 0)),
                }
            )
        return entries


def run_pmd_python(
    check: bool, index_snapshot: bool = False, save_baseline: bool = False
) -> int:
    if index_snapshot:
        with materialized_index_sources(
            DEFAULT_SOURCE_DIRS,
            frozenset({".py"}),
            root=ROOT,
            skip=_skip_pmd_source,
        ) as snapshot:
            files = _pmd_snapshot_file_list(ROOT, snapshot)
            entries = _pmd_collect_duplicates(files, relativize_root=snapshot)
        if save_baseline:
            _write_baseline(PMD_CPD_BASELINE_PATH, entries)
            return 0
        return _check_against_baseline(
            PMD_CPD_BASELINE_PATH, entries, tool_name="pmd-cpd"
        )

    if save_baseline:
        file_list = python_file_list("duplication-python-files.txt")
        files = [
            line for line in file_list.read_text(encoding="utf-8").splitlines() if line
        ]
        entries = _pmd_collect_duplicates(files, relativize_root=ROOT)
        _write_baseline(PMD_CPD_BASELINE_PATH, entries)
        return 0

    if check:
        file_list = python_file_list("duplication-python-files.txt")
        files = [
            line for line in file_list.read_text(encoding="utf-8").splitlines() if line
        ]
        entries = _pmd_collect_duplicates(files, relativize_root=ROOT)
        return _check_against_baseline(
            PMD_CPD_BASELINE_PATH, entries, tool_name="pmd-cpd"
        )

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
        "--no-fail-on-violation",
    ]
    for pattern in PMD_EXCLUDES:
        cmd.extend(["--exclude", pattern])
    AUDIT_DIR.mkdir(parents=True, exist_ok=True)
    return run(cmd)


def run_nicad_python(
    check: bool, index_snapshot: bool = False, save_baseline: bool = False
) -> int:
    del index_snapshot, save_baseline
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
    "vulture": run_vulture,
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
    parser.add_argument(
        "--index-snapshot",
        action="store_true",
        help="Analyze exact source blobs from the candidate Git index.",
    )
    parser.add_argument(
        "--save-baseline",
        action="store_true",
        help=(
            "Record current duplicate-code pairs (jscpd, pmd-python) as the "
            "known baseline instead of checking. Run after a deliberate "
            "refactor changes the known clone set; never to hide a new one."
        ),
    )
    args = parser.parse_args()

    selected = args.tool or ["jscpd", "pmd-python"]
    if args.index_snapshot and not (args.check or args.save_baseline):
        parser.error("--index-snapshot requires --check or --save-baseline")
    index_snapshot_supported = {"jscpd", "vulture", "pmd-python"}
    unsupported = set(selected) - index_snapshot_supported
    if args.index_snapshot and unsupported:
        parser.error(
            "--index-snapshot is only supported for "
            f"{', '.join(sorted(index_snapshot_supported))}; "
            f"got {', '.join(sorted(unsupported))}"
        )
    baseline_supported = {"jscpd", "pmd-python"}
    unsupported_baseline = set(selected) - baseline_supported
    if args.save_baseline and unsupported_baseline:
        parser.error(
            "--save-baseline is only supported for "
            f"{', '.join(sorted(baseline_supported))}; "
            f"got {', '.join(sorted(unsupported_baseline))}"
        )
    for name in selected:
        print(f"== {name} ==", flush=True)
        code = TOOLS[name](args.check, args.index_snapshot, args.save_baseline)
        if code:
            return code
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
