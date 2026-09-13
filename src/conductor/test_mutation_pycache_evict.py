"""The eviction plugin: pytest startup is the only hook inside fest's children."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

from conductor import mutation_pycache_evict as eviction
from conductor.bytecode_isolation import cache_paths_for, scratch_root_for
from conductor.mutation_pycache_evict import (
    PLUGIN_NAME,
    SCRATCH_ENV,
    SOURCES_ENV,
    evict_now,
)

_SRC = Path(__file__).resolve().parents[1]


def _engine_child_env(scratch: Path, sources: tuple[Path, ...]) -> dict[str, str]:
    """A minimal engine-style environment: the run's caches, named.

    Deliberately built from scratch rather than inherited, so the test says
    exactly what a fest child is told and nothing the host happens to carry.
    """

    return {
        "PATH": os.environ.get("PATH", ""),
        "PYTHONPATH": str(_SRC),
        "PYTHONPYCACHEPREFIX": str(scratch / "pycache"),
        SCRATCH_ENV: str(scratch),
        SOURCES_ENV: os.pathsep.join(str(source) for source in sources),
    }


def test_evict_now_deletes_every_tag_of_the_named_sources(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Plain, `.opt-1` and `.opt-2` copies all go -- a survivor in any tag is stale."""

    source = tmp_path / "m.py"
    source.write_text("x = 1\n", encoding="utf-8")
    scratch = scratch_root_for(tmp_path)
    caches = cache_paths_for(source, scratch / "pycache")
    for cache in caches:
        cache.parent.mkdir(parents=True, exist_ok=True)
        cache.write_bytes(b"stale")

    monkeypatch.setenv(SCRATCH_ENV, str(scratch))
    monkeypatch.setenv(SOURCES_ENV, str(source))

    removed = evict_now()

    assert len(removed) == len(caches)
    assert not any(cache.exists() for cache in caches)


def test_evict_now_without_both_engine_variables_does_nothing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """No engine named these caches, so the import must not go looking for any."""

    monkeypatch.delenv(SCRATCH_ENV, raising=False)
    monkeypatch.delenv(SOURCES_ENV, raising=False)
    assert evict_now() == []

    # Half a configuration is not a license to guess where the caches live.
    monkeypatch.setenv(SOURCES_ENV, str(tmp_path / "m.py"))
    assert evict_now() == []


def test_a_broken_eviction_fails_closed_on_the_whole_prefix(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Eviction trouble must never kill the child -- engines read that as a kill.

    A mutant of the cache mapping raises instead of mapping (the exact
    failure a shared helper would import into this child). The plugin must
    not raise: it deletes the run's whole prefix tree instead, so every child
    of the run recompiles and the tests still grade the mutant honestly --
    slower, never wrong. The scratch is a real one, marked the way the
    launcher marks it, which is what licenses the deletion.
    """

    source = tmp_path / "m.py"
    source.write_text("x = 1\n", encoding="utf-8")
    scratch = scratch_root_for(tmp_path)
    stale = cache_paths_for(source, scratch / "pycache")[0]
    stale.parent.mkdir(parents=True)
    stale.write_bytes(b"stale")
    (scratch / eviction._RUN_MARKER).write_text("", encoding="utf-8")
    monkeypatch.setenv(SCRATCH_ENV, str(scratch))
    monkeypatch.setenv(SOURCES_ENV, str(source))

    def broken(source: str, prefix: Path) -> list[Path]:
        raise TypeError("mutated away")

    monkeypatch.setattr(eviction, "_cache_paths", broken)

    assert evict_now() == []
    assert not scratch.exists()


def _broken_cache_paths(source: str, prefix: Path) -> list[Path]:
    """A cache mapping that raises -- the failure the fail-closed path answers."""

    raise TypeError("mutated away")


def test_the_fail_closed_deletion_requires_the_run_marker(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A scratch without the launcher's marker is not this run's to delete.

    The marker is the only thing distinguishing "a directory the run
    created" from "a directory someone pointed the env var at"; without it
    the deletion raises, the directory survives whole, and the failure is
    loud with the path in the message.
    """

    scratch = scratch_root_for(tmp_path)
    (scratch / "pycache").mkdir(parents=True)
    monkeypatch.setenv(SCRATCH_ENV, str(scratch))
    monkeypatch.setenv(SOURCES_ENV, str(tmp_path / "m.py"))
    monkeypatch.setattr(eviction, "_cache_paths", _broken_cache_paths)

    with pytest.raises(RuntimeError, match="is absent"):
        evict_now()
    assert (scratch / "pycache").is_dir(), "a refused deletion must not delete anything"


def test_the_fail_closed_deletion_requires_a_pycache_tree(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Marker or not, a directory with no `pycache/` is not a run scratch."""

    scratch = tmp_path / "marked-empty"
    scratch.mkdir()
    (scratch / eviction._RUN_MARKER).write_text("", encoding="utf-8")
    monkeypatch.setenv(SCRATCH_ENV, str(scratch))
    monkeypatch.setenv(SOURCES_ENV, str(tmp_path / "m.py"))
    monkeypatch.setattr(eviction, "_cache_paths", _broken_cache_paths)

    with pytest.raises(RuntimeError, match="pycache does not exist"):
        evict_now()
    assert scratch.is_dir()


def test_the_fail_closed_deletion_refuses_the_four_forbidden_places(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Root, home, the repo checkout and any parent of cwd never die, marker or not.

    The identity checks run before the contents checks, so each forbidden
    place is refused for what it is even when a `pycache/` and marker sit
    inside it -- which is exactly how this test can plant them on a fake
    repository root without touching anything real. `/` needs nothing
    planted: refusing it must not depend on what it contains.
    """

    forbidden = tmp_path / "forbidden"
    forbidden.mkdir()
    (forbidden / "pycache").mkdir()
    (forbidden / eviction._RUN_MARKER).write_text("", encoding="utf-8")

    # A fake repository root with the process cwd inside a workdir under it.
    repo = tmp_path / "repo"
    (repo / ".git").mkdir(parents=True)
    (repo / "pycache").mkdir()
    (repo / eviction._RUN_MARKER).write_text("", encoding="utf-8")
    workdir = repo / "work"
    workdir.mkdir()
    monkeypatch.chdir(workdir)

    home = tmp_path / "home"
    home.mkdir()
    (home / "pycache").mkdir()
    (home / eviction._RUN_MARKER).write_text("", encoding="utf-8")
    monkeypatch.setenv("HOME", str(home))

    places = {
        "/": "filesystem root",
        str(home): "home directory",
        str(repo): "repository root",
        str(tmp_path): "parent of the working directory",
    }
    for place, why in places.items():
        with pytest.raises(RuntimeError, match=why):
            eviction._fail_closed_delete(place)
    for victim in (home, repo, workdir):
        assert victim.is_dir(), f"{victim} must survive every refusal"
    # The refused places keep their planted contents, and the same planted
    # shape in an ordinary directory still dies: the bound refuses the four
    # places, not the deletion.
    assert (repo / "pycache").is_dir() and (repo / eviction._RUN_MARKER).is_file()
    eviction._fail_closed_delete(str(forbidden))
    assert not forbidden.exists()


def test_the_fail_closed_deletion_refuses_the_working_directory_itself(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """cwd is the one forbidden place the four-places test cannot plant on.

    That test chdirs into a workdir to make a parent-of-cwd case; the cwd
    itself is refused before any contents check, so a planted scratch at
    exactly the process cwd must raise and survive whole. This is the
    deletion's cheapest catastrophic failure -- without this refusal the
    `parent of cwd` rule is one `resolve()` short of deleting the checkout
    the run is happening in.
    """

    scratch = tmp_path / "here"
    scratch.mkdir()
    (scratch / "pycache").mkdir()
    (scratch / eviction._RUN_MARKER).write_text("", encoding="utf-8")
    monkeypatch.chdir(scratch)

    with pytest.raises(RuntimeError, match="is the working directory itself"):
        eviction._fail_closed_delete(str(scratch))

    assert scratch.is_dir() and (scratch / "pycache").is_dir()


def test_the_deletion_marker_literal_matches_the_one_the_launcher_writes() -> None:
    """Two literals, one name: the launcher writes it, the plugin demands it.

    The plugin cannot import `bytecode_isolation` (it runs inside the
    children whose modules are mutated), so the marker name exists twice on
    purpose. If one side renames it, every real fail-closed deletion starts
    refusing -- this test is where that surfaces first.
    """

    from conductor.bytecode_isolation import RUN_MARKER_NAME

    assert eviction._RUN_MARKER == RUN_MARKER_NAME


def test_the_plugin_imports_nothing_of_the_module_under_mutation() -> None:
    """The eviction must survive mutants of the code it would otherwise import.

    This plugin runs inside the children whose modules are mutated; if it
    reached for `conductor.bytecode_isolation`, a mutant that breaks those
    helpers would break the eviction too, the child would die at pytest
    startup, and an engine grading by exit code would count a kill for which
    no test ever ran. `conductor` is a namespace package, so importing the
    plugin pulls in exactly one module -- this pins that.
    """

    completed = subprocess.run(
        [
            sys.executable,
            "-c",
            "import conductor.mutation_pycache_evict, sys; "
            "assert 'conductor.bytecode_isolation' not in sys.modules",
        ],
        cwd=_SRC.parent,
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert completed.returncode == 0, completed.stderr


def test_the_plugin_evicts_at_pytest_startup_before_the_imports(
    tmp_path: Path,
) -> None:
    """End to end: the plugin closes the trap inside a real pytest child.

    Builds the engine's exact situation. A first interpreter caches `m`
    (returning 1) under the run prefix; the source is then rewritten same-size
    with its mtime pinned to the same second; a pytest child is launched the
    way fest launches its own -- `PYTEST_ADDOPTS` loading the plugin, the run's
    caches named in the environment, a test that asserts the mutant's `2`.
    The first child, run without the eviction variables, executes the stale
    bytecode and fails: that is the trap the plugin exists to close, proven
    live in this setup. The second, with the plugin loaded, passes.
    """

    source = tmp_path / "m.py"
    source.write_text("def f(): return 1\n", encoding="utf-8")
    before = source.stat()
    (tmp_path / "test_m.py").write_text(
        "import m\n\n\ndef test_mutant():\n    assert m.f() == 2\n",
        encoding="utf-8",
    )
    scratch = scratch_root_for(tmp_path)

    warm = _engine_child_env(scratch, ())
    del warm[SCRATCH_ENV], warm[SOURCES_ENV]
    subprocess.run(
        [sys.executable, "-c", "import m"],
        cwd=tmp_path,
        env=warm,
        capture_output=True,
        timeout=120,
        check=True,
    )
    assert cache_paths_for(source, scratch / "pycache")[0].is_file()

    source.write_text("def f(): return 2\n", encoding="utf-8")
    os.utime(source, ns=(before.st_atime_ns, before.st_mtime_ns))

    # The trap, live: this child reads the cache the warm run left and its
    # test fails against the unmutated bytecode.
    trapped = dict(warm)
    trapped["PYTEST_ADDOPTS"] = "-q"
    stale = subprocess.run(
        [sys.executable, "-m", "pytest", "-q", "test_m.py"],
        cwd=tmp_path,
        env=trapped,
        capture_output=True,
        text=True,
        timeout=300,
    )
    assert stale.returncode != 0
    # The stale value itself, named: this failure is the unmutated bytecode.
    assert "assert 1 == 2" in stale.stdout, stale.stdout

    # The plugin, loaded the way fest's environment loads it, evicts first.
    evicted = _engine_child_env(scratch, (source,))
    evicted["PYTEST_ADDOPTS"] = f"-q -p {PLUGIN_NAME}"
    fresh = subprocess.run(
        [sys.executable, "-m", "pytest", "-q", "test_m.py"],
        cwd=tmp_path,
        env=evicted,
        capture_output=True,
        text=True,
        timeout=300,
    )
    assert fresh.returncode == 0, fresh.stdout
