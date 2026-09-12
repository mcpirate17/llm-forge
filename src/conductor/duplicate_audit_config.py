"""Static configuration for repository duplicate analyzers."""

from pathlib import Path

from conductor.project_paths import DEFAULT_MUTATION_REGISTRY

JSCPD_BASELINE_RELATIVE = Path("conductor/jscpd_duplication_baseline.json")
PMD_CPD_BASELINE_RELATIVE = Path("conductor/pmd_cpd_duplication_baseline.json")
AUDIT_ERROR_EXIT_CODE = 2

DEFAULT_SOURCE_DIRS = (
    "research",
    "aria_core",
    "aria_designer",
    "component_fab",
    "conductor",
)

GENERATED_ARTIFACT_GLOBS = ("aria_designer/workflows/generated/**",)
JSCPD_INDEX_CONFIG_PATHS = (
    ".gitignore",
    "package.json",
    JSCPD_BASELINE_RELATIVE.as_posix(),
)
JSCPD_GENERATED_EVIDENCE_IGNORE = (
    f"**/{DEFAULT_MUTATION_REGISTRY.parent}/receipts/**"
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
VULTURE_SOURCE_DIRS = ("research", "aria_core", "aria_designer")
VULTURE_SOURCE_SUFFIXES = frozenset({".py"})

PMD_EXCLUDES = (
    "**/.venv/**",
    "**/node_modules/**",
    "**/__pycache__/**",
    "**/build/**",
    "**/dist/**",
    "**/.run/**",
    "**/tests/**",
    "research/dashboard/**",
    "research/runtime/**",
    "research/runtime_events/**",
    "research/reports/**",
    "research/data/**",
    "research/perf_artifacts/**",
)
