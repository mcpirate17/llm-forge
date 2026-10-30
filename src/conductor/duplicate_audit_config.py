"""Static configuration for repository duplicate analyzers.

Where `conductor.project_paths` already knows the answer (the package's own location,
the mutation registry it must not be confused with), it is resolved from there instead
of a second hardcoded literal. The directories scanned are the host's
``[tool.forge.duplicate_audit] source_dirs``, else this repository's own layout
(`src/`, `native/`).
"""

import argparse
import json
import tomllib
from pathlib import Path, PurePosixPath

from conductor.audit_root import resolve_audit_root
from conductor.project_paths import host_root, package_relative, registry_relative

_HOST_ROOT = host_root()
_PACKAGE_RELATIVE = package_relative(_HOST_ROOT)

JSCPD_BASELINE_RELATIVE = Path(_PACKAGE_RELATIVE / "jscpd_duplication_baseline.json")
PMD_CPD_BASELINE_RELATIVE = Path(
    _PACKAGE_RELATIVE / "pmd_cpd_duplication_baseline.json"
)
AUDIT_ERROR_EXIT_CODE = 2

DEFAULT_SOURCE_DIRS = ("src", "native")


def source_dirs(root: Path) -> tuple[str, ...]:
    """The directories under `root` its duplicate analyzers scan.

    Every host was scanned at ``DEFAULT_SOURCE_DIRS`` from 2026-09-12 to 2026-10-25,
    so a tree without `src/` or `native/` passed every duplicate check over no files.
    """
    pyproject = root / "pyproject.toml"
    if not pyproject.is_file():
        return DEFAULT_SOURCE_DIRS
    tool = tomllib.loads(pyproject.read_text(encoding="utf-8")).get("tool", {})
    table = tool.get("forge", {}).get("duplicate_audit", {})
    dirs = table.get("source_dirs", DEFAULT_SOURCE_DIRS)
    if (
        set(table) - {"source_dirs"}
        or not isinstance(dirs, (list, tuple))
        or not dirs
        or not all(
            isinstance(entry, str)
            and entry
            and not PurePosixPath(entry).is_absolute()
            and ".." not in PurePosixPath(entry).parts
            for entry in dirs
        )
    ):
        raise ValueError(
            f"{pyproject}: [tool.forge.duplicate_audit] takes one key, source_dirs, "
            "a non-empty list of directories relative to the host root"
        )
    return tuple(dirs)


def add_scan_root_arguments(parser: argparse.ArgumentParser) -> None:
    """`--repo` and `--targets`, which every reuse analyzer takes."""
    parser.add_argument(
        "--repo", default=None, help="Repo root (default: the Git worktree of the cwd)."
    )
    parser.add_argument(
        "--targets",
        nargs="+",
        default=None,
        help="Subdirs to scan (default: the repo's [tool.forge.duplicate_audit] source_dirs).",
    )


def scan_roots(args: argparse.Namespace) -> tuple[Path, list[str]]:
    """The repo root and its folders to scan: the flags, else the repo's `source_dirs`.

    The root comes from ``resolve_audit_root``, which requires the Git worktree the
    operator is standing in -- never ``__file__``, which would name whichever checkout
    supplied the imported module.
    """
    repo = resolve_audit_root(args.repo)
    return repo, args.targets if args.targets else list(source_dirs(repo))


GENERATED_ARTIFACT_GLOBS: tuple[str, ...] = ()
JSCPD_INDEX_CONFIG_PATHS = (
    ".gitignore",
    "package.json",
    JSCPD_BASELINE_RELATIVE.as_posix(),
)
JSCPD_GENERATED_EVIDENCE_IGNORE = (
    f"**/{registry_relative(_HOST_ROOT).parent}/receipts/**"
)

JSCPD_SOURCE_SUFFIXES = frozenset(
    {
        ".c",
        ".cc",
        ".cpp",
        ".cu",
        ".h",
        ".hpp",
        ".js",
        ".jsx",
        ".json",
        ".py",
        ".rs",
        ".sh",
        ".ts",
        ".tsx",
        ".yaml",
        ".yml",
    }
)
# JSCPD replaces its default extension lookup when formatsExts is supplied.
# Coverage audits must therefore retain every selected language while adding CUDA.
JSCPD_FULL_FILE_FORMATS_EXTS = {
    "python": ["py"],
    "rust": ["rs"],
    "bash": ["sh"],
    "c": ["c"],
    "c-header": ["h"],
    "cpp": ["cpp", "c++", "cc", "cxx", "cu", "cuh"],
    "cpp-header": ["hpp"],
    "javascript": ["js"],
    "jsx": ["jsx"],
    "typescript": ["ts"],
    "tsx": ["tsx"],
}
JSCPD_FULL_FILE_SUFFIXES = frozenset(
    f".{extension}"
    for extensions in JSCPD_FULL_FILE_FORMATS_EXTS.values()
    for extension in extensions
)


def jscpd_report_config(
    root: Path, paths: list[str], *, full_files: bool = False
) -> dict:
    """Load the host detector policy and apply the explicit broad-audit profile."""
    try:
        package = json.loads((root / "package.json").read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise ValueError(f"cannot load jscpd package config: {exc}") from exc
    config = package.get("jscpd") if isinstance(package, dict) else None
    if not isinstance(config, dict):
        raise ValueError("package.json must contain a jscpd object")
    ignore = config.get("ignore", [])
    if not isinstance(ignore, list) or not all(
        isinstance(pattern, str) and pattern for pattern in ignore
    ):
        raise ValueError("package.json jscpd.ignore must be a string list")
    portable_ignore = [
        pattern if pattern.startswith("**/") else f"**/{pattern}" for pattern in ignore
    ]
    config = {
        **config,
        "ignore": [*portable_ignore, JSCPD_GENERATED_EVIDENCE_IGNORE],
    }
    if full_files:
        config.update(
            maxLines=2**53 - 1,
            maxSize=2**53 - 1,
            gitignore=False,
            ignore=[],
            format=list(JSCPD_FULL_FILE_FORMATS_EXTS),
            formatsExts=JSCPD_FULL_FILE_FORMATS_EXTS,
            skipLocal=False,
            path=[str((root / path).resolve()) for path in paths],
        )
        config.pop("pattern", None)
    return config


VULTURE_SOURCE_DIRS = ("src",)
VULTURE_SOURCE_SUFFIXES = frozenset({".py"})

PMD_EXCLUDES = (
    "**/.venv/**",
    "**/node_modules/**",
    "**/__pycache__/**",
    "**/build/**",
    "**/dist/**",
    "**/.run/**",
    "**/tests/**",
    "**/target/**",
)
