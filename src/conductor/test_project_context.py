from __future__ import annotations

import hashlib
import os
import subprocess
from pathlib import Path

import pytest

from conductor import project_context
from conductor.project_context import (
    ContextError,
    require_git_context,
    resolve_project_context,
)


def _git(path: Path, *args: str) -> None:
    assert any(
        parent.name.startswith("test_") and parent.parent.name.startswith("pytest-")
        for parent in (path, *path.parents)
    )
    environment = {
        "PATH": "/usr/bin:/bin",
        "LC_ALL": "C",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
    }
    subprocess.run(
        ["/usr/bin/git", "-C", str(path), *args],
        check=True,
        env=environment,
        capture_output=True,
        timeout=10,
    )


def _repository(path: Path) -> Path:
    path.mkdir()
    _git(path, "init", "-q")
    _git(path, "config", "user.email", "fixture@example.invalid")
    _git(path, "config", "user.name", "fixture")
    (path / "tracked.txt").write_text("fixture\n", encoding="utf-8")
    _git(path, "add", "tracked.txt")
    _git(path, "commit", "-qm", "fixture")
    return path


def _error_code(call: object) -> str:
    with pytest.raises(ContextError) as caught:
        call()  # type: ignore[operator]
    return caught.value.detail.code


def _tree_digest(root: Path) -> str:
    digest = hashlib.sha256()
    for path in sorted(root.rglob("*")):
        digest.update(path.relative_to(root).as_posix().encode())
        if path.is_file() and not path.is_symlink():
            digest.update(path.read_bytes())
    return digest.hexdigest()


def test_git_context_is_immutable_and_leaves_project_unchanged(tmp_path: Path) -> None:
    repository = _repository(tmp_path / "repository")
    (repository / "nested").mkdir()
    before = _tree_digest(repository)

    context = resolve_project_context(
        project=repository / "nested",
        environment={"GIT_DIR": "/poisoned", "GIT_WORK_TREE": "/poisoned"},
    )

    assert context.repo_root == repository.resolve()
    assert context.worktree_root == repository.resolve()
    assert context.git_dir is not None and context.git_common_dir is not None
    assert context.repository_key is not None and context.worktree_key is not None
    expected_repository = hashlib.sha256(
        b"conductor.repository.v1\0" + os.fsencode(str(context.git_common_dir))
    ).hexdigest()
    expected_worktree = hashlib.sha256(
        b"conductor.worktree.v1\0"
        + os.fsencode(str(context.git_dir))
        + b"\0"
        + os.fsencode(str(context.worktree_root))
    ).hexdigest()
    assert context.repository_key == f"repo-v1-{expected_repository}"
    assert context.worktree_key == f"wt-v1-{expected_worktree}"
    assert context.project_id == context.repository_key
    assert context.state_dir is not None and not context.state_dir.exists()
    assert tuple(item.field for item in context.provenance) == tuple(
        field for field in context.__dataclass_fields__ if field != "provenance"
    )
    assert all(item.config_sha256 is None for item in context.provenance)
    assert _tree_digest(repository) == before
    with pytest.raises((AttributeError, TypeError)):
        context.repository_key = "other"  # type: ignore[misc]


def test_explicit_project_wins_and_empty_environment_refuses(tmp_path: Path) -> None:
    first = _repository(tmp_path / "first")
    second = _repository(tmp_path / "second")

    selected = resolve_project_context(
        project=first, environment={"CONDUCTOR_PROJECT_DIR": str(second)}
    )

    assert selected.repo_root == first.resolve()
    assert (
        _error_code(
            lambda: resolve_project_context(
                start_dir=first, environment={"CONDUCTOR_PROJECT_DIR": ""}
            )
        )
        == "INVALID_ARGUMENT"
    )
    assert (
        _error_code(
            lambda: resolve_project_context(
                start_dir=first, environment={"CONDUCTOR_PROJECT_DIR": "relative"}
            )
        )
        == "INVALID_PATH"
    )


def test_read_only_requires_explicit_project_and_never_discovers_git(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    directory = tmp_path / "plain"
    directory.mkdir()

    def unexpected(_selected: Path) -> tuple[Path, Path, Path]:
        raise AssertionError("read-only mode must not invoke Git")

    monkeypatch.setattr(project_context, "_git_paths", unexpected)
    context = resolve_project_context(project=directory, mode="read_only")

    assert context.repo_root == directory.resolve()
    assert context.git_dir is context.git_common_dir is None
    assert context.repository_key is context.worktree_key is None
    assert context.state_dir is context.cache_dir is context.artifact_dir is None
    assert _error_code(lambda: require_git_context(context)) == "UNSUPPORTED_REPOSITORY"
    assert (
        _error_code(
            lambda: resolve_project_context(start_dir=directory, mode="read_only")
        )
        == "INVALID_ARGUMENT"
    )


def test_config_schema_relative_paths_and_external_notes_grant(tmp_path: Path) -> None:
    repository = _repository(tmp_path / "repository")
    configuration = repository / ".conductor"
    configuration.mkdir()
    policy = configuration / "policy.toml"
    registry = configuration / "registry.json"
    notes = repository / "notes"
    outside = tmp_path / "outside-notes"
    policy.write_text("policy\n", encoding="utf-8")
    registry.write_text("{}\n", encoding="utf-8")
    notes.mkdir()
    outside.mkdir()
    config = configuration / "project.toml"
    config.write_text(
        """schema_version = 1
[project]
id = "same-label"
[paths]
policy = "policy.toml"
registry = "registry.json"
notes = ["../notes", "../notes", "../../outside-notes"]
""",
        encoding="utf-8",
    )

    assert (
        _error_code(lambda: resolve_project_context(project=repository))
        == "PATH_OUTSIDE_PROJECT"
    )
    context = resolve_project_context(
        project=repository,
        start_dir=repository,
        allowed_external_notes_roots=(Path("../outside-notes"),),
    )

    assert context.project_id == "same-label"
    assert context.policy_path == policy.resolve()
    assert context.registry_path == registry.resolve()
    assert context.notes_roots == (notes.resolve(), outside.resolve())
    config_provenance = next(
        item for item in context.provenance if item.field == "config_path"
    )
    assert (
        config_provenance.config_sha256
        == hashlib.sha256(config.read_bytes()).hexdigest()
    )
    config.write_text("schema_version = true\n", encoding="utf-8")
    assert (
        _error_code(lambda: resolve_project_context(project=repository))
        == "CONFIG_SCHEMA"
    )
    config.write_text("schema_version = 1\n", encoding="utf-8")
    outside_file = tmp_path / "outside-reference.toml"
    outside_file.write_text("outside\n", encoding="utf-8")
    assert (
        _error_code(
            lambda: resolve_project_context(project=repository, policy=outside_file)
        )
        == "PATH_OUTSIDE_PROJECT"
    )
    assert (
        _error_code(
            lambda: resolve_project_context(project=repository, registry=outside_file)
        )
        == "PATH_OUTSIDE_PROJECT"
    )
    config.write_text(
        'schema_version = 1\n[project]\nid = "valid\u0085but-control"\n',
        encoding="utf-8",
    )
    assert (
        _error_code(lambda: resolve_project_context(project=repository))
        == "CONFIG_SCHEMA"
    )


def test_config_and_reference_containment_refuse_symlink_and_escape(
    tmp_path: Path,
) -> None:
    repository = _repository(tmp_path / "repository")
    outside = tmp_path / "outside.toml"
    outside.write_text("schema_version = 1\n", encoding="utf-8")
    holder = repository / ".conductor"
    holder.mkdir()
    linked = holder / "project.toml"
    linked.symlink_to(outside)

    assert (
        _error_code(lambda: resolve_project_context(project=repository))
        == "PATH_OUTSIDE_PROJECT"
    )
    assert (
        _error_code(lambda: resolve_project_context(project=repository, config=outside))
        == "PATH_OUTSIDE_PROJECT"
    )
    assert (
        _error_code(
            lambda: resolve_project_context(
                project=repository, config=repository / "missing.toml"
            )
        )
        == "CONFIG_NOT_FOUND"
    )
    linked.unlink()
    linked.symlink_to(repository / "missing-target.toml")
    assert (
        _error_code(lambda: resolve_project_context(project=repository)) == "CONFIG_IO"
    )
    linked.unlink()
    controlled = repository / "control\nconfig.toml"
    controlled.write_text("schema_version = 1\n", encoding="utf-8")
    linked.symlink_to(controlled)
    assert (
        _error_code(lambda: resolve_project_context(project=repository))
        == "INVALID_PATH"
    )
    linked.unlink()
    holder.rmdir()
    holder.symlink_to(repository / "missing-config-directory", target_is_directory=True)
    assert (
        _error_code(lambda: resolve_project_context(project=repository)) == "CONFIG_IO"
    )


def test_config_open_refuses_fifo_and_intermediate_swap(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repository = _repository(tmp_path / "repository")
    holder = repository / ".conductor"
    holder.mkdir()
    config = holder / "project.toml"
    config.write_text("schema_version = 1\n", encoding="utf-8")
    original_open = project_context.os.open
    swapped = False

    def fifo_swap(path: object, flags: int, *, dir_fd: int | None = None) -> int:
        nonlocal swapped
        if dir_fd is not None and not swapped:
            config.unlink()
            os.mkfifo(config)
            swapped = True
        return original_open(path, flags, dir_fd=dir_fd)

    monkeypatch.setattr(project_context.os, "open", fifo_swap)
    assert (
        _error_code(lambda: resolve_project_context(project=repository)) == "CONFIG_IO"
    )
    monkeypatch.undo()
    config.unlink()
    config.write_text("schema_version = 1\n", encoding="utf-8")
    outside = tmp_path / "outside"
    outside.mkdir()
    (outside / "project.toml").write_text("schema_version = 1\n", encoding="utf-8")
    moved = repository / ".conductor-before-swap"
    swapped = False

    def ancestor_swap(path: object, flags: int, *, dir_fd: int | None = None) -> int:
        nonlocal swapped
        if dir_fd is not None and not swapped:
            holder.rename(moved)
            holder.symlink_to(outside, target_is_directory=True)
            swapped = True
        return original_open(path, flags, dir_fd=dir_fd)

    monkeypatch.setattr(project_context.os, "open", ancestor_swap)
    assert (
        _error_code(lambda: resolve_project_context(project=repository)) == "CONFIG_IO"
    )


def test_identity_distinguishes_unrelated_and_linked_worktrees(tmp_path: Path) -> None:
    first = _repository(tmp_path / "first")
    second = _repository(tmp_path / "second")
    for repository in (first, second):
        config = repository / ".conductor"
        config.mkdir()
        (config / "project.toml").write_text(
            'schema_version = 1\n[project]\nid = "same"\n', encoding="utf-8"
        )
    first_context = resolve_project_context(project=first)
    second_context = resolve_project_context(project=second)
    assert first_context.project_id == second_context.project_id == "same"
    assert first_context.repository_key != second_context.repository_key

    linked = tmp_path / "linked"
    _git(first, "worktree", "add", "--detach", str(linked), "HEAD")
    linked_context = resolve_project_context(project=linked)
    assert linked_context.repository_key == first_context.repository_key
    assert linked_context.worktree_key != first_context.worktree_key
    assert linked_context.git_dir != first_context.git_dir


def test_submodule_and_separate_git_dir_select_own_topology(tmp_path: Path) -> None:
    leaf = _repository(tmp_path / "leaf")
    superproject = _repository(tmp_path / "superproject")
    environment = {
        "PATH": "/usr/bin:/bin",
        "LC_ALL": "C",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
    }
    subprocess.run(
        [
            "/usr/bin/git",
            "-C",
            str(superproject),
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            str(leaf),
            "vendor/leaf",
        ],
        check=True,
        env=environment,
        capture_output=True,
        timeout=10,
    )
    _git(superproject, "commit", "-qm", "submodule")
    submodule = resolve_project_context(project=superproject / "vendor" / "leaf")
    parent = resolve_project_context(project=superproject)
    assert submodule.repo_root == (superproject / "vendor" / "leaf").resolve()
    assert submodule.repository_key != parent.repository_key

    separate = tmp_path / "separate"
    git_dir = tmp_path / "separate-git"
    subprocess.run(
        ["/usr/bin/git", "init", "-q", f"--separate-git-dir={git_dir}", str(separate)],
        check=True,
        env=environment,
        capture_output=True,
        timeout=10,
    )
    _git(separate, "config", "user.email", "fixture@example.invalid")
    _git(separate, "config", "user.name", "fixture")
    (separate / "tracked").write_text("x", encoding="utf-8")
    _git(separate, "add", "tracked")
    _git(separate, "commit", "-qm", "fixture")
    separate_context = resolve_project_context(project=separate)
    assert separate_context.git_dir == git_dir.resolve()


def test_git_probe_uses_fixed_environment_and_bounded_helper_contract(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    observed: dict[str, object] = {}

    class Process:
        pid = 73_421
        returncode = 0

        def __init__(self, stdout: bytes = b"ok\n") -> None:
            read_fd, write_fd = os.pipe()
            os.write(write_fd, stdout)
            os.close(write_fd)
            self.stdout = os.fdopen(read_fd, "rb", closefd=True)
            read_fd, write_fd = os.pipe()
            os.close(write_fd)
            self.stderr = os.fdopen(read_fd, "rb", closefd=True)

        def wait(self, timeout: float | None = None) -> int:
            return 0

    def fake_popen(*_args: object, **kwargs: object) -> Process:
        observed.update(kwargs)
        return Process()

    monkeypatch.setattr(project_context.subprocess, "Popen", fake_popen)
    assert project_context._bounded_git(("rev-parse",)) == b"ok\n"
    assert observed["env"] == {
        "PATH": "/usr/bin:/bin",
        "LC_ALL": "C",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_OPTIONAL_LOCKS": "0",
    }
    assert observed["start_new_session"] is True

    monkeypatch.setattr(
        project_context.subprocess,
        "Popen",
        lambda *_args, **_kwargs: Process(b"x" * (32 * 1024 + 1)),
    )
    killed: list[tuple[int, int]] = []
    monkeypatch.setattr(
        project_context.os, "killpg", lambda pid, signal: killed.append((pid, signal))
    )
    assert (
        _error_code(lambda: project_context._bounded_git(("rev-parse",)))
        == "GIT_OUTPUT_LIMIT"
    )
    assert killed == [(73_421, project_context.signal.SIGKILL)]


def test_git_probe_refuses_timeout_and_relative_topology(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    selectors_created: list[Selector] = []

    class Selector:
        def __init__(self) -> None:
            self.registered: dict[int, int] = {}
            self.closed = False
            selectors_created.append(self)

        def register(self, descriptor: int, events: int) -> None:
            if self.closed or descriptor in self.registered:
                raise RuntimeError("selector cannot register descriptor")
            self.registered[descriptor] = events

        def unregister(self, descriptor: int) -> None:
            if self.closed:
                raise RuntimeError("selector is closed")
            del self.registered[descriptor]

        def select(self, _timeout: float) -> list[object]:
            if self.closed:
                raise RuntimeError("selector is closed")
            return []

        def close(self) -> None:
            self.registered.clear()
            self.closed = True

    class Process:
        pid = 73_422

        def __init__(self) -> None:
            read_fd, self.write_fd = os.pipe()
            self.stdout = os.fdopen(read_fd, "rb", closefd=True)
            read_fd, self.error_write_fd = os.pipe()
            self.stderr = os.fdopen(read_fd, "rb", closefd=True)

        def wait(self, timeout: float | None = None) -> int:
            os.close(self.write_fd)
            os.close(self.error_write_fd)
            return 0

    monkeypatch.setattr(
        project_context.subprocess, "Popen", lambda *_args, **_kwargs: Process()
    )
    monkeypatch.setattr(project_context.selectors, "DefaultSelector", Selector)
    monotonic = iter((0.0, 4.0))
    monkeypatch.setattr(project_context.time, "monotonic", lambda: next(monotonic))
    killed: list[int] = []
    monkeypatch.setattr(
        project_context.os, "killpg", lambda pid, _signal: killed.append(pid)
    )
    assert (
        _error_code(lambda: project_context._bounded_git(("rev-parse",)))
        == "GIT_TIMEOUT"
    )
    assert killed == [73_422]
    assert len(selectors_created) == 1
    assert selectors_created[0].closed
    assert selectors_created[0].registered == {}

    relative = tmp_path / "relative"
    git_dir = tmp_path / "git"
    common_dir = tmp_path / "common"
    relative.mkdir()
    git_dir.mkdir()
    common_dir.mkdir()
    monkeypatch.chdir(tmp_path)
    topology = f"relative\n{git_dir}\n{common_dir}\n".encode()
    responses = iter((b"true\nfalse\n", topology, topology))
    monkeypatch.setattr(project_context, "_bounded_git", lambda _argv: next(responses))
    assert (
        _error_code(lambda: project_context._git_paths(relative))
        == "GIT_DISCOVERY_FAILED"
    )
