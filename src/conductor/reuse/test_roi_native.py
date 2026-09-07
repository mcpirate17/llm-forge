from __future__ import annotations

from pathlib import Path

from conductor.reuse import core as slop_core
from conductor.reuse import graph_index, roi


def test_native_repository_scan_preserves_filter_loc_and_order(tmp_path: Path) -> None:
    # Exclusions are relative to each selected root; an ancestor named like an
    # excluded component must not make the entire snapshot disappear.
    repo = tmp_path / "skip" / "repo"
    package = repo / "pkg"
    (package / "nested").mkdir(parents=True)
    (package / "tests").mkdir()
    (package / "skip").mkdir()
    (package / "module.py").write_text(
        "# header\nvalue = 1\n// generated note\nvalue += 1\n"
    )
    (package / "UPPER.RS").write_text("/* generated */\nfn main() {}\n")
    (package / "nested" / "test_helper.rs").write_text("* generated\nfn check() {}\n")
    (package / "tests" / "test_module.py").write_text(
        "-- generated\ndef test_value():\n    assert True\n"
    )
    (package / "skip" / "drop.ts").write_text("drop();\n")
    (package / "notes.txt").write_text("not source\n")

    actual = slop_core.audit_repository_scan(
        str(repo),
        ["pkg", "pkg/nested"],
        [".py", ".rs", ".ts"],
        ["skip"],
        ["#", "//", "/*", "*", "--"],
    )

    assert actual == [
        ("pkg/UPPER.RS", 1),
        ("pkg/module.py", 2),
        ("pkg/nested/test_helper.rs", 1),
        ("pkg/tests/test_module.py", 2),
    ]


def test_roi_snapshot_passes_excludes_to_native_scan_and_keeps_policy(
    tmp_path: Path,
) -> None:
    package = tmp_path / "pkg"
    (package / "skip").mkdir(parents=True)
    (package / "tests").mkdir()
    (package / "keep.py").write_text("value = 1\n")
    (package / "tests" / "test_keep.py").write_text("assert True\n")
    (package / "skip" / "drop.py").write_text("value = 2\n")

    class FakeIndex:
        def status(self, targets: list[str]) -> graph_index.GraphStatus:
            assert targets == ["pkg"]
            return graph_index.GraphStatus(True, True, "ok")

        def symbols(self) -> list[object]:
            return []

        def edge_count(self, kind: str) -> int:
            assert kind == "IMPORTS_FROM"
            return 0

    actual = roi.snapshot(
        tmp_path,
        ["pkg"],
        {"skip"},
        FakeIndex(),  # type: ignore[arg-type]
        duplicate_lines=7,
        native_candidates=3,
        contract_report={},
        incomplete_sources=[],
    )

    assert (
        actual
        == {
            "schema_version": 2,
            "snapshot_hash": (
                "3ebd1d159f53fd568fa9df9aca06d6f5901b7aa028765402deae6d734f1f9ab5"  # pragma: allowlist secret
            ),
            "production_loc": 1,
            "test_loc": 1,
            "production_files": 1,
            "test_files": 1,
            "modules": 2,
            "symbols": 0,
            "dependency_edges": 0,
            "duplicate_loc": 7,
            "native_reuse_candidates": 3,
            "contract_violations": 0,
            "unclassified_test_leaves": 0,
            "evidence_complete": True,
            "incomplete_reasons": [],
            "completion_status": "audit_exhausted_for_snapshot",
            "graph": {
                "available": True,
                "complete": True,
                "reason": "ok",
                "graph_head": "",
                "repo_head": "",
                "overlay_hash": "",
                "schema_version": 0,
            },
        }
    )
