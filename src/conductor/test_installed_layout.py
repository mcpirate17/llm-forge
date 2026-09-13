"""Prove host-root resolution works from an installed (non-checkout) layout.

``conductor`` installed via ``uv add "conductor-tooling @ git+..."`` lands in the
host venv's site-packages, nowhere near the host repo. A module that ever computed
its "repo root" from ``Path(__file__)`` would silently resolve into site-packages
instead. This builds a real installed copy into a scratch site-packages directory,
runs the CLIs as subprocesses against a fake host repo with the source tree
deliberately absent from ``sys.path``, and checks the site directory never leaks
into any answer.
"""

from __future__ import annotations

import subprocess
import sys
import time
from pathlib import Path

import pytest

_INSTALL_TIMEOUT = 120


@pytest.fixture(scope="module")
def installed_site(tmp_path_factory: pytest.TempPathFactory) -> Path:
    """A scratch site-packages containing only the installed wheel contents."""
    site = tmp_path_factory.mktemp("site")
    repo_root = Path(__file__).resolve().parents[2]
    subprocess.run(  # noqa: S603
        [
            "uv",
            "pip",
            "install",
            "--no-deps",
            "--target",
            str(site),
            str(repo_root),
        ],
        check=True,
        capture_output=True,
        text=True,
        timeout=_INSTALL_TIMEOUT,
    )
    return site


@pytest.fixture
def fake_host(tmp_path: Path) -> Path:
    """A minimal host repo: git, Makefile, session policy, notes, registry, .claude."""
    host = tmp_path / "host"
    host.mkdir()
    subprocess.run(  # noqa: S603
        ["git", "init", "--quiet", str(host)], check=True, capture_output=True
    )
    (host / "Makefile").write_text("gate:\n\t@echo gate\n", encoding="utf-8")
    (host / "pyproject.toml").write_text(
        "[tool.conductor.session]\n"
        'preamble = ["INSTALLED-LAYOUT-FAKE-HOST-MISSION: ship the resolver."]\n'
        'standing_mandates = ["MAND-1: never resolve against site-packages"]\n',
        encoding="utf-8",
    )
    (host / "research" / "notes").mkdir(parents=True)
    campaigns_dir = host / "tasks" / "mutation_campaigns"
    campaigns_dir.mkdir(parents=True)
    (campaigns_dir / "registry.json").write_text(
        '{"campaigns": [{"name": "fake-campaign", "status": "active"}]}\n',
        encoding="utf-8",
    )
    (host / ".claude").mkdir()
    return host


def _run(
    module_args: list[str], *, cwd: Path, site: Path
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(  # noqa: S603
        [sys.executable, "-m", *module_args],
        cwd=cwd,
        env={"PATH": "/usr/bin:/bin", "PYTHONPATH": str(site)},
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )


def _assert_no_site_leak(
    result: subprocess.CompletedProcess[str], site: Path, label: str
) -> None:
    site_str = str(site)
    for stream_name, stream in (("stdout", result.stdout), ("stderr", result.stderr)):
        for line in stream.splitlines():
            assert site_str not in line, (
                f"{label} {stream_name} leaks the install site dir: {line!r}"
            )
    assert result.returncode == 0, (
        f"{label} exited {result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )


def test_installed_layout_resolves_host_not_site(
    installed_site: Path, fake_host: Path
) -> None:
    started = time.monotonic()

    preamble = _run(
        ["conductor.session_preamble", "text"], cwd=fake_host, site=installed_site
    )
    _assert_no_site_leak(preamble, installed_site, "session_preamble")
    assert "INSTALLED-LAYOUT-FAKE-HOST-MISSION" in preamble.stdout

    dump = _run(["conductor.active_state", "dump"], cwd=fake_host, site=installed_site)
    _assert_no_site_leak(dump, installed_site, "active_state dump")

    gate_help = _run(["conductor.gate", "--help"], cwd=fake_host, site=installed_site)
    _assert_no_site_leak(gate_help, installed_site, "gate --help")

    elapsed = time.monotonic() - started
    assert elapsed < 20, (
        f"installed-layout subprocess run took {elapsed:.1f}s; "
        "tighten it or raise this bound deliberately"
    )
