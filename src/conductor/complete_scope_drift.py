"""Report complete-scope inventory drift before CI discovers it.

A mutation campaign may declare ``test_scopes[path].mode == "complete"``, which
requires the campaign to enumerate the file's pytest inventory *exactly and in
source order*. Adding, removing, renaming or reordering a test in such a file
makes the manifest unloadable, which surfaces only in the CI patch audit as
``new_unloadable_manifests`` -> ``REGRESSED`` -> exit 6, long after the push and
attributed to whoever happened to touch the file.

This reports that drift locally, against the same inventory function the audit
uses. It is deliberately report-only: the audit fails a manifest only when the
drift is *new* relative to ``reproducibility_baseline.json``, so exiting nonzero
on already-absorbed drift would block work CI would have let through. Rows are
therefore labelled NEW (would fail CI) or KNOWN (already in the baseline).

``_python_test_nodeids`` is imported rather than reimplemented: a third copy of
the inventory rule would be a third thing to keep in agreement with CI.
``mutation_scope`` is byte-pinned in runner lineage, so it is read, never edited.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from collections.abc import Iterable
from pathlib import Path
from typing import NamedTuple

from conductor.mutation_campaign_model import REPO_ROOT
from conductor.mutation_scope import CampaignError, _python_test_nodeids
from conductor.project_paths import campaigns_relative, registry_relative


class Drift(NamedTuple):
    manifest: str
    path: str
    declared: tuple[str, ...]
    discovered: tuple[str, ...]
    known: bool

    @property
    def missing(self) -> list[str]:
        """In the file, absent from the campaign -- i.e. tests were added."""
        return sorted(set(self.discovered) - set(self.declared))

    @property
    def extra(self) -> list[str]:
        """In the campaign, absent from the file -- removed or renamed."""
        return sorted(set(self.declared) - set(self.discovered))

    @property
    def reordered(self) -> bool:
        return not self.missing and not self.extra and self.declared != self.discovered


def _baseline_unloadable(repo_root: Path) -> frozenset[str]:
    baseline = (
        repo_root / campaigns_relative(repo_root) / "reproducibility_baseline.json"
    )
    if not baseline.exists():
        return frozenset()
    payload = json.loads(baseline.read_text(encoding="utf-8"))
    return frozenset(payload.get("unloadable_manifests", ()))


def _registered_manifests(repo_root: Path) -> frozenset[str]:
    """Manifest paths the audit actually loads.

    The audit is driven by ``registry.json``; unregistered manifests sitting in
    the campaign directory are never loaded, so their drift cannot fail CI.
    Scanning them anyway would cry wolf.
    """
    payload = json.loads(
        (repo_root / registry_relative(repo_root)).read_text(encoding="utf-8")
    )
    return frozenset(
        str(entry["manifest"])
        for entry in payload.get("campaigns", ())
        if isinstance(entry, dict) and entry.get("manifest")
    )


def _changed_paths(base: str) -> frozenset[str]:
    merge_base = subprocess.run(
        ["git", "merge-base", "HEAD", base],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    diff = subprocess.run(
        ["git", "diff", "--name-only", merge_base, "--"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split()
    status = subprocess.run(
        ["git", "status", "--porcelain", "--untracked-files=all"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.splitlines()
    dirty = {line[3:].strip() for line in status if line[3:].strip()}
    return frozenset(diff) | frozenset(dirty)


def scan(repo_root: Path, restrict: frozenset[str] | None) -> list[Drift]:
    known = _baseline_unloadable(repo_root)
    registered = _registered_manifests(repo_root)
    drifts: list[Drift] = []
    for manifest in sorted((repo_root / campaigns_relative(repo_root)).glob("*.json")):
        rel = str(manifest.relative_to(repo_root))
        if rel not in registered:
            continue
        try:
            payload = json.loads(manifest.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError) as exc:
            # A registered manifest that will not parse is a real defect, but it
            # is the audit's to report, not this reporter's to adjudicate.
            print(
                f"warning: cannot read registered manifest {rel}: {exc}",
                file=sys.stderr,
            )
            continue
        scopes = payload.get("test_scopes")
        if not isinstance(scopes, dict):
            continue
        for path, spec in scopes.items():
            if not isinstance(spec, dict) or spec.get("mode") != "complete":
                continue
            if spec.get("inventory") != "python_ast" or not str(path).endswith(".py"):
                continue
            if restrict is not None and path not in restrict:
                continue
            declared = tuple(spec.get("nodeids", ()))
            try:
                discovered = _python_test_nodeids(repo_root / path, path)
            except CampaignError as exc:
                # Unreadable or test-free scoped file: the campaign is already
                # broken in a way the audit reports directly. Say so; do not
                # fold it into the drift count, which means something narrower.
                print(
                    f"warning: {rel}: cannot inventory {path}: {exc}", file=sys.stderr
                )
                continue
            if declared != discovered:
                drifts.append(Drift(rel, path, declared, discovered, rel in known))
    return drifts


def _render(drifts: Iterable[Drift], scanned_all: bool) -> int:
    rows = list(drifts)
    new = [d for d in rows if not d.known]
    if not rows:
        print(
            "complete-scope drift: none" + ("" if scanned_all else " in changed files")
        )
        return 0
    for drift in rows:
        label = "KNOWN" if drift.known else "NEW"
        print(f"[{label}] {drift.path}")
        print(f"        campaign: {drift.manifest}")
        if drift.missing:
            print(
                f"        added to file, absent from campaign ({len(drift.missing)}):"
            )
            for nodeid in drift.missing:
                print(f"          + {nodeid.split('::', 1)[1]}")
        if drift.extra:
            print(
                f"        declared by campaign, absent from file ({len(drift.extra)}):"
            )
            for nodeid in drift.extra:
                print(f"          - {nodeid.split('::', 1)[1]}")
        if drift.reordered:
            print("        same tests, different source order (the check is ordered)")
    print(
        f"\n{len(rows)} drifted scope(s); {len(new)} NEW "
        "(would fail candidate-review with exit 6)."
    )
    if new:
        print("Repair by regenerating the campaign scope with the automatic engines")
        print(
            "(make mutation-plan / mutation-generate). Never hand-edit a manifest and"
        )
        print("never run make mutation-patch-audit-record -- recording absorbs the")
        print("regression into the baseline instead of fixing it.")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Report complete-scope test inventory drift (report-only)."
    )
    parser.add_argument(
        "--changed",
        action="store_true",
        help="only scope files touched relative to the merge base (default: whole corpus)",
    )
    parser.add_argument(
        "--base", default="origin/master", help="merge-base ref for --changed"
    )
    parser.add_argument(
        "--json", action="store_true", help="emit machine-readable JSON"
    )
    args = parser.parse_args(argv)

    repo_root = REPO_ROOT
    restrict = _changed_paths(args.base) if args.changed else None
    drifts = scan(repo_root, restrict)
    if args.json:
        print(
            json.dumps(
                [
                    {
                        "manifest": d.manifest,
                        "path": d.path,
                        "known": d.known,
                        "missing": d.missing,
                        "extra": d.extra,
                        "reordered": d.reordered,
                    }
                    for d in drifts
                ],
                indent=2,
            )
        )
        return 0
    return _render(drifts, restrict is None)


if __name__ == "__main__":
    sys.exit(main())
