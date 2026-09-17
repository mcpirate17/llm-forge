"""Paired tests for conductor.project_paths.

Every branch of the precedence chain (environment, ``[tool.conductor]``, monorepo
default) and every refusal in ``_relative`` is exercised here, because this module is
what stops the two host literals from being respelled at three dozen call sites.
"""

from __future__ import annotations

from pathlib import Path, PurePosixPath

import pytest

from conductor import project_paths as pp


def _write(root: Path, body: str) -> None:
    (root / "pyproject.toml").write_text(body, encoding="utf-8")


def test_defaults_are_the_monorepo_literals():
    assert pp.DEFAULT_CANDIDATE_POLICY == PurePosixPath(
        "conductor/candidate_policy.toml"
    )
    assert pp.DEFAULT_MUTATION_REGISTRY == PurePosixPath(
        "conductor/mutation_campaigns/registry.json"
    )
    assert pp.DEFAULTS[pp.CANDIDATE_POLICY_KEY] == pp.DEFAULT_CANDIDATE_POLICY
    assert pp.DEFAULTS[pp.MUTATION_REGISTRY_KEY] == pp.DEFAULT_MUTATION_REGISTRY


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        ("campaigns/registry.json", "campaigns/registry.json"),
        ("  campaigns/registry.json  ", "campaigns/registry.json"),
        ("campaigns\\registry.json", "campaigns/registry.json"),
        ("registry.json", "registry.json"),
    ],
)
def test_relative_accepts_root_relative_values(raw, expected):
    assert pp._relative(raw, "src") == PurePosixPath(expected)


@pytest.mark.parametrize(
    "raw",
    ["/abs/registry.json", "", "   ", "../registry.json", "a/../../b"],
)
def test_relative_refuses_unusable_values(raw):
    with pytest.raises(pp.ProjectPathError):
        pp._relative(raw, "src")


def test_relative_refuses_a_non_string():
    with pytest.raises(pp.ProjectPathError, match="must be a string, got int"):
        pp._relative(3, "src")


def test_conductor_table_is_empty_without_a_manifest(tmp_path):
    assert pp.conductor_table(tmp_path) == {}


def test_conductor_table_is_empty_without_a_conductor_section(tmp_path):
    _write(tmp_path, '[project]\nname = "x"\n')
    assert pp.conductor_table(tmp_path) == {}


def test_conductor_table_is_empty_when_tool_is_not_a_table(tmp_path):
    _write(tmp_path, 'tool = "not-a-table"\n')
    assert pp.conductor_table(tmp_path) == {}


def test_conductor_table_refuses_a_non_table_conductor_key(tmp_path):
    _write(tmp_path, '[tool]\nconductor = "nope"\n')
    with pytest.raises(pp.ProjectPathError, match="is not a table"):
        pp.conductor_table(tmp_path)


def test_conductor_table_returns_the_declared_keys(tmp_path):
    _write(tmp_path, '[tool.conductor]\ncandidate_policy = "policy.toml"\n')
    assert pp.conductor_table(tmp_path) == {"candidate_policy": "policy.toml"}


def test_unconfigured_root_falls_back_to_the_monorepo_layout(tmp_path):
    paths = pp.project_paths(tmp_path)
    assert paths.policy_relative == pp.DEFAULT_CANDIDATE_POLICY
    assert paths.registry_relative == pp.DEFAULT_MUTATION_REGISTRY
    assert paths.policy_configured is False
    assert paths.registry_configured is False


def test_pyproject_table_wins_over_the_default(tmp_path):
    _write(
        tmp_path,
        "[tool.conductor]\n"
        'candidate_policy = "candidate_policy.toml"\n'
        'mutation_registry = "campaigns/registry.json"\n',
    )
    paths = pp.project_paths(tmp_path)
    assert paths.policy_relative == PurePosixPath("candidate_policy.toml")
    assert paths.registry_relative == PurePosixPath("campaigns/registry.json")
    assert paths.policy_configured is True
    assert paths.registry_configured is True


def test_environment_wins_over_the_pyproject_table(tmp_path, monkeypatch):
    _write(
        tmp_path,
        "[tool.conductor]\n"
        'candidate_policy = "from_pyproject.toml"\n'
        'mutation_registry = "from_pyproject/registry.json"\n',
    )
    monkeypatch.setenv(pp.CANDIDATE_POLICY_ENV, "from_env.toml")
    monkeypatch.setenv(pp.MUTATION_REGISTRY_ENV, "from_env/registry.json")
    paths = pp.project_paths(tmp_path)
    assert paths.policy_relative == PurePosixPath("from_env.toml")
    assert paths.registry_relative == PurePosixPath("from_env/registry.json")


def test_a_blank_environment_variable_does_not_override(tmp_path, monkeypatch):
    _write(tmp_path, '[tool.conductor]\ncandidate_policy = "from_pyproject.toml"\n')
    monkeypatch.setenv(pp.CANDIDATE_POLICY_ENV, "   ")
    assert pp.project_paths(tmp_path).policy_relative == PurePosixPath(
        "from_pyproject.toml"
    )


def test_a_configured_absolute_path_is_refused(tmp_path, monkeypatch):
    monkeypatch.setenv(pp.MUTATION_REGISTRY_ENV, "/etc/registry.json")
    with pytest.raises(pp.ProjectPathError, match="repo-root-relative"):
        pp.project_paths(tmp_path)


def test_each_key_is_resolved_independently(tmp_path, monkeypatch):
    _write(
        tmp_path, '[tool.conductor]\nmutation_registry = "campaigns/registry.json"\n'
    )
    monkeypatch.delenv(pp.CANDIDATE_POLICY_ENV, raising=False)
    paths = pp.project_paths(tmp_path)
    assert paths.policy_relative == pp.DEFAULT_CANDIDATE_POLICY
    assert paths.policy_configured is False
    assert paths.registry_relative == PurePosixPath("campaigns/registry.json")
    assert paths.registry_configured is True


def test_absolute_properties_join_onto_the_root(tmp_path):
    _write(
        tmp_path,
        "[tool.conductor]\n"
        'candidate_policy = "candidate_policy.toml"\n'
        'mutation_registry = "campaigns/registry.json"\n',
    )
    paths = pp.project_paths(tmp_path)
    assert paths.root == tmp_path
    assert paths.policy_path == tmp_path / "candidate_policy.toml"
    assert paths.registry_path == tmp_path / "campaigns" / "registry.json"
    assert paths.campaigns_relative == PurePosixPath("campaigns")
    assert paths.campaigns_root == tmp_path / "campaigns"


def test_project_paths_accepts_a_string_root(tmp_path):
    assert pp.project_paths(str(tmp_path)).root == tmp_path


def test_module_helpers_agree_with_the_dataclass(tmp_path):
    _write(
        tmp_path, '[tool.conductor]\nmutation_registry = "campaigns/registry.json"\n'
    )
    assert pp.registry_relative(tmp_path) == PurePosixPath("campaigns/registry.json")
    assert pp.campaigns_relative(tmp_path) == PurePosixPath("campaigns")
    assert pp.receipts_relative(tmp_path) == PurePosixPath("campaigns/receipts")
    assert pp.registry_path(tmp_path) == tmp_path / "campaigns" / "registry.json"
    assert pp.campaigns_root(tmp_path) == tmp_path / "campaigns"


def test_a_top_level_registry_leaves_campaigns_at_the_root(tmp_path):
    _write(tmp_path, '[tool.conductor]\nmutation_registry = "registry.json"\n')
    assert pp.campaigns_relative(tmp_path) == PurePosixPath(".")
    assert pp.receipts_relative(tmp_path) == PurePosixPath("receipts")


def test_enclosing_repo_finds_the_nearest_ancestor_holding_dot_git(tmp_path):
    repo = tmp_path / "repo"
    deep = repo / "a" / "b"
    deep.mkdir(parents=True)
    (repo / ".git").mkdir()
    assert pp.enclosing_repo(deep) == repo
    assert pp.enclosing_repo(repo) == repo


def test_enclosing_repo_accepts_a_worktree_dot_git_file(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / ".git").write_text("gitdir: /elsewhere\n", encoding="utf-8")
    assert pp.enclosing_repo(repo) == repo


def test_enclosing_repo_returns_none_without_a_repo(tmp_path):
    deep = tmp_path / "a" / "b"
    deep.mkdir(parents=True)
    assert pp.enclosing_repo(deep) is None


def test_enclosing_repo_prefers_the_nearest_of_two_ancestors(tmp_path):
    outer = tmp_path / "outer"
    inner = outer / "inner"
    inner.mkdir(parents=True)
    (outer / ".git").mkdir()
    (inner / ".git").mkdir()
    assert pp.enclosing_repo(inner) == inner


def test_host_root_falls_back_to_the_start_directory(tmp_path):
    start = tmp_path / "plain"
    start.mkdir()
    assert pp.host_root(start) == start.resolve()


def test_host_root_returns_the_repo_root_when_there_is_one(tmp_path):
    repo = tmp_path / "repo"
    deep = repo / "a"
    deep.mkdir(parents=True)
    (repo / ".git").mkdir()
    assert pp.host_root(deep) == repo.resolve()


def test_host_root_defaults_to_the_current_directory(tmp_path, monkeypatch):
    plain = tmp_path / "cwd"
    plain.mkdir()
    monkeypatch.chdir(plain)
    assert pp.host_root() == plain.resolve()


def test_host_root_env_wins_over_an_explicit_start(tmp_path, monkeypatch):
    pinned = tmp_path / "pinned"
    pinned.mkdir()
    other = tmp_path / "other"
    other.mkdir()
    monkeypatch.setenv("CONDUCTOR_HOST_ROOT", str(pinned))
    assert pp.host_root(other) == pinned.resolve()


def test_host_root_env_must_be_absolute(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("CONDUCTOR_HOST_ROOT", "relative/path")
    with pytest.raises(pp.ProjectPathError, match="relative/path"):
        pp.host_root()


def test_host_root_env_must_exist(tmp_path, monkeypatch):
    missing = tmp_path / "does-not-exist"
    monkeypatch.setenv("CONDUCTOR_HOST_ROOT", str(missing))
    with pytest.raises(pp.ProjectPathError, match=str(missing)):
        pp.host_root()


def test_host_root_explicit_start_wins_over_cwd_walk(tmp_path, monkeypatch):
    cwd_repo = tmp_path / "cwd_repo"
    cwd_repo.mkdir()
    (cwd_repo / ".git").mkdir()
    monkeypatch.chdir(cwd_repo)

    other_repo = tmp_path / "other_repo"
    other_repo.mkdir()
    (other_repo / ".git").mkdir()

    assert pp.host_root(other_repo) == other_repo.resolve()


# --- package_root -----------------------------------------------------------
#
# The third answer this module owns: where the conductor package itself sits.
# Unlike the policy and the registry, it has an inverse -- a caller holding the
# installed package and needing the tree around it -- and that inverse is the part
# a fixed parents[n] walk gets wrong under a src layout.


def test_package_root_defaults_to_the_monorepo_literal():
    assert pp.DEFAULT_PACKAGE_ROOT == PurePosixPath("conductor")
    assert pp.DEFAULTS[pp.PACKAGE_ROOT_KEY] == pp.DEFAULT_PACKAGE_ROOT


def test_package_root_is_the_default_without_configuration(tmp_path):
    resolved = pp.project_paths(tmp_path)
    assert resolved.package_relative == PurePosixPath("conductor")
    assert resolved.package_configured is False
    assert resolved.package_path == tmp_path / "conductor"


def test_package_root_comes_from_the_conductor_table(tmp_path):
    _write(tmp_path, '[tool.conductor]\npackage_root = "src/conductor"\n')
    resolved = pp.project_paths(tmp_path)
    assert resolved.package_relative == PurePosixPath("src/conductor")
    assert resolved.package_configured is True
    assert resolved.package_path == tmp_path / "src/conductor"
    assert pp.package_relative(tmp_path) == PurePosixPath("src/conductor")
    assert pp.package_path(tmp_path) == tmp_path / "src/conductor"


def test_package_root_environment_overrides_the_table(tmp_path, monkeypatch):
    _write(tmp_path, '[tool.conductor]\npackage_root = "src/conductor"\n')
    monkeypatch.setenv(pp.PACKAGE_ROOT_ENV, "lib/conductor")
    assert pp.package_relative(tmp_path) == PurePosixPath("lib/conductor")


def test_package_root_refuses_an_unusable_value(tmp_path):
    _write(tmp_path, '[tool.conductor]\npackage_root = "/abs/conductor"\n')
    with pytest.raises(pp.ProjectPathError):
        pp.package_relative(tmp_path)


def test_package_tree_root_finds_a_flat_layout(tmp_path):
    repo = tmp_path / "repo"
    package = repo / "conductor"
    package.mkdir(parents=True)
    (repo / ".git").mkdir()
    assert pp.package_tree_root(package) == repo.resolve()


def test_package_tree_root_prefers_the_repo_over_the_nearer_ancestor(tmp_path):
    """``src/`` answers the unconfigured default; the repo is what configured it."""
    repo = tmp_path / "repo"
    package = repo / "src" / "conductor"
    package.mkdir(parents=True)
    (repo / ".git").mkdir()
    _write(repo, '[tool.conductor]\npackage_root = "src/conductor"\n')
    assert pp.package_tree_root(package) == repo.resolve()
    # The nearer ancestor would have answered, which is exactly the wrong root.
    assert pp.package_path(repo / "src") == package


def test_package_tree_root_falls_back_when_there_is_no_repository(tmp_path):
    package = tmp_path / "site-packages" / "conductor"
    package.mkdir(parents=True)
    assert pp.package_tree_root(package) == (tmp_path / "site-packages").resolve()


def test_package_tree_root_falls_back_when_the_repo_names_another_package(tmp_path):
    """A repo pointing elsewhere is not authoritative for a package it disowns."""
    repo = tmp_path / "repo"
    package = repo / "src" / "conductor"
    package.mkdir(parents=True)
    (repo / ".git").mkdir()
    _write(repo, '[tool.conductor]\npackage_root = "elsewhere/conductor"\n')
    assert pp.package_tree_root(package) == (repo / "src").resolve()


def test_package_tree_root_refuses_a_package_no_root_claims(tmp_path):
    package = tmp_path / "repo" / "src" / "not_conductor"
    package.mkdir(parents=True)
    (tmp_path / "repo" / ".git").mkdir()
    with pytest.raises(pp.ProjectPathError):
        pp.package_tree_root(package)


def test_this_repository_resolves_its_own_package():
    """The live check: the installed package and the checkout agree on the root."""
    from conductor.candidate_review import engine

    package = Path(engine.__file__).resolve().parents[1]
    root = pp.package_tree_root(package)
    assert pp.package_path(root).resolve() == package


# --- mutation_receipt_root ---------------------------------------------------
#
# Scratch staging for a receipt written without an explicit ``--receipt``, distinct
# from ``receipts_relative`` (the registered evidence directory the gate reads back).
# The unconfigured default is the monorepo's own literal so that host's behaviour is
# unchanged; a host with no ``research/`` tree repoints it via ``[tool.conductor]``.


def test_mutation_receipt_root_defaults_to_the_monorepo_literal():
    assert pp.DEFAULT_MUTATION_RECEIPT_ROOT == PurePosixPath(
        "research/reports/mutation_testing"
    )
    assert pp.DEFAULTS[pp.MUTATION_RECEIPT_ROOT_KEY] == pp.DEFAULT_MUTATION_RECEIPT_ROOT


def test_mutation_receipt_root_is_the_default_without_configuration(tmp_path):
    resolved = pp.project_paths(tmp_path)
    assert resolved.receipt_root_relative == PurePosixPath(
        "research/reports/mutation_testing"
    )
    assert resolved.receipt_root_configured is False
    assert resolved.receipt_root_path == tmp_path / "research/reports/mutation_testing"
    assert pp.mutation_receipt_root_relative(tmp_path) == PurePosixPath(
        "research/reports/mutation_testing"
    )
    assert pp.mutation_receipt_root(tmp_path) == (
        tmp_path / "research/reports/mutation_testing"
    )


def test_mutation_receipt_root_comes_from_the_conductor_table(tmp_path):
    _write(tmp_path, '[tool.conductor]\nmutation_receipt_root = "campaigns/receipts"\n')
    resolved = pp.project_paths(tmp_path)
    assert resolved.receipt_root_relative == PurePosixPath("campaigns/receipts")
    assert resolved.receipt_root_configured is True
    assert pp.mutation_receipt_root_relative(tmp_path) == PurePosixPath(
        "campaigns/receipts"
    )
    assert pp.mutation_receipt_root(tmp_path) == tmp_path / "campaigns/receipts"


def test_mutation_receipt_root_environment_overrides_the_table(tmp_path, monkeypatch):
    _write(tmp_path, '[tool.conductor]\nmutation_receipt_root = "campaigns/receipts"\n')
    monkeypatch.setenv(pp.MUTATION_RECEIPT_ROOT_ENV, "scratch/mutation")
    assert pp.mutation_receipt_root_relative(tmp_path) == PurePosixPath(
        "scratch/mutation"
    )


def test_mutation_receipt_root_refuses_an_unusable_value(tmp_path):
    _write(tmp_path, '[tool.conductor]\nmutation_receipt_root = "/abs/receipts"\n')
    with pytest.raises(pp.ProjectPathError):
        pp.mutation_receipt_root_relative(tmp_path)


def test_mutation_receipt_root_is_distinct_from_receipts_relative(tmp_path):
    """Configuring one must not move the other -- they answer different questions."""
    _write(
        tmp_path,
        "[tool.conductor]\n"
        'mutation_registry = "campaigns/registry.json"\n'
        'mutation_receipt_root = "campaigns/receipts"\n',
    )
    assert pp.receipts_relative(tmp_path) == PurePosixPath("campaigns/receipts")
    assert pp.mutation_receipt_root_relative(tmp_path) == PurePosixPath(
        "campaigns/receipts"
    )
    # Same value here by this repository's own configuration, but reached through
    # two independent keys -- changing one alone must not move the other.
    _write(
        tmp_path,
        "[tool.conductor]\n"
        'mutation_registry = "campaigns/registry.json"\n'
        'mutation_receipt_root = "scratch/staging"\n',
    )
    assert pp.receipts_relative(tmp_path) == PurePosixPath("campaigns/receipts")
    assert pp.mutation_receipt_root_relative(tmp_path) == PurePosixPath(
        "scratch/staging"
    )


def test_notes_root_defaults_to_the_monorepo_literal():
    assert pp.DEFAULT_NOTES_ROOT == PurePosixPath("research/notes")
    assert pp.DEFAULTS[pp.NOTES_ROOT_KEY] == pp.DEFAULT_NOTES_ROOT


def test_notes_root_is_the_default_without_configuration(tmp_path):
    resolved = pp.project_paths(tmp_path)
    assert resolved.notes_relative == PurePosixPath("research/notes")
    assert resolved.notes_configured is False
    assert resolved.notes_path == tmp_path / "research/notes"
    assert pp.notes_relative(tmp_path) == PurePosixPath("research/notes")
    assert pp.notes_root(tmp_path) == tmp_path / "research/notes"


def test_notes_root_comes_from_the_conductor_table(tmp_path):
    _write(tmp_path, '[tool.conductor]\nnotes_root = "docs"\n')
    resolved = pp.project_paths(tmp_path)
    assert resolved.notes_relative == PurePosixPath("docs")
    assert resolved.notes_configured is True
    assert pp.notes_relative(tmp_path) == PurePosixPath("docs")
    assert pp.notes_root(tmp_path) == tmp_path / "docs"


def test_notes_root_environment_overrides_the_table(tmp_path, monkeypatch):
    _write(tmp_path, '[tool.conductor]\nnotes_root = "docs"\n')
    monkeypatch.setenv(pp.NOTES_ROOT_ENV, "knowledge/cards")
    assert pp.notes_relative(tmp_path) == PurePosixPath("knowledge/cards")


def test_notes_root_refuses_an_unusable_value(tmp_path):
    _write(tmp_path, '[tool.conductor]\nnotes_root = "../elsewhere"\n')
    with pytest.raises(pp.ProjectPathError):
        pp.notes_relative(tmp_path)


def test_a_host_configured_with_the_monorepo_default_still_resolves_there(tmp_path):
    """A host that spells out `research/notes` gets it, not the package default's
    refusal to special-case itself: configured-literal and default agree."""
    _write(tmp_path, '[tool.conductor]\nnotes_root = "research/notes"\n')
    resolved = pp.project_paths(tmp_path)
    assert resolved.notes_configured is True
    assert pp.notes_root(tmp_path) == tmp_path / "research/notes"


def test_notes_db_defaults_beside_the_notes_tree_not_in_the_run_database():
    """The default that was wrong until 2026-09-16.

    ``index_notes`` hardcoded ``research/runs.db``, so a host that split its
    databases indexed prose into the run database while every documented reader
    -- the monorepo's ``defaults.NOTES_DB``, its CLAUDE.md, ``build_obsidian_ops``
    -- opened ``research/notes.db`` and saw a frozen index. Nothing pinned the
    literal, so the drift was silent; this is that pin.
    """
    assert pp.DEFAULT_NOTES_DB == PurePosixPath("research/notes.db")
    assert pp.DEFAULTS[pp.NOTES_DB_KEY] == pp.DEFAULT_NOTES_DB
    assert pp.DEFAULT_NOTES_DB != pp.DEFAULT_NOTES_ROOT


def test_notes_db_is_the_default_without_configuration(tmp_path):
    resolved = pp.project_paths(tmp_path)
    assert resolved.notes_db_relative == PurePosixPath("research/notes.db")
    assert resolved.notes_db_configured is False
    assert resolved.notes_db_path == tmp_path / "research/notes.db"
    assert pp.notes_db_path(tmp_path) == tmp_path / "research/notes.db"


def test_notes_db_comes_from_the_conductor_table(tmp_path):
    """A host that never split its databases says so once and keeps one file."""
    _write(tmp_path, '[tool.conductor]\nnotes_db = "research/runs.db"\n')
    resolved = pp.project_paths(tmp_path)
    assert resolved.notes_db_relative == PurePosixPath("research/runs.db")
    assert resolved.notes_db_configured is True
    assert pp.notes_db_path(tmp_path) == tmp_path / "research/runs.db"


def test_notes_db_environment_overrides_the_table(tmp_path, monkeypatch):
    _write(tmp_path, '[tool.conductor]\nnotes_db = "research/runs.db"\n')
    monkeypatch.setenv(pp.NOTES_DB_ENV, "var/prose.db")
    assert pp.notes_db_path(tmp_path) == tmp_path / "var/prose.db"


def test_notes_db_refuses_an_unusable_value(tmp_path):
    _write(tmp_path, '[tool.conductor]\nnotes_db = "/absolute/prose.db"\n')
    with pytest.raises(pp.ProjectPathError):
        pp.notes_db_path(tmp_path)


def test_guardrail_allowlist_defaults_to_the_monorepo_literal():
    assert pp.DEFAULT_GUARDRAIL_ALLOWLIST == PurePosixPath(
        "conductor/guardrail_allowlist.json"
    )
    assert pp.DEFAULTS[pp.GUARDRAIL_ALLOWLIST_KEY] == pp.DEFAULT_GUARDRAIL_ALLOWLIST


def test_guardrail_allowlist_is_the_default_without_configuration(tmp_path):
    resolved = pp.project_paths(tmp_path)
    assert resolved.guardrail_allowlist_relative == PurePosixPath(
        "conductor/guardrail_allowlist.json"
    )
    assert resolved.guardrail_allowlist_configured is False
    assert (
        resolved.guardrail_allowlist_path
        == tmp_path / "conductor/guardrail_allowlist.json"
    )
    assert pp.guardrail_allowlist_relative(tmp_path) == PurePosixPath(
        "conductor/guardrail_allowlist.json"
    )
    assert (
        pp.guardrail_allowlist_path(tmp_path)
        == tmp_path / "conductor/guardrail_allowlist.json"
    )


def test_guardrail_allowlist_comes_from_the_conductor_table(tmp_path):
    _write(tmp_path, '[tool.conductor]\nguardrail_allowlist = "policy/allow.json"\n')
    resolved = pp.project_paths(tmp_path)
    assert resolved.guardrail_allowlist_relative == PurePosixPath("policy/allow.json")
    assert resolved.guardrail_allowlist_configured is True
    assert pp.guardrail_allowlist_path(tmp_path) == tmp_path / "policy/allow.json"


def test_guardrail_allowlist_environment_overrides_the_table(tmp_path, monkeypatch):
    _write(tmp_path, '[tool.conductor]\nguardrail_allowlist = "policy/allow.json"\n')
    monkeypatch.setenv(pp.GUARDRAIL_ALLOWLIST_ENV, "other/allow.json")
    assert pp.guardrail_allowlist_relative(tmp_path) == PurePosixPath(
        "other/allow.json"
    )


def test_guardrail_allowlist_refuses_an_unusable_value(tmp_path):
    _write(tmp_path, '[tool.conductor]\nguardrail_allowlist = "../elsewhere.json"\n')
    with pytest.raises(pp.ProjectPathError):
        pp.guardrail_allowlist_relative(tmp_path)


def test_crate_roster_defaults_to_the_monorepo_literal():
    assert pp.DEFAULT_CRATE_ROSTER == PurePosixPath("tooling/native/crates.toml")
    assert pp.DEFAULTS[pp.CRATE_ROSTER_KEY] == pp.DEFAULT_CRATE_ROSTER


def test_crate_roster_is_the_default_without_configuration(tmp_path):
    resolved = pp.project_paths(tmp_path)
    assert resolved.crate_roster_relative == PurePosixPath("tooling/native/crates.toml")
    assert resolved.crate_roster_configured is False
    assert resolved.crate_roster_path == tmp_path / "tooling/native/crates.toml"
    assert pp.crate_roster_relative(tmp_path) == PurePosixPath(
        "tooling/native/crates.toml"
    )
    assert pp.crate_roster_path(tmp_path) == tmp_path / "tooling/native/crates.toml"


def test_crate_roster_comes_from_the_conductor_table(tmp_path):
    _write(tmp_path, '[tool.conductor]\ncrate_roster = "native/roster.toml"\n')
    resolved = pp.project_paths(tmp_path)
    assert resolved.crate_roster_relative == PurePosixPath("native/roster.toml")
    assert resolved.crate_roster_configured is True
    assert pp.crate_roster_path(tmp_path) == tmp_path / "native/roster.toml"


def test_crate_roster_environment_overrides_the_table(tmp_path, monkeypatch):
    _write(tmp_path, '[tool.conductor]\ncrate_roster = "native/roster.toml"\n')
    monkeypatch.setenv(pp.CRATE_ROSTER_ENV, "other/roster.toml")
    assert pp.crate_roster_relative(tmp_path) == PurePosixPath("other/roster.toml")


def test_crate_roster_refuses_an_unusable_value(tmp_path):
    _write(tmp_path, '[tool.conductor]\ncrate_roster = "../elsewhere.toml"\n')
    with pytest.raises(pp.ProjectPathError):
        pp.crate_roster_relative(tmp_path)


def test_the_distribution_name_matches_the_package_resources_copy():
    """Two spellings of one name, deliberately not shared.

    ``package_resources`` verifies wheel bytes and keeps its import graph two
    modules wide -- the minimal wheel its own tests install carries nothing else --
    so it cannot import this constant. That is the trade; this is the pin that
    stops a rename from taking only one side.
    """
    from conductor import package_resources

    assert package_resources._DISTRIBUTION_NAME == pp.DISTRIBUTION_NAME


def test_native_root_defaults_to_the_monorepo_literal():
    """The constant that disarmed two checks until 2026-09-16.

    ``native_freshness.crates`` scanned the module constant ``tooling/native`` and
    returned ``()`` for any host on another layout -- this repository among them,
    whose crates are in ``native/``. Both it and ``crg_venv_sync`` then answered
    every freshness question with "nothing to compare" rather than comparing.
    """
    assert pp.DEFAULT_NATIVE_ROOT == PurePosixPath("tooling/native")
    assert pp.DEFAULTS[pp.NATIVE_ROOT_KEY] == pp.DEFAULT_NATIVE_ROOT
    # The roster and the sources are separate keys: one names which crates are
    # linted, the other where they live, and a host can move either alone.
    assert pp.NATIVE_ROOT_KEY != pp.CRATE_ROSTER_KEY


def test_native_root_is_the_default_without_configuration(tmp_path):
    resolved = pp.project_paths(tmp_path)
    assert resolved.native_root_relative == PurePosixPath("tooling/native")
    assert resolved.native_root_configured is False
    assert resolved.native_root_path == tmp_path / "tooling/native"
    assert pp.native_root(tmp_path) == tmp_path / "tooling/native"


def test_native_root_comes_from_the_conductor_table(tmp_path):
    _write(tmp_path, '[tool.conductor]\nnative_root = "native"\n')
    resolved = pp.project_paths(tmp_path)
    assert resolved.native_root_relative == PurePosixPath("native")
    assert resolved.native_root_configured is True
    assert pp.native_root(tmp_path) == tmp_path / "native"


def test_native_root_environment_overrides_the_table(tmp_path, monkeypatch):
    _write(tmp_path, '[tool.conductor]\nnative_root = "native"\n')
    monkeypatch.setenv(pp.NATIVE_ROOT_ENV, "crates")
    assert pp.native_root(tmp_path) == tmp_path / "crates"


def test_native_root_refuses_an_unusable_value(tmp_path):
    _write(tmp_path, '[tool.conductor]\nnative_root = "/elsewhere/native"\n')
    with pytest.raises(pp.ProjectPathError):
        pp.native_root(tmp_path)


def test_this_repository_declares_where_its_own_crates_are():
    """The fix is only live because this repository says so in its own table."""
    here = Path(__file__).resolve().parents[2]
    assert pp.project_paths(here).native_root_configured is True
    assert pp.native_root(here) == here / "native"


def test_memory_sources_defaults_to_the_host_conductor_dir(tmp_path, monkeypatch):
    monkeypatch.delenv(pp.MEMORY_SOURCES_ENV, raising=False)
    paths = pp.project_paths(tmp_path)
    assert paths.memory_sources_relative == PurePosixPath(
        "conductor/memory_sources.toml"
    )
    assert paths.memory_sources_configured is False
    assert paths.memory_sources_path == tmp_path / "conductor" / "memory_sources.toml"


def test_memory_sources_follows_the_conductor_table(tmp_path, monkeypatch):
    monkeypatch.delenv(pp.MEMORY_SOURCES_ENV, raising=False)
    _write(tmp_path, '[tool.conductor]\nmemory_sources = "cfg/catalog.toml"\n')
    paths = pp.project_paths(tmp_path)
    assert paths.memory_sources_relative == PurePosixPath("cfg/catalog.toml")
    assert paths.memory_sources_configured is True


def test_memory_sources_env_beats_the_conductor_table(tmp_path, monkeypatch):
    _write(tmp_path, '[tool.conductor]\nmemory_sources = "cfg/catalog.toml"\n')
    monkeypatch.setenv(pp.MEMORY_SOURCES_ENV, "env/catalog.toml")
    assert pp.memory_sources_relative(tmp_path) == PurePosixPath("env/catalog.toml")


def test_memory_sources_refuses_an_absolute_value(tmp_path, monkeypatch):
    monkeypatch.setenv(pp.MEMORY_SOURCES_ENV, "/etc/catalog.toml")
    with pytest.raises(pp.ProjectPathError):
        pp.project_paths(tmp_path)


def test_worktree_patterns_defaults_to_the_monorepo_literals(tmp_path):
    assert pp.worktree_patterns(tmp_path) == (
        r"/tmp/llm-[\w.-]+",
        r"/home/\w+/Projects/LLM[\w.-]*",
    )


def test_worktree_patterns_follows_the_conductor_table(tmp_path):
    _write(
        tmp_path,
        "[tool.conductor]\n" r'worktree_patterns = ["/srv/work/[\\w.-]+"]' "\n",
    )
    assert pp.worktree_patterns(tmp_path) == (r"/srv/work/[\w.-]+",)


def test_worktree_patterns_has_no_environment_override(tmp_path, monkeypatch):
    # Matches retired_integration_branches: a worktree layout is host history,
    # not a per-invocation answer, so only [tool.conductor] can name it.
    monkeypatch.setenv("CONDUCTOR_WORKTREE_PATTERNS", "/should/not/apply")
    assert pp.worktree_patterns(tmp_path) == pp.DEFAULT_WORKTREE_PATTERNS


def test_worktree_patterns_refuses_a_non_list_value(tmp_path):
    _write(tmp_path, '[tool.conductor]\nworktree_patterns = "/tmp/foo"\n')
    with pytest.raises(pp.ProjectPathError, match="must be a list of strings"):
        pp.worktree_patterns(tmp_path)


def test_worktree_patterns_refuses_an_invalid_regex(tmp_path):
    _write(tmp_path, '[tool.conductor]\nworktree_patterns = ["/tmp/[unclosed"]\n')
    with pytest.raises(pp.ProjectPathError, match="not a valid regex"):
        pp.worktree_patterns(tmp_path)


def test_worktree_patterns_refuses_an_empty_string(tmp_path):
    _write(tmp_path, '[tool.conductor]\nworktree_patterns = [""]\n')
    with pytest.raises(pp.ProjectPathError, match="must not be empty"):
        pp.worktree_patterns(tmp_path)
