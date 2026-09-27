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

import tomllib
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, NoReturn

DEFAULT_CANDIDATE_POLICY = PurePosixPath("conductor/candidate_policy.toml")
DEFAULT_MUTATION_REGISTRY = PurePosixPath("conductor/mutation_campaigns/registry.json")
DEFAULT_PACKAGE_ROOT = PurePosixPath("conductor")
# The monorepo `conductor` was extracted from keeps its integration line at `master`.
# This package's own default is `main`; a host names its own line (or a retired one it
# still must recognise) via `[tool.conductor]`, never by carrying the old literal here.
DEFAULT_INTEGRATION_BRANCH = "main"
DEFAULT_RETIRED_INTEGRATION_BRANCHES: tuple[str, ...] = ()
# The monorepo `conductor` was extracted from keeps its worktrees under a temp
# scratch prefix and a fixed per-user project directory. This package's default
# carries those two literals so nothing regresses on that host; a host with
# another layout names its own patterns via `[tool.conductor]`.
DEFAULT_WORKTREE_PATTERNS: tuple[str, ...] = (
    r"/tmp/llm-[\w.-]+",
    r"/home/\w+/Projects/LLM[\w.-]*",
)
# Where a freshly-run receipt lands when nothing named an explicit ``--receipt``. This
# is scratch output, not the registered evidence directory (`receipts_relative`) --
# the monorepo treats it as auto-pruned staging under `research/reports/`, which a
# host with no `research/` tree at all must be able to repoint via `[tool.conductor]`.
DEFAULT_MUTATION_RECEIPT_ROOT = PurePosixPath("research/reports/mutation_testing")
# The knowledge tree: KB cards and durable findings. Every module that reads it
# (kb_retrieve, memory_index's catalog, the notes guards) resolves through
# `notes_root` so a host with another layout names its own tree once.
DEFAULT_NOTES_ROOT = PurePosixPath("research/notes")
# The prose-search index over the notes tree: `index_notes` writes it and
# `index_notes search` reads it. Its own file, not the run/experiment database:
# the two share no key and never join, and a host that splits them (the monorepo
# did on 2026-09-14) otherwise gets an index written to one file and every
# documented reader pointed at the other. Rebuildable cache, never a record --
# repointing it costs one `python -m conductor.index_notes` run.
DEFAULT_NOTES_DB = PurePosixPath("research/notes.db")
# The host allowlist of files/functions permitted to exceed the god-file, god-
# function and complexity guardrails. Host data, never a packaged copy: a host
# that ships one at another path names it via `[tool.conductor]`.
DEFAULT_GUARDRAIL_ALLOWLIST = PurePosixPath("conductor/guardrail_allowlist.json")
# The memory index's source catalog. Host data: every entry names a directory in
# the host's tree (or an absolute path outside it), so the host -- not this
# package -- decides what is indexable. A copy ships beside ``memory_index`` and
# is used only when the host has no catalog of its own and named none, so a
# fresh install still indexes something; see ``memory_index.host_catalog_path``.
DEFAULT_MEMORY_SOURCES = PurePosixPath("conductor/memory_sources.toml")
# The native crate roster: which crates are tested, linted, unstyled or excluded.
# A consumer repo's CI and its local gate both read this one file, so a crate
# cannot be linted in one place and not the other -- see
# ``candidate_review.cargo_lint_files``. Host data, never a packaged copy: a
# host that ships one at another path names it via ``[tool.conductor]``.
DEFAULT_CRATE_ROSTER = PurePosixPath("tooling/native/crates.toml")
# Where the extension crates' sources sit: the directory `native_freshness`
# scans for maturin `pyproject.toml` files, and through it the roster
# `crg_venv_sync` compares the graph server's interpreter against. A module
# constant until 2026-09-16, which disarmed both on every host that does not
# use the monorepo's layout -- this repository keeps its crates in `native/`,
# so `crates()` found none here and the freshness question answered "nothing
# to compare" rather than asking anything. Sibling of `crate_roster`, not its
# parent: the roster names which crates are linted, this names where they are.
DEFAULT_NATIVE_ROOT = PurePosixPath("tooling/native")
# The complexity ratchet's grandfathered scores. Host data: every key names a
# block in the host's own tree, so a packaged copy describes the wrong repo
# entirely. `radon_complexity` resolved this from ``__file__`` until 2026-09-15,
# which silently ratcheted every consumer against this package's baseline.
DEFAULT_RADON_BASELINE = PurePosixPath("conductor/radon_complexity_baseline.json")

# What this package is called once installed. Spelled out rather than derived:
# `importlib.metadata`'s reverse map from package to distribution is blind to an
# editable install, which is how this repository installs itself, so asking the
# interpreter answers `None` exactly where the tooling is being developed.
# `package_resources` keeps its own copy on purpose -- it is a byte-verifying
# reader whose import graph is deliberately two modules wide, and importing this
# one would break the minimal wheel its tests install. `test_project_paths` pins
# the two spellings together so a rename cannot take only one of them.
DISTRIBUTION_NAME = "conductor-tooling"

CANDIDATE_POLICY_ENV = "CONDUCTOR_CANDIDATE_POLICY"
MUTATION_REGISTRY_ENV = "CONDUCTOR_MUTATION_REGISTRY"
PACKAGE_ROOT_ENV = "CONDUCTOR_PACKAGE_ROOT"
MUTATION_RECEIPT_ROOT_ENV = "CONDUCTOR_MUTATION_RECEIPT_ROOT"
NOTES_ROOT_ENV = "CONDUCTOR_NOTES_ROOT"
NOTES_DB_ENV = "CONDUCTOR_NOTES_DB"
GUARDRAIL_ALLOWLIST_ENV = "CONDUCTOR_GUARDRAIL_ALLOWLIST"
MEMORY_SOURCES_ENV = "CONDUCTOR_MEMORY_SOURCES"
INTEGRATION_BRANCH_ENV = "CONDUCTOR_INTEGRATION_BRANCH"
CRATE_ROSTER_ENV = "CONDUCTOR_CRATE_ROSTER"
NATIVE_ROOT_ENV = "CONDUCTOR_NATIVE_ROOT"
RADON_BASELINE_ENV = "CONDUCTOR_RADON_BASELINE"

CANDIDATE_POLICY_KEY = "candidate_policy"
MUTATION_REGISTRY_KEY = "mutation_registry"
PACKAGE_ROOT_KEY = "package_root"
MUTATION_RECEIPT_ROOT_KEY = "mutation_receipt_root"
NOTES_ROOT_KEY = "notes_root"
NOTES_DB_KEY = "notes_db"
GUARDRAIL_ALLOWLIST_KEY = "guardrail_allowlist"
MEMORY_SOURCES_KEY = "memory_sources"
INTEGRATION_BRANCH_KEY = "integration_branch"
RETIRED_INTEGRATION_BRANCHES_KEY = "retired_integration_branches"
CRATE_ROSTER_KEY = "crate_roster"
NATIVE_ROOT_KEY = "native_root"
WORKTREE_PATTERNS_KEY = "worktree_patterns"
RADON_BASELINE_KEY = "radon_complexity_baseline"
DEFAULTS = {
    CANDIDATE_POLICY_KEY: DEFAULT_CANDIDATE_POLICY,
    MUTATION_REGISTRY_KEY: DEFAULT_MUTATION_REGISTRY,
    PACKAGE_ROOT_KEY: DEFAULT_PACKAGE_ROOT,
    MUTATION_RECEIPT_ROOT_KEY: DEFAULT_MUTATION_RECEIPT_ROOT,
    NOTES_ROOT_KEY: DEFAULT_NOTES_ROOT,
    NOTES_DB_KEY: DEFAULT_NOTES_DB,
    CRATE_ROSTER_KEY: DEFAULT_CRATE_ROSTER,
    NATIVE_ROOT_KEY: DEFAULT_NATIVE_ROOT,
    RADON_BASELINE_KEY: DEFAULT_RADON_BASELINE,
    GUARDRAIL_ALLOWLIST_KEY: DEFAULT_GUARDRAIL_ALLOWLIST,
    MEMORY_SOURCES_KEY: DEFAULT_MEMORY_SOURCES,
}


class ProjectPathError(RuntimeError):
    """A host project path is configured but unusable."""


def _raise_native_error(exc: RuntimeError) -> NoReturn:
    message = str(exc)
    prefix = "PROJECT_PATHS_MANIFEST_ERROR:"
    if message.startswith(prefix):
        root = Path(message[len(prefix) :])
        conductor_table(root)  # Preserve tomllib and OS exception types on failure.
        message = f"native TOML parser refused {root / 'pyproject.toml'}"
    raise ProjectPathError(message) from exc


def _relative(raw: object, source: str) -> PurePosixPath:
    """Compatibility return type for native root-relative validation."""
    if not isinstance(raw, str):
        raise ProjectPathError(f"{source} must be a string, got {type(raw).__name__}")
    from conductor._native import project_paths_relative_native

    try:
        return PurePosixPath(project_paths_relative_native(raw, source))
    except RuntimeError as exc:
        _raise_native_error(exc)


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


@dataclass(frozen=True)
class ProjectPaths:
    """The host project's conductor data, relative to and joined onto ``root``."""

    root: Path
    policy_relative: PurePosixPath
    registry_relative: PurePosixPath
    package_relative: PurePosixPath
    receipt_root_relative: PurePosixPath
    notes_relative: PurePosixPath
    notes_db_relative: PurePosixPath
    guardrail_allowlist_relative: PurePosixPath
    memory_sources_relative: PurePosixPath
    crate_roster_relative: PurePosixPath
    native_root_relative: PurePosixPath
    radon_baseline_relative: PurePosixPath
    policy_configured: bool
    registry_configured: bool
    package_configured: bool
    receipt_root_configured: bool
    notes_configured: bool
    notes_db_configured: bool
    guardrail_allowlist_configured: bool
    memory_sources_configured: bool
    crate_roster_configured: bool
    native_root_configured: bool
    radon_baseline_configured: bool

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

    @property
    def notes_db_path(self) -> Path:
        """The prose-search index over ``notes_path`` -- its own file."""
        return self.root / self.notes_db_relative.as_posix()

    @property
    def guardrail_allowlist_path(self) -> Path:
        """The host's guardrail allowlist -- never the package's own copy."""
        return self.root / self.guardrail_allowlist_relative.as_posix()

    @property
    def memory_sources_path(self) -> Path:
        """Where the host keeps its memory-index catalog."""
        return self.root / self.memory_sources_relative.as_posix()

    @property
    def crate_roster_path(self) -> Path:
        """The native crate roster -- CI and the local gate both read this file."""
        return self.root / self.crate_roster_relative.as_posix()

    @property
    def native_root_path(self) -> Path:
        """Where the extension crates' sources sit -- what ``crates()`` scans."""
        return self.root / self.native_root_relative.as_posix()

    @property
    def radon_baseline_path(self) -> Path:
        """The complexity ratchet's baseline for *this* host, never a packaged one."""
        return self.root / self.radon_baseline_relative.as_posix()


def project_paths(root: Path | str) -> ProjectPaths:
    """Resolve every host path against ``root``. Not cached: hosts differ per call."""
    base = Path(root)
    from conductor._native import project_paths_resolve_native

    try:
        values = project_paths_resolve_native(str(base))
    except RuntimeError as exc:
        _raise_native_error(exc)
    return ProjectPaths(
        base,
        *(PurePosixPath(value) for value, _configured in values),
        *(_configured for _value, _configured in values),
    )


def enclosing_repo(start: Path) -> Path | None:
    """The nearest ancestor (inclusive) holding ``.git`` -- a dir or a worktree file."""
    from conductor._native import project_paths_enclosing_repo_native

    result = project_paths_enclosing_repo_native(str(start))
    return Path(result) if result is not None else None


def host_root(start: Path | None = None) -> Path:
    """The host repository root -- never a path derived from ``__file__``.

    Precedence, highest first:

    1. ``CONDUCTOR_HOST_ROOT`` env var, when set: must be an absolute path
       that exists, else raises naming the offending value. Wins over
       everything, including an explicit ``start``, so a supervising
       process can pin every subprocess to one host without touching CLI
       flags.
    2. ``start`` -- the caller's explicit ``--repo-root``, for the CLIs
       that accept one.
    3. The nearest ``.git`` ancestor of the current working directory,
       else the cwd itself.
    """
    from conductor._native import project_paths_host_root_native

    try:
        return Path(
            project_paths_host_root_native(str(start) if start is not None else None)
        )
    except RuntimeError as exc:
        _raise_native_error(exc)


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


def notes_db_path(root: Path | str) -> Path:
    """The prose-search index over the notes tree, joined onto ``root``.

    Resolved at call time like :func:`notes_root`, never from a module constant:
    ``index_notes`` writes this file and ``index_notes search`` reads it, so a host
    that keeps its prose index apart from its run database names it once in
    ``[tool.conductor]`` and both ends follow.
    """
    return project_paths(root).notes_db_path


def memory_sources_relative(root: Path | str) -> PurePosixPath:
    """Where the host keeps its memory-index catalog, relative to ``root``."""
    return project_paths(root).memory_sources_relative


def memory_sources_path(root: Path | str) -> Path:
    """The host's memory-index catalog, joined onto the root the caller holds.

    May not exist: a host that ships no catalog and names none falls back to the
    packaged copy, which only ``memory_index.host_catalog_path`` decides. A host
    that *names* one and does not ship it is a configuration error, refused
    there rather than silently papered over.
    """
    return project_paths(root).memory_sources_path


def package_relative(root: Path | str) -> PurePosixPath:
    """Where the ``conductor`` package sits inside ``root`` (``src/conductor``, ...)."""
    return project_paths(root).package_relative


def package_path(root: Path | str) -> Path:
    return project_paths(root).package_path


def guardrail_allowlist_relative(root: Path | str) -> PurePosixPath:
    """Where the guardrail allowlist sits inside ``root`` (``conductor/guardrail_allowlist.json``, ...)."""
    return project_paths(root).guardrail_allowlist_relative


def guardrail_allowlist_path(root: Path | str) -> Path:
    """The host's guardrail allowlist, joined onto the root the caller already holds.

    Readers (``guardrail_audit``, ``reuse.detectors``) resolve through this at call
    time, never from a package-shipped copy: a host names its own allowlist path via
    ``[tool.conductor]`` and both readers follow.
    """
    return project_paths(root).guardrail_allowlist_path


def crate_roster_relative(root: Path | str) -> PurePosixPath:
    """Where the native crate roster sits inside ``root`` (``tooling/native/crates.toml``, ...)."""
    return project_paths(root).crate_roster_relative


def crate_roster_path(root: Path | str) -> Path:
    """The host's crate roster, joined onto the root the caller already holds.

    ``candidate_review.cargo_lint_files`` resolves through this at call time,
    never from the package literal: a host names its own roster path via
    ``[tool.conductor]`` and the reader follows. A configured path that does not
    exist on disk is not this function's problem to catch -- the caller that
    reads the file fails loud naming the resolved path, exactly as it would for
    the unconfigured default; this function never falls back to an empty roster.
    """
    return project_paths(root).crate_roster_path


def native_root(root: Path | str) -> Path:
    """Where the extension crates' sources sit, joined onto ``root``.

    ``native_freshness.crates`` resolves through this at call time rather than
    from a module constant, the way every other host path here is resolved. It
    was the constant ``tooling/native`` until 2026-09-16, so a host with another
    layout -- this repository, whose crates are in ``native/`` -- declared no
    crates as far as the freshness check could see, and both it and
    ``crg_venv_sync`` answered "nothing to compare" instead of comparing. A
    directory that does not exist is not an error here: a consumer checkout that
    installs the crates rather than building them genuinely has no sources, and
    the caller decides what that means.
    """
    return project_paths(root).native_root_path


def radon_baseline_relative(root: Path | str) -> PurePosixPath:
    """Where the complexity ratchet's baseline sits inside ``root``."""
    return project_paths(root).radon_baseline_relative


def radon_baseline_path(root: Path | str) -> Path:
    """The host's complexity baseline, joined onto the root the caller holds.

    Resolved against the host tree, never from ``__file__``: a baseline keyed by
    ``path::name`` describes one repository's blocks, so the copy shipping beside
    ``radon_complexity`` grandfathers this package's own code and nothing else.
    An absent file is the reader's to report, naming the resolved path.
    """
    return project_paths(root).radon_baseline_path


def integration_branch(root: Path | str) -> str:
    """This host's integration line: env override, then ``[tool.conductor]``, else ``main``."""
    from conductor._native import project_paths_integration_branch_native

    try:
        return project_paths_integration_branch_native(str(root))
    except RuntimeError as exc:
        _raise_native_error(exc)


def retired_integration_branches(root: Path | str) -> tuple[str, ...]:
    """Names that used to be this host's integration line but no longer are.

    A name that was once the line must still be recognised as one -- never reclassified
    as a deletable feature branch -- if it turns up on an old worktree or a stale
    remote. Defaults to empty: that history is host-specific and belongs in the host's
    own ``pyproject.toml``, never baked into the package.
    """
    from conductor._native import project_paths_retired_integration_branches_native

    try:
        return tuple(project_paths_retired_integration_branches_native(str(root)))
    except RuntimeError as exc:
        _raise_native_error(exc)


def worktree_patterns(root: Path | str) -> tuple[str, ...]:
    """Regex sources matching this host's worktree paths, for display grouping.

    Defaults to the monorepo's own layout (a ``/tmp/llm-*`` scratch prefix and a
    fixed per-user ``~/Projects/LLM*`` clone) so nothing regresses on that host.
    A host with another layout names its own patterns via ``[tool.conductor]``;
    there is no environment override, matching ``retired_integration_branches``.
    """
    import re

    from conductor._native import project_paths_worktree_patterns_native

    try:
        patterns = project_paths_worktree_patterns_native(str(root))
    except RuntimeError as exc:
        _raise_native_error(exc)
    for index, pattern in enumerate(patterns):
        try:
            re.compile(pattern)
        except re.error as exc:
            source = (
                f"[tool.conductor].worktree_patterns in {Path(root) / 'pyproject.toml'}"
            )
            raise ProjectPathError(
                f"{source}[{index}] is not a valid regex: {exc}"
            ) from exc
    return tuple(patterns)


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
    from conductor._native import project_paths_package_tree_root_native

    try:
        return Path(project_paths_package_tree_root_native(str(package_dir)))
    except RuntimeError as exc:
        _raise_native_error(exc)


def registry_path(root: Path | str) -> Path:
    return project_paths(root).registry_path


def campaigns_root(root: Path | str) -> Path:
    return project_paths(root).campaigns_root
