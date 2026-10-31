#![cfg(feature = "python-compat-tests")]
//! Rust-owned independent profile feature reference and batch contracts.

#[path = "python_contracts/reuse_ast_support.rs"]
#[allow(dead_code)]
mod ast_ref;
#[path = "python_contracts/reuse_profile_support.rs"]
mod profile_ref;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use profile_ref::reference_profile;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use serde_json::Value;
use std::fs;
use std::path::Path;
use support::{module, path, Case};

#[path = "python_contracts/source_dirs_support.rs"]
mod source_dirs_support;

#[test]
fn file_families_scan_the_pyproject_source_dirs_without_targets() {
    let case = Case::new();
    let root = source_dirs_support::host(&case);
    Python::attach(|py| {
        let seen = source_dirs_support::targets_main_scans(
            py,
            "conductor.reuse.file_families",
            "scan_file_families",
            &root,
        );
        assert_eq!(seen, ["pkg", "lib/pkg"]);
    });
}

const SOURCES: [&str; 3] = [
    r#"from alpha.beta import item
import gamma.delta as gd

class Lane:
    @decorator(flag=True, mode="fast")
    async def run(self, value: int = 1, *, scale=2, **options) -> None:
        payload = {"alpha": value, "beta": scale, "gamma": options}
        self.state = factory(value, scale=scale, mode="fast")
        async with manager() as handle:
            if payload:
                return None

        def nested(local):
            return local + 1

    def build(self) -> Product:
        from factory import Product

        return Product()
"#,
    r#"def classify(value):
    match value:
        case {"kind": kind}:
            return kind
        case _:
            return "unknown"

try:
    RESULT = classify({"alpha": 1, "beta": True, "gamma": None})
except (TypeError, ValueError) as error:
    RESULT = str(error)
"#,
    "class UnicodeLane:\n    def résumé(self, entrée=\"café\"):\n        \"\"\"A docstring excluded from the normalized method hash.\"\"\"\n        sortie = entrée.strip()\n        return sortie\n",
];

fn production_profile<'py>(
    py: Python<'py>,
    source: &Path,
    repo: &Path,
) -> Bound<'py, pyo3::types::PyAny> {
    module(py, "conductor.reuse.file_families")
        .getattr("profile_file")
        .unwrap()
        .call1((path(py, source), path(py, repo)))
        .unwrap()
}

#[test]
fn native_profiles_match_cpython_reference_for_all_feature_fields() {
    let case = Case::new();
    Python::attach(|py| {
        for (index, source) in SOURCES.iter().enumerate() {
            let file = case.root().join(format!("case_{index}.py"));
            fs::write(&file, source).unwrap();
            let expected = reference_profile(py, &file, case.root()).unwrap();
            let actual = production_profile(py, &file, case.root());
            assert!(actual.eq(expected).unwrap(), "source index {index}");
        }
    });
}

#[test]
fn native_batch_preserves_order_and_counts_syntax_errors() {
    let case = Case::new();
    let first = case.root().join("first.py");
    let broken = case.root().join("broken.py");
    let second = case.root().join("second.py");
    fs::write(&first, SOURCES[0]).unwrap();
    fs::write(&broken, "def broken(:\n").unwrap();
    fs::write(&second, SOURCES[1]).unwrap();
    Python::attach(|py| {
        let core = module(py, "conductor.reuse").getattr("core").unwrap();
        let options = PyDict::new(py);
        options
            .set_item(
                "paths",
                [first.as_path(), broken.as_path(), second.as_path()]
                    .iter()
                    .map(|value| value.to_string_lossy().to_string())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        options
            .set_item("repo", case.root().to_string_lossy().to_string())
            .unwrap();
        let result = core
            .getattr("audit_file_family_profiles")
            .unwrap()
            .call((), Some(&options))
            .unwrap();
        let rows = result.get_item(0).unwrap().cast_into::<PyList>().unwrap();
        let names = PyList::new(py, rows.iter().map(|row| row.get_item("file").unwrap())).unwrap();
        assert!(names
            .eq(PyList::new(py, ["first.py", "second.py"]).unwrap())
            .unwrap());
        assert!(result.get_item(1).unwrap().eq(1).unwrap());
    });
}

#[test]
fn native_profiles_preserve_invalid_utf8_replacement_semantics() {
    let case = Case::new();
    let file = case.root().join("replacement.py");
    fs::write(&file, b"VALUE = 'ok'\n# invalid: \xff\n").unwrap();
    Python::attach(|py| {
        let expected = reference_profile(py, &file, case.root()).unwrap();
        // The independent feature reference predates the literal-data marker.
        // This fixture has one literal assignment; decoding its comment must
        // preserve that classification as well as every pre-existing feature.
        let flags = PyDict::new(py);
        flags.set_item("literal_data_only", true).unwrap();
        let expected = module(py, "dataclasses")
            .getattr("replace")
            .unwrap()
            .call((&expected,), Some(&flags))
            .unwrap();
        let actual = production_profile(py, &file, case.root());
        assert!(actual
            .getattr("literal_data_only")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(actual.eq(expected).unwrap());
    });
}

fn registrations(sources: &[&str]) -> Vec<Value> {
    let case = Case::new();
    let paths: Vec<String> = sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let path = case.root().join(format!("shim_{index}.py"));
            fs::write(&path, source).unwrap();
            path.to_string_lossy().into_owned()
        })
        .collect();
    Python::attach(|py| {
        let result = PyList::empty(py);
        let kwargs = PyDict::new(py);
        kwargs.set_item("paths", paths).unwrap();
        kwargs
            .set_item("repo", case.root().to_string_lossy().as_ref())
            .unwrap();
        kwargs.set_item("registration_candidates", &result).unwrap();
        let profile_result = module(py, "conductor.reuse")
            .getattr("core")
            .unwrap()
            .getattr("audit_file_family_profiles")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        assert_eq!(
            profile_result.get_item(0).unwrap().len().unwrap(),
            sources.len()
        );
        assert!(profile_result.get_item(1).unwrap().eq(0).unwrap());
        let encoded: String = module(py, "json")
            .call_method1("dumps", (result,))
            .unwrap()
            .extract()
            .unwrap();
        serde_json::from_str(&encoded).unwrap()
    })
}

#[test]
fn tiny_registrations_keep_import_aliases_arguments_and_deferred_boundaries() {
    let rows = registrations(&[
        "\"\"\"Tiny registration.\"\"\"\nfrom missing.factories import make_handler as factory\nimport torch as t\nComponentHandler = factory(t.abs, native_op_name='abs')\n",
        "import missing.factories as f\nComponentHandler = f.make_handler(lambda x: x + 1, mode=-2)\n",
        "import missing.factories\nComponentHandler = missing.factories.make_handler()\n",
        "from missing.factories import make_handler\nComponentHandler = make_handler(lambda x, scale=2: x * scale)\n",
    ]);
    assert_eq!(rows.len(), 4);
    for row in &rows {
        assert_eq!(row["factory"], "missing.factories.make_handler");
        assert_eq!(row["name"], "ComponentHandler");
        assert_eq!(row["callable_return"], "unknown");
        assert_eq!(row["side_effects"], "unknown");
        assert_eq!(
            row["binding_resolution"],
            "unambiguous_absolute_import_syntax"
        );
    }
    assert_eq!(rows[0]["line_start"], 4);
    assert_eq!(
        rows[0]["import_bindings"]["factory"],
        "missing.factories.make_handler"
    );
    assert_eq!(rows[0]["import_bindings"]["t"], "torch");
    assert_eq!(rows[0]["has_deferred_implementation"], false);
    assert_eq!(rows[0]["keywords"][0]["name"], "native_op_name");
    assert!(rows[0]["arguments"][0]
        .as_str()
        .unwrap()
        .contains("attr='abs'"));
    assert_eq!(rows[1]["has_deferred_implementation"], true);
    assert!(rows[1]["arguments"][0]
        .as_str()
        .unwrap()
        .contains("Lambda("));
    assert_eq!(rows[2]["arguments"], serde_json::json!([]));
    assert_eq!(rows[3]["has_deferred_implementation"], true);
}

#[test]
fn registration_candidates_exclude_data_extra_execution_and_ambiguous_bindings() {
    let sources = [
        "HTML = '<script>function run() {}</script>'\n",
        "from factory import make_handler\nDATA = {'name': 'run'}\n",
        "from factory import make_handler\nComponentHandler = make_handler()\nprint('extra execution')\n",
        "from factory import make_handler\nComponentHandler = make_handler(load())\n",
        "from factory import make_handler\nComponentHandler = make_handler(lambda x=load(): x)\n",
        "from factory import make_handler\nComponentHandler = make_handler(**{})\n",
        "from factory import make_handler\nComponentHandler = make_handler(*[])\n",
        "from factory import make_handler\nComponentHandler = make_handler\n",
        "from factory import make_handler\nComponentHandler = make_handler()\nComponentHandler = make_handler()\n",
        "from factory import make_handler\nfrom other import make_handler\nComponentHandler = make_handler()\n",
        "from factory import *\nComponentHandler = make_handler()\n",
        "from .factory import make_handler\nComponentHandler = make_handler()\n",
        "from factory import make_handler\nobj.handler = make_handler()\n",
        "from factory import make_handler\nA = B = make_handler()\n",
        "from factory import make_handler\nmake_handler = make_handler()\n",
        "from factory import make_handler\nComponentHandler = make_handler(config)\n",
        "from factory import make_handler\nComponentHandler = make_handler()\nimport later\n",
        "from factory import load_config\nCONFIG = load_config('config.json')\n",
    ];
    assert!(registrations(&sources).is_empty());
}
