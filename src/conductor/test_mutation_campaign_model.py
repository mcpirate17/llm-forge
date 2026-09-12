"""Generated contracts must survive the Python compatibility boundary intact."""

from dataclasses import asdict, replace
import json
from pathlib import Path

import pytest

from conductor import mutation_campaign_model as model
from conductor.mutation_campaign_generate import fest_manifest
from conductor.mutation_testing import _native_campaign_contract


def generated_manifest(root: Path) -> Path:
    (root / "subject.py").write_text("def subject():\n    return 1\n")
    (root / "test_subject.py").write_text("def test_subject():\n    assert True\n")
    payload = fest_manifest(
        {"source": "subject.py", "tests": ["test_subject.py"]},
        campaign_id="fixture",
        repo_root=root,
        run_timeout_seconds=30,
    )
    path = root / "campaign.json"
    path.write_text(json.dumps(payload))
    return path


def test_generated_native_contract_roundtrip_keeps_identity_and_test_pins(tmp_path):
    path = generated_manifest(tmp_path)
    loaded = model.load_campaign(path, repo_root=tmp_path)
    native = model._native_json_call(
        "load_mutation_campaign_native",
        {"repo_root": str(tmp_path), "manifest_path": "campaign.json"},
    )
    restored = _native_campaign_contract(loaded, repo_root=tmp_path)
    assert loaded.generated is True
    assert loaded.survivor_baseline == ()
    for field in ("test_argv", "blocked_process_substrings", "host_read_dependencies"):
        assert isinstance(getattr(loaded, field), tuple)
    assert loaded.test_sha256 == native["test_sha256"]
    assert set(loaded.test_sha256) == {"test_subject.py"}
    for field in ("generated", "test_sha256"):
        assert restored[field] == native[field]
    assert list(restored["survivor_baseline"]) == native["survivor_baseline"]
    assert restored["source_drifted"] is False
    (tmp_path / "test_subject.py").write_text("def test_changed():\n    pass\n")
    assert (
        _native_campaign_contract(loaded, repo_root=tmp_path)["source_drifted"] is True
    )

    values = {
        field: getattr(loaded, field)
        for field in model.Campaign.__dataclass_fields__
        if field not in {"generated", "test_sha256", "survivor_baseline"}
    }
    first = model.Campaign(**values)
    second = model.Campaign(**values)
    assert first.generated is False
    assert first.survivor_baseline == ()
    assert first.test_sha256 == {}
    assert first.test_sha256 is not second.test_sha256

    pins = dict(loaded.source_sha256)
    bad_pins = {"subject.py": "0" * 64}
    for sources, tests in ((pins, bad_pins), (bad_pins, pins)):
        mismatched = replace(loaded, source_sha256=sources, test_sha256=tests)
        errors = model.source_drift(mismatched, tmp_path)
        assert any(row["path"] == "subject.py" for row in errors)


def test_nonempty_native_baseline_keeps_immutable_python_shape(tmp_path, monkeypatch):
    path = generated_manifest(tmp_path)
    native_call = model._native_json_call

    def with_baseline(method, request):
        row = native_call(method, request)
        # Simulated native API metadata, never registered or executed evidence.
        return {**row, "survivor_baseline": ["existing-engine-id"]}

    monkeypatch.setattr(model, "_native_json_call", with_baseline)
    loaded = model.load_campaign(path, repo_root=tmp_path)
    assert loaded.survivor_baseline == ("existing-engine-id",)
    assert asdict(loaded)["survivor_baseline"] == ("existing-engine-id",)


def test_legacy_scope_retains_nodeid_selection_and_inventory():
    scope = model._test_scope_from_native(
        "test_subject.py",
        {
            "mode": "complete",
            "inventory": "python_ast_test_functions",
            "nodeids": ["test_subject.py::test_subject"],
        },
    )
    assert scope.path == "test_subject.py"
    assert scope.mode == "complete"
    assert scope.inventory == "python_ast_test_functions"
    assert scope.nodeids == ("test_subject.py::test_subject",)
    assert scope.selection == "nodeids"


def test_malformed_native_campaign_names_its_contract(tmp_path, monkeypatch):
    path = generated_manifest(tmp_path)
    monkeypatch.setattr(model, "_native_json_call", lambda *args: [])
    with pytest.raises(model.CampaignError, match="native mutation campaign"):
        model.load_campaign(path, repo_root=tmp_path)


def test_invalid_optional_value_analysis_is_not_silently_discarded(tmp_path):
    path = generated_manifest(tmp_path)
    payload = json.loads(path.read_text())
    payload["value_analysis"] = "not-an-object"
    path.write_text(json.dumps(payload))
    with pytest.raises(
        model.CampaignError,
        match="invalid value_analysis: value_analysis must be an object",
    ):
        model.load_campaign(path, repo_root=tmp_path)


@pytest.mark.parametrize("bad", [(), ["not-an-object"]])
def test_drift_boundary_requires_a_list_of_objects(tmp_path, monkeypatch, bad):
    loaded = model.load_campaign(generated_manifest(tmp_path), repo_root=tmp_path)
    monkeypatch.setattr(model, "_native_json_call", lambda *args: bad)
    with pytest.raises(model.CampaignError, match="list of objects"):
        model.source_drift(loaded, tmp_path)


def test_runner_component_root_resolves_this_repository_to_src():
    """This checkout is a src layout: the package sits at ``src/conductor``, so a
    bare ``conductor/...`` runner-component literal must join onto ``src/``, not
    the git root -- otherwise it hashes the wrong (or a missing) file."""
    root = model.runner_component_root()
    assert root == model._PACKAGE_DIR.parent
    assert (root / "conductor" / "mutation_campaign_model.py").resolve() == (
        model._PACKAGE_DIR / "mutation_campaign_model.py"
    ).resolve()


def test_runner_component_root_is_the_repo_root_under_a_flat_layout(tmp_path):
    """The monorepo layout this module was extracted from: package_root
    unconfigured, package sits directly at the repo root. runner_component_root
    must then equal that root exactly, so a flat-layout host's runner-component
    hashes are byte-identical to what they were before package_root existed."""
    repo = tmp_path / "repo"
    package = repo / "conductor"
    package.mkdir(parents=True)
    (repo / ".git").mkdir()
    (package / "mutation_campaign_model.py").write_text("# stand-in\n")
    monkeypatch_dir = model._PACKAGE_DIR
    try:
        model._PACKAGE_DIR = package
        assert model.runner_component_root() == repo.resolve()
    finally:
        model._PACKAGE_DIR = monkeypatch_dir


def test_lineage_accepts_reads_the_lineage_file_relative_to_package_root(tmp_path):
    """``_lineage_accepts`` must resolve ``conductor/mutation_runner_lineage.json``
    from the passed-in ``package_root``, not from the git repository root -- the
    same distinction ``runner_component_root`` exists to make under a src layout."""
    package_root = tmp_path / "src"
    (package_root / "conductor").mkdir(parents=True)
    recorded = {"conductor/mutation_testing.py": "0" * 64}
    lineage = {
        "schema_version": 1,
        "entries": [
            {
                "runner_components_sha256": recorded,
            }
        ],
    }
    (package_root / "conductor" / "mutation_runner_lineage.json").write_text(
        json.dumps(lineage)
    )
    assert model._lineage_accepts(recorded, package_root) is True
    # A repo_root sibling with no lineage file at all must not be consulted instead.
    assert model._lineage_accepts(recorded, tmp_path) is False


def test_lineage_accepts_refuses_a_map_the_file_does_not_declare(tmp_path):
    package_root = tmp_path / "src"
    (package_root / "conductor").mkdir(parents=True)
    (package_root / "conductor" / "mutation_runner_lineage.json").write_text(
        json.dumps(
            {
                "schema_version": 1,
                "entries": [{"runner_components_sha256": {"a": "1" * 64}}],
            }
        )
    )
    assert model._lineage_accepts({"a": "2" * 64}, package_root) is False
    assert model._lineage_accepts(None, package_root) is False
