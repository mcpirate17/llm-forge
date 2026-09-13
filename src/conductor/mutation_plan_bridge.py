"""The native side of `mutation_campaign_generate.plan()`.

Split out of `mutation_campaign_generate.py` to keep that module under the
1250-line ceiling (`AGENTS.md`): this is the thin JSON-in/JSON-out bridge to
`conductor_native.mutation_plan_native` (native/conductor-native/src/mutation_plan.rs),
plus the key-order restoration that native's alphabetically-keyed JSON needs to
match the historical Python output byte-for-byte in the CLI's own unsorted
`json.dumps(..., indent=2)` echo. `write()` sorts keys before anything hits
disk, so this reordering never affects what actually gets committed -- only
what an operator sees on stdout from `conductor.mutation_campaign_generate plan`.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

from conductor.mutation_campaign_model import _native_json_call
from conductor.project_paths import campaigns_relative

_MANIFEST_KEY_ORDER = (
    "schema_version",
    "campaign_id",
    "title",
    "language",
    "mutation_engine",
    "generator",
    "test_argv",
    "environment",
    "source_sha256",
    "test_sha256",
    "survivor_baseline",
    "survivor_baseline_recorded",
    "survivor_baseline_note",
)
_FEST_GENERATOR_ORDER = (
    "source",
    "exclude",
    "operators",
    "seed",
    "run_timeout_seconds",
)
_CARGO_GENERATOR_ORDER = (
    "source",
    "exclude",
    "operators",
    "options",
    "seed",
    "jobs",
    "run_timeout_seconds",
)
_CARGO_OPTIONS_ORDER = ("manifest_path", "package", "package_root")
_UNPAIRED_ORDER = ("source", "lines")
_UNTESTED_ORDER = ("package", "root", "lines", "reason")


def _reordered(mapping: Mapping[str, Any], order: Sequence[str]) -> dict[str, Any]:
    """`mapping` with `order`'s keys first, in that order, then any remainder."""
    ordered = {key: mapping[key] for key in order if key in mapping}
    ordered.update((key, value) for key, value in mapping.items() if key not in ordered)
    return ordered


def _canonical_manifest(manifest: Mapping[str, Any]) -> dict[str, Any]:
    """Restore the exact key order `fest_manifest`/`cargo_manifest` build."""
    generator_order = (
        _CARGO_GENERATOR_ORDER
        if manifest.get("mutation_engine") == "cargo-mutants"
        else _FEST_GENERATOR_ORDER
    )
    result = _reordered(manifest, _MANIFEST_KEY_ORDER)
    generator = result.get("generator")
    if isinstance(generator, Mapping):
        generator = _reordered(generator, generator_order)
        options = generator.get("options")
        if isinstance(options, Mapping):
            generator["options"] = _reordered(options, _CARGO_OPTIONS_ORDER)
        result["generator"] = generator
    return result


def plan_native(
    language: str, repo_root: Path, ctx: Mapping[str, Any]
) -> dict[str, Any]:
    """Call `mutation_plan_native`: same output as the pure-Python `plan()`, native speed."""
    request = {
        "language": language,
        "repo_root": str(repo_root),
        "owner": ctx["owner"],
        "day": ctx["day"],
        "jobs": ctx["jobs"],
        "run_timeout_seconds": ctx["run_timeout_seconds"],
        "campaigns_root": str(campaigns_relative(repo_root)),
        "only_sources": list(ctx["only_sources"])
        if ctx["only_sources"] is not None
        else None,
        "include_covered": ctx["include_covered"],
        "extra_tests": {k: list(v) for k, v in (ctx["extra_tests"] or {}).items()},
    }
    data = _native_json_call("mutation_plan_native", request)
    # Native emits an alphabetically-keyed JSON object; reconstruct the
    # historical key order rather than trusting the wire order.
    return {
        "language": data["language"],
        "manifests": [_canonical_manifest(m) for m in data["manifests"]],
        "unpaired": [_reordered(u, _UNPAIRED_ORDER) for u in data["unpaired"]],
        "unpaired_lines": data["unpaired_lines"],
        "already_covered": data["already_covered"],
        "untested": [_reordered(u, _UNTESTED_ORDER) for u in data["untested"]],
    }
