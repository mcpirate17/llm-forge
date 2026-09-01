from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from conductor import dead_tests


def _git(repo: Path, *args: str) -> None:
    command = ("git", *args)
    subprocess.run(command, cwd=repo, check=True, capture_output=True, text=True)


def _init_repo(repo: Path) -> None:
    repo.mkdir(parents=True, exist_ok=True)
    commands = (
        ("init", "-b", "main"),
        ("config", "user.email", "governance-tests@example.invalid"),
        ("config", "user.name", "Governance Tests"),
        ("config", "commit.gpgsign", "false"),
    )
    for command in commands:
        _git(repo, *command)


def _write(repo: Path, relative: str, content: str) -> None:
    destination = repo.joinpath(relative)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(content, encoding="utf-8")


def _commit_broken_test(repo: Path) -> None:
    # "pkg" must be a tracked first-party directory for the missing submodule
    # import to register as `broken` rather than being ignored as third-party.
    _write(repo, "pkg/__init__.py", "")
    _write(repo, "test_probe.py", "import pkg.missing_module\n")
    _git(repo, "add", "--all")
    _git(repo, "commit", "-m", "base")


def _commit_all(repo: Path, message: str = "base") -> None:
    _git(repo, "add", "--all")
    _git(repo, "commit", "-m", message)


def test_explicit_root_scans_the_named_repo_not_cwd(tmp_path: Path) -> None:
    target = tmp_path / "target"
    decoy = tmp_path / "decoy"
    _init_repo(target)
    _init_repo(decoy)
    _commit_broken_test(target)

    assert dead_tests.main(["--root", str(target), "--check"]) == 1


def test_default_root_uses_cwd_toplevel_not_module_location(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The bug: Path(__file__)-derived resolution always points at the
    checkout that supplied the imported module. A broken test that exists
    only in this throwaway repo is invisible under that resolution; finding
    it here proves the tool followed cwd instead.
    """
    repo = tmp_path / "repo"
    _init_repo(repo)
    _commit_broken_test(repo)

    monkeypatch.chdir(repo)
    assert dead_tests.main(["--check"]) == 1


def test_cwd_outside_worktree_refuses_rather_than_falling_back(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    outside = tmp_path / "not_a_repo"
    outside.mkdir()
    monkeypatch.chdir(outside)

    assert dead_tests.main([]) == 2


def test_resolved_root_is_printed(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    repo = tmp_path / "repo"
    _init_repo(repo)
    _git(repo, "commit", "--allow-empty", "-m", "base")

    assert dead_tests.main(["--root", str(repo), "--json-out", "out.json"]) == 0
    out = capsys.readouterr().out
    assert f"root={repo.resolve()}" in out
    assert (repo / "out.json").is_file()


def test_root_mismatch_warns(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    target = tmp_path / "target"
    decoy = tmp_path / "decoy"
    _init_repo(target)
    _init_repo(decoy)
    _git(target, "commit", "--allow-empty", "-m", "base")

    monkeypatch.chdir(decoy)
    assert dead_tests.main(["--root", str(target)]) == 0
    err = capsys.readouterr().err
    assert "WARNING" in err
    assert str(target.resolve()) in err


def test_json_out_relative_path_resolves_against_root(tmp_path: Path) -> None:
    target = tmp_path / "target"
    _init_repo(target)
    _git(target, "commit", "--allow-empty", "-m", "base")

    assert dead_tests.main(["--root", str(target)]) == 0
    assert (target / "tasks" / "audit" / "dead_tests.json").is_file()


def test_resolver_resolve_untracked_honours_explicit_root(tmp_path: Path) -> None:
    target = tmp_path / "target"
    decoy = tmp_path / "decoy"
    _init_repo(target)
    _init_repo(decoy)
    _write(decoy, "pkg/helper.py", "value = 1\n")
    resolver = dead_tests.Resolver(["main.py"], root=target)

    # helper.py exists on disk under decoy, not target: the resolver must not
    # find it when scoped to target -- proof it uses the passed root, not
    # some other tree.
    assert resolver.resolve_untracked("pkg.helper", "main.py") is None

    _write(target, "pkg/helper.py", "value = 1\n")
    assert resolver.resolve_untracked("pkg.helper", "main.py") == "pkg/helper.py"


def test_native_scan_preserves_relative_guarded_and_native_imports(
    tmp_path: Path,
) -> None:
    repo = tmp_path / "repo"
    _write(repo, "pkg/__init__.py", "")
    _write(repo, "pkg/dep.py", "VALUE = 1\n")
    _write(repo, "pkg/dynamic.py", "VALUE = 2\n")
    _write(repo, "pkg/native.rs", "pub fn marker() {}\n")
    _write(
        repo,
        "pkg/sub/module.py",
        """from typing import TYPE_CHECKING
from .. import dep
import pkg.native
if FLAG:
    import pkg.hard_missing
if TYPE_CHECKING:
    import pkg.type_missing
if typing.TYPE_CHECKING:
    import pkg.attribute_missing
if __name__ == "__main__":
    import pkg.main_missing
try:
    import pkg.try_missing
except ImportError:
    pass
def lazy():
    import pkg.lazy_missing
async def async_lazy():
    import pkg.async_missing
"pkg.dynamic"
"loader.py"
""",
    )
    tracked = [
        "pkg/__init__.py",
        "pkg/dep.py",
        "pkg/dynamic.py",
        "pkg/native.rs",
        "pkg/sub/module.py",
    ]

    module = dead_tests.scan_module(
        "pkg/sub/module.py", dead_tests.Resolver(tracked, root=repo), root=repo
    )

    assert module == dead_tests.Module(
        path="pkg/sub/module.py",
        has_main=True,
        basenames={"loader.py"},
        deps={"pkg/dep.py", "pkg/dynamic.py"},
        missing={"pkg.hard_missing"},
        soft_missing={
            "pkg.async_missing",
            "pkg.attribute_missing",
            "pkg.lazy_missing",
            "pkg.main_missing",
            "pkg.try_missing",
            "pkg.type_missing",
        },
        untracked=set(),
    )


def test_native_analysis_preserves_classification_precedence_and_order(
    tmp_path: Path,
) -> None:
    repo = tmp_path / "repo"
    _init_repo(repo)
    files = {
        "Makefile": "run: pkg/configured.py\n",
        "app.py": "import pkg.live\n",
        "loader.py": 'PLUGIN = "dyn.py"\n',
        "pkg/__init__.py": "",
        "pkg/broken_target.py": "VALUE = 1\n",
        "pkg/configured.py": "VALUE = 2\n",
        "pkg/dyn.py": "VALUE = 3\n",
        "pkg/live.py": "VALUE = 4\n",
        "pkg/orphan.py": "VALUE = 5\n",
        "pkg/stale.py": "def lazy():\n    import pkg.deleted\n",
        "test_broken.py": "import pkg.broken_target\nimport pkg.missing\n",
        "test_configured.py": "import pkg.configured\n",
        "test_dynamic.py": "import pkg.dyn\n",
        "test_live.py": "import pkg.live\n",
        "test_orphan.py": "import pkg.orphan\n",
        "test_untracked_dep.py": "import pkg.untracked\n",
    }
    for path, source in files.items():
        _write(repo, path, source)
    _commit_all(repo)
    _write(repo, "pkg/untracked.py", "VALUE = 6\n")
    _write(repo, "test_untracked_extra.py", "def test_extra():\n    pass\n")
    _write(repo, "research/notes/targets.md", "pkg/orphan.py\n")

    report = dead_tests.analyse(dead_tests.tracked_files(root=repo), root=repo)

    assert report["broken"] == [
        {
            "test": "test_broken.py",
            "missing": ["pkg.missing"],
            "last_commit": report["broken"][0]["last_commit"],
        }
    ]
    assert report["depends_on_untracked"] == [
        {
            "test": "test_untracked_dep.py",
            "untracked": ["pkg/untracked.py"],
            "last_commit": report["depends_on_untracked"][0]["last_commit"],
        }
    ]
    assert report["untracked_importers"] == {
        "pkg/untracked.py": ["test_untracked_dep.py"]
    }
    assert report["orphan_target"] == [
        {
            "test": "test_orphan.py",
            "targets": ["pkg/orphan.py"],
            "notes_only": ["pkg/orphan.py"],
            "last_commit": report["orphan_target"][0]["last_commit"],
        }
    ]
    assert report["stale_imports"] == [
        {
            "module": "pkg/stale.py",
            "missing": ["pkg.deleted"],
            "last_commit": report["stale_imports"][0]["last_commit"],
        }
    ]
    assert report["untracked_tests"] == ["test_untracked_extra.py"]
    assert all(
        row["test"] not in {"test_configured.py", "test_dynamic.py", "test_live.py"}
        for row in report["orphan_target"]
    )


def test_native_closure_handles_ten_thousand_module_cycle_deterministically() -> None:
    count = 10_000
    modules = {
        f"pkg/module_{index:05d}.py": dead_tests.Module(
            path=f"pkg/module_{index:05d}.py",
            deps={f"pkg/module_{(index + 1) % count:05d}.py"},
        )
        for index in range(count)
    }
    modules["pkg/module_09999.py"].missing.add("pkg.gone")
    modules["pkg/module_05000.py"].untracked.add("pkg/local_only.py")

    first = dead_tests.closure("pkg/module_00000.py", modules)
    second = dead_tests.closure("pkg/module_00000.py", dict(reversed(modules.items())))

    assert first == ({"pkg.gone"}, {"pkg/local_only.py"})
    assert second == first


def test_native_scan_parse_error_remains_dead_tests_error(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    _write(repo, "pkg/__init__.py", "")
    _write(repo, "pkg/broken.py", "if:\n")
    resolver = dead_tests.Resolver(["pkg/__init__.py", "pkg/broken.py"], root=repo)

    with pytest.raises(
        dead_tests.DeadTestsError,
        match=r"pkg/broken.py does not parse: invalid syntax \(broken.py, line 1\)",
    ):
        dead_tests.scan_module("pkg/broken.py", resolver, root=repo)


def test_native_closure_preserves_missing_module_key_error() -> None:
    modules = {
        "test_probe.py": dead_tests.Module(path="test_probe.py", deps={"pkg/absent.py"})
    }

    with pytest.raises(KeyError, match="pkg/absent.py"):
        dead_tests.closure("test_probe.py", modules)


def test_native_module_payload_is_json_deterministic(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    _write(repo, "pkg/__init__.py", "")
    _write(repo, "pkg/b.py", "VALUE = 1\n")
    _write(repo, "pkg/a.py", "import pkg.b\n")
    resolver = dead_tests.Resolver(
        ["pkg/b.py", "pkg/a.py", "pkg/__init__.py"], root=repo
    )

    first = resolver._native.scan_module("pkg/a.py", str(repo))
    second = resolver._native.scan_module("pkg/a.py", str(repo))

    assert first == second
    assert json.loads(first)["deps"] == ["pkg/b.py"]
