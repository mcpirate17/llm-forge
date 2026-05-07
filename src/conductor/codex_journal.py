#!/usr/bin/env python3
from __future__ import annotations

import argparse
import fnmatch
import re
import subprocess
from datetime import datetime
from pathlib import Path, PurePosixPath


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_OUT_DIR = ROOT / "tasks" / "codex_journal"
SECRETISH_RE = re.compile(
    r"(?i)\b(api[_-]?key|token|password|passwd|secret|credential)\b\s*[:=]\s*\S+"
)
SECRETISH_PATH_RE = re.compile(
    r"(?i)(api[_-]?key|keys?|token|password|passwd|secret|credential)"
)
PROTECTED_PATTERNS = (
    "*.db",
    "*.db-wal",
    "*.db-shm",
    "lab_notebook*",
    "*/lab_notebook*",
    "db_backups/*",
    "*/db_backups/*",
    "research/runtime_events/*.ndjson",
    "research/scientist/runtime_events/*.ndjson",
    "research/perf_artifacts/*",
    "*.parquet",
    "*.feather",
    "research/runtime/champion_*.json",
    "research/runtime/*_status.json",
    "research/runtime/*_inventory.json",
    ".testmondata*",
    "*/.testmondata*",
)


def _run_git(args: list[str]) -> str:
    proc = subprocess.run(
        ["git", *args],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        return ""
    return proc.stdout.strip()


def _is_protected(path: str) -> bool:
    posix = PurePosixPath(path).as_posix()
    return any(fnmatch.fnmatchcase(posix, pattern) for pattern in PROTECTED_PATTERNS)


def _redact(text: str) -> str:
    return SECRETISH_RE.sub(lambda m: f"{m.group(1)}=<redacted>", text)


def _status_lines() -> list[str]:
    lines = []
    for line in _run_git(["status", "--short"]).splitlines():
        path = line[3:] if len(line) > 3 else line
        if not _is_protected(path) and not SECRETISH_PATH_RE.search(path):
            lines.append(line)
    return lines


def _bullet_lines(items: list[str], empty: str) -> str:
    if not items:
        return f"- {empty}"
    return "\n".join(f"- `{_redact(item)}`" for item in items)


def build_entry(note: str, tests: list[str]) -> str:
    branch = _run_git(["branch", "--show-current"]) or "(unknown)"
    head = _run_git(["rev-parse", "--short", "HEAD"]) or "(unknown)"
    status = _status_lines()
    timestamp = datetime.now().astimezone().isoformat(timespec="seconds")
    safe_note = _redact(note.strip()) if note.strip() else "No manual note provided."
    safe_tests = [_redact(test.strip()) for test in tests if test.strip()]

    return "\n".join(
        [
            f"## {timestamp}",
            "",
            f"- Branch: `{branch}`",
            f"- HEAD: `{head}`",
            f"- Note: {safe_note}",
            "",
            "### Changed Files",
            _bullet_lines(status, "No non-protected changes detected."),
            "",
            "### Tests",
            _bullet_lines(safe_tests, "No test commands recorded."),
            "",
        ]
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Append a small Obsidian-compatible Codex work journal entry."
    )
    parser.add_argument("--note", default="", help="Short session note to record.")
    parser.add_argument(
        "--test",
        action="append",
        default=[],
        help="Test/check command to record. May be passed more than once.",
    )
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=DEFAULT_OUT_DIR,
        help="Directory for daily Markdown journal files.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    out_dir = args.out_dir if args.out_dir.is_absolute() else ROOT / args.out_dir
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / f"{datetime.now().date().isoformat()}.md"
    entry = build_entry(args.note, args.test)
    with out_path.open("a", encoding="utf-8") as fh:
        fh.write(entry)
    print(out_path.relative_to(ROOT))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
