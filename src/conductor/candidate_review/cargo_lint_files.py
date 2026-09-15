"""Run `cargo fmt --check` or `cargo clippy -D warnings` on changed crates.

CI lints every crate on every native change. The gate cannot afford that and
should not want to: it fires ahead of a commit, so the crates worth compiling
are the ones the candidate touched. This maps changed files onto their owning
`Cargo.toml` and runs the same two commands CI runs, over that subset.

The crate roster lives at `project_paths.crate_roster_path` (`tooling/native/crates.toml`
by default; a host repoints it via `[tool.conductor].crate_roster`), which CI reads
too, so a crate cannot be linted in one place and not the other.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import tomllib
from pathlib import Path

from conductor import project_paths
from conductor.candidate_review.cargo_audit_files import rust_toolchain_env

# Kept for callers and tests that still resolve the roster the old way: the
# unconfigured default, repo-root-relative. ``Roster.load`` itself resolves
# through ``project_paths.crate_roster_path`` so a host's own configuration
# is honoured; this name never diverges from that default.
ROSTER = project_paths.DEFAULT_CRATE_ROSTER

MODES = {
    "fmt": ("cargo", "fmt", "--manifest-path", "{manifest}", "--check"),
    "clippy": (
        "cargo",
        "clippy",
        "--manifest-path",
        "{manifest}",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ),
}


class RosterError(RuntimeError):
    """The configured crate roster is absent, malformed, or declares no crate."""


class Roster:
    """The crate lists and build prerequisites declared for this checkout."""

    def __init__(self, data: dict, *, root: Path) -> None:
        crates = data.get("crates", {})
        self.root = root
        self.tested: frozenset[str] = frozenset(crates.get("tested", ()))
        self.linted: frozenset[str] = frozenset(crates.get("linted", ()))
        self.unstyled: frozenset[str] = frozenset(crates.get("unstyled", ()))
        self.excluded: frozenset[str] = frozenset(crates.get("excluded", ()))
        self.manifest_globs: tuple[str, ...] = tuple(
            data.get("globs", {}).get("manifests", ())
        )
        self.prerequisites: dict[str, dict] = dict(data.get("prerequisites", {}))

    @classmethod
    def load(cls, root: Path) -> Roster:
        path = project_paths.crate_roster_path(root)
        try:
            data = tomllib.loads(path.read_text(encoding="utf-8"))
        except OSError as error:
            raise RosterError(f"{path} is unreadable: {error}") from error
        except tomllib.TOMLDecodeError as error:
            raise RosterError(f"{path} is malformed: {error}") from error
        roster = cls(data, root=root)
        if not roster.manifest_globs:
            raise RosterError(f"{path} declares no globs.manifests")
        return roster

    def unclassified(self) -> list[str]:
        """Crates on disk that are in neither `tested` nor `excluded`.

        The same fail-closed shape CI uses: a crate that appears in neither list
        would inherit no coverage silently, so adding one forces a decision.
        """
        known = self.tested | self.excluded
        found: set[str] = set()
        for pattern in self.manifest_globs:
            for manifest in self.root.glob(pattern):
                found.add(manifest.parent.relative_to(self.root).as_posix())
        return sorted(found - known)

    def blocked_by_prerequisite(self, crate: str) -> str | None:
        """The reason `crate` cannot be compiled here, or None when it can."""
        declared = self.prerequisites.get(crate)
        if declared is None:
            return None
        artifact = declared.get("artifact")
        if artifact and (self.root / artifact).is_file():
            return None
        return (
            f"{crate}: {declared.get('reason', 'a build prerequisite is missing')}; "
            f"build it with `{declared.get('command', 'the documented command')}`"
        )


def owning_crate(path: Path, *, root: Path) -> str | None:
    """The repo-relative directory of the `Cargo.toml` that owns `path`.

    The walk is a finite sequence rather than a hand-advanced cursor. The old
    form advanced `candidate` itself and leaned on three separate statements to
    stop -- the `while` guard, an `== root` break, and the assignment -- so a
    `break_continue` mutant of the break left the cursor parked on `root` with
    the guard still true, and the mutant ran forever instead of failing. A
    mutant that never returns is unkillable by any test, which scores the
    campaign `TIMED_OUT` rather than `SURVIVED`. `parents` is finite by
    construction, so every mutant of this form terminates.
    """
    start = path if path.is_dir() else path.parent
    within = (d for d in (start, *start.parents) if d.is_relative_to(root))
    for candidate in within:
        if (candidate / "Cargo.toml").is_file():
            return candidate.relative_to(root).as_posix()
    return None


def changed_crates(values: list[str], *, root: Path) -> tuple[list[str], list[str]]:
    """Split changed paths into owning crates and paths that own none.

    Paths arrive repo-relative from the gate, so they are resolved against
    `root` rather than the process working directory.
    """
    root = root.resolve()
    crates: set[str] = set()
    orphans: list[str] = []
    for value in values:
        crate = owning_crate((root / value).resolve(), root=root)
        if crate is None:
            orphans.append(value)
        else:
            crates.add(crate)
    return sorted(crates), orphans


def _run(mode: str, crate: str, *, root: Path, env: dict[str, str]) -> int:
    manifest = str(root / crate / "Cargo.toml")
    command = [part.format(manifest=manifest) for part in MODES[mode]]
    print(f"cargo-{mode}: {crate}", file=sys.stderr)
    return subprocess.run(command, check=False, cwd=root, env=env).returncode


def _selected(mode: str, crates: list[str], roster: Roster) -> tuple[list[str], int]:
    """The crates `mode` should run on, plus the exit code so far.

    fmt only parses, so it reaches every crate that is not explicitly unstyled.
    clippy has to compile, so it reaches only the linted roster, and skips --
    loudly, on stderr -- a crate whose build prerequisite is absent locally.
    """
    if mode == "fmt":
        return [c for c in crates if c not in roster.unstyled], 0
    selected = []
    for crate in crates:
        if crate not in roster.linted:
            print(
                f"cargo-clippy: skipping {crate} (not in crates.linted)",
                file=sys.stderr,
            )
            continue
        blocked = roster.blocked_by_prerequisite(crate)
        if blocked is not None:
            print(f"cargo-clippy: skipping {blocked}", file=sys.stderr)
            continue
        selected.append(crate)
    return selected, 0


def version(mode: str) -> int:
    env = rust_toolchain_env()
    tool = "fmt" if mode == "fmt" else "clippy"
    return subprocess.run(["cargo", tool, "--version"], check=False, env=env).returncode


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=sorted(MODES), required=True)
    parser.add_argument("--version", action="store_true")
    parser.add_argument("files", nargs="*")
    args = parser.parse_args(sys.argv[1:] if argv is None else argv)
    if args.version:
        return version(args.mode)

    root = Path.cwd().resolve()
    roster = Roster.load(root)
    unclassified = roster.unclassified()
    if unclassified:
        print(
            "cargo-lint: crates in neither crates.tested nor crates.excluded: "
            + ", ".join(unclassified),
            file=sys.stderr,
        )
        return 1

    crates, orphans = changed_crates(args.files, root=root)
    if orphans:
        print(
            "cargo-lint: no owning Cargo.toml for " + ", ".join(sorted(orphans)),
            file=sys.stderr,
        )
        return 1
    if not crates:
        return 0

    selected, status = _selected(args.mode, crates, roster)
    env = rust_toolchain_env()
    for crate in selected:
        status = _run(args.mode, crate, root=root, env=env) or status
    return status


if __name__ == "__main__":
    raise SystemExit(main())
