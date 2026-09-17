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

The roster is the crates *this checkout runs on*, which is not always the crates
this tree builds. A host that installs the tooling rather than building it -- the
monorepo, since the 2026-09-14 extraction -- has no crate sources at all, so the
roster comes from its own ``.venv`` instead: the extension distributions this
package requires, at the versions installed here. Until 2026-09-16 only the
in-tree sources were looked for, so the one host this guard exists to protect
answered ``SKIP`` on every run and the skew it watches for could not be seen.

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
import re
import subprocess
import sys
from collections.abc import Mapping
from dataclasses import dataclass
from importlib import metadata
from pathlib import Path

from conductor.crg_mcp_probe import ProbeError, load_server_cmd
from conductor.project_paths import DISTRIBUTION_NAME, native_root
from tooling.hooks.dispatch.native_freshness import (
    compiled_extensions,
    crates,
    direct_url,
    dist_info,
    installed_version,
    site_packages,
)

ABSENT = "absent"
IMPORT_PROBE = "import conductor.crg_server"
PURELIB_PROBE = "import sysconfig; print(sysconfig.get_paths()['purelib'])"
# Where a requirement's name stops and its version specifier, extras or marker
# begins, in the `Requires-Dist` strings importlib hands back.
NAME_END = re.compile(r"[<>=!~;\[(\s]")
TIMEOUT_SECONDS = 900
# A SessionStart hook may not wait fifteen minutes on a wedged interpreter. Both
# probes it runs are sub-second measured (startup, and 0.26 s for the import), so
# this is a bound on pathology, not a budget the healthy path spends.
SESSION_TIMEOUT_SECONDS = 20


@dataclass(frozen=True)
class Requirement:
    """One extension crate this checkout runs on, and where to get it again.

    ``source`` is whatever ``uv pip install`` should be handed: a crate directory
    when this tree builds the crate, and a PEP 508 direct reference when it only
    installs it. Repair needs that distinction -- a consumer checkout has no
    directory to point at, and pointing at the wrong one installs someone else's
    crate into the server's interpreter.
    """

    distribution: str
    version: str
    source: str


@dataclass(frozen=True)
class Skew:
    """One crate the server's interpreter does not carry at this checkout's version."""

    requirement: Requirement
    installed: str

    @property
    def absent(self) -> bool:
        return self.installed == ABSENT

    def line(self) -> str:
        return (
            f"{self.requirement.distribution}: {self.installed} installed, "
            f"{self.requirement.version} declared"
        )


def _reference(distribution: str, info: Path) -> str | None:
    """The PEP 508 direct reference that would reinstall this distribution."""
    origin = direct_url(info)
    url = origin.get("url")
    if not isinstance(url, str):
        return None
    vcs = origin.get("vcs_info")
    if isinstance(vcs, Mapping):
        backend = vcs.get("vcs")
        commit = vcs.get("commit_id") or vcs.get("requested_revision")
        if not isinstance(backend, str) or not isinstance(commit, str):
            return None
        url = f"{backend}+{url}@{commit}"
    subdirectory = origin.get("subdirectory")
    if isinstance(subdirectory, str) and subdirectory:
        url = f"{url}#subdirectory={subdirectory}"
    return f"{distribution} @ {url}"


def declared_names() -> tuple[str, ...]:
    """What this package requires, asked of the metadata of the copy now running.

    The server imports ``conductor`` out of the checkout under inspection, so the
    requirement *names* are this code's own -- read from the distribution that
    owns this module rather than spelled out here, so a crate added upstream is
    picked up without editing this file. The *versions* come from the checkout.
    """
    try:
        declared = metadata.distribution(DISTRIBUTION_NAME).requires or ()
    except metadata.PackageNotFoundError:
        # A source checkout running straight off the tree, never installed. It has
        # its crates in-tree by construction, so this source is not what answers.
        return ()
    return tuple(
        NAME_END.split(spec, maxsplit=1)[0]
        for spec in declared
        if "extra ==" not in spec.partition(";")[2]
    )


def installed_requirements(root: Path) -> tuple[Requirement, ...]:
    """The extension crates this checkout carries as installed distributions.

    A requirement counts only when it was installed from a direct reference *and*
    ships a compiled extension. Both halves are load-bearing: the reference is
    what separates a crate of ours from a third-party wheel that also carries a
    ``.so`` (numpy, coverage, PyYAML all do), and the extension is what separates
    a crate from the pure-Python requirements installed the same way.
    """
    packages = site_packages(root)
    if packages is None:
        return ()
    found = []
    for name in declared_names():
        info = dist_info(packages, name)
        if info is None or not compiled_extensions(info):
            continue
        reference = _reference(name, info)
        if reference is None:
            continue
        found.append(Requirement(name, installed_version(info), reference))
    return tuple(found)


def required(root: Path) -> tuple[Requirement, ...]:
    """The extension crates this checkout runs on -- built here, else installed here."""
    built = crates(root)
    if built:
        return tuple(
            Requirement(crate.distribution, crate.version, str(crate.directory))
            for crate in built
        )
    return installed_requirements(root)


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


def skews(packages: Path, declared: tuple[Requirement, ...]) -> tuple[Skew, ...]:
    """Every crate whose version over there is not the version this checkout runs on."""
    found = []
    for requirement in declared:
        info = dist_info(packages, requirement.distribution)
        installed = installed_version(info) if info is not None else ABSENT
        if installed != requirement.version:
            found.append(Skew(requirement, installed))
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


def install(interpreter: Path, requirement: Requirement) -> None:
    """Put one crate into the server's interpreter without touching its other pins."""
    done = subprocess.run(
        [
            "uv",
            "pip",
            "install",
            "--python",
            str(interpreter),
            "--no-deps",
            "--reinstall",
            requirement.source,
        ],
        env=uv_env(),
        capture_output=True,
        text=True,
        timeout=TIMEOUT_SECONDS,
    )
    if done.returncode != 0:
        raise ProbeError(
            f"installing {requirement.distribution} into {interpreter} "
            f"failed:\n{done.stderr}"
        )


def repair(
    interpreter: Path, cwd: Path, env: dict[str, str], skew: tuple[Skew, ...]
) -> tuple[list[Requirement], str | None]:
    """Install the mismatches, then the absent crates only if the import is still broken."""
    installed: list[Requirement] = []
    failure: str | None = None
    stages = (
        tuple(one for one in skew if not one.absent),
        tuple(one for one in skew if one.absent),
    )
    for stage in stages:
        for one in stage:
            install(interpreter, one.requirement)
            installed.append(one.requirement)
        failure = imports_server(interpreter, cwd, env)
        if failure is None:
            return installed, None
    return installed, failure


def skipped(
    root: Path,
    declared: tuple[Requirement, ...],
    interpreter: Path,
    packages: Path | None,
) -> str | None:
    """Why there is nothing to compare, when there is nothing to compare."""
    if not interpreter.exists():
        return f"{interpreter} does not exist; the server is not installed here"
    if not declared:
        # Both places a crate can be, named: this read "this tree declares no
        # extension crates" until 2026-09-16 and only ever looked in the first,
        # so the consumer checkout it exists to guard skipped every run.
        return (
            f"no extension crates: none under {native_root(root)}, and this "
            "checkout's .venv carries none of the ones this package requires"
        )
    own = site_packages(root)
    # Both interpreters are symlinks onto the same base python, so only the
    # site-packages they resolve to separates a private venv from this one.
    if packages is not None and own is not None and packages.resolve() == own.resolve():
        return "the server runs on this checkout's own .venv; uv sync already covers it"
    return None


def run(root: Path, check_only: bool) -> tuple[str, list[str]]:
    """The verdict -- SKIP, PASS, FAIL or SYNCED -- and the lines that justify it."""
    interpreter, cwd, env = server_interpreter(root)
    declared = required(root)
    reason = skipped(root, declared, interpreter, None)
    if reason is None:
        packages = purelib(interpreter)
        reason = skipped(root, declared, interpreter, packages)
    if reason is not None:
        return "SKIP", [reason]

    skew = skews(packages, declared)
    failure = imports_server(interpreter, cwd, env)
    mismatched = tuple(one for one in skew if not one.absent)
    detail = [one.line() for one in mismatched]
    detail += [
        f"{one.requirement.distribution}: absent from the server's interpreter"
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
    names = ", ".join(one.distribution for one in installed) or "nothing"
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
    declared = required(root)
    if skipped(root, declared, interpreter, None) is not None:
        return ()
    packages = purelib(interpreter, timeout=SESSION_TIMEOUT_SECONDS)
    if skipped(root, declared, interpreter, packages) is not None:
        return ()

    mismatched = [one for one in skews(packages, declared) if not one.absent]
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
