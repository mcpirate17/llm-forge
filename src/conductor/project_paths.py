"""Where the host project keeps its conductor data.

``conductor`` was extracted from a monorepo that keeps its candidate policy at
``conductor/candidate_policy.toml`` and its mutation registry at
``conductor/mutation_campaigns/registry.json``, with the package itself at
``conductor/``. Those literals were spelled out at three dozen call sites, which made
the package unusable in any tree with another layout.

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
DEFAULT_PACKAGE_ROOT = PurePosixPath("conductor")
# The monorepo `conductor` was extracted from keeps its integration line at `master`.
# This package's own default is `main`; a host names its own line (or a retired one it
# still must recognise) via `[tool.conductor]`, never by carrying the old literal here.
DEFAULT_INTEGRATION_BRANCH = "main"
DEFAULT_RETIRED_INTEGRATION_BRANCHES: tuple[str, ...] = ()
# Where a freshly-run receipt lands when nothing named an explicit ``--receipt``. This
# is scratch output, not the registered evidence directory (`receipts_relative`) --
# the monorepo treats it as auto-pruned staging under `research/reports/`, which a
# host with no `research/` tree at all must be able to repoint via `[tool.conductor]`.
DEFAULT_MUTATION_RECEIPT_ROOT = PurePosixPath("research/reports/mutation_testing")
# The knowledge tree: KB cards and durable findings. Every module that reads it
# (kb_retrieve, memory_index's catalog, the notes guards) resolves through
# `notes_root` so a host with another layout names its own tree once.
DEFAULT_NOTES_ROOT = PurePosixPath("research/notes")

CANDIDATE_POLICY_ENV = "CONDUCTOR_CANDIDATE_POLICY"
MUTATION_REGISTRY_ENV = "CONDUCTOR_MUTATION_REGISTRY"
PACKAGE_ROOT_ENV = "CONDUCTOR_PACKAGE_ROOT"
MUTATION_RECEIPT_ROOT_ENV = "CONDUCTOR_MUTATION_RECEIPT_ROOT"
NOTES_ROOT_ENV = "CONDUCTOR_NOTES_ROOT"
INTEGRATION_BRANCH_ENV = "CONDUCTOR_INTEGRATION_BRANCH"

CANDIDATE_POLICY_KEY = "candidate_policy"
MUTATION_REGISTRY_KEY = "mutation_registry"
PACKAGE_ROOT_KEY = "package_root"
MUTATION_RECEIPT_ROOT_KEY = "mutation_receipt_root"
NOTES_ROOT_KEY = "notes_root"
INTEGRATION_BRANCH_KEY = "integration_branch"
RETIRED_INTEGRATION_BRANCHES_KEY = "retired_integration_branches"
DEFAULTS = {
    CANDIDATE_POLICY_KEY: DEFAULT_CANDIDATE_POLICY,
    MUTATION_REGISTRY_KEY: DEFAULT_MUTATION_REGISTRY,
    PACKAGE_ROOT_KEY: DEFAULT_PACKAGE_ROOT,
    MUTATION_RECEIPT_ROOT_KEY: DEFAULT_MUTATION_RECEIPT_ROOT,
    NOTES_ROOT_KEY: DEFAULT_NOTES_ROOT,
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


def _branch_name(raw: object, source: str) -> str:
    """A configured value as a non-empty branch name, or a loud refusal."""
    if not isinstance(raw, str):
        raise ProjectPathError(f"{source} must be a string, got {type(raw).__name__}")
    text = raw.strip()
    if not text:
        raise ProjectPathError(f"{source} must not be empty")
    return text


def _branch_name_tuple(raw: object, source: str) -> tuple[str, ...]:
    """A configured value as a tuple of non-empty branch names, or a loud refusal."""
    if not isinstance(raw, (list, tuple)):
        raise ProjectPathError(
            f"{source} must be a list of strings, got {type(raw).__name__}"
        )
    return tuple(_branch_name(item, f"{source}[{i}]") for i, item in enumerate(raw))


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


def _configured_integration_branch(root: Path) -> tuple[str, bool]:
    """(branch name, whether the host named it), env then ``[tool.conductor]``."""
    raw = os.environ.get(INTEGRATION_BRANCH_ENV, "").strip()
    if raw:
        return raw, True
    table = conductor_table(root)
    if INTEGRATION_BRANCH_KEY in table:
        source = (
            f"[tool.conductor].{INTEGRATION_BRANCH_KEY} in "
            f"{Path(root) / 'pyproject.toml'}"
        )
        return _branch_name(table[INTEGRATION_BRANCH_KEY], source), True
    return DEFAULT_INTEGRATION_BRANCH, False


def _configured_retired_integration_branches(
    root: Path,
) -> tuple[tuple[str, ...], bool]:
    """(retired branch names, whether the host named them). No env override.

    Unlike ``integration_branch``, a retired line is host history, not a single
    current answer worth overriding per-invocation -- it belongs in the committed
    ``pyproject.toml`` alongside the branch it retired.
    """
    table = conductor_table(root)
    if RETIRED_INTEGRATION_BRANCHES_KEY in table:
        source = (
            f"[tool.conductor].{RETIRED_INTEGRATION_BRANCHES_KEY} in "
            f"{Path(root) / 'pyproject.toml'}"
        )
        return (
            _branch_name_tuple(table[RETIRED_INTEGRATION_BRANCHES_KEY], source),
            True,
        )
    return DEFAULT_RETIRED_INTEGRATION_BRANCHES, False


@dataclass(frozen=True)
class ProjectPaths:
    """The host project's conductor data, relative to and joined onto ``root``."""

    root: Path
    policy_relative: PurePosixPath
    registry_relative: PurePosixPath
    package_relative: PurePosixPath
    receipt_root_relative: PurePosixPath
    notes_relative: PurePosixPath
    policy_configured: bool
    registry_configured: bool
    package_configured: bool
    receipt_root_configured: bool
    notes_configured: bool

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

    @property
    def package_path(self) -> Path:
        """The ``conductor`` package directory inside this root."""
        return self.root / self.package_relative.as_posix()

    @property
    def receipt_root_path(self) -> Path:
        """Where a freshly-run receipt lands absent an explicit output path."""
        return self.root / self.receipt_root_relative.as_posix()

    @property
    def notes_path(self) -> Path:
        """The knowledge tree: KB cards and durable findings."""
        return self.root / self.notes_relative.as_posix()


def project_paths(root: Path | str) -> ProjectPaths:
    """Resolve every host path against ``root``. Not cached: hosts differ per call."""
    base = Path(root)
    policy, named_policy = _configured(base, CANDIDATE_POLICY_KEY, CANDIDATE_POLICY_ENV)
    registry, named_reg = _configured(
        base, MUTATION_REGISTRY_KEY, MUTATION_REGISTRY_ENV
    )
    package, named_pkg = _configured(base, PACKAGE_ROOT_KEY, PACKAGE_ROOT_ENV)
    receipt_root, named_receipt_root = _configured(
        base, MUTATION_RECEIPT_ROOT_KEY, MUTATION_RECEIPT_ROOT_ENV
    )
    notes, named_notes = _configured(base, NOTES_ROOT_KEY, NOTES_ROOT_ENV)
    return ProjectPaths(
        base,
        policy,
        registry,
        package,
        receipt_root,
        notes,
        named_policy,
        named_reg,
        named_pkg,
        named_receipt_root,
        named_notes,
    )


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


def mutation_receipt_root_relative(root: Path | str) -> PurePosixPath:
    """Where a freshly-run receipt lands absent an explicit output path.

    Distinct from ``receipts_relative`` -- that is the registered evidence
    directory the gate reads back; this is scratch staging, configurable
    per host so a tree with no ``research/`` directory is not forced to create
    one just to run a mutation campaign without ``--receipt``.
    """
    return project_paths(root).receipt_root_relative


def mutation_receipt_root(root: Path | str) -> Path:
    return project_paths(root).receipt_root_path


def notes_relative(root: Path | str) -> PurePosixPath:
    """Where the knowledge tree sits inside ``root`` (``research/notes``, ...)."""
    return project_paths(root).notes_relative


def notes_root(root: Path | str) -> Path:
    """The knowledge tree, joined onto the root the caller already holds.

    Modules that read notes (kb_retrieve, memory_index's catalog, the notes
    guards) resolve through this at call time, never from a module constant: a
    host repoints its notes tree once in ``[tool.conductor]`` and every reader
    follows. Note the argument is the tree root, not the package directory.
    """
    return project_paths(root).notes_path


def package_relative(root: Path | str) -> PurePosixPath:
    """Where the ``conductor`` package sits inside ``root`` (``src/conductor``, ...)."""
    return project_paths(root).package_relative


def package_path(root: Path | str) -> Path:
    return project_paths(root).package_path


def integration_branch(root: Path | str) -> str:
    """This host's integration line: env override, then ``[tool.conductor]``, else ``main``."""
    return _configured_integration_branch(Path(root))[0]


def retired_integration_branches(root: Path | str) -> tuple[str, ...]:
    """Names that used to be this host's integration line but no longer are.

    A name that was once the line must still be recognised as one -- never reclassified
    as a deletable feature branch -- if it turns up on an old worktree or a stale
    remote. Defaults to empty: that history is host-specific and belongs in the host's
    own ``pyproject.toml``, never baked into the package.
    """
    return _configured_retired_integration_branches(Path(root))[0]


def integration_branches(root: Path | str) -> tuple[str, ...]:
    """Every name that counts as the integration line: current, then retired."""
    return (integration_branch(root), *retired_integration_branches(root))


def integration_refs(root: Path | str) -> tuple[str, ...]:
    """Refs naming the integration line, most specific first: ``origin/<b>``, ``<b>``."""
    branch = integration_branch(root)
    return (f"origin/{branch}", branch)


def package_tree_root(package_dir: Path) -> Path:
    """The tree root ``package_dir`` sits in, per that root's own configuration.

    The installed package cannot ask itself where it lives: under a src layout its
    parent is ``src/``, which answers the unconfigured default and would name itself
    the root. So the enclosing repository is asked first -- it is the root whose
    ``pyproject.toml`` configured the layout. A package with no repository above it (a
    wheel in site-packages), or one the repository above it disowns, falls back to the
    nearest ancestor whose own configuration resolves back onto the same directory;
    when no ancestor claims it at all, that is a refusal, not a guess.
    """
    resolved = Path(package_dir).resolve()
    repo = enclosing_repo(resolved)
    candidates = list(resolved.parents)
    if repo is not None:
        candidates.insert(0, repo)
    for candidate in candidates:
        if project_paths(candidate).package_path.resolve() == resolved:
            return candidate
    raise ProjectPathError(
        f"no tree root above {resolved} resolves its configured package root back to it"
    )


def registry_path(root: Path | str) -> Path:
    return project_paths(root).registry_path


def campaigns_root(root: Path | str) -> Path:
    return project_paths(root).campaigns_root
