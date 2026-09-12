"""Static configuration for repository duplicate analyzers.

Every path below is specific to the host tree conductor is running in -- this
repository's own layout (`src/conductor`, `native/`), not the monorepo's
(`conductor/`, `research/`, ...). Where `conductor.project_paths` already knows the
answer (the package's own location, the mutation registry it must not be confused
with), it is resolved from there instead of a second hardcoded literal.
"""

from pathlib import Path

from conductor.project_paths import host_root, package_relative, registry_relative

_HOST_ROOT = host_root()
_PACKAGE_RELATIVE = package_relative(_HOST_ROOT)

JSCPD_BASELINE_RELATIVE = Path(_PACKAGE_RELATIVE / "jscpd_duplication_baseline.json")
PMD_CPD_BASELINE_RELATIVE = Path(
    _PACKAGE_RELATIVE / "pmd_cpd_duplication_baseline.json"
)
AUDIT_ERROR_EXIT_CODE = 2

DEFAULT_SOURCE_DIRS = ("src", "native")

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
