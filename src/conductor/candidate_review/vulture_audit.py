"""Fail-closed Vulture adapter with an exact, expiring finding baseline."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from datetime import date
from pathlib import Path
from typing import Sequence

FINDING = re.compile(
    r"^(?P<path>.+?):(?P<line>\d+): (?P<message>.+ \(\d+% confidence\))$"
)
BASELINE_KEYS = {"schema_version", "generated_from_tree", "expires", "count", "entries"}
ENTRY_KEYS = {"path", "message", "owner", "justification", "expires"}


class VultureAuditError(RuntimeError):
    """Analyzer output or baseline evidence is incomplete."""


def _key(path: str, message: str) -> str:
    payload = path.encode("utf-8") + b"\0" + message.encode("utf-8")
    digest = hashlib.sha256(payload).hexdigest()[:24]
    return "-".join(digest[offset : offset + 8] for offset in range(0, 24, 8))


def _validate_generated_tree(value: object) -> None:
    if (
        not isinstance(value, list)
        or len(value) != 5
        or any(
            not isinstance(part, str) or re.fullmatch(r"[0-9a-f]{8}", part) is None
            for part in value
        )
    ):
        raise VultureAuditError(
            "Vulture baseline generated_from_tree must contain five 8-hex Git OID chunks"
        )


def _parse_output(output: str) -> dict[str, dict[str, object]]:
    findings: dict[str, dict[str, object]] = {}
    for line in output.splitlines():
        if not line.strip():
            continue
        match = FINDING.fullmatch(line.strip())
        if match is None:
            raise VultureAuditError(f"unrecognized Vulture output: {line}")
        path = match.group("path")
        message = match.group("message")
        findings[_key(path, message)] = {
            "path": path,
            "line": int(match.group("line")),
            "message": message,
        }
    return findings


def _iso_date(value: object, *, field: str) -> date:
    if not isinstance(value, str):
        raise VultureAuditError(f"{field} must be an ISO date")
    try:
        return date.fromisoformat(value)
    except ValueError as exc:
        raise VultureAuditError(f"{field} must be an ISO date") from exc


def _load_baseline(path: Path) -> dict[str, dict[str, object]]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise VultureAuditError(f"Vulture baseline is unreadable: {exc}") from exc
    if not isinstance(payload, dict) or set(payload) != BASELINE_KEYS:
        raise VultureAuditError("Vulture baseline has an invalid top-level schema")
    if payload.get("schema_version") != 1:
        raise VultureAuditError("Vulture baseline schema_version must be 1")
    _validate_generated_tree(payload.get("generated_from_tree"))
    if _iso_date(payload.get("expires"), field="baseline.expires") < date.today():
        raise VultureAuditError("Vulture baseline is expired")
    entries = payload.get("entries")
    if not isinstance(entries, dict) or payload.get("count") != len(entries):
        raise VultureAuditError("Vulture baseline count does not match entries")
    validated: dict[str, dict[str, object]] = {}
    for key, entry in entries.items():
        validated[key] = _validate_entry(key, entry)
    return validated


def _validate_entry(key: str, entry: object) -> dict[str, object]:
    if not isinstance(entry, dict) or set(entry) != ENTRY_KEYS:
        raise VultureAuditError(f"Vulture baseline entry {key!r} has invalid fields")
    path = entry.get("path")
    message = entry.get("message")
    owner = entry.get("owner")
    justification = entry.get("justification")
    if not isinstance(path, str) or not path:
        raise VultureAuditError(f"Vulture baseline entry {key!r} has invalid text")
    if not isinstance(message, str) or not message:
        raise VultureAuditError(f"Vulture baseline entry {key!r} has invalid text")
    if not isinstance(owner, str) or not owner:
        raise VultureAuditError(f"Vulture baseline entry {key!r} has invalid text")
    if not isinstance(justification, str) or len(justification) < 20:
        raise VultureAuditError(f"Vulture baseline entry {key!r} lacks justification")
    if _iso_date(entry.get("expires"), field=f"entries.{key}.expires") < date.today():
        raise VultureAuditError(f"Vulture baseline entry {key!r} is expired")
    expected = _key(path, message)
    if key != expected:
        raise VultureAuditError(
            f"Vulture baseline entry key mismatch: expected {expected}, found {key}"
        )
    return dict(entry)


def run_audit(baseline_path: Path, paths: Sequence[str]) -> int:
    baseline = _load_baseline(baseline_path)
    command = [
        "vulture",
        *paths,
        "research/tools/vulture_whitelist.py",
        "--min-confidence",
        "80",
        "--exclude",
        "*/.venv/*,*/node_modules/*,*/__pycache__/*,*/.run/*,*/tests/*,*/migrations/*",
    ]
    completed = subprocess.run(
        command,
        capture_output=True,
        text=True,
        errors="replace",
        check=False,
    )
    if completed.returncode not in {0, 3}:
        detail = (completed.stderr or completed.stdout).strip()
        raise VultureAuditError(
            f"Vulture exited {completed.returncode}; findings are untrusted: {detail}"
        )
    findings = _parse_output(completed.stdout)
    new = sorted(set(findings) - set(baseline))
    resolved = sorted(set(baseline) - set(findings))
    print(
        f"Vulture findings={len(findings)} baseline={len(baseline)} "
        f"new={len(new)} resolved={len(resolved)}"
    )
    for key in new:
        finding = findings[key]
        print(f"NEW {finding['path']}:{finding['line']}: {finding['message']}")
    if resolved:
        raise VultureAuditError(
            "Vulture baseline contains resolved findings and must be narrowed: "
            + ", ".join(resolved)
        )
    return 1 if new else 0


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, type=Path)
    parser.add_argument("paths", nargs="+")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        return run_audit(args.baseline, args.paths)
    except VultureAuditError as exc:
        print(f"Vulture audit incomplete: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
