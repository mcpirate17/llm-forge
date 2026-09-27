"""Frozen, read-only project-context resolution for additive tooling work.

This module deliberately does not create directories, trust project policy, or
change existing callers. Its only subprocess activity is bounded Git topology
discovery using the system Git executable specified by the T03a contract.
"""

from __future__ import annotations

import json
import os
import stat
from collections.abc import Mapping
from dataclasses import dataclass
from hashlib import sha256
from pathlib import Path
from typing import Final, Literal

Mode = Literal["git", "read_only"]
SourceKind = Literal["argument", "environment", "config", "git", "default", "derived"]
ResolvedValue = str | tuple[str, ...] | None

_MESSAGE_CAP: Final[int] = 1_024


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


def _path_hex(path: Path) -> str:
    return os.fsencode(path).hex()


def _path_from_hex(raw: str) -> Path:
    return Path(os.fsdecode(bytes.fromhex(raw)))


def _native_decide(operation: str, payload: dict[str, object]) -> dict[str, object]:
    from conductor._native import project_context_native

    result = json.loads(project_context_native(operation, json.dumps(payload)))
    if detail := result.get("error"):
        raise _error(detail["code"], detail["field"], detail["message"])
    return result["value"]


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


def _resolved_reference(path: Path, anchor: Path, root: Path, field: str) -> Path:
    candidate = _absolute(path, anchor)
    try:
        resolved = candidate.resolve(strict=False)
    except (OSError, RuntimeError, ValueError) as exc:
        raise _error("INVALID_PATH", field, f"{field} cannot be resolved") from exc
    _native_decide(
        "reference-inside",
        {"path_hex": _path_hex(resolved), "root_hex": _path_hex(root), "field": field},
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
    result = _native_decide(
        "bounded-git", {"argv_hex": [os.fsencode(item).hex() for item in argv]}
    )
    return bytes.fromhex(result["stdout_hex"])


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
    _native_decide("git-probe", {"raw_hex": probe.hex()})

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
        parsed = _native_decide("git-topology", {"raw_hex": raw.hex()})
        return tuple(_path_from_hex(item) for item in parsed["paths_hex"])

    top, git_dir, common_dir = topology(selected)
    _native_decide(
        "git-membership",
        {"selected_hex": _path_hex(selected), "top_hex": _path_hex(top)},
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
    raw, signatures = _read_config_bytes(resolved, root)
    _native_decide(
        "config-read",
        {
            "size": len(raw),
            "signatures": signatures,
        },
    )
    return _parse_config(raw, resolved)


def _read_config_bytes(path: Path, root: Path) -> tuple[bytes, list[list[int]]]:
    """Read through native no-follow descriptors, retaining this patchable seam."""
    result = _native_decide(
        "read-config-bytes", {"path_hex": _path_hex(path), "root_hex": _path_hex(root)}
    )
    return bytes.fromhex(result["raw_hex"]), result["signatures"]


def _parse_config(raw: bytes, resolved: Path) -> _Config:
    from conductor._native import project_context_parse_config_native

    try:
        parsed = json.loads(project_context_parse_config_native(raw))
    except ValueError as exc:
        raise _error(
            "CONFIG_PARSE" if str(exc).startswith("CONFIG_PARSE:") else "CONFIG_SCHEMA",
            "config",
            str(exc).split(":", 1)[-1],
        ) from exc
    return _Config(
        resolved,
        sha256(raw).hexdigest(),
        parsed["project_id"],
        parsed["policy"],
        parsed["registry"],
        tuple(parsed["notes"]),
    )


def _validate_config_ancestry(candidate: Path, root: Path) -> None:
    """Distinguish an absent default file from a dangling ancestor link."""
    _native_decide(
        "config-ancestry",
        {"candidate_hex": _path_hex(candidate), "root_hex": _path_hex(root)},
    )


def _safe_derived_directory(common_dir: Path, path: Path, field: str) -> None:
    _native_decide(
        "safe-derived",
        {
            "common_hex": _path_hex(common_dir),
            "path_hex": _path_hex(path),
            "field": field,
        },
    )


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
    selected = _native_decide(
        "select-project",
        {
            "invocation_hex": _path_hex(invocation_dir),
            "argument_hex": _path_hex(project) if project is not None else None,
            "environment_hex": (
                os.fsencode(environment["CONDUCTOR_PROJECT_DIR"]).hex()
                if "CONDUCTOR_PROJECT_DIR" in environment
                else None
            ),
        },
    )
    path = _path_from_hex(selected["selected_hex"])
    return path, _provenance(
        "repo_root", selected["kind"], selected["source"], str(path)
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
    keys = _native_decide(
        "keys",
        {
            "common_hex": _path_hex(common_dir),
            "git_hex": _path_hex(git_dir),
            "root_hex": _path_hex(root),
        },
    )
    return _Topology(
        root, git_dir, common_dir, keys["repository_key"], keys["worktree_key"]
    )


def _resolve_reference(
    field: str,
    argument: Path | None,
    configured: str | None,
    default: Path,
    inputs: _Inputs,
    config: _Config,
    root: Path,
) -> Path:
    if argument is not None:
        resolved = _resolved_reference(argument, inputs.invocation_dir, root, field)
    elif configured is not None:
        resolved = _resolved_reference(
            Path(configured), config.path.parent, root, field
        )
    else:
        resolved = _resolved_reference(default, root, root, field)
    try:
        exists = resolved.exists()
    except (OSError, RuntimeError, ValueError) as exc:
        raise _error("INVALID_PATH", field, f"{field} cannot be inspected") from exc
    if exists:
        _existing_regular(resolved, field, "INVALID_PATH")
    return resolved


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
        _native_decide(
            "note-authorized",
            {
                "path_hex": _path_hex(resolved),
                "root_hex": _path_hex(root),
                "allowed_hex": [_path_hex(item) for item in allowed],
            },
        )
        if resolved not in notes:
            notes.append(resolved)
    return tuple(notes)


def _derived_directories(
    topology: _Topology,
) -> tuple[Path | None, Path | None, Path | None]:
    if topology.common_dir is None:
        return None, None, None
    layout = _native_decide(
        "derived-layout",
        {
            "common_hex": _path_hex(topology.common_dir),
            "repository_key": topology.repository_key,
            "worktree_key": topology.worktree_key,
        },
    )
    state = _path_from_hex(layout["state_hex"])
    cache = _path_from_hex(layout["cache_hex"])
    artifacts = _path_from_hex(layout["artifact_hex"])
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
    policy_path = _resolve_reference(
        "policy",
        inputs.policy,
        parsed.policy,
        topology.root / ".conductor" / "policy.toml",
        inputs,
        parsed,
        topology.root,
    )
    registry_path = _resolve_reference(
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
    notes: tuple[Path, ...],
    state_dir: Path | None,
    cache_dir: Path | None,
    artifact_dir: Path | None,
) -> ProjectContext:
    assembled = _native_decide(
        "assemble",
        {
            "mode": mode,
            "selection_kind": inputs.selection.kind,
            "selection_source": inputs.selection.source,
            "root_hex": _path_hex(topology.root),
            "git_hex": _path_hex(topology.git_dir) if topology.git_dir else None,
            "common_hex": _path_hex(topology.common_dir)
            if topology.common_dir
            else None,
            "repository_key": topology.repository_key,
            "worktree_key": topology.worktree_key,
            "config_project_id": config.project_id,
            "config_path_hex": _path_hex(config.path) if config.path else None,
            "config_candidate_hex": _path_hex(candidate),
            "config_sha256": config.sha256,
            "config_explicit": inputs.config is not None,
            "policy_path_hex": _path_hex(policy_path),
            "policy_argument": inputs.policy is not None,
            "policy_configured": config.policy is not None,
            "registry_path_hex": _path_hex(registry_path),
            "registry_argument": inputs.registry is not None,
            "registry_configured": config.registry is not None,
            "state_hex": _path_hex(state_dir) if state_dir else None,
            "cache_hex": _path_hex(cache_dir) if cache_dir else None,
            "artifact_hex": _path_hex(artifact_dir) if artifact_dir else None,
            "notes_hex": [_path_hex(item) for item in notes],
            "notes_configured": bool(config.notes),
        },
    )
    provenance = tuple(_native_provenance(item) for item in assembled["provenance"])
    return ProjectContext(
        mode,
        topology.root,
        topology.root,
        topology.git_dir,
        topology.common_dir,
        topology.repository_key,
        topology.worktree_key,
        assembled["project_id"],
        config.path,
        policy_path,
        registry_path,
        state_dir,
        cache_dir,
        artifact_dir,
        notes,
        provenance,
    )


def _native_provenance(item: dict[str, object]) -> Provenance:
    source = item["source"]
    if "path_hex" in source:
        source_text = str(_path_from_hex(source["path_hex"])) + source.get("suffix", "")
    else:
        source_text = source["text"]
    value = item["value"]
    if value is None:
        resolved_value = None
    elif "path_hex" in value:
        resolved_value = str(_path_from_hex(value["path_hex"]))
    elif "paths_hex" in value:
        resolved_value = tuple(str(_path_from_hex(raw)) for raw in value["paths_hex"])
    else:
        resolved_value = value["text"]
    return Provenance(
        item["field"],
        item["kind"],
        source_text,
        resolved_value,
        item["config_sha256"],
    )


def require_git_context(context: ProjectContext) -> None:
    """Refuse a context that was resolved without Git worktree capability."""
    if not isinstance(context, ProjectContext) or context.mode != "git":
        raise _error(
            "UNSUPPORTED_REPOSITORY",
            "context",
            "this operation requires a Git project context",
        )
