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
from typing import Any
from pathlib import Path
from pathlib import PurePosixPath


ROOT = Path(__file__).resolve().parents[1]

# Duplication baselines: grandfather pre-existing clone pairs so the staged
# gate only fails on NEW duplication introduced by a commit, not on repo-wide
# debt that predates it (same "add to the whitelist with reviewer approval"
# pattern as conductor/guardrail_allowlist.json and
# conductor/radon_complexity_baseline.json). Refresh with --save-baseline
# after a deliberate refactor changes the known clone set.
JSCPD_BASELINE_RELATIVE = Path("conductor/jscpd_duplication_baseline.json")
PMD_CPD_BASELINE_RELATIVE = Path("conductor/pmd_cpd_duplication_baseline.json")
AUDIT_ERROR_EXIT_CODE = 2

DEFAULT_SOURCE_DIRS = (
    "research",
    "aria_core",
    "aria_designer",
    "component_fab",
    "conductor",
)

GENERATED_ARTIFACT_GLOBS = ("aria_designer/workflows/generated/**",)
JSCPD_INDEX_CONFIG_PATHS = (
    ".gitignore",
    "package.json",
    JSCPD_BASELINE_RELATIVE.as_posix(),
)
JSCPD_GENERATED_EVIDENCE_IGNORE = "**/conductor/mutation_campaigns/receipts/**"

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


class DuplicateAuditError(RuntimeError):
    """The analyzer evidence was incomplete or structurally invalid."""


def _resolve_audit_root(
    explicit_root: str | Path | None, *, cwd: Path | None = None
) -> Path:
    """Prefer an explicit root, otherwise use cwd's Git worktree."""
    invocation_cwd = (cwd or Path.cwd()).resolve()
    if explicit_root is not None:
        candidate = Path(explicit_root).expanduser()
        candidate = candidate if candidate.is_absolute() else invocation_cwd / candidate
        try:
            root = candidate.resolve(strict=True)
        except OSError as exc:
            message = f"explicit audit root does not exist ({candidate}): {exc}"
            raise DuplicateAuditError(message) from exc
        if not root.is_dir():
            raise DuplicateAuditError(f"explicit audit root is not a directory: {root}")
        return root

    completed = subprocess.run(
        ["git", "-C", str(invocation_cwd), "rev-parse", "--show-toplevel"],
        check=False,
        capture_output=True,
        text=True,
        errors="replace",
    )
    resolved = completed.stdout.strip()
    if completed.returncode or not resolved:
        detail = completed.stderr.strip() or f"git exited {completed.returncode}"
        raise DuplicateAuditError(
            "cannot resolve audit root from the current working directory "
            f"({invocation_cwd}): {detail}; pass --root explicitly"
        )
    root = Path(resolved).resolve()
    return root


def _git_head(root: Path) -> str:
    completed = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "HEAD"],
        check=False,
        capture_output=True,
        text=True,
        errors="replace",
    )
    head = completed.stdout.strip()
    return head if completed.returncode == 0 and head else "unavailable"


def _print_audit_provenance(root: Path, *, index_snapshot: bool) -> None:
    mode = "index-snapshot" if index_snapshot else "worktree"
    print(f"audit-root: {root} | git-head: {_git_head(root)} | mode: {mode}")


def should_skip_python(path: Path, *, root: Path = ROOT) -> bool:
    rel = path.relative_to(root).as_posix()
    parts = set(path.relative_to(root).parts)
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


def python_file_list(
    name: str,
    paths: tuple[str, ...] = DEFAULT_SOURCE_DIRS,
    *,
    root: Path = ROOT,
) -> Path:
    audit_dir = root / "tasks" / "audit"
    audit_dir.mkdir(parents=True, exist_ok=True)
    file_list = audit_dir / name
    files: list[str] = []
    for source in existing(paths, root=root):
        for path in (root / source).rglob("*.py"):
            if not should_skip_python(path, root=root):
                files.append(str(path))
    file_list.write_text("\n".join(sorted(files)) + "\n", encoding="utf-8")
    return file_list


def existing(paths: tuple[str, ...], *, root: Path = ROOT) -> list[str]:
    return [path for path in paths if (root / path).exists()]


def _existing_absolute(paths: tuple[str, ...], *, root: Path) -> list[str]:
    return [str(root / path) for path in paths if (root / path).exists()]


def _live_jscpd_sources(*, root: Path) -> list[str] | None:
    """Return Git-visible sources, or None for a caller-materialized export."""
    worktree = subprocess.run(
        ["git", "rev-parse", "--is-inside-work-tree"],
        cwd=root,
        check=False,
        capture_output=True,
    )
    if worktree.returncode or worktree.stdout.strip() != b"true":
        return None

    completed = subprocess.run(
        [
            "git",
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            *DEFAULT_SOURCE_DIRS,
        ],
        cwd=root,
        check=False,
        capture_output=True,
    )
    if completed.returncode:
        detail = completed.stderr.decode("utf-8", "replace").strip()
        suffix = f": {detail}" if detail else ""
        raise DuplicateAuditError(f"git could not enumerate JSCPD sources{suffix}")

    def include(candidate: str) -> bool:
        disk_path = root / candidate
        return bool(
            candidate
            and PurePosixPath(candidate).suffix.lower() in JSCPD_SOURCE_SUFFIXES
            and disk_path.is_file()
            and not disk_path.is_symlink()
        )

    candidates = completed.stdout.decode("utf-8", "surrogateescape").split("\0")
    return sorted({candidate for candidate in candidates if include(candidate)})


@contextmanager
def materialized_live_jscpd_sources(
    sources: list[str] | None, *, root: Path
) -> Iterator[Path]:
    """Yield directory-shaped JSCPD input containing only Git-visible files."""
    if sources is None:
        yield root
        return

    selected = set(sources) | set(JSCPD_INDEX_CONFIG_PATHS)
    with tempfile.TemporaryDirectory(prefix="llm-live-jscpd-sources-") as temporary:
        snapshot = Path(temporary)
        for relative in sorted(selected):
            source = root / relative
            if not source.is_file() or source.is_symlink():
                continue
            destination = snapshot / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
        yield snapshot


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


def _relativize_jscpd_name(file_entry: object, cwd: Path) -> str | None:
    """Repo-relative posix path for a jscpd firstFile/secondFile entry.

    jscpd's JSON reporter only populates ``fragment`` (the duplicated text
    ``_stable_dup_key`` hashes for identity) when invoked with ``--absolute``,
    which reports absolute paths instead of bare basenames; convert back to
    a repo-relative path so findings match the rest of the governance tooling.
    """
    if not isinstance(file_entry, dict):
        return None
    name = file_entry.get("name")
    if not isinstance(name, str) or not name:
        return None
    try:
        return Path(name).resolve().relative_to(cwd.resolve()).as_posix()
    except ValueError:
        return name


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


def _write_baseline(path: Path, entries: list[dict], *, root: Path = ROOT) -> None:
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
        cwd=root,
        check=False,
        capture_output=True,
    )
    print(f"Wrote {len(keyed)} baseline entries to {_display_path(path, root=root)}")


def _display_path(path: Path, *, root: Path = ROOT) -> str:
    try:
        return path.relative_to(root).as_posix()
    except ValueError:
        return str(path)


def _baseline_entries(path: Path) -> dict[str, dict[str, Any]]:
    if not path.is_file():
        raise DuplicateAuditError(f"required baseline report is missing: {path}")
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise DuplicateAuditError(
            f"baseline is not valid JSON ({path}): {exc}"
        ) from exc
    if not isinstance(payload, dict):
        raise DuplicateAuditError(f"baseline root must be an object: {path}")
    expected_keys = {"_comment", "count", "entries"}
    actual_keys = set(payload)
    if actual_keys != expected_keys:
        raise DuplicateAuditError(
            f"baseline keys are invalid ({path}): expected {sorted(expected_keys)}, "
            f"got {sorted(actual_keys)}"
        )
    count = payload["count"]
    entries = payload["entries"]
    if isinstance(count, bool) or not isinstance(count, int) or count < 0:
        raise DuplicateAuditError(
            f"baseline count must be a non-negative integer ({path}); got {count!r}"
        )
    if not isinstance(entries, dict):
        raise DuplicateAuditError(f"baseline entries must be an object: {path}")
    if count != len(entries):
        raise DuplicateAuditError(
            f"baseline count mismatch ({path}): declared {count}, found {len(entries)}"
        )
    validated: dict[str, dict[str, Any]] = {}
    for key, entry in entries.items():
        if not isinstance(key, str) or not key:
            raise DuplicateAuditError(
                f"baseline contains an invalid entry key: {key!r}"
            )
        if not isinstance(entry, dict) or set(entry) != {"files", "lines"}:
            raise DuplicateAuditError(
                f"baseline entry {key!r} must contain exactly 'files' and 'lines'"
            )
        files = entry["files"]
        lines = entry["lines"]
        if (
            not isinstance(files, list)
            or len(files) != 2
            or not all(isinstance(item, str) and item for item in files)
            or files != sorted(files)
        ):
            raise DuplicateAuditError(
                f"baseline entry {key!r} has invalid sorted file-pair metadata"
            )
        if isinstance(lines, bool) or not isinstance(lines, int) or lines <= 0:
            raise DuplicateAuditError(
                f"baseline entry {key!r} has invalid line count {lines!r}"
            )
        pair, separator, digest = key.rpartition("::")
        if (
            not separator
            or pair != "::".join(files)
            or len(digest) != 16
            or any(character not in "0123456789abcdef" for character in digest)
        ):
            raise DuplicateAuditError(
                f"baseline entry key does not match its file pair/hash schema: {key!r}"
            )
        validated[key] = entry
    return validated


def _check_against_baseline(
    path: Path, entries: list[dict], *, tool_name: str, root: Path = ROOT
) -> int:
    try:
        baseline = _baseline_entries(path)
    except DuplicateAuditError as exc:
        print(f"ERROR: {tool_name}: {exc}", file=sys.stderr)
        return AUDIT_ERROR_EXIT_CODE
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
        f"with --save-baseline to record it in {_display_path(path, root=root)}."
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


def _run_report_command(
    cmd: list[str], *, cwd: Path, tool_name: str
) -> subprocess.CompletedProcess[str]:
    """Run an analyzer whose report is required as trustworthy evidence."""
    print("+ " + " ".join(cmd), flush=True)
    try:
        completed = subprocess.run(
            cmd,
            cwd=cwd,
            check=False,
            capture_output=True,
            text=True,
            errors="replace",
        )
    except OSError as exc:
        raise DuplicateAuditError(f"{tool_name} could not execute: {exc}") from exc
    if completed.returncode:
        detail = (completed.stderr or completed.stdout).strip()
        suffix = f": {detail}" if detail else ""
        raise DuplicateAuditError(
            f"{tool_name} exited {completed.returncode}; report rejected{suffix}"
        )
    return completed


def _run_jscpd_paths(
    check: bool,
    paths: list[str],
    *,
    cwd: Path,
    audit_dir: Path,
    executable: str | None = None,
) -> int:
    cmd = [_resolve_jscpd_executable(cwd, executable)]
    if not check:
        cmd.extend(
            [
                "--reporters",
                "json",
                "--output",
                str(audit_dir / "duplication-jscpd"),
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
        try:
            package = json.loads((cwd / "package.json").read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError) as exc:
            raise DuplicateAuditError(
                f"cannot load jscpd package config: {exc}"
            ) from exc
        config = package.get("jscpd") if isinstance(package, dict) else None
        if not isinstance(config, dict):
            raise DuplicateAuditError("package.json must contain a jscpd object")
        ignore = config.get("ignore", [])
        if not isinstance(ignore, list) or not all(
            isinstance(pattern, str) and pattern for pattern in ignore
        ):
            raise DuplicateAuditError("package.json jscpd.ignore must be a string list")
        portable_ignore = [
            pattern if pattern.startswith("**/") else f"**/{pattern}"
            for pattern in ignore
        ]
        config = {
            **config,
            "ignore": [*portable_ignore, JSCPD_GENERATED_EVIDENCE_IGNORE],
        }
        config_path = out_dir / "jscpd.config.json"
        config_path.write_text(json.dumps(config), encoding="utf-8")
        cmd = [
            _resolve_jscpd_executable(cwd, executable),
            "--absolute",
            "--config",
            str(config_path),
        ]
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
        _run_report_command(cmd, cwd=cwd, tool_name="jscpd")
        report = out_dir / "jscpd-report.json"
        if not report.is_file():
            raise DuplicateAuditError(
                f"jscpd exited successfully but did not produce {report.name}"
            )
        try:
            data = json.loads(report.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError) as exc:
            raise DuplicateAuditError(f"jscpd report is not valid JSON: {exc}") from exc
        if not isinstance(data, dict) or not isinstance(data.get("duplicates"), list):
            raise DuplicateAuditError(
                "jscpd report must be an object containing a duplicates array"
            )
        entries: list[dict] = []
        for index, dup in enumerate(data["duplicates"]):
            if not isinstance(dup, dict):
                raise DuplicateAuditError(f"jscpd duplicate {index} must be an object")
            first_file = dup.get("firstFile")
            second_file = dup.get("secondFile")
            first = _relativize_jscpd_name(first_file, cwd)
            second = _relativize_jscpd_name(second_file, cwd)
            fragment = dup.get("fragment")
            lines = dup.get("lines")
            if not isinstance(first, str) or not first:
                raise DuplicateAuditError(
                    f"jscpd duplicate {index} has an invalid firstFile.name"
                )
            if not isinstance(second, str) or not second:
                raise DuplicateAuditError(
                    f"jscpd duplicate {index} has an invalid secondFile.name"
                )
            if not isinstance(fragment, str) or not fragment:
                raise DuplicateAuditError(
                    f"jscpd duplicate {index} has an invalid fragment"
                )
            if isinstance(lines, bool) or not isinstance(lines, int) or lines <= 0:
                raise DuplicateAuditError(
                    f"jscpd duplicate {index} has an invalid line count {lines!r}"
                )
            entries.append(
                {
                    "key": _stable_dup_key(first, second, fragment),
                    "firstFile": first,
                    "secondFile": second,
                    "lines": lines,
                }
            )
        return entries


def _resolve_jscpd_executable(cwd: Path, executable: str | None) -> str:
    """Resolve one analyzer binary without npm/global-package ambiguity."""
    if executable:
        return executable
    local = cwd / "node_modules" / ".bin" / "jscpd"
    if local.is_file():
        return str(local.resolve())
    resolved = shutil.which("jscpd")
    if resolved:
        return resolved
    raise DuplicateAuditError(
        "jscpd executable is unavailable; install the repository-pinned analyzer"
    )


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
            try:
                entries = _jscpd_collect_duplicates(
                    paths, cwd=snapshot, executable=str(executable.resolve())
                )
            except DuplicateAuditError as exc:
                print(f"ERROR: jscpd: {exc}", file=sys.stderr)
                return AUDIT_ERROR_EXIT_CODE
            if not save_baseline:
                return _check_against_baseline(
                    snapshot / JSCPD_BASELINE_RELATIVE,
                    entries,
                    tool_name="jscpd",
                    root=snapshot,
                )
        if save_baseline:
            _write_baseline(root / JSCPD_BASELINE_RELATIVE, entries, root=root)
            return 0

    try:
        live_sources = _live_jscpd_sources(root=root)
        executable = _resolve_jscpd_executable(root, None)
    except DuplicateAuditError as exc:
        print(f"ERROR: jscpd: {exc}", file=sys.stderr)
        return AUDIT_ERROR_EXIT_CODE

    with materialized_live_jscpd_sources(live_sources, root=root) as scan_root:
        scan_paths = existing(DEFAULT_SOURCE_DIRS, root=scan_root)
        if not (save_baseline or check):
            code = _run_jscpd_paths(
                check,
                scan_paths,
                cwd=scan_root,
                audit_dir=root / "tasks" / "audit",
                executable=executable,
            )
            if code:
                return code
            return run_jscpd_generated(root=root)

        try:
            entries = _jscpd_collect_duplicates(
                scan_paths, cwd=scan_root, executable=executable
            )
        except DuplicateAuditError as exc:
            print(f"ERROR: jscpd: {exc}", file=sys.stderr)
            return AUDIT_ERROR_EXIT_CODE

        if check:
            return _check_against_baseline(
                scan_root / JSCPD_BASELINE_RELATIVE,
                entries,
                tool_name="jscpd",
                root=scan_root,
            )

    if save_baseline:
        _write_baseline(root / JSCPD_BASELINE_RELATIVE, entries, root=root)
        return 0

    raise AssertionError("unreachable jscpd mode")


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


def run_jscpd_generated(*, root: Path = ROOT) -> int:
    audit_dir = root / "tasks" / "audit"
    generated_paths = [
        pattern.rsplit("/**", 1)[0]
        for pattern in GENERATED_ARTIFACT_GLOBS
        if (root / pattern.rsplit("/**", 1)[0]).exists()
    ]
    if not generated_paths:
        return 0
    audit_dir.mkdir(parents=True, exist_ok=True)
    generated_config = audit_dir / "duplication-jscpd-generated-config.json"
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
        str(audit_dir / "duplication-jscpd-generated"),
        "--silent",
        "--threshold",
        "100",
        *generated_paths,
    ]
    print("== jscpd-generated ==", flush=True)
    return run(cmd, allow_findings=True, cwd=root)


def run_pylint(
    check: bool,
    index_snapshot: bool = False,
    save_baseline: bool = False,
    *,
    root: Path = ROOT,
) -> int:
    del index_snapshot, save_baseline
    audit_dir = root / "tasks" / "audit"
    report = audit_dir / "duplication-pylint.txt"
    file_list = python_file_list(
        "duplication-python-files.txt",
        ("research", "aria_core", "aria_designer"),
        root=root,
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
    print(f"Writing {report.relative_to(root)}", flush=True)
    audit_dir.mkdir(parents=True, exist_ok=True)
    with report.open("w", encoding="utf-8") as handle:
        completed = subprocess.run(
            cmd, cwd=root, stdout=handle, stderr=subprocess.STDOUT, check=False
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


def _parse_pmd_report(report: Path, relativize_root: Path) -> list[dict]:
    if not report.is_file():
        raise DuplicateAuditError(
            f"pmd-cpd exited successfully but did not produce {report.name}"
        )
    try:
        tree = ET.parse(report)
    except (OSError, UnicodeError, ET.ParseError) as exc:
        raise DuplicateAuditError(f"pmd-cpd report is not valid XML: {exc}") from exc
    report_root = tree.getroot()
    ns = {"cpd": "https://pmd-code.org/schema/cpd-report"}
    if report_root.tag != f"{{{ns['cpd']}}}pmd-cpd":
        raise DuplicateAuditError(
            f"pmd-cpd report has unexpected root element {report_root.tag!r}"
        )
    errors = report_root.findall("cpd:error", ns)
    if errors:
        first_error = errors[0]
        detail = first_error.get("msg") or (first_error.text or "").strip()
        raise DuplicateAuditError(
            f"pmd-cpd report contains {len(errors)} analyzer error(s): "
            f"{detail or 'no detail provided'}"
        )
    return [
        _pmd_duplicate_entry(dup, index, ns, relativize_root)
        for index, dup in enumerate(report_root.findall("cpd:duplication", ns))
    ]


def _pmd_duplicate_entry(
    duplication: ET.Element,
    index: int,
    namespace: dict[str, str],
    relativize_root: Path,
) -> dict:
    file_elements = duplication.findall("cpd:file", namespace)
    if len(file_elements) < 2:
        raise DuplicateAuditError(
            f"pmd-cpd duplication {index} contains fewer than two files"
        )
    first_path = file_elements[0].get("path")
    second_path = file_elements[1].get("path")
    if not first_path or not second_path:
        raise DuplicateAuditError(
            f"pmd-cpd duplication {index} contains an empty file path"
        )
    fragment_el = duplication.find("cpd:codefragment", namespace)
    fragment = (fragment_el.text or "") if fragment_el is not None else ""
    if not fragment:
        raise DuplicateAuditError(f"pmd-cpd duplication {index} has no code fragment")
    lines_raw = duplication.get("lines")
    try:
        lines = int(lines_raw) if lines_raw is not None else 0
    except ValueError as exc:
        raise DuplicateAuditError(
            f"pmd-cpd duplication {index} has invalid lines {lines_raw!r}"
        ) from exc
    if lines <= 0:
        raise DuplicateAuditError(
            f"pmd-cpd duplication {index} has invalid lines {lines_raw!r}"
        )
    first = _relativize(first_path, relativize_root)
    second = _relativize(second_path, relativize_root)
    return {
        "key": _stable_dup_key(first, second, fragment),
        "firstFile": first,
        "secondFile": second,
        "lines": lines,
    }


def _pmd_collect_duplicates(
    files: list[str], *, relativize_root: Path, cwd: Path = ROOT
) -> list[dict]:
    """Run PMD CPD with the XML reporter and return normalized clone entries."""
    if not files:
        return []
    with tempfile.TemporaryDirectory(prefix="llm-pmd-xml-") as tmp:
        file_list = Path(tmp) / "files.txt"
        file_list.write_text("\n".join(files) + "\n", encoding="utf-8")
        report = Path(tmp) / "cpd-report.xml"
        command = [
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
            command.extend(["--exclude", pattern])
        _run_report_command(command, cwd=cwd, tool_name="pmd-cpd")
        return _parse_pmd_report(report, relativize_root)


def run_pmd_python(
    check: bool,
    index_snapshot: bool = False,
    save_baseline: bool = False,
    *,
    root: Path = ROOT,
) -> int:
    if index_snapshot:
        with materialized_index_sources(
            DEFAULT_SOURCE_DIRS,
            frozenset({".py"}),
            root=root,
            skip=_skip_pmd_source,
            exact_paths=(PMD_CPD_BASELINE_RELATIVE.as_posix(),),
        ) as snapshot:
            files = _pmd_snapshot_file_list(root, snapshot)
            try:
                entries = _pmd_collect_duplicates(
                    files, relativize_root=snapshot, cwd=root
                )
            except DuplicateAuditError as exc:
                print(f"ERROR: pmd-cpd: {exc}", file=sys.stderr)
                return AUDIT_ERROR_EXIT_CODE
            if not save_baseline:
                return _check_against_baseline(
                    snapshot / PMD_CPD_BASELINE_RELATIVE,
                    entries,
                    tool_name="pmd-cpd",
                    root=snapshot,
                )
        if save_baseline:
            _write_baseline(root / PMD_CPD_BASELINE_RELATIVE, entries, root=root)
            return 0

    if save_baseline:
        file_list = python_file_list("duplication-python-files.txt", root=root)
        files = [
            line for line in file_list.read_text(encoding="utf-8").splitlines() if line
        ]
        try:
            entries = _pmd_collect_duplicates(files, relativize_root=root, cwd=root)
        except DuplicateAuditError as exc:
            print(f"ERROR: pmd-cpd: {exc}", file=sys.stderr)
            return AUDIT_ERROR_EXIT_CODE
        _write_baseline(root / PMD_CPD_BASELINE_RELATIVE, entries, root=root)
        return 0

    if check:
        file_list = python_file_list("duplication-python-files.txt", root=root)
        files = [
            line for line in file_list.read_text(encoding="utf-8").splitlines() if line
        ]
        try:
            entries = _pmd_collect_duplicates(files, relativize_root=root, cwd=root)
        except DuplicateAuditError as exc:
            print(f"ERROR: pmd-cpd: {exc}", file=sys.stderr)
            return AUDIT_ERROR_EXIT_CODE
        return _check_against_baseline(
            root / PMD_CPD_BASELINE_RELATIVE,
            entries,
            tool_name="pmd-cpd",
            root=root,
        )

    audit_dir = root / "tasks" / "audit"
    report = audit_dir / "duplication-pmd-python.txt"
    file_list = python_file_list("duplication-python-files.txt", root=root)
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
        str(root),
        "--format",
        "text",
        "--report-file",
        str(report),
        "--no-fail-on-violation",
    ]
    for pattern in PMD_EXCLUDES:
        cmd.extend(["--exclude", pattern])
    audit_dir.mkdir(parents=True, exist_ok=True)
    return run(cmd, cwd=root)


def run_nicad_python(
    check: bool,
    index_snapshot: bool = False,
    save_baseline: bool = False,
    *,
    root: Path = ROOT,
) -> int:
    del index_snapshot, save_baseline
    nicad = command_path("nicad")
    if not nicad:
        print(
            "nicad not found. Install NiCad/OpenTxl, or put nicad on PATH.",
            file=sys.stderr,
        )
        return 127 if check else 0

    nicad_dir = root / "tasks" / "audit" / "nicad"
    nicad_dir.mkdir(parents=True, exist_ok=True)
    cmd = [nicad, "functions", "py", str(root / "research"), "notests-report"]
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
        "--root",
        help=(
            "Repository tree to audit. Defaults to the Git worktree containing "
            "the current working directory, never the checkout that supplied "
            "the imported conductor module."
        ),
    )
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
    try:
        root = _resolve_audit_root(args.root)
    except DuplicateAuditError as exc:
        print(f"ERROR: audit-root: {exc}", file=sys.stderr)
        return AUDIT_ERROR_EXIT_CODE
    _print_audit_provenance(root, index_snapshot=args.index_snapshot)
    for name in selected:
        print(f"== {name} ==", flush=True)
        code = TOOLS[name](
            args.check,
            args.index_snapshot,
            args.save_baseline,
            root=root,
        )
        if code:
            return code
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
