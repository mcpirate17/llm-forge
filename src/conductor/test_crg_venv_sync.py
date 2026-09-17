"""Contracts for the code-review-graph interpreter sync.

The interpreter under inspection is a real subprocess throughout: a small shell
script standing in for the server's python, steered by environment variables, so
``purelib`` and ``imports_server`` run the commands they really run. Only
``install`` -- which would write into a venv -- is substituted, and only in the
cases that exist to pin the staging order.
"""

from __future__ import annotations

import json
import os
import stat
from pathlib import Path

import pytest

from conductor import crg_venv_sync as sync
from conductor.crg_mcp_probe import ProbeError
from tooling.hooks.dispatch.native_freshness import normalized

FAKE_INTERPRETER = """#!/bin/sh
case "$2" in
  *sysconfig*) echo "$FAKE_PURELIB" ;;
  *crg_server*)
    if [ -n "$FAKE_IMPORT_ERROR" ]; then
      echo "Traceback (most recent call last):" >&2
      echo "ImportError: $FAKE_IMPORT_ERROR" >&2
      exit 1
    fi
    ;;
  *) echo "unexpected: $2" >&2; exit 2 ;;
esac
"""

PYPROJECT = """
[project]
name = "{name}"
version = "{version}"

[tool.maturin]
module-name = "{module}"
"""


def crate_dir(root: Path, name: str, version: str) -> Path:
    """Declare one maturin crate under the tree's tooling/native, as the real ones are."""
    module = name.replace("-", "_")
    directory = root / "tooling" / "native" / name
    directory.mkdir(parents=True)
    (directory / "pyproject.toml").write_text(
        PYPROJECT.format(name=name, version=version, module=module), encoding="utf-8"
    )
    return directory


def interpreter(root: Path) -> Path:
    """A stand-in for the server's python that answers both probes the sync runs."""
    path = root / "fake-python"
    path.write_text(FAKE_INTERPRETER, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


def declare_server(root: Path, command: Path) -> None:
    (root / ".mcp.json").write_text(
        json.dumps(
            {
                "mcpServers": {
                    "code-review-graph": {
                        "command": str(command),
                        "args": ["-m", "conductor.crg_server"],
                        "cwd": str(root),
                    }
                }
            }
        ),
        encoding="utf-8",
    )


def installed(packages: Path, distribution: str, version: str) -> None:
    (packages / f"{normalized(distribution)}-{version}.dist-info").mkdir(parents=True)


def consumer_venv(
    root: Path,
    distribution: str,
    version: str,
    *,
    origin: dict[str, object] | None = None,
    extension: bool = True,
) -> Path:
    """Install one distribution into the checkout's *own* venv, as a consumer has it.

    ``origin`` is the PEP 610 record a direct reference leaves behind and an index
    install does not; ``extension`` is whether the wheel carried a compiled module.
    Together they are the filter that tells a crate of ours from numpy.
    """
    module = normalized(distribution)
    packages = root / ".venv" / "lib" / "python3.12" / "site-packages"
    info = packages / f"{module}-{version}.dist-info"
    info.mkdir(parents=True)
    payload = (
        f"{module}/{module}.cpython-312-x86_64-linux-gnu.so"
        if extension
        else f"{module}/__init__.py"
    )
    (info / "RECORD").write_text(f"{payload},,\n", encoding="utf-8")
    if origin is not None:
        (info / "direct_url.json").write_text(json.dumps(origin), encoding="utf-8")
    return info


GIT_ORIGIN: dict[str, object] = {
    "url": "https://github.com/example/forge",
    "vcs_info": {"vcs": "git", "commit_id": "c0ffee"},
    "subdirectory": "native/demo-native",
}


@pytest.fixture
def tree(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A checkout declaring one crate and a server on its own private interpreter."""
    packages = tmp_path / "server-venv" / "site-packages"
    packages.mkdir(parents=True)
    crate_dir(tmp_path, "conductor-native", "0.1.30")
    declare_server(tmp_path, interpreter(tmp_path))
    monkeypatch.setenv("FAKE_PURELIB", str(packages))
    monkeypatch.delenv("FAKE_IMPORT_ERROR", raising=False)
    return tmp_path


def packages_of(tree: Path) -> Path:
    return tree / "server-venv" / "site-packages"


def test_matching_version_and_a_clean_import_passes(tree: Path) -> None:
    installed(packages_of(tree), "conductor-native", "0.1.30")
    verdict, detail = sync.run(tree, check_only=True)
    assert verdict == "PASS"
    assert detail


def test_version_mismatch_is_drift(tree: Path) -> None:
    installed(packages_of(tree), "conductor-native", "0.1.25")
    verdict, detail = sync.run(tree, check_only=True)
    assert verdict == "FAIL"
    assert any("0.1.25 installed, 0.1.30 declared" in line for line in detail)


def test_absent_crate_is_not_drift_while_the_server_imports(tree: Path) -> None:
    verdict, detail = sync.run(tree, check_only=True)
    assert verdict == "PASS"
    assert any("absent from the server's interpreter" in line for line in detail)


def test_a_failing_import_is_drift_at_a_matching_version(
    tree: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    installed(packages_of(tree), "conductor-native", "0.1.30")
    monkeypatch.setenv("FAKE_IMPORT_ERROR", "cannot import name 'new_symbol_native'")
    verdict, detail = sync.run(tree, check_only=True)
    assert verdict == "FAIL"
    assert any("new_symbol_native" in line for line in detail)


def test_check_only_never_installs(tree: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    installed(packages_of(tree), "conductor-native", "0.1.25")
    monkeypatch.setattr(
        sync, "install", lambda *_: pytest.fail("--check must not install")
    )
    assert sync.run(tree, check_only=True)[0] == "FAIL"


def test_sync_repairs_the_mismatch_and_reports_what_it_installed(
    tree: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    packages = packages_of(tree)
    installed(packages, "conductor-native", "0.1.25")
    monkeypatch.setenv("FAKE_IMPORT_ERROR", "cannot import name 'new_symbol_native'")

    def fake_install(_: Path, one: sync.Requirement) -> None:
        monkeypatch.delenv("FAKE_IMPORT_ERROR", raising=False)
        installed(packages, one.distribution, one.version)

    monkeypatch.setattr(sync, "install", fake_install)
    verdict, detail = sync.run(tree, check_only=False)
    assert verdict == "SYNCED"
    assert any("reinstalled conductor-native" in line for line in detail)


def test_absent_crates_are_installed_only_after_the_mismatches_fail(
    tree: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    crate_dir(tree, "slop-core", "0.1.6")
    installed(packages_of(tree), "conductor-native", "0.1.25")
    monkeypatch.setenv("FAKE_IMPORT_ERROR", "cannot import name 'new_symbol_native'")
    order: list[str] = []

    def fake_install(_: Path, one: sync.Requirement) -> None:
        order.append(one.distribution)
        if len(order) == 2:  # only the second stage clears the import
            monkeypatch.delenv("FAKE_IMPORT_ERROR", raising=False)

    monkeypatch.setattr(sync, "install", fake_install)
    verdict, _ = sync.run(tree, check_only=False)
    assert verdict == "SYNCED"
    assert order == ["conductor-native", "slop-core"]


def test_an_import_that_stays_broken_fails_loud(
    tree: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    installed(packages_of(tree), "conductor-native", "0.1.25")
    monkeypatch.setenv("FAKE_IMPORT_ERROR", "cannot import name 'new_symbol_native'")
    monkeypatch.setattr(sync, "install", lambda *_: None)
    verdict, detail = sync.run(tree, check_only=False)
    assert verdict == "FAIL"
    assert any("still broken" in line for line in detail)


def test_a_missing_interpreter_is_a_skip(tmp_path: Path) -> None:
    crate_dir(tmp_path, "conductor-native", "0.1.30")
    declare_server(tmp_path, tmp_path / "nowhere" / "python")
    verdict, detail = sync.run(tmp_path, check_only=True)
    assert verdict == "SKIP"
    assert any("does not exist" in line for line in detail)


def test_the_checkouts_own_venv_is_a_skip(
    tree: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    own = tree / ".venv" / "lib" / "python3.12" / "site-packages"
    own.mkdir(parents=True)
    monkeypatch.setenv("FAKE_PURELIB", str(own))
    verdict, detail = sync.run(tree, check_only=True)
    assert verdict == "SKIP"
    assert any("uv sync" in line for line in detail)


def test_a_tree_with_neither_sources_nor_an_install_is_a_skip(tmp_path: Path) -> None:
    declare_server(tmp_path, interpreter(tmp_path))
    verdict, detail = sync.run(tmp_path, check_only=True)
    assert verdict == "SKIP"
    assert any("no extension crates" in line for line in detail)
    # Both places it looked, named -- the message said only "this tree declares
    # none" until 2026-09-16 and had only ever looked in the first of them.
    assert any("tooling/native" in line and ".venv" in line for line in detail)


@pytest.fixture
def consumer(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A checkout with no crate sources that installs them: the monorepo's shape."""
    packages = tmp_path / "server-venv" / "site-packages"
    packages.mkdir(parents=True)
    consumer_venv(tmp_path, "demo-native", "0.1.30", origin=GIT_ORIGIN)
    declare_server(tmp_path, interpreter(tmp_path))
    monkeypatch.setattr(sync, "declared_names", lambda: ("demo-native",))
    monkeypatch.setenv("FAKE_PURELIB", str(packages))
    monkeypatch.delenv("FAKE_IMPORT_ERROR", raising=False)
    return tmp_path


def test_a_consumer_checkout_is_compared_against_what_it_installed(
    consumer: Path,
) -> None:
    """The defect this guard spent 2026-09-14..09-16 unable to see.

    The crates left the monorepo's tree in the extraction, so ``crates()`` found
    none, ``skipped()`` said "this tree declares no extension crates", and the one
    host with a separate server interpreter -- the only host the check exists for
    -- returned SKIP and exit 0 on every run. Nothing pinned that it ever compared.
    """
    installed(packages_of(consumer), "demo-native", "0.1.25")
    verdict, detail = sync.run(consumer, check_only=True)
    assert verdict == "FAIL"
    assert any("0.1.25 installed, 0.1.30 declared" in line for line in detail)


def test_a_consumer_repair_installs_the_reference_not_a_directory(
    consumer: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """There is no crate directory to point at, and pointing at one installs a lie."""
    installed(packages_of(consumer), "demo-native", "0.1.25")
    monkeypatch.setenv("FAKE_IMPORT_ERROR", "cannot import name 'new_symbol_native'")
    sources: list[str] = []

    def fake_install(_: Path, one: sync.Requirement) -> None:
        sources.append(one.source)
        monkeypatch.delenv("FAKE_IMPORT_ERROR", raising=False)

    monkeypatch.setattr(sync, "install", fake_install)
    assert sync.run(consumer, check_only=False)[0] == "SYNCED"
    assert sources == [
        "demo-native @ git+https://github.com/example/forge@c0ffee"
        "#subdirectory=native/demo-native"
    ]


def test_an_index_installed_requirement_is_not_a_crate(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """numpy, coverage and PyYAML all ship a ``.so`` and none of them is ours."""
    consumer_venv(tmp_path, "demo-native", "0.1.30", origin=None)
    monkeypatch.setattr(sync, "declared_names", lambda: ("demo-native",))
    assert sync.required(tmp_path) == ()


def test_a_pure_python_requirement_is_not_a_crate(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Installed from a direct reference, but there is no extension to be stale."""
    consumer_venv(tmp_path, "demo-native", "0.1.30", origin=GIT_ORIGIN, extension=False)
    monkeypatch.setattr(sync, "declared_names", lambda: ("demo-native",))
    assert sync.required(tmp_path) == ()


def test_crate_sources_in_the_tree_win_over_what_is_installed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A host that builds its crates is measured against the build, not the install.

    The installed copy is whatever was last synced; the sources are what the next
    `make` produces. Reading the install on a tree that has sources would hide
    exactly the version bump the freshness check exists to catch.
    """
    directory = crate_dir(tmp_path, "demo-native", "0.2.0")
    consumer_venv(tmp_path, "demo-native", "0.1.30", origin=GIT_ORIGIN)
    monkeypatch.setattr(sync, "declared_names", lambda: ("demo-native",))
    assert sync.required(tmp_path) == (
        sync.Requirement("demo-native", "0.2.0", str(directory)),
    )


def test_the_requirement_names_come_from_this_packages_own_metadata() -> None:
    """Not a literal here: a crate added upstream must not need this file edited."""
    names = sync.declared_names()
    assert "conductor-native" in names
    # Extras are another install's problem -- the graph server's own pin among them.
    assert not any(name.startswith("code-review-graph") for name in names)


def test_an_undeclared_server_fails_loud(tmp_path: Path) -> None:
    with pytest.raises(ProbeError):
        sync.run(tmp_path, check_only=True)


def test_an_interpreter_that_cannot_report_site_packages_fails_loud(
    tree: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    broken = tree / "broken-python"
    broken.write_text("#!/bin/sh\nexit 3\n", encoding="utf-8")
    broken.chmod(broken.stat().st_mode | stat.S_IXUSR)
    with pytest.raises(ProbeError):
        sync.purelib(broken)


def test_main_prints_the_verdict_and_exits_nonzero_on_drift(
    tree: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    installed(packages_of(tree), "conductor-native", "0.1.25")
    assert sync.main(["--repo", str(tree), "--check"]) == 1
    assert "crg-venv-sync | FAIL" in capsys.readouterr().out


def test_main_reports_a_probe_error_as_a_failure(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    assert sync.main(["--repo", str(tmp_path), "--check"]) == 1
    assert "crg-venv-sync | FAIL |" in capsys.readouterr().out


def test_uv_env_drops_the_inherited_virtualenv(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("VIRTUAL_ENV", "/somewhere/else")
    monkeypatch.setenv("PATH", os.environ["PATH"])
    assert "VIRTUAL_ENV" not in sync.uv_env()
    assert sync.uv_env()["PATH"] == os.environ["PATH"]


def test_a_checkout_in_step_is_silent_at_session_start(tree: Path) -> None:
    installed(packages_of(tree), "conductor-native", "0.1.30")
    assert sync.session_report(tree) == ""


def test_a_version_skew_names_the_crate_and_the_remedy(tree: Path) -> None:
    installed(packages_of(tree), "conductor-native", "0.1.25")
    report = sync.session_report(tree)
    assert report.startswith("GRAPH SERVER natives out of date in ")
    # The remedy rides on the crate's own line: which version is which is
    # test_version_mismatch_is_drift's contract, not this one's.
    assert any(
        line.startswith("- conductor-native:") and line.endswith("; `make crg-sync`")
        for line in report.splitlines()
    )


def test_a_skew_the_server_still_survives_says_so(tree: Path) -> None:
    installed(packages_of(tree), "conductor-native", "0.1.25")
    assert any("still imports" in line for line in sync.session_findings(tree))


def test_a_skew_that_killed_the_server_names_connection_closed(
    tree: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    installed(packages_of(tree), "conductor-native", "0.1.25")
    monkeypatch.setenv("FAKE_IMPORT_ERROR", "cannot import name 'new_symbol_native'")
    lines = sync.session_findings(tree)
    assert any("CONNECTION_CLOSED" in line for line in lines)
    assert any("new_symbol_native" in line for line in lines)


def test_an_absent_crate_never_buys_the_import_probe(
    tree: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The expensive probe is what absent-is-not-drift exists to avoid paying."""
    crate_dir(tree, "slop-core", "0.1.6")
    installed(packages_of(tree), "conductor-native", "0.1.30")
    monkeypatch.setattr(
        sync,
        "imports_server",
        lambda *_, **__: pytest.fail("probed for an absent crate"),
    )
    assert sync.session_findings(tree) == ()


def test_an_undeclared_server_is_silent_at_session_start(tmp_path: Path) -> None:
    """A foreign checkout starts sessions too, and has nothing to compare."""
    assert sync.session_findings(tmp_path) == ()


def test_the_session_probes_never_wait_the_install_timeout(
    tree: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    installed(packages_of(tree), "conductor-native", "0.1.25")
    waits: list[float] = []
    spawn = sync.subprocess.run

    def recording(*args: object, **kwargs: object):
        waits.append(kwargs["timeout"])
        return spawn(*args, **kwargs)

    monkeypatch.setattr(sync.subprocess, "run", recording)
    sync.session_findings(tree)
    assert sync.SESSION_TIMEOUT_SECONDS < sync.TIMEOUT_SECONDS
    assert waits == [sync.SESSION_TIMEOUT_SECONDS, sync.SESSION_TIMEOUT_SECONDS]
