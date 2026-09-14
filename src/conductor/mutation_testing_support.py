"""Circular-safe implementation helpers for :mod:`conductor.mutation_testing`.

The orchestration module keeps its established private API as thin wrappers so
callers and tests can continue to monkeypatch those names.  Implementations in
this module receive their cross-module dependencies explicitly.

Runnable directly for the orphan reaper:
``python -m conductor.mutation_testing_support reap [--apply]``.
"""

from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time
from typing import (
    Any,
    Callable,
    Container,
    Mapping,
    MutableMapping,
    Protocol,
    Sequence,
    TypeVar,
)

# Imports nothing from `conductor`, so this stays circular-safe.
from conductor.mutation_patch_apply import PatchApplyError, apply_patch_text
from conductor.project_paths import (
    DEFAULT_MUTATION_REGISTRY,
    host_root,
    receipts_relative,
)


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
# The registry as it stood in the anchor commit. The host may keep its registry
# elsewhere today (``conductor.project_paths``), so callers pass the anchored
# spelling in; this default is the one the monorepo anchor was written against.
ANCHOR_REGISTRY_PATH = str(DEFAULT_MUTATION_REGISTRY)


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
    registry_path: str = ANCHOR_REGISTRY_PATH,
) -> str | None:
    """Require the campaign identity in the anchor's immutable registry and manifest."""
    registry = _git_bytes(
        anchor_repo, ["cat-file", "blob", f"{anchor_commit}:{registry_path}"]
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
    registry_path: str = ANCHOR_REGISTRY_PATH,
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
        registry_path=registry_path,
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
    """Build the stable machine-readable evidence-check result.

    The native verifier owns this shape; this twin exists so the Python-side
    reader of a recorded result never has to guess at the envelope. v2 carries
    classified rejections (``kind`` beside every ``detail``) and the aggregate
    ``rejection_counts``, matching ``verify_mutation_evidence_native``.
    """
    counts: dict[str, int] = {}
    for row in missing:
        reason_kind = row.get("reason_kind", "not_pass")
        counts[reason_kind] = counts.get(reason_kind, 0) + 1
        for rejection in row.get("receipt_rejections", []):
            kind = (
                rejection.get("kind", "schema_error")
                if isinstance(rejection, Mapping)
                else "schema_error"
            )
            counts[kind] = counts.get(kind, 0) + 1
    # A receipt the loader could not parse at all is the same defect class as
    # one whose detail does not decode.
    counts["decode_error"] = counts.get("decode_error", 0) + len(malformed)
    return {
        "schema_version": "llm.mutation-testing.evidence-check.v2",
        "status": "PASS" if not missing else "FAIL",
        "enforcement": "changed_tests",
        "checked_test_paths": list(sorted(set(normalized))),
        "evidence": list(evidence),
        "missing_evidence": list(missing),
        "rejection_counts": dict(sorted(counts.items())),
        "malformed_receipts": list(malformed),
    }


def _merge_host_dependency(
    source: Path,
    destination: Path,
    relative: str,
    *,
    materialize: Callable[[Path, Path], None],
    error_type: type[Exception],
) -> None:
    """Fill one declared host path into the snapshot without rewriting its tree."""

    if destination.is_symlink():
        raise error_type(
            f"snapshot already contains host read dependency path: {relative}"
        )
    if not destination.exists():
        destination.parent.mkdir(parents=True, exist_ok=True)
        materialize(source, destination)
        return
    if source.is_dir() != destination.is_dir():
        raise error_type(
            f"snapshot copy of host read dependency differs from the host: {relative}"
        )
    if not source.is_dir():
        if source.read_bytes() != destination.read_bytes():
            raise error_type(
                "snapshot copy of host read dependency differs from the host: "
                f"{relative}"
            )
        return
    for child in sorted(source.iterdir()):
        _merge_host_dependency(
            child,
            destination / child.name,
            f"{relative}/{child.name}",
            materialize=materialize,
            error_type=error_type,
        )


def link_host_dependencies(
    campaign: _CampaignSupport,
    snapshot_root: Path,
    host_root: Path,
    *,
    materialize: Callable[[Path, Path], None],
    error_type: type[Exception],
) -> None:
    """Materialize declared host-read dependencies inside a snapshot.

    A declared path is one the snapshot -- a git tree export -- cannot supply, so
    the host copy has to be filled in.  Refusing outright when the path already
    exists is wrong twice over: a directory under ``research/reports`` is only
    *partially* tracked, so the snapshot can hold the committed files and still be
    missing every gitignored one the campaign actually reads.  Merge instead --
    fill in what the snapshot lacks, leave what it already carries, and descend
    rather than deciding once for a whole directory.

    Divergence is the case worth refusing: when both copies hold the same path with
    different bytes, no receipt can be attributed to either, and that is a host
    carrying uncommitted work under a path the campaign reads.
    """

    for relative in campaign.host_read_dependencies:
        source = host_root / relative
        if not source.exists():
            raise error_type(f"host read dependency is missing: {relative}")
        _merge_host_dependency(
            source,
            snapshot_root / relative,
            relative,
            materialize=materialize,
            error_type=error_type,
        )


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


GROUP_DRAIN_SECONDS = 30


class OrphanedProcessGroupError(RuntimeError):
    """A timed-out command left descendants that SIGKILL did not reap."""


def _kill_process_group(
    proc: subprocess.Popen[str], drain_seconds: int = GROUP_DRAIN_SECONDS
) -> tuple[str, str]:
    """SIGKILL the whole session a timed-out command owns, then drain its pipes.

    `subprocess.run(..., timeout=)` kills only the direct child. Every engine
    here runs mutants out of process -- fest with `backend = "subprocess"`,
    cargo-mutants and mull the same way -- so killing the engine alone leaves
    one pytest per in-flight mutant orphaned and unbounded. On 2026-09-08 four
    such mutant processes ran 18 hours at 100% CPU after their engine was
    timed out. The command is started with `start_new_session=True`, so it is
    its own session and group leader and its pgid is its pid; one `killpg`
    reaps the engine and every mutant it spawned.
    """

    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except ProcessLookupError:
        # guardrail: allow-fallback -- the group is gone, which is the outcome
        # this call exists to produce. It races the command's own exit on every
        # run that finishes microseconds after the timeout expires, and there is
        # nothing left to report: the drain below still collects the output.
        pass
    try:
        return proc.communicate(timeout=drain_seconds)
    except subprocess.TimeoutExpired as exc:
        raise OrphanedProcessGroupError(
            f"pgid {proc.pid} still held its pipes {drain_seconds}s after "
            f"SIGKILL; surviving descendants must be killed by hand: {proc.args}"
        ) from exc


# <linux/prctl.h>. Spelled here rather than pulled from a third-party module:
# one constant, stable in the Linux ABI since 2.1.57.
PR_SET_PDEATHSIG = 1


def _parent_death_preexec(platform: str = sys.platform) -> Callable[[], None]:
    """The ``preexec_fn`` binding one spawned command to its spawner's death.

    ``prctl(PR_SET_PDEATHSIG, SIGKILL)`` makes the kernel SIGKILL this child
    the moment the THREAD that forked it exits -- thread, not process, so a
    short-lived spawning thread would tear down every command it spawned. The
    engine chain is single-threaded end to end (``python -m
    conductor.mutation_engine_generated run`` -> adapter -> ``run_command``;
    no thread is started anywhere between them), so the spawning thread is the
    main thread and the binding fires exactly when the engine process dies:
    Ctrl-C at the terminal, a session end, OOM, sandbox teardown.

    PDEATHSIG is cleared on fork, so it binds only the direct child -- the
    engine binary -- never the mutants that binary already spawned. Stopping
    the engine stops every further spawn; reaping the orphans it left behind
    is the registry's job (``reap_orphaned_runs``, below).

    ``preexec_fn`` runs Python between fork and exec, which the subprocess
    docs warn about only when other threads exist at fork time; there are
    none here. Non-Linux is refused rather than silently left unbound.
    """

    if platform != "linux":
        raise NotImplementedError(
            "run_command binds spawned commands to their spawner's death with "
            f"Linux PR_SET_PDEATHSIG; {platform!r} needs an equivalent, not a "
            "silent skip"
        )

    def bind_to_parent_death() -> None:
        libc = ctypes.CDLL(None, use_errno=True)
        if libc.prctl(PR_SET_PDEATHSIG, signal.SIGKILL, 0, 0, 0) != 0:
            error = ctypes.get_errno()
            raise OSError(error, os.strerror(error))

    return bind_to_parent_death


LIVE_PGIDS_FILENAME = "live_pgids.json"
LIVE_PGIDS_KEY = "live"


def live_pgids_path(repo_root: Path) -> Path:
    """The pgid registry: the receipts' own ``.iterations`` scratch directory.

    The same directory ``make mutation-engine-run`` writes iteration receipts
    into, so a host that redirects its receipts redirects the registry with
    them and the reaper never needs a second knob.
    """

    return (
        repo_root / receipts_relative(repo_root) / ".iterations" / LIVE_PGIDS_FILENAME
    )


def _live_pgid_entries(path: Path) -> list[dict[str, Any]]:
    """Read the registry; a missing file is empty, any other shape is refused."""

    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return []
    entries = payload.get(LIVE_PGIDS_KEY) if isinstance(payload, dict) else None
    if not isinstance(entries, list):
        raise ValueError(
            f"live pgid registry {path} is not a JSON object with a list"
            f" under {LIVE_PGIDS_KEY!r}"
        )
    return list(entries)


def record_live_pgid(path: Path, *, pgid: int, engine_pid: int, argv0: str) -> None:
    """Append one live run; engine runs are serialized, publish is atomic."""

    entries = _live_pgid_entries(path)
    entries.append({"argv0": argv0, "engine_pid": engine_pid, "pgid": pgid})
    atomic_json(path, {LIVE_PGIDS_KEY: entries})


def forget_live_pgid(path: Path, pgid: int) -> None:
    """Remove one finished run; an absent entry is the outcome this produces.

    When the last live entry is forgotten, remove the registry file (and its
    now-empty ``.iterations`` directory) instead of writing back an empty
    document -- an empty file left on every clean run dirtied the host tree
    and made the next mutation run refuse with "dirty mutation scope".
    """

    entries = _live_pgid_entries(path)
    if not any(entry.get("pgid") == pgid for entry in entries):
        return
    remaining = [e for e in entries if e.get("pgid") != pgid]
    if remaining:
        atomic_json(path, {LIVE_PGIDS_KEY: remaining})
        return
    path.unlink(missing_ok=True)
    try:
        path.parent.rmdir()
    except OSError:
        pass


def run_command(
    argv: Sequence[str],
    *,
    cwd: Path,
    timeout_seconds: int,
    environment: Mapping[str, str],
    pin_argv: Callable[[Sequence[str]], list[str]],
    result_factory: Callable[..., _CommandResultT],
    output_tail_chars: int,
    stdout_sink: Callable[[str], None] | None = None,
    pgid_registry: Path | None = None,
) -> _CommandResultT:
    """Run one bounded command and retain only the configured output tails.

    ``stdout_sink`` receives the FULL stdout before it is truncated. A test runner
    that reports per-test outcomes on stdout rather than into a file -- libtest --
    cannot be attributed from a tail, and widening the stored tail to fit would put
    megabytes of test chatter into every receipt. The sink keeps the receipt bounded
    and the attribution complete. It is called exactly once per run, including on a
    timeout, so a partial run still attributes the tests that did report.

    The command is bound to this process's lifetime with ``PR_SET_PDEATHSIG``
    and its pgid is recorded in ``pgid_registry`` (default: the host's
    ``<receipts>/.iterations/live_pgids.json``) for exactly the run's duration,
    so a run orphaned by this process's death is findable and killable by
    ``python -m conductor.mutation_testing_support reap``.
    """

    env = os.environ.copy()
    env.update(environment)
    resolved_argv = pin_argv(argv)
    registry = (
        pgid_registry if pgid_registry is not None else live_pgids_path(host_root())
    )
    preexec_fn = _parent_death_preexec()
    started = time.monotonic()
    with subprocess.Popen(
        resolved_argv,
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        # The command owns a fresh session, so the timeout path can kill every
        # process it spawned rather than only the process it started.
        start_new_session=True,
        # And it dies with this process rather than outliving it: see
        # _parent_death_preexec for what that binding does and does not cover.
        preexec_fn=preexec_fn,
    ) as proc:
        try:
            try:
                record_live_pgid(
                    registry,
                    pgid=proc.pid,
                    engine_pid=os.getpid(),
                    argv0=resolved_argv[0],
                )
            except OSError:
                # A run that cannot be reaped must not start: kill what was
                # just spawned rather than leave the first orphan of its own.
                try:
                    _kill_process_group(proc, drain_seconds=1)
                except OrphanedProcessGroupError:
                    pass
                raise
            try:
                stdout, stderr = proc.communicate(timeout=timeout_seconds)
            except subprocess.TimeoutExpired:
                stdout, stderr = _kill_process_group(proc)
                duration = time.monotonic() - started
                if stdout_sink is not None:
                    stdout_sink(stdout)
                return result_factory(
                    returncode=None,
                    timed_out=True,
                    duration_seconds=duration,
                    stdout_tail=stdout[-output_tail_chars:],
                    stderr_tail=stderr[-output_tail_chars:],
                )
            returncode = proc.returncode
        finally:
            forget_live_pgid(registry, proc.pid)
    if stdout_sink is not None:
        stdout_sink(stdout)
    return result_factory(
        returncode=returncode,
        timed_out=False,
        duration_seconds=time.monotonic() - started,
        stdout_tail=stdout[-output_tail_chars:],
        stderr_tail=stderr[-output_tail_chars:],
    )


def _pid_alive(pid: int) -> bool:
    """Signal-0 liveness; EPERM counts as alive -- that process exists."""

    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _killpg_if_present(pgid: int) -> bool:
    """SIGKILL one recorded group; False when its leader died mid-reap."""

    try:
        os.killpg(pgid, signal.SIGKILL)
    except ProcessLookupError:
        return False
    return True


def reap_orphaned_runs(registry: Path, *, apply: bool) -> tuple[list[str], int]:
    """Report runs whose engine died while the command it spawned still lives.

    PDEATHSIG stops the ENGINE the moment the process that spawned it dies,
    but the mutants that engine had already spawned inherit nothing and keep
    running: ten were measured at 100% CPU, 8-10 h old, on 2026-09-13. An
    entry is touched only when its recorded engine pid is dead while its
    recorded pgid still leads a live group -- an alive engine pid means the
    run is in flight, and no pid outside the registry is ever signalled.
    Returns the report lines and how many orphaned groups were found (killed
    when ``apply``, only listed without it).
    """

    if not registry.exists():
        return [f"no live pgid registry at {registry}"], 0
    lines: list[str] = []
    found = 0
    killed: list[int] = []
    for entry in _live_pgid_entries(registry):
        pgid = int(entry["pgid"])
        engine_pid = int(entry["engine_pid"])
        argv0 = str(entry.get("argv0", "?"))
        if _pid_alive(engine_pid):
            lines.append(
                f"pgid {pgid} ({argv0}): engine pid {engine_pid} alive"
                " -- in flight, untouched"
            )
        elif not _pid_alive(pgid):
            lines.append(
                f"pgid {pgid} ({argv0}): leader gone -- stale entry, untouched"
            )
        else:
            found += 1
            if not apply:
                lines.append(
                    f"pgid {pgid} ({argv0}): engine pid {engine_pid} dead,"
                    " group alive -- would SIGKILL (dry run)"
                )
            elif _killpg_if_present(pgid):
                killed.append(pgid)
                lines.append(
                    f"pgid {pgid} ({argv0}): engine pid {engine_pid} dead -- SIGKILLed"
                )
            else:
                lines.append(
                    f"pgid {pgid} ({argv0}): leader died before the kill"
                    " -- already gone"
                )
    for pgid in killed:
        forget_live_pgid(registry, pgid)
    return lines, found


def main(argv: Sequence[str] | None = None) -> int:
    """``python -m conductor.mutation_testing_support reap [--apply]``."""

    parser = argparse.ArgumentParser(
        prog="python -m conductor.mutation_testing_support",
        description="Reap engine runs orphaned by an engine that died mid-run.",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)
    reaper = subparsers.add_parser(
        "reap",
        help="list orphaned engine runs; --apply SIGKILLs the recorded groups",
    )
    reaper.add_argument(
        "--repo-root",
        type=Path,
        default=None,
        help="repository whose receipts tree holds the registry"
        " (default: the repository enclosing the working directory)",
    )
    reaper.add_argument(
        "--registry",
        type=Path,
        default=None,
        help="read the registry from this path instead of"
        " <receipts>/.iterations/live_pgids.json",
    )
    reaper.add_argument(
        "--apply",
        action="store_true",
        help="SIGKILL every orphaned recorded group instead of listing it",
    )
    args = parser.parse_args(argv)
    registry = args.registry or live_pgids_path(host_root(args.repo_root))
    lines, found = reap_orphaned_runs(registry, apply=args.apply)
    if not lines:
        lines = [f"live pgid registry {registry} records no runs"]
    for line in lines:
        print(line)
    # A dry run that finds orphans is a failed check: the fix is either
    # --apply or an engine that is still supposed to be alive.
    return 1 if found and not args.apply else 0


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
    # `git apply` stays the primary path, unchanged. It decides every patch it can
    # apply, byte for byte, exactly as it always has -- so any patch that has ever
    # produced a receipt is applied by the same code that produced it.
    #
    # The anchored applier runs ONLY where git refused. That state used to abort the
    # whole campaign before any mutant executed, so no receipt can exist for it, and
    # nothing an existing receipt recorded can change here. What it buys is the
    # layout fragility: git places a hunk by the line numbers in its `@@` header and
    # the three context lines around it, so an edit to a NEIGHBOURING line, or an
    # append past an EOF-anchored hunk, retires a mutant whose mutated construct is
    # untouched.
    check = subprocess.run(
        ["git", "apply", "--check", str(mutation.patch_file)],
        cwd=snapshot_root,
        check=False,
        capture_output=True,
        text=True,
    )
    if check.returncode == 0:
        applied = subprocess.run(
            ["git", "apply", str(mutation.patch_file)],
            cwd=snapshot_root,
            check=False,
            capture_output=True,
            text=True,
        )
        if applied.returncode != 0:
            raise error_type(
                f"mutation {mutation.mutation_id!r} patch failed: "
                f"{(applied.stderr or applied.stdout).strip()[:2000]}"
            )
    else:
        try:
            apply_patch_text(mutation.patch_file.read_text(), snapshot_root)
        except PatchApplyError as exc:
            raise error_type(
                f"mutation {mutation.mutation_id!r} patch failed: "
                f"{(check.stderr or check.stdout).strip()[:1000]}; "
                f"anchored retry: {str(exc)[:1000]}"
            ) from exc
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


def select_campaigns(
    campaigns: Sequence[Any],
    campaign_ids: Sequence[str] | None,
    error_type: type[Exception],
) -> list[Any]:
    """The campaigns a caller asked for, refusing ids that name none of them.

    An id matching nothing selects nothing, and a selection of nothing reports as
    "nothing drifted" -- indistinguishable from a clean tree. `run` takes a manifest
    PATH while `--campaign` takes a campaign ID, so handing over the path is the easy
    mistake, and it answered CLEAN while a pin was drifted. A filter that matches
    nothing is a question that was never asked, and must never be answered "no".
    """

    wanted = set(campaign_ids or [])
    unknown = sorted(wanted - {campaign.campaign_id for campaign in campaigns})
    if unknown:
        raise error_type(
            "no registered campaign has id "
            + ", ".join(repr(identifier) for identifier in unknown)
            + "; --campaign takes a campaign id, not a manifest path"
        )
    return [
        campaign
        for campaign in campaigns
        if not wanted or campaign.campaign_id in wanted
    ]


def receipt_directory(registry: Mapping[str, Any], error_type: type[Exception]) -> str:
    """The tracked directory the evidence gate reads receipts from.

    A receipt written anywhere else is not evidence: `research/reports/` is gitignored,
    so a receipt published there vanishes from the corpus the gate consults while the
    command that wrote it reports PASS.
    """

    directories = registry.get("receipt_directories") or []
    if not directories:
        raise error_type(
            "registry declares no receipt_directories, so a regenerated receipt "
            "has nowhere to be published where the evidence gate will read it"
        )
    return str(directories[0])


TEST_NODEID_TABLE_KEY = "test_nodeid_table"
_INTERNED_NODEID_FIELDS = ("failed_nodeids", "missing_nodeids", "ambiguous_nodeids")


class ReceiptEncodingError(Exception):
    """A receipt's interned nodeid indices do not resolve against its table."""


def intern_test_attribution(
    receipt: MutableMapping[str, Any], report: Mapping[str, Any]
) -> dict[str, Any]:
    """Index one batch report's ranked nodeids into a table shared by the receipt.

    A campaign records every ranked test's outcome for every mutant, so the same
    ~70-character nodeid is written once per (test, mutant) pair: 3591 times in the
    campaign that first crossed `max_file_bytes`, most of them beside the word
    "PASSED". Interning replaces that repetition with an integer index and nothing
    else. Outcomes, case counts and durations are carried through unchanged --
    `test_value` reads the full pass/fail matrix to compute domination and breaks
    the MERGE/CORE tie on `runtime_seconds_median`, so dropping either to save
    bytes would change verdicts.

    Only ranked nodeids are interned. `unmapped_cases` and `unranked_failures` name
    cases OUTSIDE the ranked scope; they are a different, unbounded namespace and
    stay literal so the table cannot be read as the campaign's ranked set.
    """

    table = receipt.setdefault(TEST_NODEID_TABLE_KEY, [])
    index = {nodeid: position for position, nodeid in enumerate(table)}

    def position(nodeid: str) -> int:
        if nodeid not in index:
            index[nodeid] = len(table)
            table.append(nodeid)
        return index[nodeid]

    interned = dict(report)
    tests = report.get("tests")
    if isinstance(tests, Mapping):
        interned["tests"] = {
            str(position(nodeid)): dict(row) for nodeid, row in tests.items()
        }
    for field in _INTERNED_NODEID_FIELDS:
        values = report.get(field)
        if isinstance(values, list):
            interned[field] = [position(nodeid) for nodeid in values]
    return interned


def expand_test_attribution(
    receipt: Mapping[str, Any], attribution: Mapping[str, Any]
) -> dict[str, Any]:
    """Resolve interned indices back to nodeids; the exact inverse of interning.

    A receipt written before interning carries no table and is returned unchanged,
    so one reader handles both encodings. An index the table cannot resolve is a
    corrupt receipt, not a missing test, and refuses rather than guessing.
    """

    table = receipt.get(TEST_NODEID_TABLE_KEY)
    if not isinstance(table, list):
        return dict(attribution)

    def nodeid(value: Any) -> str:
        if not isinstance(value, int) or isinstance(value, bool):
            raise ReceiptEncodingError(f"interned nodeid is not an index: {value!r}")
        if not 0 <= value < len(table):
            raise ReceiptEncodingError(
                f"interned nodeid index {value} is outside a table of {len(table)}"
            )
        return str(table[value])

    expanded = dict(attribution)
    tests = attribution.get("tests")
    if isinstance(tests, Mapping):
        expanded["tests"] = {nodeid(int(key)): dict(row) for key, row in tests.items()}
    for field in _INTERNED_NODEID_FIELDS:
        values = attribution.get(field)
        if isinstance(values, list):
            expanded[field] = [nodeid(value) for value in values]
    return expanded


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


class _RunnableCampaign(Protocol):
    """The part of a campaign that decides whether a hand run can score anything."""

    campaign_id: str
    mutation_engine: str
    mutations: Sequence[Any]
    planned_mutations: Sequence[Any]


def vacuous_run_reason(
    campaign: _RunnableCampaign, generated_engines: Container[str]
) -> str | None:
    """Why scoring `campaign` by hand would report PASS over an empty mutant set.

    Both shapes reach the hand runner with nothing to kill: a generated campaign
    whose mutants only exist once its engine has produced them, and a campaign
    that carries planned mutations but never materialized them. `None` when the
    campaign has real mutants to score.
    """

    if campaign.mutation_engine in generated_engines:
        return (
            f"campaign {campaign.campaign_id} declares the generated engine "
            f"{campaign.mutation_engine!r}. Its mutants do not exist until the "
            "engine produces them, so this runner would score an empty set and "
            "call it PASS. Run it with "
            "`python -m conductor.mutation_engine_generated run` instead."
        )
    if not campaign.mutations:
        planned = (
            f" (only {len(campaign.planned_mutations)} planned)"
            if campaign.planned_mutations
            else ""
        )
        return (
            f"campaign {campaign.campaign_id} carries no materialized mutations"
            f"{planned}. A run with nothing to kill cannot produce evidence and "
            "must not report PASS."
        )
    return None


if __name__ == "__main__":
    raise SystemExit(main())
