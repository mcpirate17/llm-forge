#!/usr/bin/env python3
from __future__ import annotations

import argparse
import ast
import hashlib
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Sequence


ROOT = Path(__file__).resolve().parents[1]
ROOTS = ("research", "aria_core", "aria_designer", "component_fab")
SKIP_PARTS = {"tests", "test", ".venv", "node_modules", "__pycache__", "migrations"}
MIN_BODY_LINES = 8


@dataclass(frozen=True)
class FunctionBody:
    path: str
    name: str
    lineno: int
    digest: str


def _git(args: list[str]) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(["git", *args], cwd=ROOT, capture_output=True, check=False)


def _skip(path: str) -> bool:
    return any(part in SKIP_PARTS for part in Path(path).parts)


def _tracked_python_files(ref: str) -> list[str]:
    proc = _git(["ls-tree", "-r", "--name-only", ref, "--", *ROOTS])
    if proc.returncode != 0:
        detail = proc.stderr.decode("utf-8", "replace").strip()
        raise RuntimeError(f"git ls-tree failed: {detail or proc.returncode}")
    return [
        path
        for path in proc.stdout.decode("utf-8", "replace").splitlines()
        if path.endswith(".py") and not _skip(path)
    ]


def _changed_python_files(base_ref: str | None = None) -> list[str]:
    # Include D (deleted) so the move-detector recognizes the removal-side
    # of a split refactor (delete foo.py + add foo/foo_part.py). Without D,
    # the deleted source path is treated as "still on disk" and the hook
    # falsely flags every moved body as a duplication.
    args = ["diff"]
    if base_ref is None:
        args.append("--cached")
    else:
        args.extend([base_ref, "HEAD"])
    args.extend(
        [
            "--no-renames",
            "--name-only",
            "--diff-filter=ACMRD",
            "-z",
            "--",
            *ROOTS,
        ]
    )
    proc = _git(args)
    if proc.returncode != 0:
        detail = proc.stderr.decode("utf-8", "replace").strip()
        raise RuntimeError(f"git diff failed: {detail or proc.returncode}")
    return [
        path
        for path in proc.stdout.decode("utf-8", "replace").split("\0")
        if path.endswith(".py") and not _skip(path)
    ]


def _staged_python_files() -> list[str]:
    return _changed_python_files()


def _merge_base(from_ref: str) -> str:
    proc = _git(["merge-base", from_ref, "HEAD"])
    if proc.returncode != 0:
        detail = proc.stderr.decode("utf-8", "replace").strip()
        raise RuntimeError(
            f"cannot resolve merge base for {from_ref!r}: {detail or proc.returncode}"
        )
    return proc.stdout.decode("utf-8", "replace").strip()


def _read_ref(path: str, ref: str) -> str:
    proc = _git(["show", f"{ref}:{path}"])
    if proc.returncode != 0:
        return ""
    return proc.stdout.decode("utf-8", "replace")


def _read_index(path: str) -> str:
    proc = _git(["show", f":{path}"])
    if proc.returncode != 0:
        return ""
    return proc.stdout.decode("utf-8", "replace")


def _function_digest(node: ast.FunctionDef | ast.AsyncFunctionDef) -> str | None:
    end_lineno = getattr(node, "end_lineno", node.lineno)
    if end_lineno - node.lineno + 1 < MIN_BODY_LINES:
        return None
    clone = ast.FunctionDef(
        name="_",
        args=node.args,
        body=node.body,
        decorator_list=[],
        returns=node.returns,
        type_comment=getattr(node, "type_comment", None),
    )
    ast.fix_missing_locations(clone)
    payload = ast.dump(clone, include_attributes=False)
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def _functions(path: str, content: str) -> list[FunctionBody]:
    if not content.strip():
        return []
    try:
        tree = ast.parse(content, filename=path)
    except SyntaxError:
        return []
    found: list[FunctionBody] = []
    for node in ast.walk(tree):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        digest = _function_digest(node)
        if digest is None:
            continue
        found.append(FunctionBody(path, node.name, node.lineno, digest))
    return found


def _duplicate_pairs(
    from_ref: str | None = None,
) -> list[tuple[FunctionBody, FunctionBody]]:
    base_ref = _merge_base(from_ref) if from_ref is not None else "HEAD"
    existing_by_digest: dict[str, FunctionBody] = {}
    for path in _tracked_python_files(base_ref):
        for fn in _functions(path, _read_ref(path, base_ref)):
            existing_by_digest.setdefault(fn.digest, fn)

    # Build the exact candidate snapshot. Local pre-commit reads the index;
    # CI range checks read HEAD. In either mode, changed/deleted source paths
    # are represented in the snapshot so split refactors count as moves.
    changed_paths = set(_changed_python_files(base_ref if from_ref else None))
    candidate_functions_by_path: dict[str, list[FunctionBody]] = {}
    for path in changed_paths:
        content = _read_ref(path, "HEAD") if from_ref else _read_index(path)
        candidate_functions_by_path[path] = _functions(path, content)
    candidate_digests_by_path = {
        path: {fn.digest for fn in functions}
        for path, functions in candidate_functions_by_path.items()
    }

    def _digest_still_present(path: str, digest: str) -> bool:
        if path in changed_paths:
            return digest in candidate_digests_by_path[path]
        return True

    duplicate_pairs: list[tuple[FunctionBody, FunctionBody]] = []
    for path in changed_paths:
        base_digests = {fn.digest for fn in _functions(path, _read_ref(path, base_ref))}
        for fn in candidate_functions_by_path[path]:
            if fn.digest in base_digests:
                continue
            existing = existing_by_digest.get(fn.digest)
            if not existing or existing.path == fn.path:
                continue
            # If the original location's staged version no longer has this
            # function body, treat it as a move, not a duplication.
            if not _digest_still_present(existing.path, fn.digest):
                continue
            duplicate_pairs.append((fn, existing))
    return duplicate_pairs


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Block copied Python function bodies")
    parser.add_argument(
        "--from-ref",
        help="Check files changed from the merge base with REF to HEAD",
    )
    args = parser.parse_args(argv)

    duplicate_pairs = _duplicate_pairs(args.from_ref)

    if not duplicate_pairs:
        return 0

    print(
        "BLOCKED duplicate function body in candidate Python changes:", file=sys.stderr
    )
    for new, old in duplicate_pairs:
        print(
            f"  - {new.path}:{new.lineno} {new.name} duplicates "
            f"{old.path}:{old.lineno} {old.name}",
            file=sys.stderr,
        )
    print(
        "Reuse the existing function or extract a shared helper instead of copying "
        "the implementation.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
