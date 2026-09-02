#!/usr/bin/env python3
"""Bash impact analyzer for the PreToolUse/Bash hook.

Reads the proposed command from stdin (Claude Code hook payload), classifies it,
and emits a JSON decision. For borderline-destructive commands, computes a quick
impact summary (file count, size, row count) and surfaces it so the agent must
echo the impact back to the user before proceeding.

Tiers:
  hard_deny   — never allowed regardless of context (rm -rf /, push --force, etc.)
                (handled by pre-bash.sh, kept here only for symmetry)
  soft_warn   — allowed but the additionalContext force-surfaces the impact
                to the user; the agent must summarize before continuing.
  allow       — fast path for everything else.

Designed so the user can ask "clean up failed experiments" without being
blocked, while still getting an impact summary every time something
destructive is about to run.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path
from typing import List, Optional, Tuple


def _read_command() -> str:
    try:
        payload = json.load(sys.stdin)
    except json.JSONDecodeError:
        return ""
    return str(payload.get("tool_input", {}).get("command") or "")


def _emit(decision: str, reason: str = "", additional: str = "") -> None:
    out = {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": decision,
        }
    }
    if reason:
        out["hookSpecificOutput"]["permissionDecisionReason"] = reason
    if additional:
        out["hookSpecificOutput"]["additionalContext"] = additional
    print(json.dumps(out))
    sys.exit(0)


def _du_summary(path: str) -> Tuple[int, str]:
    """Return (file_count, human_size). Best-effort, never raises."""
    p = Path(path)
    if not p.exists():
        return 0, "0B"
    try:
        if p.is_file():
            return 1, _human_size(p.stat().st_size)
        files = sum(1 for _ in p.rglob("*") if _.is_file())
        size = (
            subprocess.run(
                ["du", "-sh", path], capture_output=True, text=True, timeout=5
            )
            .stdout.split("\t", 1)[0]
            .strip()
        )
        return files, size or "?"
    except Exception:
        return -1, "?"


def _human_size(n: int) -> str:
    for unit in ("B", "K", "M", "G"):
        if n < 1024:
            return f"{n:.0f}{unit}"
        n //= 1024
    return f"{n}T"


_SIZE_UNITS = {"B": 1, "K": 1024, "M": 1024**2, "G": 1024**3, "T": 1024**4}


def _to_bytes(human: str) -> int:
    """Inverse of _du_summary's human size. Best-effort, returns 0 on parse fail."""
    if not human:
        return 0
    m = re.match(r"^([\d.]+)\s*([BKMGT])", human.strip(), re.IGNORECASE)
    if not m:
        return 0
    try:
        return int(float(m.group(1)) * _SIZE_UNITS[m.group(2).upper()])
    except (KeyError, ValueError):
        return 0


def _sql_row_count(db_path: str, table: str, where: Optional[str]) -> Optional[int]:
    if not Path(db_path).exists():
        return None
    sql = f"SELECT COUNT(*) FROM {table}"
    if where:
        sql += f" WHERE {where}"
    try:
        proc = subprocess.run(
            ["sqlite3", db_path, sql], capture_output=True, text=True, timeout=10
        )
        if proc.returncode != 0:
            return None
        return int(proc.stdout.strip())
    except Exception:
        return None


# ── Pattern bank ────────────────────────────────────────────────────────────

_RM_RF_ARGS = re.compile(r"\brm\s+-[rRf]+\w*\s+([^;|&\n]+)")
_FIND_DELETE = re.compile(r"\bfind\s+(\S+).*-delete\b")
_GIT_CLEAN = re.compile(r"\bgit\s+clean\s+-[fdxX]+\s*([^\s;|&]*)")
_SQLITE_MUTATE = re.compile(
    r"\bsqlite3\s+(\S+)\s+[\"']?\s*(DELETE|DROP|UPDATE|TRUNCATE)\b([^\"']*)",
    re.IGNORECASE,
)
_SQLITE_DELETE_FROM = re.compile(
    r"DELETE\s+FROM\s+(\w+)\s*(?:WHERE\s+(.+?))?\s*[\"';]?$", re.IGNORECASE | re.DOTALL
)


def _classify(cmd: str) -> Tuple[str, str]:
    """Return (tier, impact_text). tier in {soft_warn, allow}."""
    impacts: List[str] = []

    rm_total_files = 0
    rm_total_bytes = 0
    rm_lines: List[str] = []
    for m in _RM_RF_ARGS.finditer(cmd):
        args_blob = m.group(1)
        # Strip trailing redirect/and/or operators that may have leaked in
        args_blob = re.split(r"\s(?:&&|\|\||>>?|<<?)\s", args_blob, maxsplit=1)[0]
        for token in args_blob.split():
            target = token.strip("\"'")
            if not target or target.startswith("-"):
                continue
            files, size = _du_summary(target)
            if files <= 0:
                continue
            rm_total_files += files
            rm_total_bytes += _to_bytes(size)
            rm_lines.append(f"rm -rf {target}: {files} files, {size}")
    if rm_lines:
        if len(rm_lines) > 1:
            rm_lines.append(
                f"TOTAL: {rm_total_files} files, {_human_size(rm_total_bytes)}"
            )
        impacts.extend(rm_lines)

    for m in _FIND_DELETE.finditer(cmd):
        target = m.group(1).strip("\"'")
        files, size = _du_summary(target)
        if files > 0:
            impacts.append(f"find -delete in {target}: up to {files} files, {size}")

    for m in _GIT_CLEAN.finditer(cmd):
        target = m.group(1).strip("\"'") or "."
        files, size = _du_summary(target)
        if files > 0:
            impacts.append(f"git clean in {target}: scope ~{files} files, {size}")

    for m in _SQLITE_MUTATE.finditer(cmd):
        db_path = m.group(1).strip("\"'")
        verb = m.group(2).upper()
        rest = m.group(3) or ""
        row_msg = ""
        if verb == "DELETE":
            from_match = _SQLITE_DELETE_FROM.search(verb + rest)
            if from_match:
                table = from_match.group(1)
                where = (from_match.group(2) or "").strip().rstrip(";\"' ")
                count = _sql_row_count(db_path, table, where or None)
                if count is not None:
                    row_msg = f" → {count} rows"
        impacts.append(f"sqlite3 {db_path} {verb}{rest}{row_msg}")

    if impacts:
        return "soft_warn", "\n  • " + "\n  • ".join(impacts)
    return "allow", ""


def main() -> None:
    cmd = _read_command()
    if not cmd:
        _emit("allow")

    # Skip pure inspection commands (du, find without -delete, ls, sqlite SELECT, etc.)
    # No work to do — defer to pre-bash.sh hard-deny rules.
    tier, impacts = _classify(cmd)
    if tier == "allow":
        _emit("allow")

    # soft_warn: allow but force the impact to surface in the conversation so
    # the agent must reflect it back to the user. The user (or the agent) can
    # then proceed knowingly. This is the "smart impact check" the user asked
    # for: never silently destructive, always with the receipts.
    additional = (
        "DESTRUCTIVE COMMAND IMPACT (must summarize to user before/after running):"
        + impacts
        + "\nIf the user did not explicitly authorize this scope, stop and ask. "
        "Do not run cleanups for impact >100 files / >100 MB / >1000 rows "
        "without explicit confirmation in the current turn."
    )
    _emit("allow", additional=additional)


if __name__ == "__main__":
    main()
