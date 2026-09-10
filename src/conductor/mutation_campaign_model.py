"""Campaign manifests, runner hashing and drift -- the model half of the runner.

`conductor.mutation_testing` orchestrates campaigns; this module owns what a campaign
IS: the manifest dataclasses, the hashes that pin it to a tree, and the two drift
questions the framework asks before trusting evidence -- has the source moved
(`source_drift`), and is there still a receipt the gate accepts (`receipt_drift`,
which lives with the receipt validators in `mutation_testing`).

Both halves are runner components: a change here invalidates receipts exactly as a
change to the orchestrator does, which is why this path is named in
`RUNNER_COMPONENT_PATHS` below.
"""

from __future__ import annotations

import ast
import hashlib
import json
import re
from collections.abc import Mapping
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from conductor import mutation_testing_support as _support
from conductor.mutation_scope import (
    CampaignError,
    TestFileScope,
    _require_mapping,
)
from conductor.mutation_value import (
    ValueAnalysisSpec,
    ValueEvidenceError,
    load_value_analysis,
)

RECEIPT_SCHEMA = "llm.mutation-testing.receipt.v3"
LEGACY_RECEIPT_SCHEMA = "llm.mutation-testing.receipt.v2"
LEGACY_RECEIPT_ANCHOR_COMMIT = "61343f575215dd222a74fc2c060d0328692ded5e"
LEGACY_RECEIPT_ANCHOR_TREE = "b01877ba62c32445f7649450f9b395dd70de306a"
LEGACY_RECEIPT_PREFIX = "conductor/mutation_campaigns/receipts/"
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
CANONICAL_TEST_PATTERNS = _support.CANONICAL_TEST_PATTERNS
OUTPUT_TAIL_CHARS = 12_000
REPO_ROOT = Path(__file__).resolve().parents[1]
RUNNER_COMPONENT_PATHS = (
    "conductor/mutation_campaign_model.py",
    "conductor/mutation_patch_apply.py",
    "conductor/mutation_receipt_build.py",
    "conductor/mutation_scope.py",
    "conductor/mutation_testing.py",
    "conductor/mutation_testing_support.py",
    "conductor/mutation_value.py",
    "conductor/snapshot_worktree.py",
    "tooling/native/conductor-native/src/mutation_evidence.rs",
    "tooling/native/conductor-native/src/mutation_manifest.rs",
    "tooling/native/conductor-native/src/mutation_receipt.rs",
)


@dataclass(frozen=True, slots=True)
class RankedTest:
    """One test selected for a campaign, ordered by contract importance."""

    rank: int
    nodeid: str
    contract: str
    rationale: str


@dataclass(frozen=True, slots=True)
class PlannedMutation:
    """A mutation design slot that contains no executable code change."""

    mutation_id: str
    target_path: str
    contract: str
    description: str
    expected_killers: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class Mutation:
    """One materialized, first-order mutation represented by a patch file."""

    mutation_id: str
    patch_file: Path
    patch_sha256: str
    allowed_paths: tuple[str, ...]
    expected_killers: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class Campaign:
    """Validated mutation campaign loaded from a machine-readable manifest."""

    manifest_path: Path
    manifest_sha256: str
    campaign_id: str
    title: str
    language: str
    mutation_engine: str
    expected_mutations: int
    source_sha256: Mapping[str, str]
    ranked_tests: tuple[RankedTest, ...]
    planned_mutations: tuple[PlannedMutation, ...]
    mutations: tuple[Mutation, ...]
    test_argv: tuple[str, ...]
    timeout_seconds: int
    blocked_process_substrings: tuple[str, ...]
    poll_seconds: int
    environment: Mapping[str, str]
    host_read_dependencies: tuple[str, ...]
    test_scopes: Mapping[str, TestFileScope] = field(default_factory=dict)
    # Optional per-symbol pins. A path here is checked symbol-by-symbol instead
    # of whole-file, so an edit outside the pinned functions does not drift the
    # campaign. Absent means the old whole-file behaviour, unchanged.
    source_symbols: Mapping[str, Mapping[str, str]] = field(default_factory=dict)
    value_analysis: ValueAnalysisSpec | None = None
    generated: bool = False
    survivor_baseline: tuple[str, ...] = ()
    test_sha256: Mapping[str, str] = field(default_factory=dict)


@dataclass(frozen=True, slots=True)
class CommandResult:
    """Bounded subprocess evidence for a baseline or mutant test run."""

    returncode: int | None
    timed_out: bool
    duration_seconds: float
    stdout_tail: str
    stderr_tail: str

    def as_dict(self) -> dict[str, Any]:
        """Return a JSON-safe representation."""

        return {
            "returncode": self.returncode,
            "timed_out": self.timed_out,
            "duration_seconds": round(self.duration_seconds, 6),
            "stdout_tail": self.stdout_tail,
            "stderr_tail": self.stderr_tail,
        }


def _native_row(model: Any, row: Mapping[str, Any]) -> Any:
    """Materialize one normalized native row as its stable Python dataclass."""

    values = {
        name: row["id"] if name == "mutation_id" else row[name]
        for name in model.__dataclass_fields__
    }
    for name in ("allowed_paths", "expected_killers"):
        if name in values:
            values[name] = tuple(values[name])
    if "patch_file" in values:
        values["patch_file"] = Path(values["patch_file"])
    return model(**values)


def _native_json_call(
    function_name: str,
    payload: Mapping[str, Any],
) -> Any:
    """Call one native mutation primitive and decode its JSON result."""

    try:
        from conductor import _native as runtime

        operation = getattr(runtime, function_name)
        encoded = operation(
            json.dumps(payload, ensure_ascii=False, separators=(",", ":"))
        )
        return json.loads(encoded)
    except (ImportError, AttributeError, ValueError, json.JSONDecodeError) as exc:
        raise CampaignError(str(exc)) from exc


def _patch_paths(patch_path: Path) -> tuple[str, ...]:
    """Extract and validate repository-relative paths from a unified diff."""

    try:
        from conductor._native import mutation_patch_paths_native

        return tuple(mutation_patch_paths_native(str(patch_path)))
    except (ImportError, AttributeError, ValueError) as exc:
        raise CampaignError(str(exc)) from exc


def _test_scope_from_native(relative: str, scope: Mapping[str, Any]) -> TestFileScope:
    """Build one test scope from the native loader's row.

    A generated campaign names its test files in the run command instead of
    enumerating nodeids, so the loader reports `selection: "test_argv"` and no
    inventory. Reading `inventory` unconditionally is what made every generated
    manifest unloadable from Python while the native validator accepted it.
    """

    selection = scope.get("selection", "nodeids")
    if selection == "test_argv":
        return TestFileScope(
            path=relative,
            mode=scope["mode"],
            inventory="",
            nodeids=(),
            selection=selection,
        )
    return TestFileScope(
        path=relative,
        mode=scope["mode"],
        inventory=scope["inventory"],
        nodeids=tuple(scope["nodeids"]),
        selection=selection,
    )


def load_campaign(path: Path, *, repo_root: Path = REPO_ROOT) -> Campaign:
    """Load a mutation campaign through the native deterministic validator."""

    root = repo_root.resolve()
    manifest_path = path.resolve()
    try:
        relative_manifest = manifest_path.relative_to(root).as_posix()
    except ValueError as exc:
        raise CampaignError("campaign manifest must be inside the repository") from exc
    try:
        raw = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise CampaignError(f"cannot load campaign {manifest_path}: {exc}") from exc
    result = _native_json_call(
        "load_mutation_campaign_native",
        {"repo_root": str(root), "manifest_path": relative_manifest},
    )
    data = _require_mapping(result, "native mutation campaign")
    ranked_tests = tuple(_native_row(RankedTest, row) for row in data["ranked_tests"])
    planned_mutations = tuple(
        _native_row(PlannedMutation, row) for row in data["planned_mutations"]
    )
    mutations = tuple(_native_row(Mutation, row) for row in data["mutations"])
    test_scopes = {
        relative: _test_scope_from_native(relative, scope)
        for relative, scope in data["test_scopes"].items()
    }
    try:
        value_analysis = load_value_analysis(
            raw.get("value_analysis") if isinstance(raw, dict) else None,
            ranked_nodeids=[test.nodeid for test in ranked_tests],
            mutation_ids=[mutation.mutation_id for mutation in planned_mutations],
            source_paths=list(data["source_sha256"]),
        )
    except ValueEvidenceError as exc:
        raise CampaignError(f"invalid value_analysis: {exc}") from exc
    kwargs = {
        name: data[name]
        for name in Campaign.__dataclass_fields__
        if name in data and name not in {"value_analysis", "test_scopes"}
    }
    kwargs.update(
        manifest_path=root / data["manifest"],
        ranked_tests=ranked_tests,
        planned_mutations=planned_mutations,
        mutations=mutations,
        test_scopes=test_scopes,
        value_analysis=value_analysis,
    )
    for name in (
        "test_argv",
        "blocked_process_substrings",
        "host_read_dependencies",
    ):
        kwargs[name] = tuple(kwargs[name])
    kwargs["survivor_baseline"] = tuple(data.get("survivor_baseline", ()))
    return Campaign(**kwargs)


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _runner_components_sha256() -> dict[str, str]:
    """Bind every first-party module that can affect mutation execution."""

    root = Path(__file__).resolve().parents[1]
    components: dict[str, str] = {}
    for relative in RUNNER_COMPONENT_PATHS:
        path = root / relative
        if not path.is_file() or path.is_symlink():
            raise CampaignError(
                f"mutation runner component is missing or unsafe: {relative}"
            )
        components[relative] = _sha256(path)
    return components


RUNNER_LINEAGE_PATH = "conductor/mutation_runner_lineage.json"


def _lineage_accepts(recorded: object, repo_root: Path) -> bool:
    """Return whether a runner-component map is explicitly accepted."""

    try:
        from conductor._native import mutation_runner_lineage_accepts_native

        return bool(
            mutation_runner_lineage_accepts_native(
                json.dumps(
                    {"repo_root": str(repo_root.resolve()), "recorded": recorded},
                    ensure_ascii=False,
                    separators=(",", ":"),
                )
            )
        )
    except (ImportError, AttributeError, ValueError, TypeError):
        return False


def symbol_hashes(path: Path) -> dict[str, str]:
    """AST hash per top-level symbol, and per method, in a Python file.

    `ast.dump(..., include_attributes=False)` drops line and column numbers, so a
    comment, a docstring reflow, an import added above, or any edit to a NEIGHBOURING
    function leaves a symbol's hash untouched. Only a change to that symbol's own
    syntax tree moves it. That is the whole point: a campaign pins the functions its
    mutants actually touch, and edits elsewhere in the file stop voiding it.

    Raises rather than returning a partial map: a file that cannot be parsed must not
    silently produce "no drift".
    """
    try:
        tree = ast.parse(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, SyntaxError) as exc:
        raise CampaignError(f"cannot inventory symbols in {path}: {exc}") from exc

    hashes: dict[str, str] = {}

    def record(qualname: str, node: ast.AST) -> None:
        dumped = ast.dump(node, annotate_fields=True, include_attributes=False)
        hashes[qualname] = hashlib.sha256(dumped.encode("utf-8")).hexdigest()

    definition = (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)
    for node in tree.body:
        if not isinstance(node, definition):
            continue
        record(node.name, node)
        if isinstance(node, ast.ClassDef):
            for child in node.body:
                if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    record(f"{node.name}.{child.name}", child)
    return hashes


def source_drift(campaign: Campaign, root: Path) -> list[dict[str, Any]]:
    """Return native file drift plus CPython-AST symbol drift."""

    current = {
        relative: symbol_hashes(root / relative)
        for relative in campaign.source_symbols
        if (root / relative).is_file() and not (root / relative).is_symlink()
    }
    bindings = [(campaign.source_sha256, campaign.source_symbols, current)]
    if campaign.generated:
        # Keep separate maps: an overlapping source/test path must satisfy both
        # pins, and test files require full-file rather than symbol-only checks.
        bindings.append((campaign.test_sha256, {}, {}))
    drift = []
    for pins, symbols, hashes in bindings:
        result = _native_json_call(
            "mutation_source_drift_native",
            {
                "repo_root": str(root.resolve()),
                "source_sha256": dict(pins),
                "source_symbols": symbols,
                "symbol_hashes": hashes,
            },
        )
        if not isinstance(result, list) or not all(
            isinstance(row, dict) for row in result
        ):
            raise CampaignError(
                "native mutation source drift must be a list of objects"
            )
        drift.extend(result)
    return drift


def _campaign_receipts(
    repo_root: Path, registry_path: Path
) -> dict[str, list[tuple[Path, Mapping[str, Any]]]]:
    """Every published receipt, parsed once and indexed by the campaign it reports on.

    A campaign accumulates receipts; superseded ones stay on disk beside the current
    one, so the question is never "is THE receipt good" but "is ANY receipt good".
    """

    directory = repo_root / _support.receipt_directory(
        _load_registry(registry_path, repo_root), CampaignError
    )
    index: dict[str, list[tuple[Path, Mapping[str, Any]]]] = {}
    if not directory.is_dir():
        return index
    for path in sorted(directory.glob("*.json")):
        try:
            payload = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError):
            # A receipt that cannot be parsed is not evidence; the gate says so in its
            # own words, and swallowing it here would only hide which file is broken.
            continue
        if not isinstance(payload, Mapping):
            continue
        campaign_id = payload.get("campaign_id")
        if isinstance(campaign_id, str):
            index.setdefault(campaign_id, []).append((path, payload))
    return index


def _load_registry(path: Path, repo_root: Path) -> Mapping[str, Any]:
    root = repo_root.resolve()
    try:
        relative = path.resolve().relative_to(root).as_posix()
    except ValueError as exc:
        raise CampaignError("mutation registry must be inside the repository") from exc
    return _require_mapping(
        _native_json_call(
            "load_mutation_registry_native",
            {
                "repo_root": str(root),
                "registry_path": relative,
                "canonical_test_patterns": list(CANONICAL_TEST_PATTERNS),
            },
        ),
        "registry",
    )
