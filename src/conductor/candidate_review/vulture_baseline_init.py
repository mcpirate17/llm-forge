"""Generate the Vulture justified-findings baseline from real tool output.

``vulture_audit.py`` has no ``--save-baseline`` mode because its baseline is not
a plain measurement: each entry is a human justification for keeping a specific
false positive, and inventing those justifications here would be exactly the
hand-editing the mutation-evidence policy forbids for baselines. What this
script *can* measure honestly is the two structural fields Vulture itself
cannot: the tree the baseline was cut from (``generated_from_tree``), and the
empty allowlist a project starts from before anyone has justified an exception
(``entries: {}``). It runs the exact Vulture invocation ``run_audit`` uses so an
operator can see, on stderr, whatever findings exist right now -- current
findings become non-blocking "inherited" debt under ``run_audit`` until someone
adds a justified entry for them or fixes the underlying code; nothing here
approves them.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
from collections.abc import Sequence
from pathlib import Path

from conductor.candidate_review.vulture_audit import _parse_output, whitelist_args


class VultureBaselineInitError(RuntimeError):
    """Vulture could not be run, or the current tree has no committed HEAD."""


def git_tree_chunks(root: Path) -> list[str]:
    """Split ``git rev-parse HEAD`` into the five 8-hex chunks the schema wants."""
    completed = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    oid = completed.stdout.strip()
    if completed.returncode != 0 or len(oid) != 40:
        detail = (completed.stderr or completed.stdout).strip()
        raise VultureBaselineInitError(f"git rev-parse HEAD failed: {detail or oid!r}")
    return [oid[offset : offset + 8] for offset in range(0, 40, 8)]


def run_vulture_findings(
    root: Path, paths: Sequence[str]
) -> dict[str, dict[str, object]]:
    """Run the same Vulture invocation ``run_audit`` uses and parse its findings."""
    executable = shutil.which("vulture")
    if executable is None:
        raise VultureBaselineInitError(
            "vulture is not installed or not on PATH; install it with "
            "`uv sync --extra test` before generating this baseline."
        )
    command = [
        executable,
        *paths,
        *whitelist_args(root),
        "--min-confidence",
        "80",
        "--exclude",
        "*/.venv/*,*/node_modules/*,*/__pycache__/*,*/.run/*,*/tests/*,*/migrations/*",
    ]
    completed = subprocess.run(
        command,
        cwd=root,
        capture_output=True,
        text=True,
        errors="replace",
        check=False,
    )
    if completed.returncode not in {0, 3}:
        detail = (completed.stderr or completed.stdout).strip()
        raise VultureBaselineInitError(
            f"vulture exited {completed.returncode}: {detail}"
        )
    return _parse_output(completed.stdout)


def build_baseline(
    root: Path, paths: Sequence[str], expires: str
) -> tuple[dict[str, object], dict[str, dict[str, object]]]:
    findings = run_vulture_findings(root, paths)
    baseline = {
        "schema_version": 1,
        "generated_from_tree": git_tree_chunks(root),
        "expires": expires,
        "count": 0,
        "entries": {},
    }
    return baseline, findings


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, type=Path)
    parser.add_argument(
        "--expires", required=True, help="ISO date the emitted baseline expires on."
    )
    parser.add_argument("paths", nargs="+")
    args = parser.parse_args(argv)

    try:
        baseline, findings = build_baseline(Path.cwd(), args.paths, args.expires)
    except VultureBaselineInitError as exc:
        print(f"vulture-baseline-init: {exc}", file=sys.stderr)
        return 2

    if findings:
        print(
            f"NOTE: {len(findings)} current Vulture finding(s) are not in this "
            "baseline's (empty) allowlist. They will surface as inherited debt, "
            "or as blocking if they land in a candidate's changed files, until "
            "justified in the baseline by hand or fixed outright:",
            file=sys.stderr,
        )
        for finding in sorted(findings.values(), key=lambda f: (f["path"], f["line"])):
            print(
                f"  {finding['path']}:{finding['line']}: {finding['message']}",
                file=sys.stderr,
            )

    args.baseline.write_text(
        json.dumps(baseline, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(f"Wrote {args.baseline} (entries=0, current findings={len(findings)})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
