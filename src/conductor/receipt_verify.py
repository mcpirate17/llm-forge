"""Authenticate a mutation-campaign receipt against a TARGET git tree.

``python -m conductor.receipt_verify --receipt <file> --tree <committish>``

Helm landing serialization runs the four-part receipt validity test by hand
today: "the manifest is clean at the tip" and "a receipt exists at some ref"
are both satisfiable while the receipt still cannot validate. This CLI runs
the whole test against one named target tree, reading every byte via
``git cat-file`` -- never the working tree, never paths derived from
``__file__`` -- and prints the resolved repo root and tree oid up front so a
wrong-tree measurement is visible, not silent:

1. the receipt's ``manifest`` path resolves to a blob in the target tree;
2. the receipt's ``manifest_sha256`` equals sha256 of that blob's bytes;
3. every source pin in the manifest's ``source_sha256`` hashes to its pinned
   value against the target tree's blob bytes -- a missing path or a hash
   mismatch is a hard FAIL naming the path and both hashes;
4. inventory-digest reproduction: the receipt's recorded ``source_sha256``
   map, rendered as ``sha256sum`` lines (``<sha256>  <path>``, two spaces,
   one newline-terminated line per pin, ``sort -k1,1`` order) and hashed,
   must equal the same rendering rebuilt from the target tree's actual blob
   hashes over the manifest's pin paths.

Receipt schema v3 records its pin inventory as the ``source_sha256`` map, not
as a standalone digest field, so part 4 derives the "recorded" digest from
that map and the "reproduced" digest from the tree. It is the only part that
catches a receipt whose own recorded map silently diverges from the manifest
it names (parts 1-3 never read the receipt's map).

Exit codes: 0 all four parts PASS; 1 verification FAIL; 4 REFUSED (malformed
receipt or manifest, or an unusable repository -- nothing was measured).
"""

from __future__ import annotations

import argparse
import json
import sys
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from conductor import mutation_testing_support as _support
from conductor._native import (
    load_tree_receipt_native,
    receipt_inventory_digest_native,
    receipt_manifest_pins_native,
    receipt_sha256_native,
    verify_tree_receipt_native,
)
from conductor.mutation_scope import CampaignError, _require_string

EXIT_PASS = 0
EXIT_FAIL = 1
EXIT_REFUSED = 4

PART1 = "manifest_blob_in_tree"
PART2 = "manifest_sha256"
PART3 = "source_pins"
PART4 = "inventory_digest"


@dataclass(frozen=True, slots=True)
class LoadedReceipt:
    """The authenticated surface of a receipt: identity fields plus pin map."""

    path: Path
    schema_version: str | None
    manifest: str
    manifest_sha256: str
    source_sha256: dict[str, str]


def load_receipt(path: Path) -> LoadedReceipt:
    """Parse the receipt file; structural invalidity is REFUSED, never a FAIL."""
    try:
        receipt = json.loads(load_tree_receipt_native(str(path)))
    except ValueError as exc:
        raise CampaignError(str(exc)) from exc
    return LoadedReceipt(
        path=path,
        schema_version=receipt["schema_version"],
        manifest=receipt["manifest"],
        manifest_sha256=receipt["manifest_sha256"],
        source_sha256=receipt["source_sha256"],
    )


def resolve_repo_root(repo: Path | None) -> Path:
    """Repository whose object store serves the target tree (cwd discovery)."""
    start = Path.cwd() if repo is None else repo
    if not start.is_dir():
        raise CampaignError(f"--repo is not a directory: {start}")
    result = _support._git_bytes(start, ["rev-parse", "--show-toplevel"])
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise CampaignError(f"cannot resolve a git repository from {start}: {detail}")
    return Path(result.stdout.decode("utf-8").strip())


def resolve_tree_oid(repo_root: Path, treeish: str) -> str:
    """Peel a committish or tree oid to the tree oid actually measured."""
    label = _require_string(treeish, "--tree").strip()
    result = _support._git_bytes(
        repo_root,
        ["rev-parse", "--verify", "--end-of-options", f"{label}^{{tree}}"],
    )
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise CampaignError(f"cannot resolve {label!r} to a tree: {detail}")
    return result.stdout.decode("utf-8").strip()


def _tree_blob(repo_root: Path, tree_oid: str, path: str) -> tuple[bytes | None, str]:
    """Blob bytes at ``<tree>:<path>``, or ``(None, reason)`` when unreadable."""
    result = _support._git_bytes(repo_root, ["cat-file", "blob", f"{tree_oid}:{path}"])
    if result.returncode != 0:
        reason = (
            result.stderr.decode("utf-8", "replace").strip()
            or f"git cat-file exited {result.returncode}"
        )
        return None, reason
    return result.stdout, ""


def inventory_digest(pins: Mapping[str, str]) -> str:
    """Return the native sha256sum-format digest of a pin inventory."""
    return receipt_inventory_digest_native(list(pins.items()))


def verify_receipt(
    receipt_path: Path, repo_root: Path, tree_oid: str
) -> dict[str, object]:
    """Run the four-part test; every byte comes from the target tree."""
    receipt = load_receipt(receipt_path)
    blob, reason = _tree_blob(repo_root, tree_oid, receipt.manifest)
    source_rows: list[tuple[str, str | None, str]] = []
    if blob is not None:
        try:
            manifest_pins = json.loads(
                receipt_manifest_pins_native(blob, receipt.manifest)
            )
        except ValueError as exc:
            raise CampaignError(str(exc)) from exc
        for path in sorted(manifest_pins):
            source_blob, source_reason = _tree_blob(repo_root, tree_oid, path)
            source_sha256 = (
                receipt_sha256_native(source_blob) if source_blob is not None else None
            )
            source_rows.append((path, source_sha256, source_reason))
    receipt_payload = json.dumps(
        {
            "path": str(receipt.path),
            "schema_version": receipt.schema_version,
            "manifest": receipt.manifest,
            "manifest_sha256": receipt.manifest_sha256,
            "source_sha256": receipt.source_sha256,
        }
    )
    try:
        return json.loads(
            verify_tree_receipt_native(
                receipt_payload,
                str(repo_root),
                tree_oid,
                blob,
                reason,
                source_rows,
            )
        )
    except ValueError as exc:
        raise CampaignError(str(exc)) from exc


def _print_human(verdict: dict[str, Any]) -> None:
    if verdict["status"] == "PASS":
        part4 = verdict["checks"][PART4]
        print(
            f"PASS receipt={verdict['receipt']} manifest={verdict['manifest']} "
            f"tree={verdict['tree_oid']} pins={part4['pins']} "
            f"inventory_digest={part4['digest']}"
        )
        return
    for line in verdict["failures"]:
        print(f"FAIL {line}")
    print(
        f"FAIL {len(verdict['failures'])} failure(s) against tree {verdict['tree_oid']}"
    )


def _normalize_dashes(argv: list[str]) -> list[str]:
    """Terminal smart-dash repair: a pasted ``—flag`` means ``--flag``."""
    return ["--" + arg[1:] if arg[:1] in {"—", "–"} else arg for arg in argv]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m conductor.receipt_verify",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--receipt",
        type=Path,
        required=True,
        help="receipt JSON file (the artifact being authenticated)",
    )
    parser.add_argument(
        "--tree", required=True, help="committish or tree oid naming the TARGET tree"
    )
    parser.add_argument(
        "--repo",
        type=Path,
        default=None,
        help=(
            "repository to read blobs from (default: discovered via "
            "`git rev-parse --show-toplevel` from the current directory)"
        ),
    )
    parser.add_argument(
        "--json",
        action="store_true",
        dest="json_output",
        help="emit a machine-readable verdict object on stdout",
    )
    args = parser.parse_args(
        _normalize_dashes(list(sys.argv[1:] if argv is None else argv))
    )
    try:
        repo_root = resolve_repo_root(args.repo)
        tree_oid = resolve_tree_oid(repo_root, args.tree)
        # Print what is actually measured BEFORE verifying; a silent wrong-tree
        # measurement is a known defect class. In --json mode these go to
        # stderr so stdout stays a single parseable object.
        resolved = sys.stderr if args.json_output else sys.stdout
        print(f"repo_root={repo_root}", file=resolved)
        print(f"tree_oid={tree_oid}", file=resolved)
        verdict = verify_receipt(args.receipt, repo_root, tree_oid)
    except CampaignError as exc:
        if args.json_output:
            print(json.dumps({"status": "REFUSED", "error": str(exc)}, indent=2))
        else:
            print(f"REFUSED: {exc}", file=sys.stderr)
        return EXIT_REFUSED
    if args.json_output:
        print(json.dumps(verdict, indent=2, sort_keys=True))
    else:
        _print_human(verdict)
    return EXIT_PASS if verdict["status"] == "PASS" else EXIT_FAIL


if __name__ == "__main__":
    raise SystemExit(main())
