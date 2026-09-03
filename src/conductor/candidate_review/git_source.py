"""Resolve and safely materialize exact candidates from Git objects."""

from __future__ import annotations

import os
import re
import stat
import subprocess
import tempfile
from contextlib import contextmanager
from dataclasses import replace
from pathlib import Path, PurePosixPath
from typing import Iterator, Sequence

from conductor.candidate_review.model import Candidate, Change, TreeEntry

EMPTY_TREE_OID = "4b825dc642cb6eb9a060e54bf8d69288fbee4904"  # pragma: allowlist secret
ZERO_OID = "0" * 40


class GitSourceError(RuntimeError):
    """Candidate identity or materialization could not be proven."""


MAX_MATERIALIZED_BLOB_BYTES = 64 * 1024 * 1024
MAX_MATERIALIZED_TREE_BYTES = 2 * 1024 * 1024 * 1024
# The integration line, in resolution order; CONDUCTOR_INTEGRATION_REF overrides it.
DEFAULT_INTEGRATION_REFS: tuple[str, ...] = ("origin/master", "master")
INTEGRATION_REF_ENV = "CONDUCTOR_INTEGRATION_REF"


def run_git(
    repo: Path,
    args: Sequence[str],
    *,
    input_bytes: bytes | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[bytes]:
    completed = subprocess.run(
        ["git", *args],
        cwd=repo,
        input=input_bytes,
        capture_output=True,
        check=False,
    )
    if check and completed.returncode:
        detail = completed.stderr.decode("utf-8", "replace").strip()
        raise GitSourceError(
            f"git {' '.join(args)} failed ({completed.returncode}): {detail}"
        )
    return completed


def repository_root(path: Path) -> Path:
    completed = run_git(path, ["rev-parse", "--show-toplevel"])
    return Path(completed.stdout.decode().strip()).resolve()


def git_common_dir(repo: Path) -> Path:
    raw = run_git(repo, ["rev-parse", "--git-common-dir"]).stdout.decode().strip()
    candidate = Path(raw)
    return (
        candidate.resolve() if candidate.is_absolute() else (repo / candidate).resolve()
    )


def resolve_commit(repo: Path, ref: str) -> str:
    if not ref or ref.startswith("-"):
        raise GitSourceError(f"invalid or ambiguous Git ref: {ref!r}")
    resolved_ref = ref
    if (
        not ref.startswith("refs/")
        and ref != "HEAD"
        and re.fullmatch(r"[A-Za-z0-9._/-]+", ref)
        and not re.fullmatch(r"[0-9a-fA-F]{40,64}", ref)
    ):
        namespaces = (
            f"refs/heads/{ref}",
            f"refs/tags/{ref}",
            f"refs/remotes/{ref}",
            f"refs/{ref}",
        )
        matches = [
            candidate
            for candidate in namespaces
            if run_git(
                repo, ["show-ref", "--verify", "--quiet", candidate], check=False
            ).returncode
            == 0
        ]
        if len(matches) > 1:
            raise GitSourceError(
                f"ambiguous Git ref {ref!r}; use one exact namespace: {', '.join(matches)}"
            )
        if matches:
            resolved_ref = matches[0]
    completed = run_git(
        repo, ["rev-parse", "--verify", f"{resolved_ref}^{{commit}}"]
    ).stdout
    oid = completed.decode().strip()
    if len(oid) < 40:
        raise GitSourceError(f"Git ref did not resolve to a full commit OID: {ref!r}")
    return oid


def commit_tree(repo: Path, commit_oid: str) -> str:
    return (
        run_git(repo, ["rev-parse", f"{commit_oid}^{{tree}}"]).stdout.decode().strip()
    )


def _head_or_empty(repo: Path) -> tuple[str | None, str]:
    head = run_git(repo, ["rev-parse", "--verify", "HEAD^{commit}"], check=False)
    if head.returncode:
        return None, EMPTY_TREE_OID
    commit_oid = head.stdout.decode().strip()
    return commit_oid, commit_tree(repo, commit_oid)


def integration_refs() -> tuple[str, ...]:
    """Refs that name the integration line, most specific first."""

    override = os.environ.get(INTEGRATION_REF_ENV, "").strip()
    return (override,) if override else DEFAULT_INTEGRATION_REFS


def resolve_integration_base(
    repo: Path, *, tip: str, refs: Sequence[str] | None = None
) -> tuple[str | None, str]:
    """Merge base between `tip` and the integration line, with why when there is none.

    This is the base a CI `range` review computes for the same branch, so a waiver
    pinned to it is honoured identically by both. Resolution is fail-closed: an
    unresolvable integration line yields None and a reason, never a guess, and the
    caller falls back to the review base (which only makes waivers inert, never
    active). The result is verified to be an ancestor of `tip` -- a base that is not
    one describes a different history, and binding a waiver to it would carry an
    exemption across a rebase, which is exactly what the pinning exists to prevent.
    """

    tried: list[str] = []
    for ref in refs if refs is not None else integration_refs():
        resolved = run_git(
            repo, ["rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}"], check=False
        )
        if resolved.returncode:
            tried.append(ref)
            continue
        merge = run_git(repo, ["merge-base", ref, tip], check=False)
        oid = merge.stdout.decode().strip()
        if merge.returncode or not oid:
            return None, f"no merge base between {ref} and {tip[:12]}"
        return _verify_ancestor(repo, oid, tip=tip, detail=f"merge base with {ref}")
    return None, f"no integration ref resolved (tried {', '.join(tried) or 'none'})"


def _verify_ancestor(
    repo: Path, oid: str, *, tip: str, detail: str
) -> tuple[str | None, str]:
    ancestor = run_git(repo, ["merge-base", "--is-ancestor", oid, tip], check=False)
    if ancestor.returncode:
        return None, f"{oid[:12]} ({detail}) is not an ancestor of {tip[:12]}"
    return oid, detail


def _parents(repo: Path, commit_oid: str) -> list[str]:
    raw = (
        run_git(repo, ["show", "-s", "--format=%P", commit_oid]).stdout.decode().strip()
    )
    return raw.split() if raw else []


def _diff_changes(
    repo: Path, base_tree: str, candidate_tree: str
) -> tuple[Change, ...]:
    raw = run_git(
        repo,
        ["diff-tree", "--raw", "-z", "-r", "-M", "-C", base_tree, candidate_tree],
    ).stdout
    fields = raw.split(b"\0")
    changes: list[Change] = []
    cursor = 0
    while cursor < len(fields) and fields[cursor]:
        header = fields[cursor].decode("ascii", "strict")
        cursor += 1
        if not header.startswith(":"):
            raise GitSourceError(f"malformed raw Git diff header: {header!r}")
        old_mode, new_mode, old_oid, new_oid, status = header[1:].split()
        if cursor >= len(fields) or not fields[cursor]:
            raise GitSourceError("raw Git diff omitted a candidate path")
        first_path = fields[cursor].decode("utf-8", "surrogateescape")
        cursor += 1
        old_path: str | None = None
        path = first_path
        if status[0] in {"R", "C"}:
            if cursor >= len(fields) or not fields[cursor]:
                raise GitSourceError("raw Git rename/copy omitted its destination path")
            old_path = first_path
            path = fields[cursor].decode("utf-8", "surrogateescape")
            cursor += 1
        changes.append(
            Change(
                status=status,
                path=path,
                old_path=old_path,
                old_mode=old_mode,
                new_mode=new_mode,
                old_oid=old_oid,
                new_oid=new_oid,
            )
        )
    return tuple(changes)


def resolve_candidate(
    repo: Path,
    *,
    kind: str,
    target_ref: str = "HEAD",
    base_ref: str | None = None,
) -> Candidate:
    repo = repository_root(repo)
    if kind == "index":
        head_commit, head_tree = _head_or_empty(repo)
        if base_ref is not None:
            base_commit = resolve_commit(repo, base_ref)
            base_tree = commit_tree(repo, base_commit)
        else:
            base_commit, base_tree = head_commit, head_tree
        tree = run_git(repo, ["write-tree"]).stdout.decode().strip()
        # The staged diff is taken against `base_commit` (HEAD by default); waivers
        # bind to the integration line instead, so both bases are carried.
        if head_commit is None:
            integration_oid, integration_detail = None, "HEAD names no commit"
        elif base_ref is not None:
            integration_oid, integration_detail = _verify_ancestor(
                repo,
                str(base_commit),
                tip=head_commit,
                detail=f"explicit base ref {base_ref}",
            )
        else:
            integration_oid, integration_detail = resolve_integration_base(
                repo, tip=head_commit
            )
        return Candidate(
            kind=kind,
            tree_oid=tree,
            base_tree_oid=base_tree,
            base_commit_oid=base_commit,
            commit_oid=None,
            target_ref=None,
            changes=_diff_changes(repo, base_tree, tree),
            integration_base_oid=integration_oid,
            integration_base_detail=integration_detail,
        )
    target = resolve_commit(repo, target_ref)
    target_tree = commit_tree(repo, target)
    if kind == "commit":
        if base_ref:
            base_commit = resolve_commit(repo, base_ref)
        else:
            parents = _parents(repo, target)
            if len(parents) > 1:
                raise GitSourceError("merge commits require an explicit --base-ref")
            base_commit = parents[0] if parents else None
        base_tree = commit_tree(repo, base_commit) if base_commit else EMPTY_TREE_OID
    elif kind == "range":
        if not base_ref:
            raise GitSourceError("range candidates require an explicit --base-ref")
        resolved_base = resolve_commit(repo, base_ref)
        merge_base = (
            run_git(repo, ["merge-base", resolved_base, target]).stdout.decode().strip()
        )
        if not merge_base:
            raise GitSourceError(
                f"no merge base between {base_ref!r} and {target_ref!r}"
            )
        base_commit = merge_base
        base_tree = commit_tree(repo, merge_base)
    else:
        raise GitSourceError(f"unsupported candidate kind: {kind!r}")
    # A waiver binds to a base only once that base is proven to be on this candidate's
    # own history. For `range` (a merge base) and a parentless `commit` the check is a
    # formality; for a `commit` with an explicit --base-ref it is the only thing that
    # stops a base off an unrelated history from activating a waiver. Fail closed: an
    # unverifiable base yields no waiver base, which leaves every waiver inert.
    if base_commit is None:
        integration_oid: str | None = None
        integration_detail = f"{kind} candidate has no base commit"
    else:
        integration_oid, integration_detail = _verify_ancestor(
            repo, base_commit, tip=target, detail=f"{kind} candidate base"
        )
    return Candidate(
        kind=kind,
        tree_oid=target_tree,
        base_tree_oid=base_tree,
        base_commit_oid=base_commit,
        commit_oid=target,
        target_ref=target_ref,
        changes=_diff_changes(repo, base_tree, target_tree),
        integration_base_oid=integration_oid,
        integration_base_detail=integration_detail,
    )


def classify_candidate(candidate: Candidate, classifier: object) -> Candidate:
    classify = getattr(classifier, "classify_change")
    return replace(
        candidate, changes=tuple(classify(change) for change in candidate.changes)
    )


def list_tree(repo: Path, tree_oid: str) -> tuple[TreeEntry, ...]:
    raw = run_git(repo, ["ls-tree", "-r", "-z", "-l", "--full-tree", tree_oid]).stdout
    entries: list[TreeEntry] = []
    for record in raw.split(b"\0"):
        if not record:
            continue
        try:
            metadata, path_raw = record.split(b"\t", 1)
            mode, object_type, oid, size_raw = metadata.decode("ascii").split()
        except ValueError as exc:
            raise GitSourceError("malformed git ls-tree record") from exc
        path = path_raw.decode("utf-8", "surrogateescape")
        _validate_tree_path(path)
        entries.append(
            TreeEntry(
                path=path,
                mode=mode,
                object_type=object_type,
                oid=oid,
                size=int(size_raw) if size_raw != "-" else None,
            )
        )
    return tuple(entries)


def _validate_tree_path(path: str) -> None:
    pure = PurePosixPath(path)
    if not path or pure.is_absolute() or ".." in pure.parts or "." in pure.parts:
        raise GitSourceError(f"unsafe path in Git tree: {path!r}")


def _batch_blobs(repo: Path, entries: Sequence[TreeEntry]) -> dict[str, bytes]:
    blobs = [entry for entry in entries if entry.object_type == "blob"]
    if not blobs:
        return {}
    payload = "".join(f"{entry.oid}\n" for entry in blobs).encode("ascii")
    output = run_git(repo, ["cat-file", "--batch"], input_bytes=payload).stdout
    cursor = 0
    content: dict[str, bytes] = {}
    for entry in blobs:
        line_end = output.find(b"\n", cursor)
        if line_end < 0:
            raise GitSourceError("git cat-file batch response ended before its header")
        header = output[cursor:line_end].decode("ascii", "strict").split()
        if len(header) != 3 or header[0] != entry.oid or header[1] != "blob":
            raise GitSourceError(f"unexpected git cat-file header for {entry.path!r}")
        size = int(header[2])
        start = line_end + 1
        end = start + size
        if end >= len(output):
            raise GitSourceError(f"truncated git blob for {entry.path!r}")
        content[entry.oid] = output[start:end]
        cursor = end + 1
    return content


def _validate_materialization_budget(
    entries: Sequence[TreeEntry], *, max_blob_bytes: int, max_tree_bytes: int
) -> None:
    sizes: list[int] = []
    for entry in entries:
        if entry.object_type != "blob":
            continue
        if entry.size is None:
            raise GitSourceError(f"blob size is unavailable for {entry.path!r}")
        if entry.size > max_blob_bytes:
            raise GitSourceError(
                f"Git blob exceeds materialization limit: {entry.path!r} "
                f"is {entry.size} bytes, limit {max_blob_bytes}"
            )
        sizes.append(entry.size)
    total = sum(sizes)
    if total > max_tree_bytes:
        raise GitSourceError(
            f"Git tree exceeds materialization limit: {total} bytes, "
            f"limit {max_tree_bytes}"
        )


def _safe_link_target(root: Path, path: Path, raw_target: bytes) -> str:
    target = raw_target.decode("utf-8", "surrogateescape")
    target_path = Path(target)
    if target_path.is_absolute():
        raise GitSourceError(
            f"absolute symlink target is forbidden: {path.relative_to(root)} -> {target}"
        )
    resolved = (path.parent / target_path).resolve(strict=False)
    try:
        resolved.relative_to(root.resolve())
    except ValueError as exc:
        raise GitSourceError(
            f"symlink escapes candidate snapshot: {path.relative_to(root)} -> {target}"
        ) from exc
    return target


@contextmanager
def materialize_tree(
    repo: Path,
    tree_oid: str,
    *,
    max_blob_bytes: int = MAX_MATERIALIZED_BLOB_BYTES,
    max_tree_bytes: int = MAX_MATERIALIZED_TREE_BYTES,
) -> Iterator[tuple[Path, tuple[TreeEntry, ...]]]:
    entries = list_tree(repo, tree_oid)
    _validate_materialization_budget(
        entries,
        max_blob_bytes=max_blob_bytes,
        max_tree_bytes=max_tree_bytes,
    )
    blobs = _batch_blobs(repo, entries)
    with tempfile.TemporaryDirectory(prefix=f"llm-candidate-{tree_oid[:12]}-") as raw:
        root = Path(raw).resolve()
        for entry in entries:
            if entry.mode == "160000":
                continue
            path = root / entry.path
            path.parent.mkdir(parents=True, exist_ok=True)
            data = blobs.get(entry.oid)
            if data is None:
                raise GitSourceError(
                    f"tree entry is not a materializable blob: {entry.path}"
                )
            if entry.mode == "120000":
                os.symlink(_safe_link_target(root, path, data), path)
                continue
            path.write_bytes(data)
            permissions = stat.S_IRUSR | stat.S_IWUSR | stat.S_IRGRP | stat.S_IROTH
            if entry.mode == "100755":
                permissions |= stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH
            path.chmod(permissions)
        yield root, entries


def _rename_sources(repo: Path, candidate: Candidate) -> dict[str, str]:
    """Map each moved destination path to the source path git paired it with.

    A pathspec limited to destination paths hides the other half of a move, so
    git cannot pair the two and scores the whole file as new code. This pass
    runs unrestricted so the pairing exists before the line-level diff is
    scoped. ``--find-copies`` is needed as well as ``--find-renames`` because a
    move that leaves an alias shim behind keeps the source path alive, which is
    a copy to git, not a rename. ``--find-copies`` must come last: a later
    ``-M`` turns copy detection back off.
    """
    raw = run_git(
        repo,
        [
            "diff",
            "--name-status",
            "--find-renames",
            "--find-copies",
            "--diff-filter=RC",
            "--no-color",
            "-z",
            candidate.base_tree_oid,
            candidate.tree_oid,
        ],
    ).stdout.decode("utf-8", "replace")
    fields = raw.split("\0")
    sources: dict[str, str] = {}
    index = 0
    while index + 2 < len(fields) and fields[index]:
        sources[fields[index + 2]] = fields[index + 1]
        index += 3
    return sources


def changed_line_numbers(
    repo: Path, candidate: Candidate, paths: Sequence[str]
) -> dict[str, set[int]]:
    if not paths:
        return {}
    wanted = set(paths)
    sources = _rename_sources(repo, candidate)
    scope = sorted(wanted | {sources[path] for path in wanted & sources.keys()})
    raw = run_git(
        repo,
        [
            "diff",
            "--unified=0",
            "--find-renames",
            "--find-copies",
            "--no-color",
            candidate.base_tree_oid,
            candidate.tree_oid,
            "--",
            *scope,
        ],
    ).stdout.decode("utf-8", "replace")
    current: str | None = None
    result: dict[str, set[int]] = {}
    for line in raw.splitlines():
        if line.startswith("+++ b/"):
            current = line[6:] if line[6:] in wanted else None
            if current is not None:
                result.setdefault(current, set())
        elif line.startswith("@@") and current:
            plus = line.split(" ")[2][1:]
            start_raw, _, count_raw = plus.partition(",")
            start = int(start_raw)
            count = int(count_raw or "1")
            result[current].update(range(start, start + count))
    return result
