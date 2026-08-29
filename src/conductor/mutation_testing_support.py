"""Circular-safe implementation helpers for :mod:`conductor.mutation_testing`.

The orchestration module keeps its established private API as thin wrappers so
callers and tests can continue to monkeypatch those names.  Implementations in
this module receive their cross-module dependencies explicitly.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
from typing import Any, Callable, Container, Mapping, Protocol, Sequence, TypeVar


CANONICAL_TEST_PATTERNS = (
    "**/test_*.py",
    "**/*_test.py",
    "**/*.test.js",
    "**/*.test.jsx",
    "**/*.test.ts",
    "**/*.test.tsx",
    "**/*.spec.js",
    "**/*.spec.jsx",
    "**/*.spec.ts",
    "**/*.spec.tsx",
    "**/*Test.java",
    "**/test_*.c",
    "**/*_test.c",
    "**/test_*.cc",
    "**/*_test.cc",
    "**/test_*.cpp",
    "**/*_test.cpp",
    "**/test_*.cxx",
    "**/*_test.cxx",
)
ANCHOR_REGISTRY_PATH = "conductor/mutation_campaigns/registry.json"


class _CampaignSupport(Protocol):
    @property
    def host_read_dependencies(self) -> Sequence[str]: ...

    @property
    def mutations(self) -> Sequence[_MutationSupport]: ...


class _MutationSupport(Protocol):
    @property
    def mutation_id(self) -> str: ...

    @property
    def patch_file(self) -> Path: ...

    @property
    def patch_sha256(self) -> str: ...

    @property
    def allowed_paths(self) -> tuple[str, ...]: ...


_CommandResultT = TypeVar("_CommandResultT")


def _git_bytes(repo: Path, args: Sequence[str]) -> subprocess.CompletedProcess[bytes]:
    """Run a bounded Git query without replace refs or caller Git redirection."""
    environment = {
        key: value for key, value in os.environ.items() if not key.startswith("GIT_")
    }
    environment["GIT_NO_REPLACE_OBJECTS"] = "1"
    try:
        return subprocess.run(
            ["git", "--no-replace-objects", *args],
            cwd=repo,
            env=environment,
            capture_output=True,
            check=False,
            timeout=15,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return subprocess.CompletedProcess(
            ["git", *args], 127, stdout=b"", stderr=str(exc).encode("utf-8")
        )


def _lexical_regular_file(path: Path, root: Path, label: str) -> tuple[str | None, str]:
    """Return a lexical repo path only when no descendant component is a symlink."""
    root_absolute = Path(os.path.abspath(root))
    path_absolute = Path(os.path.abspath(path))
    if root_absolute.is_symlink():
        return f"{label} repository root is a symlink", ""
    try:
        relative = path_absolute.relative_to(root_absolute)
    except ValueError:
        return f"{label} path escapes the repository", ""
    current = root_absolute
    for part in relative.parts:
        current /= part
        if current.is_symlink():
            return f"{label} path has a symlink component", ""
    if not path_absolute.is_file():
        return f"{label} path is missing or unsafe", ""
    return None, relative.as_posix()


def _registered_anchor_manifest_error(
    anchor_repo: Path,
    *,
    anchor_commit: str,
    manifest_path: str,
    manifest_sha256: str,
) -> str | None:
    """Require the campaign identity in the anchor's immutable registry and manifest."""
    registry = _git_bytes(
        anchor_repo, ["cat-file", "blob", f"{anchor_commit}:{ANCHOR_REGISTRY_PATH}"]
    )
    if registry.returncode != 0:
        return "legacy receipt anchor registry is unavailable"
    try:
        payload = json.loads(registry.stdout)
        rows = payload["campaigns"]
        registered = {
            row["manifest"]
            for row in rows
            if isinstance(row, dict) and "manifest" in row
        }
    except (KeyError, TypeError, ValueError, json.JSONDecodeError):
        return "legacy receipt anchor registry is malformed"
    if manifest_path not in registered:
        return "legacy receipt campaign was not registered at the anchor"
    manifest = _git_bytes(
        anchor_repo, ["cat-file", "blob", f"{anchor_commit}:{manifest_path}"]
    )
    if manifest.returncode != 0:
        return "legacy receipt anchor manifest is unavailable"
    if hashlib.sha256(manifest.stdout).hexdigest() != manifest_sha256:
        return "legacy receipt anchor manifest hash mismatch"
    return None


def legacy_receipt_anchor_errors(
    receipt_path: Path | None,
    receipt_bytes: bytes | None,
    repo_root: Path,
    anchor_repo: Path,
    *,
    anchor_commit: str,
    anchor_tree: str,
    receipt_prefix: str,
    manifest_path: str,
    manifest_sha256: str,
) -> list[str]:
    """Accept v2 only as an exact registered receipt and campaign at the anchor."""
    if receipt_path is None or receipt_bytes is None:
        return ["legacy receipt path or parsed bytes are unavailable"]
    path_error, relative = _lexical_regular_file(
        receipt_path, repo_root, "legacy receipt"
    )
    if path_error:
        return [path_error]
    if not relative.startswith(receipt_prefix) or not relative.endswith(".json"):
        return ["legacy receipt path is outside the anchored receipt directory"]
    top = _git_bytes(anchor_repo, ["rev-parse", "--show-toplevel"])
    expected_top = Path(os.path.abspath(anchor_repo)).as_posix()
    if (
        top.returncode != 0
        or top.stdout.decode("utf-8", "replace").strip() != expected_top
    ):
        return ["legacy receipt anchor repository is unavailable"]
    commit_type = _git_bytes(anchor_repo, ["cat-file", "-t", anchor_commit])
    if commit_type.returncode != 0 or commit_type.stdout.strip() != b"commit":
        return ["legacy receipt anchor commit is unavailable"]
    tree = _git_bytes(anchor_repo, ["rev-parse", f"{anchor_commit}^{{tree}}"])
    if tree.returncode != 0 or tree.stdout.decode("ascii", "replace").strip() != (
        anchor_tree
    ):
        return ["legacy receipt anchor tree mismatch"]
    manifest_error = _registered_anchor_manifest_error(
        anchor_repo,
        anchor_commit=anchor_commit,
        manifest_path=manifest_path,
        manifest_sha256=manifest_sha256,
    )
    if manifest_error:
        return [manifest_error]
    entry = _git_bytes(anchor_repo, ["ls-tree", anchor_commit, "--", relative])
    expected_suffix = f"\t{relative}\n".encode("utf-8")
    if (
        entry.returncode != 0
        or not entry.stdout.startswith(b"100644 blob ")
        or not entry.stdout.endswith(expected_suffix)
    ):
        return ["legacy receipt is absent or unsafe at the anchor"]
    blob = _git_bytes(anchor_repo, ["cat-file", "blob", f"{anchor_commit}:{relative}"])
    if blob.returncode != 0 or blob.stdout != receipt_bytes:
        return ["legacy receipt parsed bytes differ from the anchor"]
    return []


def load_receipts(
    repo_root: Path,
    directories: Sequence[str],
    *,
    safe_relative: Callable[[Any, str], str],
    require_mapping: Callable[[Any, str], Mapping[str, Any]],
    error_type: type[Exception],
) -> tuple[list[tuple[Path, Mapping[str, Any], bytes]], list[str]]:
    """Load each receipt once so parsed and anchor-checked bytes are identical."""
    receipt_paths: list[Path] = []
    for relative in directories:
        directory = repo_root / safe_relative(relative, "receipt directory")
        if directory.is_dir():
            receipt_paths.extend(sorted(directory.glob("*.json")))
    receipts: list[tuple[Path, Mapping[str, Any], bytes]] = []
    malformed: list[str] = []
    for path in receipt_paths:
        try:
            raw_bytes = path.read_bytes()
            parsed = require_mapping(
                json.loads(raw_bytes.decode("utf-8")), f"receipt {path}"
            )
        except (error_type, OSError, UnicodeError, json.JSONDecodeError) as exc:
            malformed.append(f"{path.relative_to(repo_root)}: {exc}")
            continue
        receipts.append((path, parsed, raw_bytes))
    return receipts, malformed


def evidence_result(
    normalized: Sequence[str],
    evidence: Sequence[Mapping[str, Any]],
    missing: Sequence[Mapping[str, Any]],
    malformed: Sequence[str],
) -> dict[str, Any]:
    """Build the stable machine-readable evidence-check result."""
    return {
        "schema_version": "llm.mutation-testing.evidence-check.v1",
        "status": "PASS" if not missing else "FAIL",
        "enforcement": "changed_tests",
        "checked_test_paths": list(sorted(set(normalized))),
        "evidence": list(evidence),
        "missing_evidence": list(missing),
        "malformed_receipts": list(malformed),
    }


def link_host_dependencies(
    campaign: _CampaignSupport,
    snapshot_root: Path,
    host_root: Path,
    *,
    materialize: Callable[[Path, Path], None],
    error_type: type[Exception],
) -> None:
    """Materialize declared host-read dependencies inside a snapshot."""

    for relative in campaign.host_read_dependencies:
        source = host_root / relative
        if not source.exists():
            raise error_type(f"host read dependency is missing: {relative}")
        destination = snapshot_root / relative
        if destination.exists() or destination.is_symlink():
            raise error_type(
                f"snapshot already contains host read dependency path: {relative}"
            )
        destination.parent.mkdir(parents=True, exist_ok=True)
        materialize(source, destination)


def materialize(source: Path, destination: Path) -> None:
    """Place a host path inside a snapshot as real files, never a symlink."""

    if source.is_dir():
        destination.mkdir()
        for child in source.iterdir():
            materialize(child, destination / child.name)
        return
    try:
        os.link(source, destination)
    except OSError:
        shutil.copy2(source, destination)


def link_mutation_patches(
    campaign: _CampaignSupport,
    snapshot_root: Path,
    host_root: Path,
    *,
    sha256: Callable[[Path], str],
    error_type: type[Exception],
) -> None:
    """Copy reviewed patch artifacts into snapshots without staging them."""

    if snapshot_root.resolve() == host_root.resolve():
        return
    for mutation in campaign.mutations:
        source = mutation.patch_file.resolve()
        try:
            relative = source.relative_to(host_root.resolve())
        except ValueError as exc:
            raise error_type(
                f"mutation patch is outside the host repository: {source}"
            ) from exc
        destination = snapshot_root / relative
        if destination.exists() or destination.is_symlink():
            actual = sha256(destination) if destination.is_file() else None
            if actual != mutation.patch_sha256:
                raise error_type(
                    f"snapshot mutation patch hash drifted for {relative}: "
                    f"expected {mutation.patch_sha256}, got {actual}"
                )
            continue
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)


def pin_interpreter(
    argv: Sequence[str], *, bare_interpreters: Container[str], executable: str
) -> list[str]:
    """Resolve a bare Python command to the runner's interpreter."""

    resolved = list(argv)
    if resolved and resolved[0] in bare_interpreters:
        resolved[0] = executable
    return resolved


def torch_version(interpreter: str) -> str | None:
    """Return the interpreter's torch version when a bounded probe succeeds."""

    try:
        probe = subprocess.run(
            [interpreter, "-c", "import torch; print(torch.__version__)"],
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    return probe.stdout.strip() or None


def run_command(
    argv: Sequence[str],
    *,
    cwd: Path,
    timeout_seconds: int,
    environment: Mapping[str, str],
    pin_argv: Callable[[Sequence[str]], list[str]],
    result_factory: Callable[..., _CommandResultT],
    output_tail_chars: int,
) -> _CommandResultT:
    """Run one bounded command and retain only the configured output tails."""

    env = os.environ.copy()
    env.update(environment)
    resolved_argv = pin_argv(argv)
    started = time.monotonic()
    try:
        proc = subprocess.run(
            resolved_argv,
            cwd=cwd,
            env=env,
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout_seconds,
        )
    except subprocess.TimeoutExpired as exc:
        duration = time.monotonic() - started
        stdout = (
            exc.stdout.decode() if isinstance(exc.stdout, bytes) else (exc.stdout or "")
        )
        stderr = (
            exc.stderr.decode() if isinstance(exc.stderr, bytes) else (exc.stderr or "")
        )
        return result_factory(
            returncode=None,
            timed_out=True,
            duration_seconds=duration,
            stdout_tail=stdout[-output_tail_chars:],
            stderr_tail=stderr[-output_tail_chars:],
        )
    return result_factory(
        returncode=proc.returncode,
        timed_out=False,
        duration_seconds=time.monotonic() - started,
        stdout_tail=proc.stdout[-output_tail_chars:],
        stderr_tail=proc.stderr[-output_tail_chars:],
    )


def apply_mutation(
    mutation: _MutationSupport,
    snapshot_root: Path,
    *,
    sha256: Callable[[Path], str],
    error_type: type[Exception],
) -> None:
    """Apply one hash-bound patch and verify its exact changed-path set."""

    actual_patch_sha256 = sha256(mutation.patch_file)
    if actual_patch_sha256 != mutation.patch_sha256:
        raise error_type(
            f"mutation {mutation.mutation_id!r} patch changed after campaign load: "
            f"expected {mutation.patch_sha256}, got {actual_patch_sha256}"
        )
    for command in (
        ["git", "apply", "--check", str(mutation.patch_file)],
        ["git", "apply", str(mutation.patch_file)],
    ):
        proc = subprocess.run(
            command,
            cwd=snapshot_root,
            check=False,
            capture_output=True,
            text=True,
        )
        if proc.returncode != 0:
            raise error_type(
                f"mutation {mutation.mutation_id!r} patch failed: "
                f"{(proc.stderr or proc.stdout).strip()[:2000]}"
            )
    changed = subprocess.run(
        ["git", "diff", "--name-only"],
        cwd=snapshot_root,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.splitlines()
    changed_paths = tuple(sorted(path for path in changed if path))
    if changed_paths != mutation.allowed_paths:
        raise error_type(
            f"mutation {mutation.mutation_id!r} changed {changed_paths}, "
            f"expected exactly {mutation.allowed_paths}"
        )


def atomic_json(path: Path, payload: Mapping[str, Any]) -> None:
    """Atomically publish a JSON object and clean any temporary residue."""

    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    try:
        temporary.write_text(
            json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)
