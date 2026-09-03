"""Run cargo-audit against lockfiles owning changed Rust candidate files."""

from __future__ import annotations

import os
import pwd
import subprocess
import sys
from collections.abc import Callable
from pathlib import Path


def _owning_lockfile(path: Path, *, root: Path) -> Path | None:
    candidate = path if path.name == "Cargo.lock" else path.parent
    while candidate.is_relative_to(root):
        lockfile = (
            candidate if candidate.name == "Cargo.lock" else candidate / "Cargo.lock"
        )
        if lockfile.is_file():
            return lockfile
        if candidate == root:
            break
        candidate = candidate.parent
    return None


def _login_home() -> Path | None:
    """The invoking account's home from the passwd database, not from $HOME.

    The review sandbox replaces HOME with a throwaway runtime dir, so $HOME is
    exactly the value that cannot be trusted here.
    """
    try:
        return Path(pwd.getpwuid(os.getuid()).pw_dir)
    except (KeyError, OSError):
        return None


def _is_usable_rustup_home(home: Path) -> bool:
    """True only for a rustup home that can actually resolve `cargo`.

    Merely existing is not enough, and this is the trap: invoking cargo once under
    the sandbox HOME makes rustup *create* `$HOME/.rustup` with a settings.toml
    that names no toolchain. A later run then finds that shell of a home, treats it
    as real, and fails exactly as before. A home is usable only if it names a
    default toolchain and has one installed.
    """
    settings = home / "settings.toml"
    try:
        if "default_toolchain" not in settings.read_text(encoding="utf-8"):
            return False
        return any((home / "toolchains").iterdir())
    except (OSError, UnicodeDecodeError):
        return False


def _is_usable_cargo_home(home: Path) -> bool:
    return (home / "bin").is_dir()


def _first_usable(
    candidates: list[Path | None], predicate: Callable[[Path], bool]
) -> Path | None:
    for candidate in candidates:
        if candidate is not None and predicate(candidate):
            return candidate
    return None


def rust_toolchain_env(environ: dict[str, str] | None = None) -> dict[str, str]:
    """Environment for cargo, with RUSTUP_HOME/CARGO_HOME recovered if unset.

    `cargo` on the PATH is usually a rustup shim that reads its default toolchain
    from RUSTUP_HOME, defaulting to `$HOME/.rustup`. Under the review sandbox HOME
    points at an empty runtime dir, so the shim finds no toolchain and dies with
    "could not choose a version of cargo to run" -- which surfaces as a CRITICAL
    unavailable-analyzer finding on every candidate that touches Rust, on a host
    where cargo works fine outside the gate.

    Recovering CARGO_HOME matters too: left at the sandbox HOME, cargo-audit
    re-clones the RustSec advisory database on every single gate run.

    An explicit setting always wins, and a host with no rustup home (cargo
    installed by a distro package, say) is left exactly as it was.
    """
    env = dict(os.environ if environ is None else environ)
    login_home = _login_home()
    for key, directory, is_usable in (
        ("RUSTUP_HOME", ".rustup", _is_usable_rustup_home),
        ("CARGO_HOME", ".cargo", _is_usable_cargo_home),
    ):
        if env.get(key):
            continue
        home = env.get("HOME")
        resolved = _first_usable(
            [
                Path(home) / directory if home else None,
                login_home / directory if login_home else None,
            ],
            is_usable,
        )
        if resolved is not None:
            env[key] = str(resolved)
    return env


def _report_missing_rustup_home(env: dict[str, str]) -> None:
    if not env.get("RUSTUP_HOME"):
        print(
            "cargo-audit: no rustup home was found under $HOME or the login "
            "account's home; if cargo is a rustup shim, export RUSTUP_HOME "
            "before running the gate",
            file=sys.stderr,
        )


def version(env: dict[str, str] | None = None) -> int:
    """Probe `cargo audit --version` under the same toolchain the check itself uses.

    The gate's version probe runs inside the review sandbox, where HOME is replaced;
    resolving the toolchain here is what makes the probe agree with the check.
    """
    env = rust_toolchain_env() if env is None else env
    completed = subprocess.run(["cargo", "audit", "--version"], check=False, env=env)
    if completed.returncode:
        _report_missing_rustup_home(env)
    return completed.returncode


def main(argv: list[str] | None = None) -> int:
    root = Path.cwd().resolve()
    values = sys.argv[1:] if argv is None else argv
    if values == ["--version"]:
        return version()
    lockfiles = {
        lockfile
        for value in values
        if (lockfile := _owning_lockfile(Path(value).resolve(), root=root)) is not None
    }
    if not lockfiles:
        print(
            "cargo-audit: no owning Cargo.lock found for changed Rust files",
            file=sys.stderr,
        )
        return 1

    env = rust_toolchain_env()
    for lockfile in sorted(lockfiles):
        completed = subprocess.run(
            ["cargo", "audit", "--file", str(lockfile)],
            check=False,
            env=env,
        )
        if completed.returncode:
            _report_missing_rustup_home(env)
            return completed.returncode
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
