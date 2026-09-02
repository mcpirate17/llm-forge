"""Where the candidate policy lives.

Every site that used to spell ``conductor/candidate_policy.toml`` as a cwd-relative
literal resolves it here. A standalone ``conductor-tooling`` install reviews a foreign
tree from a foreign cwd, where that literal names a file that does not exist.
"""

from __future__ import annotations

import os
from pathlib import Path, PurePosixPath

from conductor.candidate_review.policy import PolicyError

POLICY_ENV = "CONDUCTOR_POLICY"
DEFAULT_POLICY_RELATIVE = PurePosixPath("conductor/candidate_policy.toml")
# The policy shipped next to the package: conductor/candidate_policy.toml.
PACKAGE_POLICY = Path(__file__).resolve().parents[1] / "candidate_policy.toml"


def _requested(explicit: str | os.PathLike[str] | None) -> tuple[str, str | None]:
    """(source, raw path): the CLI flag, else the environment, else the default."""
    if explicit is not None and os.fspath(explicit):
        return "--policy", os.fspath(explicit)
    raw = os.environ.get(POLICY_ENV, "").strip()
    if raw:
        return POLICY_ENV, raw
    return "default", None


def _tree_relative(raw: str, source: str) -> PurePosixPath:
    path = PurePosixPath(raw)
    if path.is_absolute() or ".." in path.parts or not path.parts:
        raise PolicyError(f"{source} policy path must be candidate-relative: {raw!r}")
    return path


def enclosing_repo(start: Path) -> Path | None:
    """The nearest ancestor (inclusive) holding ``.git`` -- a dir or a worktree file."""
    for candidate in (start, *start.parents):
        if candidate == candidate.parent:
            break  # the filesystem root is never a repo; do not probe /.git
        if (candidate / ".git").exists():
            return candidate
    return None


def resolve_policy_path(
    explicit: str | os.PathLike[str] | None = None, *, tree: Path | None = None
) -> Path:
    """The policy file to load; raises ``PolicyError`` when nothing resolves.

    Order: ``explicit`` (a ``--policy`` flag), then ``$CONDUCTOR_POLICY``, then the
    default ``conductor/candidate_policy.toml``.

    With ``tree`` (a materialized candidate) every value is candidate-relative and is
    joined to the tree: the policy is read from the exported candidate, never from the
    working tree or the installed package, so a checkout cannot change a verdict.
    Without a tree the resolved file must exist: an explicit or environment path is
    taken as given, and the default
    is looked for at the enclosing repository root (from the cwd), then next to the
    installed package -- the shipped policy a standalone install carries.
    """
    source, raw = _requested(explicit)
    if tree is not None:
        # One candidate, no search: the tree decides, and ``load_policy`` reports
        # an absent file as loudly as a malformed one.
        relative = _tree_relative(raw, source) if raw else DEFAULT_POLICY_RELATIVE
        return tree / relative.as_posix()
    if raw:
        candidates = [Path(raw)]
    else:
        root = enclosing_repo(Path.cwd().resolve())
        candidates = [] if root is None else [root / DEFAULT_POLICY_RELATIVE.as_posix()]
        candidates.append(PACKAGE_POLICY)
    for candidate in candidates:
        if candidate.is_file():
            return candidate
    tried = ", ".join(str(candidate) for candidate in candidates)
    raise PolicyError(f"no candidate policy ({source}); tried: {tried}")
