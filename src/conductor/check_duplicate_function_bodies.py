#!/usr/bin/env python3
from __future__ import annotations

import ast
import hashlib
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path


ROOTS = ("research", "aria_core", "aria_designer")
SKIP_PARTS = {"tests", "test", ".venv", "node_modules", "__pycache__", "migrations"}
MIN_BODY_LINES = 8


@dataclass(frozen=True)
class FunctionBody:
    path: str
    name: str
    lineno: int
    digest: str


def _git(args: list[str]) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(["git", *args], capture_output=True, check=False)


def _skip(path: str) -> bool:
    return any(part in SKIP_PARTS for part in Path(path).parts)


def _tracked_python_files(ref: str) -> list[str]:
    proc = _git(["ls-tree", "-r", "--name-only", ref, "--", *ROOTS])
    if proc.returncode != 0:
        return []
    return [
        path
        for path in proc.stdout.decode("utf-8", "replace").splitlines()
        if path.endswith(".py") and not _skip(path)
    ]


def _staged_python_files() -> list[str]:
    proc = _git(
        [
            "diff",
            "--cached",
            "--name-only",
            "--diff-filter=ACMR",
            "-z",
            "--",
            *ROOTS,
        ]
    )
    if proc.returncode != 0:
        return []
    return [
        path
        for path in proc.stdout.decode("utf-8", "replace").split("\0")
        if path.endswith(".py") and not _skip(path)
    ]


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


def main() -> int:
    existing_by_digest: dict[str, FunctionBody] = {}
    for path in _tracked_python_files("HEAD"):
        for fn in _functions(path, _read_ref(path, "HEAD")):
            existing_by_digest.setdefault(fn.digest, fn)

    duplicate_pairs: list[tuple[FunctionBody, FunctionBody]] = []
    for path in _staged_python_files():
        head_digests = {fn.digest for fn in _functions(path, _read_ref(path, "HEAD"))}
        for fn in _functions(path, _read_index(path)):
            if fn.digest in head_digests:
                continue
            existing = existing_by_digest.get(fn.digest)
            if existing and existing.path != fn.path:
                duplicate_pairs.append((fn, existing))

    if not duplicate_pairs:
        return 0

    print("BLOCKED duplicate function body in staged Python changes:", file=sys.stderr)
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
