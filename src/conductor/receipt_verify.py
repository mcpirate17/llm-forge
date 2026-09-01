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
import hashlib
import json
import sys
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from conductor import mutation_testing_support as _support
from conductor.mutation_scope import (
    CampaignError,
    _require_mapping,
    _require_string,
    _safe_relative_path,
)
from conductor.mutation_testing import SHA256_RE

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


def _require_sha256(value: object, label: str) -> str:
    text = _require_string(value, label)
    if not SHA256_RE.fullmatch(text):
        raise CampaignError(f"{label} must be a lowercase SHA-256 digest")
    return text


def _load_pin_map(value: object, label: str) -> dict[str, str]:
    """Validate a non-empty ``path -> sha256`` mapping with safe relative keys."""
    mapping = _require_mapping(value, label)
    if not mapping:
        raise CampaignError(f"{label} must not be empty")
    pins: dict[str, str] = {}
    for raw_path, raw_digest in mapping.items():
        pin_path = _safe_relative_path(raw_path, f"{label} key")
        pins[pin_path] = _require_sha256(raw_digest, f"{label}[{pin_path}]")
    return pins


def load_receipt(path: Path) -> LoadedReceipt:
    """Parse the receipt file; structural invalidity is REFUSED, never a FAIL."""
    if not path.is_file():
        raise CampaignError(f"receipt file is missing: {path}")
    raw = path.read_bytes()
    if not raw:
        # Existence is not content: a 0-byte stub passes is_file().
        raise CampaignError(f"receipt file is empty: {path}")
    try:
        payload = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as exc:
        raise CampaignError(f"receipt is not valid JSON: {path}: {exc}") from exc
    receipt = _require_mapping(payload, f"receipt {path}")
    schema = receipt.get("schema_version")
    return LoadedReceipt(
        path=path,
        schema_version=schema if isinstance(schema, str) else None,
        manifest=_safe_relative_path(receipt.get("manifest"), "receipt.manifest"),
        manifest_sha256=_require_sha256(
            receipt.get("manifest_sha256"), "receipt.manifest_sha256"
        ),
        source_sha256=_load_pin_map(
            receipt.get("source_sha256"), "receipt.source_sha256"
        ),
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


def _manifest_pins(blob: bytes, manifest_rel: str) -> dict[str, str]:
    """Source pins from the manifest blob as stored in the target tree."""
    try:
        payload = json.loads(blob.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as exc:
        raise CampaignError(
            f"manifest blob is not valid JSON: {manifest_rel}: {exc}"
        ) from exc
    manifest = _require_mapping(payload, f"manifest {manifest_rel}")
    return _load_pin_map(
        manifest.get("source_sha256"), f"manifest {manifest_rel} source_sha256"
    )


def inventory_digest(pins: Mapping[str, str]) -> str:
    """Digest of a pin inventory in ``sha256sum`` line format.

    One ``<sha256>  <path>\\n`` line per pin (two spaces, exactly the bytes
    ``sha256sum`` emits), ordered with ``sort -k1,1`` semantics -- primary key
    the hash field, ties broken by the whole line -- then sha256 over the
    concatenated lines.
    """
    lines = [f"{digest}  {path}\n" for path, digest in pins.items()]
    lines.sort(key=lambda line: (line.split("  ", 1)[0], line))
    return hashlib.sha256("".join(lines).encode("utf-8")).hexdigest()


def _pin_divergence(
    recorded: Mapping[str, str], reproduced: Mapping[str, str]
) -> dict[str, list[str]]:
    """Name exactly how the receipt's recorded inventory differs from the tree's."""
    shared = set(recorded) & set(reproduced)
    return {
        "only_in_receipt": sorted(set(recorded) - set(reproduced)),
        "only_in_tree_inventory": sorted(set(reproduced) - set(recorded)),
        "hash_mismatch": sorted(p for p in shared if recorded[p] != reproduced[p]),
    }


def _verdict(
    receipt: LoadedReceipt,
    repo_root: Path,
    tree_oid: str,
    checks: dict[str, dict[str, Any]],
    failures: list[str],
) -> dict[str, Any]:
    passed = not failures and all(c.get("status") == "PASS" for c in checks.values())
    return {
        "status": "PASS" if passed else "FAIL",
        "repo_root": str(repo_root),
        "tree_oid": tree_oid,
        "receipt": str(receipt.path),
        "receipt_schema_version": receipt.schema_version,
        "manifest": receipt.manifest,
        "checks": checks,
        "failures": failures,
    }


def verify_receipt(
    receipt_path: Path, repo_root: Path, tree_oid: str
) -> dict[str, Any]:
    """Run the four-part test; every byte comes from the target tree."""
    receipt = load_receipt(receipt_path)
    checks: dict[str, dict[str, Any]] = {}
    failures: list[str] = []

    # Part 1: the manifest path must be a blob in the target tree.
    blob, reason = _tree_blob(repo_root, tree_oid, receipt.manifest)
    if blob is None:
        checks[PART1] = {"status": "FAIL", "detail": reason}
        failures.append(
            f"part 1 ({PART1}): {receipt.manifest} is not a blob in "
            f"tree {tree_oid}: {reason}"
        )
        blocked = {"status": "BLOCKED", "detail": "manifest blob unavailable"}
        for name in (PART2, PART3, PART4):
            checks[name] = dict(blocked)
        return _verdict(receipt, repo_root, tree_oid, checks, failures)
    checks[PART1] = {"status": "PASS", "size_bytes": len(blob)}

    # Part 2: recorded manifest hash against the blob's actual bytes.
    actual_manifest_sha = hashlib.sha256(blob).hexdigest()
    if actual_manifest_sha == receipt.manifest_sha256:
        checks[PART2] = {"status": "PASS", "sha256": actual_manifest_sha}
    else:
        checks[PART2] = {
            "status": "FAIL",
            "recorded": receipt.manifest_sha256,
            "actual": actual_manifest_sha,
        }
        failures.append(
            f"part 2 ({PART2}): recorded {receipt.manifest_sha256} != "
            f"actual {actual_manifest_sha}"
        )

    # Parts 3 and 4 read the manifest's pins and the receipt's recorded map.
    manifest_pins = _manifest_pins(blob, receipt.manifest)
    _check_pins(receipt, repo_root, tree_oid, manifest_pins, checks, failures)
    return _verdict(receipt, repo_root, tree_oid, checks, failures)


def _check_pins(
    receipt: LoadedReceipt,
    repo_root: Path,
    tree_oid: str,
    manifest_pins: Mapping[str, str],
    checks: dict[str, dict[str, Any]],
    failures: list[str],
) -> None:
    """Parts 3 and 4: pin-by-pin hashes, then inventory-digest reproduction."""
    # Part 3: every manifest pin against the tree's blob bytes.
    pin_failures: list[str] = []
    actual_hashes: dict[str, str] = {}
    unreadable = 0
    for path in sorted(manifest_pins):
        pinned = manifest_pins[path]
        source_blob, source_reason = _tree_blob(repo_root, tree_oid, path)
        if source_blob is None:
            unreadable += 1
            pin_failures.append(
                f"{path}: pinned {pinned}, but the path is not a blob in the "
                f"target tree ({source_reason})"
            )
            continue
        actual = hashlib.sha256(source_blob).hexdigest()
        actual_hashes[path] = actual
        if actual != pinned:
            pin_failures.append(f"{path}: pinned {pinned} != actual {actual}")
    if pin_failures:
        checks[PART3] = {
            "status": "FAIL",
            "checked": len(manifest_pins),
            "failures": pin_failures,
        }
        failures.extend(f"part 3 ({PART3}): {line}" for line in pin_failures)
    else:
        checks[PART3] = {"status": "PASS", "checked": len(manifest_pins)}

    # Part 4: recorded inventory digest reproduced from the tree's own hashes.
    recorded_digest = inventory_digest(receipt.source_sha256)
    if unreadable:
        checks[PART4] = {
            "status": "BLOCKED",
            "recorded_digest": recorded_digest,
            "detail": f"{unreadable} pinned path(s) unreadable in the target tree",
        }
        failures.append(
            f"part 4 ({PART4}): blocked -- {unreadable} pinned path(s) "
            "unreadable in the target tree; reproduction impossible"
        )
    else:
        reproduced_digest = inventory_digest(actual_hashes)
        if recorded_digest == reproduced_digest:
            checks[PART4] = {
                "status": "PASS",
                "digest": recorded_digest,
                "pins": len(actual_hashes),
            }
        else:
            divergence = _pin_divergence(receipt.source_sha256, actual_hashes)
            checks[PART4] = {
                "status": "FAIL",
                "recorded_digest": recorded_digest,
                "reproduced_digest": reproduced_digest,
                "divergence": divergence,
            }
            failures.append(
                f"part 4 ({PART4}): recorded {recorded_digest} != reproduced "
                f"{reproduced_digest} (divergence: {json.dumps(divergence)})"
            )


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
