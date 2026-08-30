"""Fail-closed Vulture adapter with an exact, expiring finding baseline."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import sys
from datetime import date
from pathlib import Path
from typing import Sequence

from conductor.changed_files_cli import (
    add_changed_files_arguments,
    resolve_changed_files,
)

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


def run_audit(
    baseline_path: Path,
    paths: Sequence[str],
    *,
    changed_files: frozenset[str] | None = None,
) -> int:
    baseline = _load_baseline(baseline_path)
    executable = shutil.which("vulture")
    if executable is None:
        raise VultureAuditError(
            "vulture is not installed or not on PATH, so dead-code findings cannot "
            "be produced and the baseline cannot be trusted. It is a declared "
            "dependency (pyproject.toml); install it with "
            "`uv pip install --python <venv> vulture`."
        )
    command = [
        executable,
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

    if changed_files is None:
        caused, inherited = new, []
    else:
        caused = [key for key in new if findings[key]["path"] in changed_files]
        inherited = [key for key in new if key not in set(caused)]

    for key in caused:
        finding = findings[key]
        print(f"NEW {finding['path']}:{finding['line']}: {finding['message']}")
    if inherited:
        print(
            f"{len(inherited)} new finding(s) are INHERITED (pre-existing debt "
            "outside this candidate's changed files; NOT blocking this "
            "candidate):"
        )
        for key in inherited:
            finding = findings[key]
            print(
                f"  INHERITED {finding['path']}:{finding['line']}: {finding['message']}"
            )

    if resolved:
        raise VultureAuditError(
            "Vulture baseline contains resolved findings and must be narrowed: "
            + ", ".join(resolved)
        )
    return 1 if caused else 0


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, type=Path)
    add_changed_files_arguments(parser)
    parser.add_argument("paths", nargs="+")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    changed_files = resolve_changed_files(args)
    try:
        return run_audit(args.baseline, args.paths, changed_files=changed_files)
    except VultureAuditError as exc:
        print(f"Vulture audit incomplete: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
