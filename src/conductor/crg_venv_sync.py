"""Is the interpreter that runs the code-review-graph MCP server carrying this tree's natives?

    python -m conductor.crg_venv_sync [--repo ROOT] [--check]

The MCP server runs on the interpreter named in ``.mcp.json`` -- normally a pipx venv
of its own -- but imports ``conductor/`` straight out of this checkout. So the two
halves drift independently: ``uv sync`` rebuilds the native crates into ``.venv`` and
leaves that interpreter on whatever wheel it was last installed with. The first symbol
``conductor/_native.py`` gains after the skew opens turns every new session's server
into ``code-review-graph (CONNECTION_CLOSED)``, because the import dies before the
stdio handshake and the client only ever sees a dead pipe.

:mod:`tooling.hooks.dispatch.native_freshness` asks this of the checkout's own
``.venv``; this module asks it of the server's interpreter, and can repair it. Two
questions per crate -- the version installed over there against the one this tree
declares -- and then the one that decides it: can that interpreter import
``conductor.crg_server`` at all.

A crate that is merely *absent* over there is not drift while the server still
imports: ``slop_core`` is optional by construction in ``conductor/_native.py`` and the
server never reaches it, so putting it in a venv that exists to serve graph tools would
be cargo, not repair. Repair is therefore staged -- the version mismatches first, the
absent crates only if the import is still broken afterwards.

``--check`` reports and exits 1 on drift. Without it the drift is repaired, with
``--no-deps`` so the server's other pins (fastmcp, a2a-sdk) are left alone, and the
import is re-run to prove it. An interpreter that does not exist -- CI, or any host
that never installed the server -- is a skip, not a failure.

:func:`session_findings` is the same question asked at SessionStart, where nobody is
waiting on an answer and the session that needs it is the one already looking at a
dead server. It buys the disk half unconditionally and the import probe only once a
mismatch is in hand.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

from conductor.crg_mcp_probe import ProbeError, load_server_cmd
from tooling.hooks.dispatch.native_freshness import (
    Crate,
    crates,
    dist_info,
    installed_version,
    site_packages,
)

ABSENT = "absent"
IMPORT_PROBE = "import conductor.crg_server"
PURELIB_PROBE = "import sysconfig; print(sysconfig.get_paths()['purelib'])"
TIMEOUT_SECONDS = 900
# A SessionStart hook may not wait fifteen minutes on a wedged interpreter. Both
# probes it runs are sub-second measured (startup, and 0.26 s for the import), so
# this is a bound on pathology, not a budget the healthy path spends.
SESSION_TIMEOUT_SECONDS = 20


@dataclass(frozen=True)
class Skew:
    """One crate the server's interpreter does not carry at this tree's version."""

    crate: Crate
    installed: str

    @property
    def absent(self) -> bool:
        return self.installed == ABSENT

    def line(self) -> str:
        return (
            f"{self.crate.distribution}: {self.installed} installed, "
            f"{self.crate.version} declared"
        )


def uv_env() -> dict[str, str]:
    """uv warns on every call when it inherits a VIRTUAL_ENV it is not targeting."""
    return {key: value for key, value in os.environ.items() if key != "VIRTUAL_ENV"}


def server_interpreter(root: Path) -> tuple[Path, Path, dict[str, str]]:
    """The interpreter the declared server command runs on, with its cwd and env."""
    argv, cwd, env = load_server_cmd(root)
    return Path(argv[0]), cwd, env


def purelib(interpreter: Path, timeout: float = TIMEOUT_SECONDS) -> Path:
    """Ask the interpreter for its own site-packages rather than guessing the layout."""
    done = subprocess.run(
        [str(interpreter), "-c", PURELIB_PROBE],
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    if done.returncode != 0:
        raise ProbeError(
            f"{interpreter} could not report its site-packages: {done.stderr.strip()}"
        )
    return Path(done.stdout.strip())


def skews(packages: Path, declared: tuple[Crate, ...]) -> tuple[Skew, ...]:
    """Every crate whose version over there is not the version this tree declares."""
    found = []
    for crate in declared:
        info = dist_info(packages, crate.distribution)
        installed = installed_version(info) if info is not None else ABSENT
        if installed != crate.version:
            found.append(Skew(crate, installed))
    return tuple(found)


def imports_server(
    interpreter: Path,
    cwd: Path,
    env: dict[str, str],
    timeout: float = TIMEOUT_SECONDS,
) -> str | None:
    """None when the server module imports; otherwise the last line of the failure."""
    done = subprocess.run(
        [str(interpreter), "-c", IMPORT_PROBE],
        cwd=str(cwd),
        env={**os.environ, **env},
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    if done.returncode == 0:
        return None
    lines = [line for line in done.stderr.splitlines() if line.strip()]
    return lines[-1] if lines else f"exit {done.returncode}"


def install(interpreter: Path, crate: Crate) -> None:
    """Rebuild one crate into the server's interpreter without touching its other pins."""
    done = subprocess.run(
        [
            "uv",
            "pip",
            "install",
            "--python",
            str(interpreter),
            "--no-deps",
            "--reinstall",
            str(crate.directory),
        ],
        env=uv_env(),
        capture_output=True,
        text=True,
        timeout=TIMEOUT_SECONDS,
    )
    if done.returncode != 0:
        raise ProbeError(
            f"installing {crate.distribution} into {interpreter} failed:\n{done.stderr}"
        )


def repair(
    interpreter: Path, cwd: Path, env: dict[str, str], skew: tuple[Skew, ...]
) -> tuple[list[Crate], str | None]:
    """Install the mismatches, then the absent crates only if the import is still broken."""
    installed: list[Crate] = []
    failure: str | None = None
    stages = (
        tuple(one for one in skew if not one.absent),
        tuple(one for one in skew if one.absent),
    )
    for stage in stages:
        for one in stage:
            install(interpreter, one.crate)
            installed.append(one.crate)
        failure = imports_server(interpreter, cwd, env)
        if failure is None:
            return installed, None
    return installed, failure


def skipped(root: Path, interpreter: Path, packages: Path | None) -> str | None:
    """Why there is nothing to compare, when there is nothing to compare."""
    if not interpreter.exists():
        return f"{interpreter} does not exist; the server is not installed here"
    if not crates(root):
        return "this tree declares no extension crates"
    own = site_packages(root)
    # Both interpreters are symlinks onto the same base python, so only the
    # site-packages they resolve to separates a private venv from this one.
    if packages is not None and own is not None and packages.resolve() == own.resolve():
        return "the server runs on this checkout's own .venv; uv sync already covers it"
    return None


def run(root: Path, check_only: bool) -> tuple[str, list[str]]:
    """The verdict -- SKIP, PASS, FAIL or SYNCED -- and the lines that justify it."""
    interpreter, cwd, env = server_interpreter(root)
    reason = skipped(root, interpreter, None)
    if reason is None:
        packages = purelib(interpreter)
        reason = skipped(root, interpreter, packages)
    if reason is not None:
        return "SKIP", [reason]

    skew = skews(packages, crates(root))
    failure = imports_server(interpreter, cwd, env)
    mismatched = tuple(one for one in skew if not one.absent)
    detail = [one.line() for one in mismatched]
    detail += [
        f"{one.crate.distribution}: absent from the server's interpreter"
        for one in skew
        if one.absent
    ]
    if failure is not None:
        detail.append(f"cannot import conductor.crg_server: {failure}")
    if not mismatched and failure is None:
        return "PASS", [f"{interpreter} carries every crate the server needs", *detail]
    if check_only:
        return "FAIL", detail

    installed, remaining = repair(interpreter, cwd, env, skew)
    names = ", ".join(crate.distribution for crate in installed) or "nothing"
    if remaining is not None:
        return "FAIL", [*detail, f"reinstalled {names}, still broken: {remaining}"]
    return "SYNCED", [*detail, f"reinstalled {names} into {interpreter}"]


def session_findings(root: Path) -> tuple[str, ...]:
    """The drift a session needs told about, bought as cheaply as it can be bought.

    A SessionStart hook cannot afford :func:`run`: that probes the import
    unconditionally and may go on to build a crate. So this asks the question off
    disk first -- ``dist-info`` directory names against the versions this tree
    declares -- and spends the import probe only once a mismatch is already in
    hand, to separate *dead now* from *due to die at the next symbol*. A checkout
    in step pays one interpreter startup and stops.

    Absent crates are not drift here either, for the reason in the module
    docstring, so ``slop_core`` never buys the probe. What this trades away is the
    skew that leaves every version equal -- a rebuilt wheel whose surface moved --
    which stays the job of ``make crg-check``.
    """
    try:
        interpreter, cwd, env = server_interpreter(root)
    except ProbeError:
        # A checkout that does not declare the server has nothing to compare, the
        # same silence native_freshness keeps for a checkout with no .venv.
        return ()
    if skipped(root, interpreter, None) is not None:
        return ()
    packages = purelib(interpreter, timeout=SESSION_TIMEOUT_SECONDS)
    if skipped(root, interpreter, packages) is not None:
        return ()

    mismatched = [one for one in skews(packages, crates(root)) if not one.absent]
    if not mismatched:
        return ()
    lines = [f"{one.line()}; `make crg-sync`" for one in mismatched]
    failure = imports_server(interpreter, cwd, env, timeout=SESSION_TIMEOUT_SECONDS)
    if failure is None:
        lines.append("the server still imports; `make crg-sync` before it stops")
    else:
        lines.append(
            f"cannot import conductor.crg_server: {failure} -- every new session "
            "gets CONNECTION_CLOSED until `make crg-sync` runs"
        )
    return tuple(lines)


def session_report(root: Path) -> str:
    """The SessionStart block, empty when the server's interpreter matches the tree."""
    found = session_findings(root)
    if not found:
        return ""
    lines = "\n".join(f"- {line}" for line in found)
    return f"GRAPH SERVER natives out of date in {root}:\n{lines}"


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo",
        default=".",
        type=Path,
        help="checkout whose .mcp.json declares the server",
    )
    parser.add_argument(
        "--check", action="store_true", help="report drift without repairing it"
    )
    args = parser.parse_args(argv)
    try:
        verdict, detail = run(args.repo.resolve(), args.check)
    except (ProbeError, subprocess.TimeoutExpired) as error:
        print(f"crg-venv-sync | FAIL | {error}")
        return 1
    for line in detail:
        print(f"  {line}")
    print(f"crg-venv-sync | {verdict}")
    return 1 if verdict == "FAIL" else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
