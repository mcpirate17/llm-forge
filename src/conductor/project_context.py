"""Frozen, read-only project-context resolution for additive tooling work.

This module deliberately does not create directories, trust project policy, or
change existing callers. Its only subprocess activity is bounded Git topology
discovery using the system Git executable specified by the T03a contract.
"""

from __future__ import annotations

import os
import selectors
import signal
import stat
import subprocess
import time
import tomllib
import unicodedata
from collections.abc import Mapping
from dataclasses import dataclass
from hashlib import sha256
from pathlib import Path
from typing import Final, Literal

Mode = Literal["git", "read_only"]
SourceKind = Literal["argument", "environment", "config", "git", "default", "derived"]
ResolvedValue = str | tuple[str, ...] | None

_GIT: Final[Path] = Path("/usr/bin/git")
_GIT_TIMEOUT_SECONDS: Final[float] = 3.0
_GIT_STDOUT_CAP: Final[int] = 32 * 1024
_GIT_STDERR_CAP: Final[int] = 4 * 1024
_CONFIG_CAP: Final[int] = 64 * 1024
_MESSAGE_CAP: Final[int] = 1_024
_GIT_ENV: Final[dict[str, str]] = {
    "PATH": "/usr/bin:/bin",
    "LC_ALL": "C",
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_OPTIONAL_LOCKS": "0",
}


@dataclass(frozen=True, slots=True)
class Provenance:
    field: str
    kind: SourceKind
    source: str
    value: ResolvedValue
    config_sha256: str | None


@dataclass(frozen=True, slots=True)
class ErrorDetail:
    code: str
    field: str | None
    message: str


class ContextError(RuntimeError):
    """A stable resolver failure with an immutable, structured detail."""

    __slots__ = ("detail",)

    def __init__(self, detail: ErrorDetail) -> None:
        if not isinstance(detail, ErrorDetail):
            raise TypeError("ContextError requires an ErrorDetail")
        self.detail = detail
        super().__init__(detail.message)


@dataclass(frozen=True, slots=True)
class ProjectContext:
    mode: Mode
    repo_root: Path
    worktree_root: Path
    git_dir: Path | None
    git_common_dir: Path | None
    repository_key: str | None
    worktree_key: str | None
    project_id: str | None
    config_path: Path | None
    policy_path: Path
    registry_path: Path
    state_dir: Path | None
    cache_dir: Path | None
    artifact_dir: Path | None
    notes_roots: tuple[Path, ...]
    provenance: tuple[Provenance, ...]


def _error(code: str, field: str | None, message: str) -> ContextError:
    return ContextError(ErrorDetail(code, field, message[:_MESSAGE_CAP]))


def _path_argument(value: Path | None, field: str) -> Path | None:
    if value is None:
        return None
    if not isinstance(value, Path):
        raise _error("INVALID_ARGUMENT", field, f"{field} must be a pathlib.Path")
    text = str(value)
    if not text or "\x00" in text or "\r" in text or "\n" in text:
        raise _error(
            "INVALID_PATH", field, f"{field} contains a forbidden path character"
        )
    return value


def _absolute(path: Path, invocation_dir: Path) -> Path:
    return path if path.is_absolute() else invocation_dir / path


def _clean_canonical(path: Path, field: str) -> Path:
    if any(mark in str(path) for mark in ("\x00", "\r", "\n")):
        raise _error(
            "INVALID_PATH", field, f"{field} resolves to a forbidden path character"
        )
    return path


def _existing_directory(path: Path, field: str) -> Path:
    try:
        resolved = path.resolve(strict=True)
    except FileNotFoundError as exc:
        raise _error("PATH_NOT_FOUND", field, f"{field} does not exist") from exc
    except (OSError, RuntimeError, ValueError) as exc:
        raise _error("INVALID_PATH", field, f"{field} cannot be resolved") from exc
    if not resolved.is_dir():
        raise _error("PATH_NOT_DIRECTORY", field, f"{field} must be a directory")
    return _clean_canonical(resolved, field)


def _inside(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
    except ValueError:
        return False
    return True


def _resolved_reference(path: Path, anchor: Path, root: Path, field: str) -> Path:
    candidate = _absolute(path, anchor)
    try:
        resolved = candidate.resolve(strict=False)
    except (OSError, RuntimeError, ValueError) as exc:
        raise _error("INVALID_PATH", field, f"{field} cannot be resolved") from exc
    if not _inside(resolved, root):
        raise _error(
            "PATH_OUTSIDE_PROJECT",
            field,
            f"{field} must remain inside the selected project",
        )
    return _clean_canonical(resolved, field)


def _resolved_note(path: Path, anchor: Path, field: str) -> Path:
    try:
        return _clean_canonical(_absolute(path, anchor).resolve(strict=False), field)
    except (OSError, RuntimeError, ValueError) as exc:
        raise _error("INVALID_PATH", field, f"{field} cannot be resolved") from exc


def _existing_regular(path: Path, field: str, code: str) -> None:
    try:
        file_stat = path.stat()
    except FileNotFoundError as exc:
        raise _error(code, field, f"{field} does not exist") from exc
    except (OSError, RuntimeError, ValueError) as exc:
        raise _error(code, field, f"{field} cannot be inspected") from exc
    if not stat.S_ISREG(file_stat.st_mode):
        raise _error(code, field, f"{field} must be a regular file")


def _provenance(
    field: str,
    kind: SourceKind,
    source: str,
    value: ResolvedValue,
    config_sha256: str | None = None,
) -> Provenance:
    return Provenance(field, kind, source, value, config_sha256)


def _bounded_git(argv: tuple[str, ...]) -> bytes:
    if not _GIT.is_file() or not os.access(_GIT, os.X_OK):
        raise _error(
            "GIT_UNAVAILABLE",
            None,
            "trusted /usr/bin/git is not an executable regular file",
        )
    try:
        process = subprocess.Popen(
            (_GIT.as_posix(), *argv),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=dict(_GIT_ENV),
            start_new_session=True,
        )
    except OSError as exc:
        raise _error(
            "GIT_UNAVAILABLE", None, "trusted /usr/bin/git could not be started"
        ) from exc

    def terminate() -> None:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            # guardrail: allow-fallback -- the group is already gone. git exited
            # between the deadline check and the kill, which is the ordinary race
            # on a fast probe, not a failure to report: process.wait() below still
            # reaps it and the caller still sees the timeout/exit status it earned.
            pass
        process.wait()

    assert process.stdout is not None
    assert process.stderr is not None
    streams: dict[int, str] = {
        process.stdout.fileno(): "stdout",
        process.stderr.fileno(): "stderr",
    }
    selector = selectors.DefaultSelector()
    output = {"stdout": bytearray(), "stderr": bytearray()}
    deadline = time.monotonic() + _GIT_TIMEOUT_SECONDS
    failure: tuple[str, str] | None = None
    try:
        for fd in streams:
            selector.register(fd, selectors.EVENT_READ)
        while streams:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                failure = (
                    "GIT_TIMEOUT",
                    "Git discovery exceeded its 3-second deadline",
                )
                break
            for key, _events in selector.select(remaining):
                fd = key.fd
                name = streams[fd]
                chunk = os.read(fd, 8_192)
                if not chunk:
                    selector.unregister(fd)
                    streams.pop(fd)
                    continue
                cap = _GIT_STDOUT_CAP if name == "stdout" else _GIT_STDERR_CAP
                remaining_cap = cap - len(output[name])
                if len(chunk) > remaining_cap:
                    output[name].extend(chunk[:remaining_cap])
                    failure = (
                        "GIT_OUTPUT_LIMIT",
                        f"Git {name} exceeded its output limit",
                    )
                    break
                output[name].extend(chunk)
            if failure is not None:
                break
        if failure is not None:
            terminate()
            raise _error(failure[0], None, failure[1])
        returncode = process.wait(timeout=max(0.0, deadline - time.monotonic()))
    except subprocess.TimeoutExpired as exc:
        terminate()
        raise _error(
            "GIT_TIMEOUT", None, "Git discovery exceeded its 3-second deadline"
        ) from exc
    except OSError as exc:
        terminate()
        raise _error(
            "GIT_DISCOVERY_FAILED", None, "Git discovery pipe I/O failed"
        ) from exc
    finally:
        selector.close()
        process.stdout.close()
        process.stderr.close()
    if returncode != 0:
        raise _error(
            "GIT_DISCOVERY_FAILED",
            None,
            "Git discovery failed for the selected project",
        )
    return bytes(output["stdout"])


def _git_paths(selected: Path) -> tuple[Path, Path, Path]:
    probe = _bounded_git(
        (
            "-C",
            str(selected),
            "rev-parse",
            "--is-inside-work-tree",
            "--is-bare-repository",
        )
    )
    if probe != b"true\nfalse\n":
        if probe in {b"false\nfalse\n", b"false\ntrue\n", b"true\ntrue\n"}:
            raise _error(
                "UNSUPPORTED_REPOSITORY",
                "project",
                "selected project is not a non-bare Git worktree",
            )
        raise _error(
            "GIT_DISCOVERY_FAILED",
            "project",
            "Git returned malformed worktree discovery output",
        )

    def topology(directory: Path) -> tuple[Path, Path, Path]:
        raw = _bounded_git(
            (
                "-C",
                str(directory),
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--absolute-git-dir",
                "--git-common-dir",
            )
        )
        if raw.endswith(b"\n"):
            raw = raw[:-1]
        parts = raw.split(b"\n")
        if len(parts) != 3 or any(not part for part in parts):
            raise _error(
                "GIT_DISCOVERY_FAILED",
                "project",
                "Git returned malformed topology output",
            )
        try:
            texts = tuple(os.fsdecode(part) for part in parts)
            if any(
                not Path(text).is_absolute()
                or any(mark in text for mark in ("\x00", "\r", "\n"))
                for text in texts
            ):
                raise ValueError("non-absolute or malformed Git path")
            values = tuple(
                _clean_canonical(Path(text).resolve(strict=True), "project")
                for text in texts
            )
        except (OSError, RuntimeError, UnicodeError, ValueError) as exc:
            raise _error(
                "GIT_DISCOVERY_FAILED",
                "project",
                "Git returned an unreadable topology path",
            ) from exc
        top, git_dir, common_dir = values
        if not top.is_dir() or not git_dir.is_dir() or not common_dir.is_dir():
            raise _error(
                "GIT_DISCOVERY_FAILED",
                "project",
                "Git returned a non-directory topology path",
            )
        return top, git_dir, common_dir

    top, git_dir, common_dir = topology(selected)
    if not _inside(selected, top):
        raise _error(
            "PROJECT_MISMATCH",
            "project",
            "selected directory is outside Git worktree top level",
        )
    if topology(top) != (top, git_dir, common_dir):
        raise _error(
            "PROJECT_MISMATCH", "project", "Git topology changed during discovery"
        )
    return top, git_dir, common_dir


@dataclass(frozen=True, slots=True)
class _Config:
    path: Path | None
    sha256: str | None
    project_id: str | None
    policy: str | None
    registry: str | None
    notes: tuple[str, ...]


def _read_config(candidate: Path, explicit: bool, root: Path) -> _Config:
    _validate_config_ancestry(candidate, root)
    try:
        dangling = candidate.is_symlink() and not candidate.exists()
        exists = candidate.exists()
    except (OSError, RuntimeError, ValueError) as exc:
        raise _error(
            "CONFIG_IO", "config", "configuration cannot be inspected"
        ) from exc
    if dangling:
        raise _error(
            "CONFIG_IO", "config", "configuration symlink target does not exist"
        )
    resolved = _resolved_reference(candidate, candidate.parent, root, "config")
    if not exists:
        if explicit:
            raise _error(
                "CONFIG_NOT_FOUND", "config", "explicit configuration does not exist"
            )
        return _Config(None, None, None, None, None, ())
    _existing_regular(resolved, "config", "CONFIG_IO")
    raw, before, during, after = _read_config_bytes(resolved, root)
    if len(raw) > _CONFIG_CAP:
        raise _error("CONFIG_TOO_LARGE", "config", "configuration exceeds 65536 bytes")
    if _stat_signature(before) != _stat_signature(during) or _stat_signature(
        before
    ) != _stat_signature(after):
        raise _error(
            "INPUT_CHANGED", "config", "configuration changed while it was read"
        )
    return _parse_config(raw, resolved)


def _stat_signature(entry: os.stat_result) -> tuple[int, int, int, int]:
    return entry.st_dev, entry.st_ino, entry.st_size, entry.st_mtime_ns


def _read_config_bytes(
    path: Path, root: Path
) -> tuple[bytes, os.stat_result, os.stat_result, os.stat_result]:
    try:
        before = path.stat()
        descriptor = _open_contained_regular(root, path)
        try:
            during = os.fstat(descriptor)
            if not stat.S_ISREG(during.st_mode):
                raise _error(
                    "CONFIG_IO", "config", "opened configuration is not a regular file"
                )
            chunks: list[bytes] = []
            remaining = _CONFIG_CAP + 1
            while remaining:
                chunk = os.read(descriptor, remaining)
                if not chunk:
                    break
                chunks.append(chunk)
                remaining -= len(chunk)
        finally:
            os.close(descriptor)
        after = path.stat()
    except OSError as exc:
        raise _error("CONFIG_IO", "config", "configuration could not be read") from exc
    return b"".join(chunks), before, during, after


def _open_contained_regular(root: Path, path: Path) -> int:
    """Open a canonical config path through no-follow directory descriptors."""
    try:
        relative = path.relative_to(root)
    except ValueError as exc:
        raise _error(
            "PATH_OUTSIDE_PROJECT",
            "config",
            "config must remain inside the selected project",
        ) from exc
    directory_flags = (
        os.O_RDONLY
        | os.O_DIRECTORY
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    file_flags = (
        os.O_RDONLY
        | os.O_NONBLOCK
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    directory = os.open(root, directory_flags)
    try:
        for component in relative.parts[:-1]:
            next_directory = os.open(component, directory_flags, dir_fd=directory)
            os.close(directory)
            directory = next_directory
        return os.open(relative.name, file_flags, dir_fd=directory)
    finally:
        os.close(directory)


def _parse_config(raw: bytes, resolved: Path) -> _Config:
    try:
        parsed = tomllib.loads(raw.decode("utf-8", "strict"))
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as exc:
        raise _error(
            "CONFIG_PARSE", "config", "configuration is not strict UTF-8 TOML"
        ) from exc
    project, paths = _config_tables(parsed)
    project_id = _config_project_id(project)
    policy, registry, notes = _config_paths(paths)
    return _Config(
        resolved, sha256(raw).hexdigest(), project_id, policy, registry, notes
    )


def _validate_config_ancestry(candidate: Path, root: Path) -> None:
    """Distinguish an absent default file from a dangling ancestor link."""
    try:
        ancestors = candidate.relative_to(root).parts[:-1]
    except ValueError:
        return
    cursor = root
    for component in ancestors:
        cursor = cursor / component
        try:
            entry = cursor.lstat()
        except FileNotFoundError:
            return
        except OSError as exc:
            raise _error(
                "CONFIG_IO", "config", "configuration ancestry cannot be inspected"
            ) from exc
        if stat.S_ISLNK(entry.st_mode):
            try:
                target = cursor.resolve(strict=True)
            except (OSError, RuntimeError, ValueError) as exc:
                raise _error(
                    "CONFIG_IO",
                    "config",
                    "configuration ancestry has a dangling symlink",
                ) from exc
            if not target.is_dir():
                raise _error(
                    "CONFIG_IO", "config", "configuration ancestry is not a directory"
                )
        elif not stat.S_ISDIR(entry.st_mode):
            raise _error(
                "CONFIG_IO", "config", "configuration ancestry is not a directory"
            )


def _config_tables(parsed: object) -> tuple[dict[str, object], dict[str, object]]:
    if not isinstance(parsed, dict) or set(parsed) - {
        "schema_version",
        "project",
        "paths",
    }:
        raise _error(
            "CONFIG_SCHEMA", "config", "configuration contains unknown top-level keys"
        )
    version = parsed.get("schema_version")
    if type(version) is not int or version != 1:
        raise _error("CONFIG_SCHEMA", "config", "schema_version must be integer 1")
    project, paths = parsed.get("project", {}), parsed.get("paths", {})
    if not isinstance(project, dict) or set(project) - {"id"}:
        raise _error("CONFIG_SCHEMA", "config", "[project] only supports id")
    if not isinstance(paths, dict) or set(paths) - {"policy", "registry", "notes"}:
        raise _error("CONFIG_SCHEMA", "config", "[paths] has unknown keys")
    return project, paths


def _config_project_id(project: Mapping[str, object]) -> str | None:
    project_id = project.get("id")
    valid = isinstance(project_id, str) and 1 <= len(project_id) <= 128
    valid = (
        valid
        and project_id == project_id.strip()
        and not any(unicodedata.category(char) == "Cc" for char in project_id)
    )
    if project_id is not None and not valid:
        raise _error(
            "CONFIG_SCHEMA",
            "project.id",
            "project.id must be a trimmed 1-128 character non-control string",
        )
    return project_id


def _config_paths(
    paths: Mapping[str, object],
) -> tuple[str | None, str | None, tuple[str, ...]]:
    policy, registry = paths.get("policy"), paths.get("registry")
    for field, value in (("policy", policy), ("registry", registry)):
        if value is not None and (
            not isinstance(value, str)
            or not value
            or any(mark in value for mark in ("\x00", "\r", "\n"))
        ):
            raise _error(
                "CONFIG_SCHEMA",
                f"paths.{field}",
                f"paths.{field} must be a non-empty string",
            )
    notes = paths.get("notes", [])
    if not isinstance(notes, list) or any(
        not isinstance(item, str)
        or not item
        or any(mark in item for mark in ("\x00", "\r", "\n"))
        for item in notes
    ):
        raise _error(
            "CONFIG_SCHEMA",
            "paths.notes",
            "paths.notes must be an array of non-empty strings",
        )
    return policy, registry, tuple(notes)


def _safe_derived_directory(common_dir: Path, path: Path, field: str) -> None:
    cursor = common_dir
    for component in path.relative_to(common_dir).parts:
        cursor = cursor / component
        try:
            item = cursor.lstat()
        except FileNotFoundError:
            return
        except (OSError, RuntimeError, ValueError) as exc:
            raise _error(
                "UNSAFE_STATE_PATH", field, f"{field} cannot be inspected"
            ) from exc
        if stat.S_ISLNK(item.st_mode) or not stat.S_ISDIR(item.st_mode):
            raise _error(
                "UNSAFE_STATE_PATH", field, f"{field} has an unsafe existing ancestor"
            )


def _key(prefix: str, payload: bytes) -> str:
    return prefix + sha256(payload).hexdigest()


@dataclass(frozen=True, slots=True)
class _Inputs:
    project: Path | None
    config: Path | None
    policy: Path | None
    registry: Path | None
    invocation_dir: Path
    selected: Path
    selection: Provenance


@dataclass(frozen=True, slots=True)
class _Topology:
    root: Path
    git_dir: Path | None
    common_dir: Path | None
    repository_key: str | None
    worktree_key: str | None


def _captured_environment(environment: Mapping[str, str] | None) -> Mapping[str, str]:
    if environment is None:
        return dict(os.environ)
    if isinstance(environment, Mapping) and all(
        isinstance(key, str) and isinstance(value, str)
        for key, value in environment.items()
    ):
        return dict(environment)
    raise _error(
        "INVALID_ARGUMENT", "environment", "environment must map strings to strings"
    )


def _invocation_directory(start_dir: Path | None) -> Path:
    try:
        cwd = Path.cwd()
    except (OSError, RuntimeError) as exc:
        raise _error(
            "INVALID_PATH", "start_dir", "current working directory cannot be resolved"
        ) from exc
    return _existing_directory(
        _absolute(start_dir, cwd) if start_dir else cwd, "start_dir"
    )


def _select_project(
    project: Path | None, invocation_dir: Path, environment: Mapping[str, str]
) -> tuple[Path, Provenance]:
    if project is not None:
        selected = _existing_directory(_absolute(project, invocation_dir), "project")
        return selected, _provenance("repo_root", "argument", "project", str(selected))
    if "CONDUCTOR_PROJECT_DIR" not in environment:
        return invocation_dir, _provenance(
            "repo_root", "default", "invocation directory", str(invocation_dir)
        )
    raw_project = environment["CONDUCTOR_PROJECT_DIR"]
    if not raw_project:
        raise _error(
            "INVALID_ARGUMENT",
            "CONDUCTOR_PROJECT_DIR",
            "CONDUCTOR_PROJECT_DIR must not be empty",
        )
    if (
        any(mark in raw_project for mark in ("\x00", "\r", "\n"))
        or not Path(raw_project).is_absolute()
    ):
        raise _error(
            "INVALID_PATH",
            "CONDUCTOR_PROJECT_DIR",
            "CONDUCTOR_PROJECT_DIR must be an absolute clean path",
        )
    selected = _existing_directory(Path(raw_project), "CONDUCTOR_PROJECT_DIR")
    return selected, _provenance(
        "repo_root", "environment", "CONDUCTOR_PROJECT_DIR", str(selected)
    )


def _resolve_inputs(
    project: Path | None,
    config: Path | None,
    policy: Path | None,
    registry: Path | None,
    start_dir: Path | None,
    environment: Mapping[str, str] | None,
) -> _Inputs:
    project = _path_argument(project, "project")
    config = _path_argument(config, "config")
    policy = _path_argument(policy, "policy")
    registry = _path_argument(registry, "registry")
    start_dir = _path_argument(start_dir, "start_dir")
    invocation_dir = _invocation_directory(start_dir)
    selected, selection = _select_project(
        project, invocation_dir, _captured_environment(environment)
    )
    return _Inputs(
        project, config, policy, registry, invocation_dir, selected, selection
    )


def _resolve_topology(mode: Mode, inputs: _Inputs) -> _Topology:
    if mode == "read_only":
        if inputs.project is None:
            raise _error(
                "INVALID_ARGUMENT",
                "project",
                "read_only mode requires an explicit project directory",
            )
        return _Topology(inputs.selected, None, None, None, None)
    root, git_dir, common_dir = _git_paths(inputs.selected)
    repository_key = _key(
        "repo-v1-", b"conductor.repository.v1\0" + os.fsencode(str(common_dir))
    )
    worktree_key = _key(
        "wt-v1-",
        b"conductor.worktree.v1\0"
        + os.fsencode(str(git_dir))
        + b"\0"
        + os.fsencode(str(root)),
    )
    return _Topology(root, git_dir, common_dir, repository_key, worktree_key)


def _resolve_reference(
    field: str,
    argument: Path | None,
    configured: str | None,
    default: Path,
    inputs: _Inputs,
    config: _Config,
    root: Path,
) -> tuple[Path, Provenance]:
    if argument is not None:
        resolved, kind, source, digest = (
            _resolved_reference(argument, inputs.invocation_dir, root, field),
            "argument",
            field,
            None,
        )
    elif configured is not None:
        resolved, kind, source, digest = (
            _resolved_reference(Path(configured), config.path.parent, root, field),
            "config",
            f"{config.path}#paths.{field}",
            config.sha256,
        )
    else:
        resolved, kind, source, digest = (
            _resolved_reference(default, root, root, field),
            "default",
            str(default),
            None,
        )
    try:
        exists = resolved.exists()
    except (OSError, RuntimeError, ValueError) as exc:
        raise _error("INVALID_PATH", field, f"{field} cannot be inspected") from exc
    if exists:
        _existing_regular(resolved, field, "INVALID_PATH")
    return resolved, _provenance(field + "_path", kind, source, str(resolved), digest)


def _allowed_notes_roots(
    roots: tuple[Path, ...], invocation_dir: Path
) -> tuple[Path, ...]:
    resolved: list[Path] = []
    for item in roots:
        if not isinstance(item, Path):
            raise _error(
                "INVALID_ARGUMENT",
                "allowed_external_notes_roots",
                "allowed external roots must be pathlib.Path values",
            )
        resolved.append(
            _existing_directory(
                _absolute(
                    _path_argument(item, "allowed_external_notes_roots") or Path(),
                    invocation_dir,
                ),
                "allowed_external_notes_roots",
            )
        )
    return tuple(resolved)


def _resolve_notes(
    config: _Config, root: Path, allowed: tuple[Path, ...]
) -> tuple[Path, ...]:
    notes: list[Path] = []
    for value in config.notes:
        resolved = _resolved_note(Path(value), config.path.parent, "paths.notes")
        try:
            is_directory = resolved.exists() and resolved.is_dir()
        except (OSError, RuntimeError, ValueError) as exc:
            raise _error(
                "INVALID_PATH", "paths.notes", "notes root cannot be inspected"
            ) from exc
        if not is_directory:
            raise _error(
                "PATH_NOT_DIRECTORY",
                "paths.notes",
                "each notes root must be an existing directory",
            )
        if not _inside(resolved, root) and not any(
            _inside(resolved, item) for item in allowed
        ):
            raise _error(
                "PATH_OUTSIDE_PROJECT",
                "paths.notes",
                "external notes root lacks a caller grant",
            )
        if resolved not in notes:
            notes.append(resolved)
    return tuple(notes)


def _derived_directories(
    topology: _Topology,
) -> tuple[Path | None, Path | None, Path | None]:
    if topology.common_dir is None:
        return None, None, None
    state = (
        topology.common_dir / "conductor" / "projects" / str(topology.repository_key)
    )
    cache = state / "worktrees" / str(topology.worktree_key) / "cache"
    artifacts = state / "worktrees" / str(topology.worktree_key) / "artifacts"
    for field, value in (
        ("state_dir", state),
        ("cache_dir", cache),
        ("artifact_dir", artifacts),
    ):
        _safe_derived_directory(topology.common_dir, value, field)
    return state, cache, artifacts


def resolve_project_context(
    *,
    project: Path | None = None,
    config: Path | None = None,
    policy: Path | None = None,
    registry: Path | None = None,
    start_dir: Path | None = None,
    environment: Mapping[str, str] | None = None,
    mode: Mode = "git",
    allowed_external_notes_roots: tuple[Path, ...] = (),
) -> ProjectContext:
    """Resolve a project without side effects or policy interpretation."""
    if mode not in ("git", "read_only"):
        raise _error("INVALID_ARGUMENT", "mode", "mode must be 'git' or 'read_only'")
    if not isinstance(allowed_external_notes_roots, tuple):
        raise _error(
            "INVALID_ARGUMENT",
            "allowed_external_notes_roots",
            "allowed_external_notes_roots must be a tuple",
        )
    inputs = _resolve_inputs(project, config, policy, registry, start_dir, environment)
    topology = _resolve_topology(mode, inputs)
    candidate = (
        _absolute(inputs.config, inputs.invocation_dir)
        if inputs.config
        else topology.root / ".conductor" / "project.toml"
    )
    parsed = _read_config(candidate, inputs.config is not None, topology.root)
    policy_path, policy_provenance = _resolve_reference(
        "policy",
        inputs.policy,
        parsed.policy,
        topology.root / ".conductor" / "policy.toml",
        inputs,
        parsed,
        topology.root,
    )
    registry_path, registry_provenance = _resolve_reference(
        "registry",
        inputs.registry,
        parsed.registry,
        topology.root / ".conductor" / "mutation" / "registry.json",
        inputs,
        parsed,
        topology.root,
    )
    notes = _resolve_notes(
        parsed,
        topology.root,
        _allowed_notes_roots(allowed_external_notes_roots, inputs.invocation_dir),
    )
    state_dir, cache_dir, artifact_dir = _derived_directories(topology)
    return _project_context(
        mode,
        inputs,
        topology,
        parsed,
        candidate,
        policy_path,
        registry_path,
        policy_provenance,
        registry_provenance,
        notes,
        state_dir,
        cache_dir,
        artifact_dir,
    )


def _project_context(
    mode: Mode,
    inputs: _Inputs,
    topology: _Topology,
    config: _Config,
    candidate: Path,
    policy_path: Path,
    registry_path: Path,
    policy_provenance: Provenance,
    registry_provenance: Provenance,
    notes: tuple[Path, ...],
    state_dir: Path | None,
    cache_dir: Path | None,
    artifact_dir: Path | None,
) -> ProjectContext:
    project_id, identity_provenance = _identity_provenance(topology, config)
    selection = _provenance(
        "repo_root", inputs.selection.kind, inputs.selection.source, str(topology.root)
    )
    provenance = (
        (_provenance("mode", "argument", "mode", mode), selection)
        + _topology_provenance(topology)
        + identity_provenance
        + _config_provenance(inputs, config, candidate)
        + (policy_provenance, registry_provenance)
        + _state_provenance(state_dir, cache_dir, artifact_dir)
        + _notes_provenance(config, notes)
    )
    return ProjectContext(
        mode,
        topology.root,
        topology.root,
        topology.git_dir,
        topology.common_dir,
        topology.repository_key,
        topology.worktree_key,
        project_id,
        config.path,
        policy_path,
        registry_path,
        state_dir,
        cache_dir,
        artifact_dir,
        notes,
        provenance,
    )


def _topology_provenance(topology: _Topology) -> tuple[Provenance, ...]:
    is_git = topology.git_dir is not None
    return (
        _provenance(
            "worktree_root",
            "git" if is_git else "argument",
            "git:show-toplevel" if is_git else "project",
            str(topology.root),
        ),
        _provenance(
            "git_dir",
            "git" if is_git else "default",
            "git:absolute-git-dir" if is_git else "read_only",
            str(topology.git_dir) if is_git else None,
        ),
        _provenance(
            "git_common_dir",
            "git" if is_git else "default",
            "git:git-common-dir" if is_git else "read_only",
            str(topology.common_dir) if is_git else None,
        ),
    )


def _identity_provenance(
    topology: _Topology, config: _Config
) -> tuple[str | None, tuple[Provenance, ...]]:
    project_id = (
        config.project_id if config.project_id is not None else topology.repository_key
    )
    derived = topology.repository_key is not None
    project_kind: SourceKind = (
        "config"
        if config.project_id is not None
        else ("derived" if derived else "default")
    )
    project_source = (
        f"{config.path}#project.id"
        if config.project_id is not None
        else ("repository_key" if derived else "read_only")
    )
    entries = (
        _provenance(
            "repository_key",
            "derived" if derived else "default",
            "repository_key" if derived else "read_only",
            topology.repository_key,
        ),
        _provenance(
            "worktree_key",
            "derived" if derived else "default",
            "worktree_key" if derived else "read_only",
            topology.worktree_key,
        ),
        _provenance(
            "project_id",
            project_kind,
            project_source,
            project_id,
            config.sha256 if config.project_id is not None else None,
        ),
    )
    return project_id, entries


def _config_provenance(
    inputs: _Inputs, config: _Config, candidate: Path
) -> tuple[Provenance, ...]:
    return (
        _provenance(
            "config_path",
            "argument" if inputs.config else "default",
            str(config.path) if config.path else str(candidate),
            str(config.path) if config.path else None,
            config.sha256 if config.path else None,
        ),
    )


def _state_provenance(
    state: Path | None, cache: Path | None, artifacts: Path | None
) -> tuple[Provenance, ...]:
    return (
        _provenance(
            "state_dir",
            "derived" if state else "default",
            "git_common_dir" if state else "read_only",
            str(state) if state else None,
        ),
        _provenance(
            "cache_dir",
            "derived" if cache else "default",
            "state_dir/worktree_key" if cache else "read_only",
            str(cache) if cache else None,
        ),
        _provenance(
            "artifact_dir",
            "derived" if artifacts else "default",
            "state_dir/worktree_key" if artifacts else "read_only",
            str(artifacts) if artifacts else None,
        ),
    )


def _notes_provenance(
    config: _Config, notes: tuple[Path, ...]
) -> tuple[Provenance, ...]:
    return (
        _provenance(
            "notes_roots",
            "config" if config.notes else "default",
            f"{config.path}#paths.notes" if config.notes else "empty notes default",
            tuple(str(item) for item in notes),
            config.sha256 if config.notes else None,
        ),
    )


def require_git_context(context: ProjectContext) -> None:
    """Refuse a context that was resolved without Git worktree capability."""
    if not isinstance(context, ProjectContext) or context.mode != "git":
        raise _error(
            "UNSUPPORTED_REPOSITORY",
            "context",
            "this operation requires a Git project context",
        )
