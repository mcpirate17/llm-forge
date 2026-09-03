"""Rust toolchain recovery for the cargo-audit analyzer."""

from __future__ import annotations

from pathlib import Path

import pytest

from conductor.candidate_review import cargo_audit_files


def _rustup(root: Path, name: str = ".rustup") -> Path:
    """A rustup home that can actually resolve cargo."""
    home = root / name
    (home / "toolchains" / "stable-x86_64-unknown-linux-gnu").mkdir(parents=True)
    (home / "settings.toml").write_text('default_toolchain = "stable"\n')
    return home


def _rustup_shell(root: Path, name: str = ".rustup") -> Path:
    """What rustup itself leaves behind after one run under an empty HOME.

    The directory and settings.toml both exist; no toolchain is named or installed,
    so cargo still fails with "no default is configured".
    """
    home = root / name
    (home / "toolchains").mkdir(parents=True)
    (home / "settings.toml").write_text('version = "12"\n\n[overrides]\n')
    return home


def _cargo(root: Path, name: str = ".cargo") -> Path:
    home = root / name
    (home / "bin").mkdir(parents=True)
    return home


def test_explicit_settings_are_never_overridden(tmp_path, monkeypatch):
    """A caller's pin wins even when a different valid home is discoverable."""
    sandbox = tmp_path / "sandbox"
    sandbox.mkdir()
    _rustup(sandbox)
    _cargo(sandbox)
    monkeypatch.setattr(cargo_audit_files, "_login_home", lambda: None)

    env = cargo_audit_files.rust_toolchain_env(
        {
            "HOME": str(sandbox),
            "RUSTUP_HOME": "/pinned/rustup",
            "CARGO_HOME": "/pinned/cargo",
        }
    )

    assert env["RUSTUP_HOME"] == "/pinned/rustup"
    assert env["CARGO_HOME"] == "/pinned/cargo"


def test_login_home_recovers_what_the_sandbox_home_hides(tmp_path, monkeypatch):
    """The sandbox HOME is empty, so the passwd entry is the only usable source."""
    sandbox = tmp_path / "runtime-home"
    sandbox.mkdir()
    login = tmp_path / "login"
    login.mkdir()
    _rustup(login)
    _cargo(login)
    monkeypatch.setattr(cargo_audit_files, "_login_home", lambda: login)

    env = cargo_audit_files.rust_toolchain_env({"HOME": str(sandbox)})

    assert env["RUSTUP_HOME"] == str(login / ".rustup")
    assert env["CARGO_HOME"] == str(login / ".cargo")


def test_sandbox_home_is_preferred_over_the_login_home(tmp_path, monkeypatch):
    """Order matters: a usable $HOME must not be skipped for the passwd entry."""
    sandbox = tmp_path / "sandbox"
    sandbox.mkdir()
    login = tmp_path / "login"
    login.mkdir()
    _rustup(sandbox)
    _rustup(login)
    monkeypatch.setattr(cargo_audit_files, "_login_home", lambda: login)

    env = cargo_audit_files.rust_toolchain_env({"HOME": str(sandbox)})

    assert env["RUSTUP_HOME"] == str(sandbox / ".rustup")
    assert env["RUSTUP_HOME"] != str(login / ".rustup")


def test_a_directory_without_its_marker_is_not_a_home(tmp_path, monkeypatch):
    """An empty `.rustup` is what the sandbox produces; it must not be accepted."""
    sandbox = tmp_path / "sandbox"
    (sandbox / ".rustup").mkdir(parents=True)
    (sandbox / ".cargo").mkdir(parents=True)
    login = tmp_path / "login"
    login.mkdir()
    _rustup(login)
    _cargo(login)
    monkeypatch.setattr(cargo_audit_files, "_login_home", lambda: login)

    env = cargo_audit_files.rust_toolchain_env({"HOME": str(sandbox)})

    assert env["RUSTUP_HOME"] == str(login / ".rustup")
    assert env["CARGO_HOME"] == str(login / ".cargo")


def test_the_shell_home_rustup_leaves_behind_is_rejected(tmp_path, monkeypatch):
    """Regression: one failed cargo run seeds `$HOME/.rustup` with a settings.toml.

    Accepting it on the next run reproduces the original failure while looking
    resolved, so the sandbox's own leftovers must lose to the real home.
    """
    sandbox = tmp_path / "sandbox"
    sandbox.mkdir()
    _rustup_shell(sandbox)
    login = tmp_path / "login"
    login.mkdir()
    _rustup(login)
    monkeypatch.setattr(cargo_audit_files, "_login_home", lambda: login)

    env = cargo_audit_files.rust_toolchain_env({"HOME": str(sandbox)})

    assert env["RUSTUP_HOME"] == str(login / ".rustup")


def test_a_named_toolchain_that_is_not_installed_is_rejected(tmp_path, monkeypatch):
    """settings.toml can name a default whose toolchain dir was never created."""
    sandbox = tmp_path / "sandbox"
    (sandbox / ".rustup" / "toolchains").mkdir(parents=True)
    (sandbox / ".rustup" / "settings.toml").write_text('default_toolchain = "stable"\n')
    login = tmp_path / "login"
    login.mkdir()
    _rustup(login)
    monkeypatch.setattr(cargo_audit_files, "_login_home", lambda: login)

    env = cargo_audit_files.rust_toolchain_env({"HOME": str(sandbox)})

    assert env["RUSTUP_HOME"] == str(login / ".rustup")


def test_each_home_is_validated_by_its_own_marker(tmp_path, monkeypatch):
    """`settings.toml` proves a rustup home, `bin` proves a cargo home."""
    login = tmp_path / "login"
    login.mkdir()
    _rustup(login)
    # A `.cargo` carrying rustup's marker instead of its own is still not a cargo home.
    (login / ".cargo").mkdir()
    (login / ".cargo" / "settings.toml").write_text("")
    monkeypatch.setattr(cargo_audit_files, "_login_home", lambda: login)

    env = cargo_audit_files.rust_toolchain_env({"HOME": str(tmp_path / "absent")})

    assert env["RUSTUP_HOME"] == str(login / ".rustup")
    assert "CARGO_HOME" not in env


def test_a_host_without_rustup_is_left_alone(tmp_path, monkeypatch):
    """Distro-packaged cargo needs no RUSTUP_HOME; inventing one would break it."""
    monkeypatch.setattr(cargo_audit_files, "_login_home", lambda: None)

    env = cargo_audit_files.rust_toolchain_env({"HOME": str(tmp_path / "absent")})

    assert "RUSTUP_HOME" not in env
    assert "CARGO_HOME" not in env


def test_a_missing_home_variable_still_consults_the_login_home(tmp_path, monkeypatch):
    login = tmp_path / "login"
    login.mkdir()
    _rustup(login)
    monkeypatch.setattr(cargo_audit_files, "_login_home", lambda: login)

    env = cargo_audit_files.rust_toolchain_env({})

    assert env["RUSTUP_HOME"] == str(login / ".rustup")


@pytest.mark.parametrize("empty", ["", None])
def test_a_blank_setting_is_not_a_setting(tmp_path, monkeypatch, empty):
    """An exported-but-empty RUSTUP_HOME is worse than none; resolve past it."""
    login = tmp_path / "login"
    login.mkdir()
    _rustup(login)
    monkeypatch.setattr(cargo_audit_files, "_login_home", lambda: login)
    environ = {"HOME": str(tmp_path / "absent")}
    if empty is not None:
        environ["RUSTUP_HOME"] = empty

    env = cargo_audit_files.rust_toolchain_env(environ)

    assert env["RUSTUP_HOME"] == str(login / ".rustup")


def test_login_home_survives_a_missing_passwd_entry(monkeypatch):
    """A uid with no passwd row must degrade, not raise, inside the sandbox."""

    def _raise(_uid):
        raise KeyError("no passwd entry")

    monkeypatch.setattr(cargo_audit_files.pwd, "getpwuid", _raise)

    assert cargo_audit_files._login_home() is None


def test_the_owning_lockfile_is_the_nearest_one_above_the_file(tmp_path):
    root = tmp_path
    crate = root / "native" / "crate"
    (crate / "src").mkdir(parents=True)
    (root / "Cargo.lock").write_text("")
    (crate / "Cargo.lock").write_text("")

    found = cargo_audit_files._owning_lockfile(crate / "src" / "lib.rs", root=root)

    assert found == crate / "Cargo.lock"


def test_a_path_outside_the_root_owns_no_lockfile(tmp_path):
    root = tmp_path / "repo"
    root.mkdir()
    (root / "Cargo.lock").write_text("")
    outside = tmp_path / "elsewhere"
    outside.mkdir()

    assert cargo_audit_files._owning_lockfile(outside / "lib.rs", root=root) is None


def test_the_version_probe_runs_under_the_recovered_toolchain(tmp_path, monkeypatch):
    """The gate probes `cargo audit --version` inside a sandbox whose HOME hides rustup.

    Routing the probe through the wrapper resolves the toolchain the same way the
    check command does, so the analyzer cannot be "unavailable" for the probe yet
    available for the check.
    """
    rustup = _rustup(tmp_path)
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.delenv("RUSTUP_HOME", raising=False)
    monkeypatch.delenv("CARGO_HOME", raising=False)
    seen: list[tuple[list[str], str | None]] = []

    def fake_run(argv, check, env):
        seen.append((argv, env.get("RUSTUP_HOME")))
        return type("Done", (), {"returncode": 0})()

    monkeypatch.setattr(cargo_audit_files.subprocess, "run", fake_run)
    assert cargo_audit_files.main(["--version"]) == 0
    assert seen == [(["cargo", "audit", "--version"], str(rustup))]


def test_a_failed_version_probe_names_the_missing_rustup_home(
    tmp_path, monkeypatch, capsys
):
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.delenv("RUSTUP_HOME", raising=False)
    monkeypatch.delenv("CARGO_HOME", raising=False)
    monkeypatch.setattr(cargo_audit_files, "_login_home", lambda: None)
    monkeypatch.setattr(
        cargo_audit_files.subprocess,
        "run",
        lambda argv, check, env: type("Done", (), {"returncode": 101})(),
    )
    assert cargo_audit_files.main(["--version"]) == 101
    assert "export RUSTUP_HOME" in capsys.readouterr().err
