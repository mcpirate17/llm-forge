"""Token-efficient fuzzy Python file-family detection.

Profiles files with AST-derived structural, API, state, call, control-flow, and import
features. MinHash/LSH produces a small candidate-pair set; weighted exact scoring and
complete-link clustering then identify 70/80/90-percent implementation families without
an O(n^2) repository scan.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

from conductor.audit_root import resolve_audit_root
from conductor.duplicate_audit_config import DEFAULT_SOURCE_DIRS
from conductor.reuse import _support, consolidation
from conductor.reuse import core as slop_core

DEFAULT_MIN_SIMILARITY = 0.70
DEFAULT_MIN_FILE_LOC = 40
DEFAULT_MIN_SHARED_FEATURES = 12
DEFAULT_MIN_NET_DELETED_LOC = 40
DEFAULT_MAX_FAMILY_SIZE = 8
DEFAULT_MAX_CANDIDATES = 50
DEFAULT_NUM_PERMUTATIONS = 48
DEFAULT_BAND_SIZE = 4
_PRIME = (1 << 61) - 1
_HIGH_RISK_PARTS = {"generator", "mechanisms", "models", "ops", "synthesis"}


@dataclass(frozen=True)
class FileProfile:
    file: str
    loc: int
    classes: frozenset[str]
    function_names: frozenset[str]
    method_names: frozenset[str]
    method_hashes: dict[str, frozenset[str]]
    api: frozenset[str]
    fields: frozenset[str]
    calls: frozenset[str]
    control: frozenset[str]
    imports: frozenset[str]
    structure: frozenset[str]
    schemas: frozenset[str] = frozenset()

    @property
    def lsh_features(self) -> frozenset[str]:
        return frozenset().union(
            self.structure,
            self.api,
            self.fields,
            self.calls,
            self.control,
        )


@dataclass(frozen=True)
class PairSimilarity:
    score: float
    containment: float
    shared_features: int
    components: dict[str, float]


@dataclass
class FileFamily:
    files: list[str]
    similarity_min: float
    similarity_avg: float
    similarity_max: float
    containment_min: float
    shared_methods: list[str]
    variable_methods: list[str]
    common_fields: list[str]
    recommended_abstraction: str
    suggested_home: str | None
    gross_duplicate_loc: int
    estimated_net_deleted_loc: int
    confidence: float
    risk: str
    band: str
    disposition: str
    before_loc: int = 0
    after_loc: int = 0
    target_shape: str = ""
    id: str = ""


def _profile_from_native(raw: dict) -> FileProfile:
    return FileProfile(
        file=raw["file"],
        loc=raw["loc"],
        classes=frozenset(raw["classes"]),
        function_names=frozenset(raw["function_names"]),
        method_names=frozenset(raw["method_names"]),
        method_hashes={
            name: frozenset(values) for name, values in raw["method_hashes"].items()
        },
        api=frozenset(raw["api"]),
        fields=frozenset(raw["fields"]),
        calls=frozenset(raw["calls"]),
        control=frozenset(raw["control"]),
        imports=frozenset(raw["imports"]),
        structure=frozenset(raw["structure"]),
        schemas=frozenset(raw["schemas"]),
    )


def profile_file(path: Path, repo: Path) -> FileProfile | None:
    raw_profiles, _ = slop_core.audit_file_family_profiles(
        paths=[str(path)], repo=str(repo)
    )
    return _profile_from_native(raw_profiles[0]) if raw_profiles else None


def collect_profiles(
    repo: Path,
    targets: list[str],
    exclude: set[str],
    min_file_loc: int,
) -> tuple[list[FileProfile], int]:
    paths = consolidation.iter_python_files(repo, targets, exclude)
    raw_profiles, unparsable = slop_core.audit_file_family_profiles(
        paths=[str(path) for path in paths], repo=str(repo)
    )
    profiles = (_profile_from_native(raw) for raw in raw_profiles)
    return [
        profile
        for profile in profiles
        if profile.loc >= min_file_loc and len(profile.lsh_features) >= 4
    ], unparsable


def _jaccard(left: frozenset[str], right: frozenset[str]) -> float | None:
    union = left | right
    if not union:
        return None
    return len(left & right) / len(union)


def compare_profiles(left: FileProfile, right: FileProfile) -> PairSimilarity:
    score, containment, shared_features, components = (
        slop_core.audit_file_family_compare(left, right)
    )
    return PairSimilarity(
        score=score,
        containment=containment,
        shared_features=shared_features,
        components=dict(components),
    )


def _feature_hash(feature: str) -> int:
    digest = hashlib.blake2b(feature.encode("utf-8"), digest_size=8).digest()
    return int.from_bytes(digest, "big") & _PRIME


def candidate_pairs(
    profiles: list[FileProfile],
    *,
    permutations: int = DEFAULT_NUM_PERMUTATIONS,
    band_size: int = DEFAULT_BAND_SIZE,
) -> set[tuple[int, int]]:
    if permutations % band_size:
        raise ValueError("num_permutations must be divisible by band_size")
    feature_hashes = [
        [_feature_hash(feature) for feature in profile.lsh_features]
        for profile in profiles
    ]
    pairs = slop_core.audit_file_family_lsh_pairs(
        feature_hashes, permutations, band_size
    )
    return set(map(tuple, pairs))


def _jaccard_upper_bound(left: frozenset[str], right: frozenset[str]) -> float | None:
    """Cardinality-only upper bound used to prune exact pair scoring safely."""
    if not left and not right:
        return None
    larger = max(len(left), len(right))
    return min(len(left), len(right)) / larger if larger else None


def exact_candidate_pairs(
    profiles: list[FileProfile],
    *,
    min_similarity: float,
    min_shared_features: int,
) -> tuple[set[tuple[int, int]], int]:
    """Return every pair that can mathematically reach the configured threshold.

    The bound uses the same component weights and empty-component renormalization as
    ``compare_profiles``.  It may retain false positives, but cannot prune a pair whose
    exact score can pass.
    """
    pairs, universe = slop_core.audit_file_family_exact_pairs(
        profiles, min_similarity, min_shared_features
    )
    return set(map(tuple, pairs)), universe


_ABSTRACTION_STEMS = {
    "declarative_builder": "builders",
    "parameterized_runner": "runner",
    "study_registry": "registry",
    "merged_entrypoint": "main",
    "base_class": "base",
    "template_method_base": "base",
    "mixin": "mixins",
    "protocol_composition": "protocols",
    "shared_module": "common",
}


def _common_purpose_stem(files: list[str]) -> str | None:
    """Longest ordered run of filename tokens shared by every member, e.g.
    `_motif_catalog_{core,extended,slots}` -> `motif_catalog`;
    `{hyperbolic,slot}_param_step_sweep` -> `param_step_sweep`."""
    token_lists = [PurePosixPath(f).stem.strip("_").split("_") for f in files]
    if not token_lists:
        return None
    shared = [
        token
        for token in token_lists[0]
        if len(token) > 2 and all(token in rest for rest in token_lists[1:])
    ]
    seen: set[str] = set()
    ordered = [t for t in shared if not (t in seen or seen.add(t))]
    return "_".join(ordered) or None


def purpose_named_home(files: list[str], common_dir: str, abstraction: str) -> str:
    """A concrete shared home named for what the family actually shares — a purpose-named
    module (`_motif_catalog.py`, `_param_step_sweep.py`), never a generic `_shared.py`
    junk drawer. Falls back to an abstraction-derived stem when filenames share no token."""
    stem = _common_purpose_stem(files) or _ABSTRACTION_STEMS.get(abstraction, "common")
    return str(PurePosixPath(common_dir) / f"_{stem}.py")


def _suggested_home(files: list[str], abstraction: str) -> str | None:
    existing = [
        file
        for file in files
        if PurePosixPath(file).name.startswith(("_base", "base", "shared_"))
    ]
    if len(existing) == 1:
        return existing[0]
    common = PurePosixPath(files[0]).parent
    for file in files[1:]:
        other = PurePosixPath(file).parent
        while str(common) != "." and common not in (other, *other.parents):
            common = common.parent
    if str(common) == ".":
        return None  # spans repo roots — leader selects placement during validation
    # Propose a concrete, purpose-named shared module in the common directory. The old
    # "never auto-grow _shared.py" taboo blocked legitimate consolidation; the fix is a
    # meaningful name, not refusing to name one.
    return purpose_named_home(files, str(common), abstraction)


def _cli_family(profiles: list[FileProfile]) -> bool:
    markers = ("import:argparse", "call:ArgumentParser", "call:add_argument")
    return any(
        marker in profile.imports or marker in profile.calls
        for profile in profiles
        for marker in markers
    )


def _registry_named(profiles: list[FileProfile]) -> bool:
    tokens = ("catalog", "manifest", "registry", "register", "study", "studies")
    return any(
        token in PurePosixPath(profile.file).name
        for profile in profiles
        for token in tokens
    )


def _choose_abstraction(class_based: str | None, signals: dict[str, object]) -> str:
    """First matching rule wins (data-driven to keep cyclomatic complexity low)."""
    if class_based:
        return class_based
    rules = (
        (signals["all_have_main"] and signals["cli"], "merged_entrypoint"),
        (signals["common_schemas"] and signals["registry"], "study_registry"),
        (bool(signals["common_schemas"]), "declarative_builder"),
        (signals["common_calls"] and signals["runner_like"], "parameterized_runner"),
    )
    return next((name for cond, name in rules if cond), "shared_module")


def _class_abstraction(
    all_class_files: bool,
    shared_methods: set,
    variable_methods: set,
    common: set,
    fields: set,
) -> str | None:
    if not all_class_files:
        return None
    rules = (
        (bool(shared_methods and variable_methods), "template_method_base"),
        (bool(shared_methods and fields), "base_class"),
        (bool(shared_methods), "mixin"),
        (bool(common), "protocol_composition"),
    )
    return next((name for cond, name in rules if cond), None)


def _recommend_abstraction(
    profiles: list[FileProfile],
) -> tuple[str, list[str], list[str], list[str]]:
    all_class_files = all(profile.classes for profile in profiles)
    common_methods = set.intersection(
        *(set(profile.method_names) for profile in profiles)
    )
    common_fields = set.intersection(*(set(profile.fields) for profile in profiles))
    shared_methods = {
        method
        for method in common_methods
        if set.intersection(
            *(set(profile.method_hashes.get(method, ())) for profile in profiles)
        )
    }
    variable_methods = common_methods - shared_methods
    common_schemas = (
        set.intersection(*(set(profile.schemas) for profile in profiles))
        if profiles
        else set()
    )
    class_based = _class_abstraction(
        all_class_files, shared_methods, variable_methods, common_methods, common_fields
    )
    abstraction = _choose_abstraction(
        class_based,
        {
            "common_schemas": common_schemas,
            "common_calls": set.intersection(
                *(set(profile.calls) for profile in profiles)
            ),
            "all_have_main": all(
                "main" in profile.function_names for profile in profiles
            ),
            "cli": _cli_family(profiles),
            "registry": _registry_named(profiles),
            "runner_like": any(
                "main" in profile.function_names or "run" in profile.function_names
                for profile in profiles
            ),
        },
    )
    return (
        abstraction,
        sorted(shared_methods),
        sorted(variable_methods),
        sorted(field.removeprefix("field:") for field in common_fields),
    )


def _family_from_profiles(
    profiles: list[FileProfile],
    similarities: list[PairSimilarity],
    min_net_deleted_loc: int,
) -> FileFamily:
    files = sorted(profile.file for profile in profiles)
    scores = [similarity.score for similarity in similarities]
    containments = [similarity.containment for similarity in similarities]
    abstraction, shared_methods, variable_methods, common_fields = (
        _recommend_abstraction(profiles)
    )
    # Net LOC from the PROPOSED COMPACT SHAPE, not `similarity × shortest file`: the shared
    # skeleton is written once (the fullest member approximates the union), each file keeps
    # only its non-shared remainder, and declarative/entrypoint families collapse most of
    # that remainder into data/params inside the shared home.
    locs = [profile.loc for profile in profiles]
    shared_fraction = min(min(scores), min(containments))
    before_loc = sum(locs)
    shared_loc = round(max(locs) * shared_fraction)
    gross = shared_loc * (len(profiles) - 1)
    residual = sum(loc - round(loc * shared_fraction) for loc in locs)
    if abstraction in ("declarative_builder", "study_registry", "merged_entrypoint"):
        residual = round(residual * 0.45)
    overhead = 4 * len(profiles) + 6 * len(variable_methods)
    after_loc = shared_loc + residual + overhead
    net = max(0, before_loc - after_loc)
    roots = {PurePosixPath(file).parts[0] for file in files}
    directories = {PurePosixPath(file).parent for file in files}
    semantic_risk = any(
        set(PurePosixPath(file).parts) & _HIGH_RISK_PARTS for file in files
    )
    risk = (
        "high"
        if semantic_risk or len(roots) > 1
        else "low"
        if len(directories) == 1
        else "medium"
    )
    locality = 1.0 if len(directories) == 1 else 0.88 if len(roots) == 1 else 0.70
    confidence = max(
        0.05, round(min(scores) * locality - (0.08 if semantic_risk else 0), 2)
    )
    average = sum(scores) / len(scores)
    band = "90%+" if average >= 0.90 else "80-89%" if average >= 0.80 else "70-79%"
    disposition = (
        "validate" if net >= min_net_deleted_loc and confidence >= 0.55 else "ignore"
    )
    home = _suggested_home(files, abstraction)
    keeps_data = abstraction in (
        "declarative_builder",
        "study_registry",
        "merged_entrypoint",
    )
    target_shape = (
        f"{abstraction}: {len(files)} files -> {home or '<leader-selected home>'}"
        f" ({before_loc}->{after_loc} LOC; shared skeleton + per-file "
        f"{'data' if keeps_data else 'specialization'})"
    )
    return FileFamily(
        files=files,
        similarity_min=round(min(scores), 4),
        similarity_avg=round(average, 4),
        similarity_max=round(max(scores), 4),
        containment_min=round(min(containments), 4),
        shared_methods=shared_methods,
        variable_methods=variable_methods,
        common_fields=common_fields,
        recommended_abstraction=abstraction,
        suggested_home=home,
        gross_duplicate_loc=gross,
        estimated_net_deleted_loc=net,
        confidence=confidence,
        risk=risk,
        band=band,
        disposition=disposition,
        before_loc=before_loc,
        after_loc=after_loc,
        target_shape=target_shape,
    )


def build_families(
    profiles: list[FileProfile],
    pairs: set[tuple[int, int]],
    *,
    min_similarity: float,
    min_shared_features: int,
    min_net_deleted_loc: int,
    max_family_size: int,
    max_candidates: int,
) -> tuple[list[FileFamily], int]:
    groups, pairs_scored = slop_core.audit_file_family_groups(
        profiles,
        list(pairs),
        min_similarity,
        min_shared_features,
        max_family_size,
    )
    families: list[FileFamily] = []
    for ordered, native_scores in groups:
        pair_scores = [
            PairSimilarity(
                score=score,
                containment=containment,
                shared_features=shared_features,
                components={},
            )
            for score, containment, shared_features in native_scores
        ]
        family = _family_from_profiles(
            [profiles[index] for index in ordered], pair_scores, min_net_deleted_loc
        )
        if family.disposition != "ignore":
            families.append(family)
    families.sort(
        key=lambda family: (family.estimated_net_deleted_loc, family.similarity_avg),
        reverse=True,
    )
    families = families[:max_candidates]
    for index, family in enumerate(families, 1):
        family.id = f"F{index:03d}"
    return families, pairs_scored


def scan_file_families(
    repo: Path,
    targets: list[str],
    exclude: set[str],
    *,
    min_similarity: float = DEFAULT_MIN_SIMILARITY,
    min_file_loc: int = DEFAULT_MIN_FILE_LOC,
    min_shared_features: int = DEFAULT_MIN_SHARED_FEATURES,
    min_net_deleted_loc: int = DEFAULT_MIN_NET_DELETED_LOC,
    max_family_size: int = DEFAULT_MAX_FAMILY_SIZE,
    max_candidates: int = DEFAULT_MAX_CANDIDATES,
    permutations: int = DEFAULT_NUM_PERMUTATIONS,
    band_size: int = DEFAULT_BAND_SIZE,
) -> tuple[list[FileFamily], dict]:
    if not 0.0 < min_similarity <= 1.0:
        raise ValueError(f"min_similarity must be in (0, 1], got {min_similarity}")
    if min_file_loc < 1 or min_shared_features < 1 or min_net_deleted_loc < 0:
        raise ValueError("file-family size/value thresholds must be positive")
    if max_family_size < 2 or max_candidates < 1:
        raise ValueError("max_family_size must be >=2 and max_candidates must be >=1")
    if permutations < 1 or band_size < 1 or permutations % band_size:
        raise ValueError(
            "minhash_permutations must be divisible by positive lsh_band_size"
        )
    profiles, files_unparsable = collect_profiles(repo, targets, exclude, min_file_loc)
    lsh_pairs = candidate_pairs(
        profiles, permutations=permutations, band_size=band_size
    )
    pairs, pair_universe = exact_candidate_pairs(
        profiles,
        min_similarity=min_similarity,
        min_shared_features=min_shared_features,
    )
    families, pairs_scored = build_families(
        profiles,
        pairs,
        min_similarity=min_similarity,
        min_shared_features=min_shared_features,
        min_net_deleted_loc=min_net_deleted_loc,
        max_family_size=max_family_size,
        max_candidates=max_candidates,
    )
    return families, {
        "files_profiled": len(profiles),
        "files_unparsable": files_unparsable,
        "pair_universe": pair_universe,
        "exact_candidate_pairs": len(pairs),
        "pairs_pruned_by_safe_bound": pair_universe - len(pairs),
        "lsh_candidate_pairs": len(lsh_pairs),
        "lsh_recall": round(len(lsh_pairs & pairs) / len(pairs), 4) if pairs else 1.0,
        "pairs_scored": pairs_scored,
        "families": len(families),
        "estimated_net_deleted_loc": sum(
            family.estimated_net_deleted_loc for family in families
        ),
    }


def family_to_dict(family: FileFamily) -> dict:
    return {
        "id": family.id,
        "files": family.files,
        "similarity_min": family.similarity_min,
        "similarity_avg": family.similarity_avg,
        "similarity_max": family.similarity_max,
        "containment_min": family.containment_min,
        "band": family.band,
        "shared_methods": family.shared_methods,
        "variable_methods": family.variable_methods,
        "common_fields": family.common_fields,
        "recommended_abstraction": family.recommended_abstraction,
        "suggested_home": family.suggested_home,
        "gross_duplicate_loc": family.gross_duplicate_loc,
        "estimated_net_deleted_loc": family.estimated_net_deleted_loc,
        "confidence": family.confidence,
        "risk": family.risk,
        "disposition": family.disposition,
    }


def build_output(families: list[FileFamily], stats: dict, settings: dict) -> dict:
    return _support.generated_output(
        base={"settings": settings, "summary": stats},
        items_key="families",
        items=families,
        serialize=family_to_dict,
    )


def write_markdown(path: Path, data: dict) -> None:
    summary = data["summary"]
    rows: list[str] = []
    for family in data["families"]:
        files = _support.plus_more_list(family["files"], show=5)
        rows.append(
            f"| {family['id']} | {family['band']} | {family['similarity_avg']:.1%} | "
            f"{family['containment_min']:.1%} | {family['recommended_abstraction']} | "
            f"{family['estimated_net_deleted_loc']} | {family['confidence']:.2f} | {files} |"
        )
    _support.write_markdown_table(
        path,
        title=f"# File families - generated {data['generated_at']}",
        summary=(
            f"{summary['families']} families from {summary['files_profiled']} profiled files; "
            f"{summary['lsh_candidate_pairs']} LSH pairs, {summary['pairs_scored']} exact scores; "
            f"~{summary['estimated_net_deleted_loc']} estimated net deletable LOC."
        ),
        columns=[
            "id",
            "band",
            "similarity",
            "containment",
            "abstraction",
            "net LOC",
            "confidence",
            "files",
        ],
        rows=rows,
    )


# --------------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------------

DEFAULT_OUT_REL = Path("tasks/audit/file_families.json")
DEFAULT_MD_REL = Path("tasks/audit/file_families.md")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "MinHash/LSH near-duplicate file-family detection over the source tree."
        )
    )
    parser.add_argument(
        "--repo", default=None, help="Repo root (default: the Git worktree of the cwd)."
    )
    parser.add_argument(
        "--targets",
        nargs="+",
        default=None,
        help="Subdirs to scan (default: conductor DEFAULT_SOURCE_DIRS).",
    )
    parser.add_argument(
        "--exclude", nargs="+", default=None, help="Path parts to skip."
    )
    parser.add_argument("--min-similarity", type=float, default=DEFAULT_MIN_SIMILARITY)
    parser.add_argument("--min-file-loc", type=int, default=DEFAULT_MIN_FILE_LOC)
    parser.add_argument(
        "--min-shared-features", type=int, default=DEFAULT_MIN_SHARED_FEATURES
    )
    parser.add_argument(
        "--min-net-deleted-loc", type=int, default=DEFAULT_MIN_NET_DELETED_LOC
    )
    parser.add_argument("--max-family-size", type=int, default=DEFAULT_MAX_FAMILY_SIZE)
    parser.add_argument("--max-candidates", type=int, default=DEFAULT_MAX_CANDIDATES)
    parser.add_argument("--permutations", type=int, default=DEFAULT_NUM_PERMUTATIONS)
    parser.add_argument("--band-size", type=int, default=DEFAULT_BAND_SIZE)
    parser.add_argument("--out", default=None, help="Output JSON path.")
    parser.add_argument("--md", default=None, help="Output markdown path.")
    return parser


def main(argv: list[str] | None = None) -> int:
    """Scan for file families and write the JSON/markdown pair.

    Replaces ``audit/orchestrator/orchestrate.py cmd_families``, which read its
    thresholds from that loop's ``config.toml``. They are flags here, so the analyzer
    has an entry point of its own rather than depending on the retired audit loop.
    """
    args = build_parser().parse_args(argv)
    repo = resolve_audit_root(args.repo)
    targets = args.targets if args.targets else list(DEFAULT_SOURCE_DIRS)
    exclude = set(args.exclude) if args.exclude else set()

    settings = {
        "min_similarity": args.min_similarity,
        "min_file_loc": args.min_file_loc,
        "min_shared_features": args.min_shared_features,
        "min_net_deleted_loc": args.min_net_deleted_loc,
        "max_family_size": args.max_family_size,
        "max_candidates": args.max_candidates,
        "permutations": args.permutations,
        "band_size": args.band_size,
    }
    families, stats = scan_file_families(repo, targets, exclude, **settings)

    out_path = Path(args.out) if args.out else repo / DEFAULT_OUT_REL
    md_path = Path(args.md) if args.md else repo / DEFAULT_MD_REL
    out_path.parent.mkdir(parents=True, exist_ok=True)
    md_path.parent.mkdir(parents=True, exist_ok=True)

    data = build_output(families, stats, settings)
    out_path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    write_markdown(md_path, data)

    print(
        f"file families: {len(families)} candidate(s), "
        f"~{stats['estimated_net_deleted_loc']} net deletable LOC, "
        f"{stats['lsh_candidate_pairs']} LSH pairs -> {out_path} / {md_path}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
