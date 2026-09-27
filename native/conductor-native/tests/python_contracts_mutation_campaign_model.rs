#![cfg(feature = "python-compat-tests")]
//! Campaign model conversion, drift, and runner lineage contracts.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyModule, PyTuple};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use support::{assert_error, module, path, text, AttrPatch, Case};

fn py_json<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn json_value(value: &Bound<'_, PyAny>) -> Value {
    let encoded: String = value
        .py()
        .import("json")
        .unwrap()
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&encoded).unwrap()
}

fn generated_manifest(py: Python<'_>, case: &Case) -> PathBuf {
    case.write("subject.py", "def subject():\n    return 1\n");
    case.write("test_subject.py", "def test_subject():\n    assert True\n");
    let kwargs = PyDict::new(py);
    kwargs.set_item("repo_root", path(py, case.root())).unwrap();
    kwargs.set_item("day", "20260910").unwrap();
    kwargs.set_item("run_timeout_seconds", 30).unwrap();
    let planner = module(py, "conductor.mutation_campaign_generate");
    let result = planner
        .getattr("plan")
        .unwrap()
        .call(("python",), Some(&kwargs))
        .unwrap();
    let mut manifest = json_value(&result)["manifests"][0].clone();
    manifest["campaign_id"] = json!("fixture");
    let saved = case.root().join("campaign.json");
    fs::write(&saved, manifest.to_string()).unwrap();
    saved
}

fn load<'py>(
    py: Python<'py>,
    model: &Bound<'py, PyModule>,
    manifest: &Path,
    root: &Path,
) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("repo_root", path(py, root)).unwrap();
    model
        .getattr("load_campaign")
        .unwrap()
        .call((path(py, manifest),), Some(&kwargs))
}

fn drift<'py>(
    py: Python<'py>,
    model: &Bound<'py, PyModule>,
    campaign: &Bound<'py, PyAny>,
    root: &Path,
) -> PyResult<Bound<'py, PyAny>> {
    model
        .getattr("source_drift")
        .unwrap()
        .call1((campaign, path(py, root)))
}

fn assert_defaults_and_mismatched_pins(
    py: Python<'_>,
    model: &Bound<'_, PyModule>,
    loaded: &Bound<'_, PyAny>,
    root: &Path,
) {
    let values = PyDict::new(py);
    let fields = model
        .getattr("Campaign")
        .unwrap()
        .getattr("__dataclass_fields__")
        .unwrap();
    for field in fields.call_method0("keys").unwrap().try_iter().unwrap() {
        let name: String = field.unwrap().extract().unwrap();
        if !["generated", "test_sha256", "survivor_baseline"].contains(&name.as_str()) {
            values
                .set_item(&name, loaded.getattr(name.as_str()).unwrap())
                .unwrap();
        }
    }
    let class = model.getattr("Campaign").unwrap();
    let first = class.call((), Some(&values)).unwrap();
    let second = class.call((), Some(&values)).unwrap();
    assert!(!first
        .getattr("generated")
        .unwrap()
        .extract::<bool>()
        .unwrap());
    assert!(first
        .getattr("survivor_baseline")
        .unwrap()
        .eq(PyTuple::empty(py))
        .unwrap());
    assert!(first
        .getattr("test_sha256")
        .unwrap()
        .eq(PyDict::new(py))
        .unwrap());
    assert!(!first
        .getattr("test_sha256")
        .unwrap()
        .is(second.getattr("test_sha256").unwrap()));

    let source_pins = loaded.getattr("source_sha256").unwrap();
    let bad_pins = py_json(py, &json!({"subject.py":"0".repeat(64)}));
    let dataclasses = module(py, "dataclasses");
    for (sources, tests) in [(&source_pins, &bad_pins), (&bad_pins, &source_pins)] {
        let replacement = PyDict::new(py);
        replacement.set_item("source_sha256", sources).unwrap();
        replacement.set_item("test_sha256", tests).unwrap();
        let mismatched = dataclasses
            .getattr("replace")
            .unwrap()
            .call((loaded,), Some(&replacement))
            .unwrap();
        let errors = json_value(&drift(py, model, &mismatched, root).unwrap());
        assert!(
            errors
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["path"] == "subject.py"),
            "mismatched source/test pins must report subject.py: {errors}"
        );
    }
}

#[test]
fn generated_native_roundtrip_keeps_identity_test_pins_defaults_and_drift() {
    let case = Case::new();
    Python::attach(|py| {
        let model = module(py, "conductor.mutation_campaign_model");
        let testing = module(py, "conductor.mutation_testing");
        let manifest = generated_manifest(py, &case);
        let loaded = load(py, &model, &manifest, case.root()).unwrap();
        let request = json!({"repo_root":case.root().to_str().unwrap(),
            "manifest_path":"campaign.json"});
        let native = model
            .getattr("_native_json_call")
            .unwrap()
            .call1(("load_mutation_campaign_native", py_json(py, &request)))
            .unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("repo_root", path(py, case.root())).unwrap();
        let restored = testing
            .getattr("_native_campaign_contract")
            .unwrap()
            .call((&loaded,), Some(&kwargs))
            .unwrap();
        assert!(loaded
            .getattr("generated")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(loaded
            .getattr("survivor_baseline")
            .unwrap()
            .eq(PyTuple::empty(py))
            .unwrap());
        for field in [
            "test_argv",
            "blocked_process_substrings",
            "host_read_dependencies",
        ] {
            assert!(
                loaded
                    .getattr(field)
                    .unwrap()
                    .is_instance(&py.get_type::<PyTuple>())
                    .unwrap(),
                "{field} must remain a tuple"
            );
        }
        let pins = json_value(&loaded.getattr("test_sha256").unwrap());
        assert_eq!(pins, json_value(&native.get_item("test_sha256").unwrap()));
        assert_eq!(
            pins.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["test_subject.py"]
        );
        for field in ["generated", "test_sha256"] {
            assert_eq!(
                json_value(&restored.get_item(field).unwrap()),
                json_value(&native.get_item(field).unwrap())
            );
        }
        assert_eq!(
            json_value(&restored.get_item("survivor_baseline").unwrap()),
            json_value(&native.get_item("survivor_baseline").unwrap())
        );
        assert!(!restored
            .get_item("source_drifted")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        case.write("test_subject.py", "def test_changed():\n    pass\n");
        let after = testing
            .getattr("_native_campaign_contract")
            .unwrap()
            .call((&loaded,), Some(&kwargs))
            .unwrap();
        assert!(after
            .get_item("source_drifted")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert_defaults_and_mismatched_pins(py, &model, &loaded, case.root());
    });
}

#[test]
fn nonempty_native_baseline_keeps_immutable_python_shape() {
    let case = Case::new();
    Python::attach(|py| {
        let model = module(py, "conductor.mutation_campaign_model");
        let manifest = generated_manifest(py, &case);
        let original = model.getattr("_native_json_call").unwrap().unbind();
        let with_baseline = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>,
                  kwargs: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                let result = original.bind(args.py()).call(args, kwargs)?;
                result.set_item("survivor_baseline", vec!["existing-engine-id"])?;
                Ok(result.unbind())
            },
        )
        .unwrap();
        let _patch =
            AttrPatch::replace(model.as_any(), "_native_json_call", with_baseline.as_any());
        let loaded = load(py, &model, &manifest, case.root()).unwrap();
        assert_eq!(
            loaded
                .getattr("survivor_baseline")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            vec!["existing-engine-id"]
        );
        assert!(loaded
            .getattr("survivor_baseline")
            .unwrap()
            .is_instance(&py.get_type::<PyTuple>())
            .unwrap());
        let fields = module(py, "dataclasses")
            .getattr("asdict")
            .unwrap()
            .call1((&loaded,))
            .unwrap();
        let baseline = fields.get_item("survivor_baseline").unwrap();
        assert_eq!(
            baseline.extract::<Vec<String>>().unwrap(),
            vec!["existing-engine-id"]
        );
        assert!(baseline.is_instance(&py.get_type::<PyTuple>()).unwrap());
    });
}

#[test]
fn legacy_scope_retains_nodeid_selection_and_inventory() {
    let _case = Case::new();
    Python::attach(|py| {
        let model = module(py, "conductor.mutation_campaign_model");
        let row = py_json(
            py,
            &json!({"mode":"complete",
            "inventory":"python_ast_test_functions",
            "nodeids":["test_subject.py::test_subject"]}),
        );
        let scope = model
            .getattr("_test_scope_from_native")
            .unwrap()
            .call1(("test_subject.py", row))
            .unwrap();
        for (field, expected) in [
            ("path", "test_subject.py"),
            ("mode", "complete"),
            ("inventory", "python_ast_test_functions"),
            ("selection", "nodeids"),
        ] {
            assert_eq!(text(&scope.getattr(field).unwrap()), expected);
        }
        assert_eq!(
            scope
                .getattr("nodeids")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            vec!["test_subject.py::test_subject"]
        );
        assert!(scope
            .getattr("nodeids")
            .unwrap()
            .is_instance(&py.get_type::<PyTuple>())
            .unwrap());
    });
}

#[test]
fn malformed_native_campaign_names_its_contract() {
    let case = Case::new();
    Python::attach(|py| {
        let model = module(py, "conductor.mutation_campaign_model");
        let manifest = generated_manifest(py, &case);
        let malformed = PyCFunction::new_closure(
            py,
            None,
            None,
            |args: &Bound<'_, PyTuple>,
             _kwargs: Option<&Bound<'_, PyDict>>|
             -> PyResult<Py<PyAny>> {
                Ok(PyList::empty(args.py()).into_any().unbind())
            },
        )
        .unwrap();
        let _patch = AttrPatch::replace(model.as_any(), "_native_json_call", malformed.as_any());
        let error =
            load(py, &model, &manifest, case.root()).expect_err("non-object native campaign");
        assert_error(
            py,
            error,
            &model.getattr("CampaignError").unwrap(),
            "native mutation campaign",
        );
    });
}

#[test]
fn invalid_optional_value_analysis_is_not_discarded() {
    let case = Case::new();
    Python::attach(|py| {
        let model = module(py, "conductor.mutation_campaign_model");
        let manifest = generated_manifest(py, &case);
        let mut payload: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        payload["value_analysis"] = json!("not-an-object");
        fs::write(&manifest, payload.to_string()).unwrap();
        let error = load(py, &model, &manifest, case.root()).expect_err("invalid value analysis");
        assert_error(
            py,
            error,
            &model.getattr("CampaignError").unwrap(),
            "invalid value_analysis: value_analysis must be an object",
        );
    });
}

#[test]
fn drift_boundary_requires_a_list_of_objects_for_both_bad_shapes() {
    let case = Case::new();
    Python::attach(|py| {
        let model = module(py, "conductor.mutation_campaign_model");
        let loaded = load(py, &model, &generated_manifest(py, &case), case.root()).unwrap();
        for bad in [json!({"tuple":[]}), json!(["not-an-object"])] {
            // A tuple is built natively so the row exercises the original tuple case.
            let malformed = match bad {
                Value::Object(_) => PyTuple::empty(py).into_any(),
                _ => py_json(py, &bad),
            }
            .unbind();
            let callback = PyCFunction::new_closure(
                py,
                None,
                None,
                move |_args: &Bound<'_, PyTuple>,
                      _kwargs: Option<&Bound<'_, PyDict>>|
                      -> PyResult<Py<PyAny>> {
                    Ok(malformed.clone_ref(_args.py()))
                },
            )
            .unwrap();
            let _patch = AttrPatch::replace(model.as_any(), "_native_json_call", callback.as_any());
            let error =
                drift(py, &model, &loaded, case.root()).expect_err("bad native drift shape");
            assert_error(
                py,
                error,
                &model.getattr("CampaignError").unwrap(),
                "list of objects",
            );
        }
    });
}

#[test]
fn runner_component_root_resolves_this_src_layout() {
    let _case = Case::new();
    Python::attach(|py| {
        let model = module(py, "conductor.mutation_campaign_model");
        let root: PathBuf = model
            .getattr("runner_component_root")
            .unwrap()
            .call0()
            .unwrap()
            .extract()
            .unwrap();
        let package: PathBuf = model.getattr("_PACKAGE_DIR").unwrap().extract().unwrap();
        assert_eq!(root, package.parent().unwrap());
        assert_eq!(
            root.join("conductor/mutation_campaign_model.py")
                .canonicalize()
                .unwrap(),
            package
                .join("mutation_campaign_model.py")
                .canonicalize()
                .unwrap()
        );
    });
}

#[test]
fn runner_component_root_is_repo_root_in_flat_layout() {
    let case = Case::new();
    let package = case.mkdir("repo/conductor");
    case.mkdir("repo/.git");
    case.write("repo/conductor/mutation_campaign_model.py", "# stand-in\n");
    Python::attach(|py| {
        let model = module(py, "conductor.mutation_campaign_model");
        let package_path = path(py, &package);
        let _patch = AttrPatch::replace(model.as_any(), "_PACKAGE_DIR", &package_path);
        let root: PathBuf = model
            .getattr("runner_component_root")
            .unwrap()
            .call0()
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(root, case.root().join("repo").canonicalize().unwrap());
    });
}

#[test]
fn lineage_reads_record_relative_to_package_root_only() {
    let case = Case::new();
    let package_root = case.mkdir("src/conductor").parent().unwrap().to_path_buf();
    let recorded = json!({"conductor/mutation_testing.py":"0".repeat(64)});
    let lineage = json!({"schema_version":1,
        "entries":[{"runner_components_sha256":recorded}]});
    case.write(
        "src/conductor/mutation_runner_lineage.json",
        &lineage.to_string(),
    );
    Python::attach(|py| {
        let model = module(py, "conductor.mutation_campaign_model");
        let accepts = model.getattr("_lineage_accepts").unwrap();
        assert!(accepts
            .call1((py_json(py, &recorded), path(py, &package_root)))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(!accepts
            .call1((py_json(py, &recorded), path(py, case.root())))
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

#[test]
fn lineage_refuses_undeclared_map_and_none() {
    let case = Case::new();
    let package_root = case.mkdir("src/conductor").parent().unwrap().to_path_buf();
    let lineage = json!({"schema_version":1,
        "entries":[{"runner_components_sha256":{"a":"1".repeat(64)}}]});
    case.write(
        "src/conductor/mutation_runner_lineage.json",
        &lineage.to_string(),
    );
    Python::attach(|py| {
        let model = module(py, "conductor.mutation_campaign_model");
        let accepts = model.getattr("_lineage_accepts").unwrap();
        assert!(!accepts
            .call1((
                py_json(py, &json!({"a":"2".repeat(64)})),
                path(py, &package_root)
            ))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(!accepts
            .call1((py.None(), path(py, &package_root)))
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}
