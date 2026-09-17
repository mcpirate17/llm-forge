"""Is the native tooling installed in this checkout's venv the one this tree builds?

``conductor`` imports its Rust crates through ``conductor/_native.py``; every hook
body, the gate and the exposure report run on them. When the installed extension
and the crate sources disagree, nothing fails loudly -- the wrong code just runs,
or a symbol is missing and the caller reports a dark result. Three ways that has
happened here: a venv that never had ``conductor_native`` at all (the exposure
report silently lost a section), a wheel still resolving out of a deleted
worktree, and ``uv sync`` serving a cached wheel for an unchanged version after
the crate's surface had changed.

So this module asks four questions of each crate under the host's native root
(``project_paths.native_root``; ``tooling/native`` unless the host says otherwise)
that declares a ``[tool.maturin] module-name``, and the SessionStart hook prints
what it finds:

* installed at all, in *this* checkout's ``.venv``;
* installed version equal to the version the crate declares;
* built from this checkout's crate directory (``direct_url.json``);
* built from the crate sources as they stand now (the build stamp).

Only the last needs the build to cooperate: ``make conductor-native`` and ``make
slop-core`` record ``.venv/.native-stamp.json`` after installing. mtimes cannot
answer it -- uv reinstalls from cache with a fresh install time and the wheel's
own mtimes are preserved from whenever it was built, so both clocks lie in
exactly the case worth catching.

Everything is read off disk rather than imported, so the answer is about the
session's checkout and not about whichever interpreter the hook happens to run
under. A checkout with no ``.venv``, or no native root -- a consumer project that
installs the crates rather than building them -- has nothing to compare here and
says nothing; :mod:`conductor.crg_venv_sync` asks that host its own question.
"""

from __future__ import annotations

import hashlib
import json
import re
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Final, Iterator, Mapping

from conductor.project_paths import native_root

STAMP: Final[str] = ".venv/.native-stamp.json"
VENV: Final[str] = ".venv"

# What the crate is built from. `target/` holds cargo's own generated sources
# (`generated_alias.rs`, `host.rs`) which change on every build and belong to
# dependencies, not to this crate.
SOURCE_SUFFIXES: Final[frozenset[str]] = frozenset({".rs"})
SOURCE_NAMES: Final[frozenset[str]] = frozenset(
    {"Cargo.toml", "Cargo.lock", "pyproject.toml"}
)
EXCLUDED_DIRS: Final[frozenset[str]] = frozenset({"target", ".git"})
# What a compiled extension is called once installed, on either platform.
EXTENSION_SUFFIXES: Final[tuple[str, ...]] = (".so", ".pyd")


@dataclass(frozen=True)
class Crate:
    """One Python extension crate: what the tree declares about it."""

    directory: Path
    distribution: str
    version: str
    module: str

    @property
    def make_target(self) -> str:
        return self.directory.name


@dataclass(frozen=True)
class Finding:
    crate: str
    detail: str

    def line(self) -> str:
        return f"{self.crate}: {self.detail}"


def normalized(distribution: str) -> str:
    """The ``.dist-info`` spelling of a distribution name (PEP 503, then ``_``)."""
    return re.sub(r"[-_.]+", "_", distribution).lower()


def _declared(pyproject: Path) -> Crate | None:
    try:
        data = tomllib.loads(pyproject.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError):
        return None
    project = data.get("project")
    maturin = data.get("tool", {}).get("maturin")
    if not isinstance(project, Mapping) or not isinstance(maturin, Mapping):
        return None
    name, version = project.get("name"), project.get("version")
    module = maturin.get("module-name")
    if not (
        isinstance(name, str) and isinstance(version, str) and isinstance(module, str)
    ):
        return None
    return Crate(pyproject.parent, name, version, module)


def crates(root: Path) -> tuple[Crate, ...]:
    """Every extension crate this tree declares, in directory order.

    A crate with no ``pyproject.toml`` (``snapshot-retention``,
    ``tooling-standalone-smoke``) is a Rust binary, not something installed into
    the venv, and there is nothing here to be stale.

    The directory is the host's, resolved per call: it was the module constant
    ``tooling/native`` until 2026-09-16, which returned ``()`` for every host on
    another layout and so answered every freshness question with silence.
    """
    native = native_root(root)
    if not native.is_dir():
        return ()
    found = (
        _declared(entry / "pyproject.toml")
        for entry in sorted(native.iterdir())
        if entry.is_dir()
    )
    return tuple(crate for crate in found if crate is not None)


def source_files(directory: Path) -> Iterator[Path]:
    """The files a rebuild would read, deepest-first order left to the caller."""
    for path in directory.rglob("*"):
        if not path.is_file():
            continue
        if EXCLUDED_DIRS & set(path.relative_to(directory).parts[:-1]):
            continue
        if path.suffix in SOURCE_SUFFIXES or path.name in SOURCE_NAMES:
            yield path


def source_digest(directory: Path) -> str:
    """One hash over every source file's path and bytes, order-independent."""
    digest = hashlib.sha256()
    for path in sorted(source_files(directory)):
        digest.update(path.relative_to(directory).as_posix().encode("utf-8"))
        digest.update(b"\0")
        digest.update(hashlib.sha256(path.read_bytes()).digest())
    return digest.hexdigest()


def site_packages(root: Path) -> Path | None:
    """This checkout's own ``site-packages``, never an activated one elsewhere."""
    for lib in sorted((root / VENV).glob("lib/python*/site-packages")):
        if lib.is_dir():
            return lib
    return None


def dist_info(packages: Path, distribution: str) -> Path | None:
    """``<escaped name>-<version>.dist-info``: the name never contains a hyphen."""
    wanted = normalized(distribution)
    for entry in sorted(packages.glob("*.dist-info")):
        if entry.name.split("-", 1)[0].lower() == wanted:
            return entry
    return None


def installed_version(info: Path) -> str:
    """The version in the directory name -- the one part of a dist-info that cannot drift."""
    return info.name[: -len(".dist-info")].rpartition("-")[2]


def direct_url(info: Path) -> Mapping[str, object]:
    """PEP 610: where this distribution came from, empty when it came from an index.

    Present only for a direct reference -- a path, an archive or a VCS -- so its
    mere existence separates "built from a source tree we own" from "resolved from
    PyPI", which is the one filter that tells an extension crate of ours apart from
    a third-party wheel that also happens to ship a ``.so`` (numpy, coverage).
    """
    try:
        data = json.loads((info / "direct_url.json").read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, ValueError):
        return {}
    return data if isinstance(data, Mapping) else {}


def compiled_extensions(info: Path) -> tuple[str, ...]:
    """The extension modules this distribution installed, from its own RECORD."""
    try:
        record = (info / "RECORD").read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return ()
    installed = (line.split(",", 1)[0] for line in record.splitlines())
    return tuple(name for name in installed if name.endswith(EXTENSION_SUFFIXES))


def built_from(info: Path) -> Path | None:
    """The directory the wheel was built from, when the installer recorded one."""
    url = direct_url(info).get("url")
    if not isinstance(url, str) or not url.startswith("file://"):
        return None
    return Path(url[len("file://") :])


def read_stamp(root: Path) -> Mapping[str, str]:
    try:
        data = json.loads((root / STAMP).read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, ValueError):
        return {}
    if not isinstance(data, Mapping):
        return {}
    return {str(key): value for key, value in data.items() if isinstance(value, str)}


def write_stamp(root: Path, crate: Crate) -> str:
    """Record what was just built. Returns the digest written."""
    digest = source_digest(crate.directory)
    stamp = dict(read_stamp(root))
    stamp[crate.distribution] = digest
    path = root / STAMP
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(stamp, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return digest


def _crate_findings(
    packages: Path, crate: Crate, stamp: Mapping[str, str]
) -> list[Finding]:
    info = dist_info(packages, crate.distribution)
    if info is None:
        return [
            Finding(
                crate.distribution,
                f"not installed in {VENV}; `make {crate.make_target}` builds it",
            )
        ]
    findings: list[Finding] = []
    version = installed_version(info)
    if version != crate.version:
        findings.append(
            Finding(
                crate.distribution,
                f"installed {version}, this tree declares {crate.version}; "
                f"`make {crate.make_target}`",
            )
        )
    source = built_from(info)
    if source is not None and source.resolve() != crate.directory.resolve():
        findings.append(
            Finding(
                crate.distribution,
                f"built from {source}, not this checkout; `make {crate.make_target}`",
            )
        )
    recorded = stamp.get(crate.distribution)
    if recorded is not None and recorded != source_digest(crate.directory):
        findings.append(
            Finding(
                crate.distribution,
                f"crate sources changed since the last build; `make {crate.make_target}`",
            )
        )
    return findings


def findings(root: Path) -> tuple[Finding, ...]:
    """Every disagreement between this tree's crates and this checkout's venv."""
    packages = site_packages(root)
    if packages is None:
        return ()
    stamp = read_stamp(root)
    found: list[Finding] = []
    for crate in crates(root):
        found.extend(_crate_findings(packages, crate, stamp))
    return tuple(found)


def report(root: Path) -> str:
    """The SessionStart block, empty when the venv matches the tree."""
    found = findings(root)
    if not found:
        return ""
    lines = "\n".join(f"- {finding.line()}" for finding in found)
    return f"NATIVE TOOLING out of date in {root}:\n{lines}"


def main(argv: list[str]) -> int:
    """``stamp <crate-dir>`` after a build; ``check [root]`` prints the report."""
    if len(argv) >= 2 and argv[0] == "stamp":
        directory = Path(argv[1]).resolve()
        crate = _declared(directory / "pyproject.toml")
        if crate is None:
            print(f"not an extension crate: {directory}", file=sys.stderr)
            return 2
        root = Path(argv[2]).resolve() if len(argv) > 2 else Path.cwd()
        print(f"native stamp {crate.distribution} {write_stamp(root, crate)[:12]}")
        return 0
    if argv and argv[0] == "check":
        root = Path(argv[1]).resolve() if len(argv) > 1 else Path.cwd()
        text = report(root)
        if text:
            print(text)
            return 1
        print("native tooling matches this tree")
        return 0
    print(
        "usage: native_freshness stamp <crate-dir> [root] | check [root]",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
