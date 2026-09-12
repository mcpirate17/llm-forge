"""Where the host project keeps its conductor data.

``conductor`` was extracted from a monorepo that keeps its candidate policy at
``conductor/candidate_policy.toml`` and its mutation registry at
``conductor/mutation_campaigns/registry.json``. Those two literals were spelled out at
three dozen call sites, which made the package unusable in any tree with another layout.

This module is the single resolution point, and every answer is repo-root-relative so a
caller joins it to the root it already holds. Precedence: the environment variables
below; then the host's ``pyproject.toml`` ``[tool.conductor]`` table, read from the root
being resolved so a candidate snapshot answers for itself; then the monorepo literals,
so that host needs no configuration. A configured value that is absolute, empty or
escaping the root is refused here; a configured file that does not exist is refused by
the caller that needs it.
"""

from __future__ import annotations

import os
import tomllib
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any

DEFAULT_CANDIDATE_POLICY = PurePosixPath("conductor/candidate_policy.toml")
DEFAULT_MUTATION_REGISTRY = PurePosixPath("conductor/mutation_campaigns/registry.json")

CANDIDATE_POLICY_ENV = "CONDUCTOR_CANDIDATE_POLICY"
MUTATION_REGISTRY_ENV = "CONDUCTOR_MUTATION_REGISTRY"

CANDIDATE_POLICY_KEY = "candidate_policy"
MUTATION_REGISTRY_KEY = "mutation_registry"
DEFAULTS = {
    CANDIDATE_POLICY_KEY: DEFAULT_CANDIDATE_POLICY,
    MUTATION_REGISTRY_KEY: DEFAULT_MUTATION_REGISTRY,
}


class ProjectPathError(RuntimeError):
    """A host project path is configured but unusable."""


def _relative(raw: object, source: str) -> PurePosixPath:
    """A configured value as a root-relative posix path, or a loud refusal."""
    if not isinstance(raw, str):
        raise ProjectPathError(f"{source} must be a string, got {type(raw).__name__}")
    text = raw.strip()
    if not text:
        raise ProjectPathError(f"{source} must not be empty")
    path = PurePosixPath(text.replace("\\", "/"))
    if path.is_absolute() or ".." in path.parts or not path.parts:
        raise ProjectPathError(f"{source} must be repo-root-relative: {text!r}")
    return path


def conductor_table(root: Path) -> Mapping[str, Any]:
    """The host's ``[tool.conductor]`` table, empty when there is no manifest."""
    manifest = Path(root) / "pyproject.toml"
    if not manifest.is_file():
        return {}
    with manifest.open("rb") as handle:
        payload = tomllib.load(handle)
    tool = payload.get("tool")
    table = tool.get("conductor") if isinstance(tool, Mapping) else None
    if table is None:
        return {}
    if not isinstance(table, Mapping):
        raise ProjectPathError(f"[tool.conductor] in {manifest} is not a table")
    return table


def _configured(root: Path, key: str, env: str) -> tuple[PurePosixPath, bool]:
    """(relative path, whether the host named it) for one key."""
    raw = os.environ.get(env, "").strip()
    if raw:
        return _relative(raw, f"${env}"), True
    table = conductor_table(root)
    if key in table:
        source = f"[tool.conductor].{key} in {Path(root) / 'pyproject.toml'}"
        return _relative(table[key], source), True
    return DEFAULTS[key], False


@dataclass(frozen=True)
class ProjectPaths:
    """The host project's conductor data, relative to and joined onto ``root``."""

    root: Path
    policy_relative: PurePosixPath
    registry_relative: PurePosixPath
    policy_configured: bool
    registry_configured: bool

    @property
    def policy_path(self) -> Path:
        return self.root / self.policy_relative.as_posix()

    @property
    def registry_path(self) -> Path:
        return self.root / self.registry_relative.as_posix()

    @property
    def campaigns_relative(self) -> PurePosixPath:
        """The directory holding manifests, receipts and patches."""
        return self.registry_relative.parent

    @property
    def campaigns_root(self) -> Path:
        return self.root / self.campaigns_relative.as_posix()


def project_paths(root: Path | str) -> ProjectPaths:
    """Resolve every host path against ``root``. Not cached: hosts differ per call."""
    base = Path(root)
    policy, named_policy = _configured(base, CANDIDATE_POLICY_KEY, CANDIDATE_POLICY_ENV)
    registry, named_reg = _configured(
        base, MUTATION_REGISTRY_KEY, MUTATION_REGISTRY_ENV
    )
    return ProjectPaths(base, policy, registry, named_policy, named_reg)


def enclosing_repo(start: Path) -> Path | None:
    """The nearest ancestor (inclusive) holding ``.git`` -- a dir or a worktree file."""
    for candidate in (start, *start.parents):
        if candidate == candidate.parent:
            break  # the filesystem root is never a repo; do not probe /.git
        if (candidate / ".git").exists():
            return candidate
    return None


def host_root(start: Path | None = None) -> Path:
    """The host repository root, falling back to ``start`` when there is no repo."""
    base = (start or Path.cwd()).resolve()
    return enclosing_repo(base) or base


def registry_relative(root: Path | str) -> PurePosixPath:
    return project_paths(root).registry_relative


def campaigns_relative(root: Path | str) -> PurePosixPath:
    return project_paths(root).campaigns_relative


def receipts_relative(root: Path | str) -> PurePosixPath:
    return project_paths(root).campaigns_relative / "receipts"


def registry_path(root: Path | str) -> Path:
    return project_paths(root).registry_path


def campaigns_root(root: Path | str) -> Path:
    return project_paths(root).campaigns_root
