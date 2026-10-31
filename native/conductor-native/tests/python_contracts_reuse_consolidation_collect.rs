#![cfg(feature = "python-compat-tests")]
//! Rust-owned independent AST-normalization and function-collection contracts.

#[path = "python_contracts/reuse_ast_support.rs"]
#[allow(dead_code)]
mod ast_ref;
#[path = "python_contracts/reuse_collect_support.rs"]
mod collect_ref;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use ast_ref::{is_any, parse, walk};
use collect_ref::{collect_production, payload, reference_collect, reference_normalize};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use support::{module, Case};

const NORMALIZER_SOURCE: &str = r#"def alpha(item: "Input", /, scale=2, *rest, enabled=True, **options) -> "Output":
    """outer documentation is not semantic"""
    import package.module as pm
    local = pm.convert(item, scale)
    try:
        result = global_call(local, literal=3)
    except Exception as problem:
        recovered = problem
        result = recovered
    def inner(value):
        """nested documentation is not semantic"""
        return value + local
    class Box:
        """class documentation is not semantic"""
        pass
    transform = lambda lambda_value: lambda_value + local
    return inner(result), Box, transform, rest, enabled, options

def beta(value, factor=2):
    renamed = value + factor
    return renamed
"#;

const ORDER_SOURCE: &str = "def outer(value):\n    first = value + 1\n    def nested(item):\n        return item * 2\n    second = nested(first)\n    return second\n\nasync def later(value):\n    first = value + 1\n    second = first + 1\n    third = second + 1\n    fourth = third + 1\n    return fourth\n";

const ENCODED: &[u8] = b"def encoded(value):\r\n    text = 'caf\xc3\xa9'\r\n    invalid = '\xff'\r\n    combined = text + invalid\r\n    result = combined + str(value)\r\n    return result\r\n";

#[test]
fn native_normalizer_matches_cpython_reference() {
    let _case = Case::new();
    Python::attach(|py| {
        let tree = parse(py, NORMALIZER_SOURCE, "<unknown>").unwrap();
        let consolidation = module(py, "conductor.reuse.consolidation");
        let mut functions = 0;
        for node in walk(&tree) {
            if !is_any(&node, &["FunctionDef", "AsyncFunctionDef"]) {
                continue;
            }
            functions += 1;
            let expected = reference_normalize(py, &node);
            let actual = consolidation
                .getattr("_normalize_hash")
                .unwrap()
                .call1((&node,))
                .unwrap();
            assert!(actual.eq(expected.into_pyobject(py).unwrap()).unwrap());
        }
        assert_eq!(functions, 3); // alpha, beta, and nested inner
    });
}

#[test]
fn native_collect_preserves_boundary_async_and_breadth_first_order() {
    let case = Case::new();
    let order = case.root().join("order.py");
    fs::write(&order, ORDER_SOURCE).unwrap();
    Python::attach(|py| {
        let expected = reference_collect(py, &[order.as_path()], case.root(), 6);
        let actual = collect_production(py, &[order.as_path()], case.root(), 6);
        assert!(payload(py, &actual).eq(payload(py, &expected)).unwrap());
        let names =
            PyList::new(py, actual.0.iter().map(|row| row.getattr("name").unwrap())).unwrap();
        assert!(names
            .eq(PyList::new(py, ["outer", "later"]).unwrap())
            .unwrap());
    });
}

#[test]
fn native_collect_preserves_text_decoding_and_failure_accounting() {
    let case = Case::new();
    let valid = case.root().join("encoded.py");
    let broken = case.root().join("broken.py");
    let missing = case.root().join("missing.py");
    fs::write(&valid, ENCODED).unwrap();
    fs::write(&broken, "def broken(:\n").unwrap();
    let paths = [valid.as_path(), broken.as_path(), missing.as_path()];
    Python::attach(|py| {
        let expected = reference_collect(py, &paths, case.root(), 6);
        let actual = collect_production(py, &paths, case.root(), 6);
        assert!(payload(py, &actual).eq(payload(py, &expected)).unwrap());
        assert_eq!(actual.1, 2);
        let source: String = actual
            .0
            .get_item(0)
            .unwrap()
            .getattr("source")
            .unwrap()
            .extract()
            .unwrap();
        assert!(source.contains('é'));
        assert!(source.contains('\u{fffd}'));
        assert!(!source.contains('\r'));
    });
}

fn json_value(value: &Bound<'_, PyAny>) -> Value {
    let encoded: String = module(value.py(), "json")
        .call_method1("dumps", (value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&encoded).unwrap()
}

fn collect_profiles<'py>(
    py: Python<'py>,
    case: &Case,
    sources: &[(&str, &str)],
) -> Bound<'py, PyTuple> {
    let paths: Vec<String> = sources
        .iter()
        .map(|(name, source)| case.write(name, source).to_string_lossy().into_owned())
        .collect();
    module(py, "slop_core")
        .call_method1(
            "audit_consolidation_collect_profiles",
            (paths, case.root().to_string_lossy().as_ref(), 4),
        )
        .unwrap()
        .cast_into::<PyTuple>()
        .unwrap()
}

fn abstraction_pairs<'py>(
    py: Python<'py>,
    profiles: &Bound<'py, PyAny>,
    exhaustive: bool,
    max_pairs: usize,
    max_bucket: usize,
) -> Bound<'py, PyAny> {
    module(py, "slop_core")
        .call_method1(
            "audit_abstraction_pairs",
            (profiles, 0.70, 8, max_pairs, max_bucket, exhaustive),
        )
        .unwrap()
}

#[test]
fn structural_profiles_preserve_clones_and_expose_import_global_and_decorator_context() {
    let case = Case::new();
    let source = "import source_a as ops\nLIMIT = 2\n@cached\ndef run(value, scale=2):\n    current = ops.transform(value)\n    shifted = current + LIMIT\n    return finish(shifted, scale)\n";
    let changed = source
        .replace("source_a", "source_b")
        .replace("LIMIT = 2", "LIMIT = 3")
        .replace("@cached", "@traced");
    Python::attach(|py| {
        let result = collect_profiles(py, &case, &[("a.py", source), ("b.py", &changed)]);
        let records = result.get_item(0).unwrap();
        let profiles = result.get_item(1).unwrap();
        let rows = json_value(&profiles);
        assert_eq!(rows[0]["node_hash"], rows[1]["node_hash"]);
        assert_ne!(
            rows[0]["semantic"]["global_bindings"],
            rows[1]["semantic"]["global_bindings"]
        );
        assert_ne!(
            rows[0]["semantic"]["decorators"],
            rows[1]["semantic"]["decorators"]
        );
        let paths = vec![
            case.root().join("a.py").to_string_lossy().into_owned(),
            case.root().join("b.py").to_string_lossy().into_owned(),
        ];
        let legacy = module(py, "slop_core")
            .call_method1(
                "audit_consolidation_collect",
                (paths, case.root().to_string_lossy().as_ref(), 4),
            )
            .unwrap();
        assert_eq!(
            json_value(&records),
            json_value(&legacy.get_item(0).unwrap())
        );
        let report = json_value(&abstraction_pairs(py, &profiles, true, 100, 80));
        assert_eq!(report["edges"].as_array().unwrap().len(), 1);
        let differences = report["edges"][0]["differences"].as_array().unwrap();
        assert!(differences.contains(&Value::String("global_bindings".to_owned())));
        assert!(differences.contains(&Value::String("decorators".to_owned())));
        assert!(!differences.contains(&Value::String("normalized_ast".to_owned())));
        assert_eq!(report["edges"][0]["review_only"], true);
    });
}

#[test]
fn indexed_abstractions_retrieve_varied_shapes_and_make_every_limit_visible() {
    let case = Case::new();
    let source = "def run(items, scale=2):\n    result = []\n    for value in items:\n        adjusted = transform(value) * scale\n        if adjusted > 1:\n            result.append(adjusted)\n    return result\n";
    let other = source
        .replace("transform(value)", "convert(value)")
        .replace("scale=2", "scale=3")
        .replace("> 1", "> 4");
    let shape = source.replace("result.append(adjusted)", "result.append(adjusted + 1)");
    let data = "def constants():\n    first = 'javascript'\n    second = 'stylesheet'\n    return first, second\n";
    Python::attach(|py| {
        let result = collect_profiles(
            py,
            &case,
            &[
                ("a.py", source),
                ("b.py", &other),
                ("c.py", &shape),
                ("data.py", data),
            ],
        );
        let profiles = result.get_item(1).unwrap();
        let exhaustive = json_value(&abstraction_pairs(py, &profiles, true, 100, 80));
        let indexed = json_value(&abstraction_pairs(py, &profiles, false, 100, 80));
        let pairs = |report: &Value| {
            report["edges"]
                .as_array()
                .unwrap()
                .iter()
                .map(|edge| {
                    (
                        edge["left"].as_u64().unwrap(),
                        edge["right"].as_u64().unwrap(),
                    )
                })
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(pairs(&indexed), pairs(&exhaustive));
        assert_eq!(pairs(&exhaustive).len(), 3);
        let differences: BTreeSet<&str> = exhaustive["edges"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|edge| edge["differences"].as_array().unwrap())
            .filter_map(Value::as_str)
            .collect();
        assert!(
            differences.contains("defaults")
                && differences.contains("callees")
                && differences.contains("literals")
        );
        assert!(exhaustive["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|edge| edge["evidence_kind"] == "structural"));
        assert_eq!(exhaustive["coverage"]["eligible_profiles"], 3);
        assert_eq!(exhaustive["coverage"]["complete"], true);
        assert_eq!(indexed["coverage"]["complete"], false);
        assert_eq!(indexed["coverage"]["recall"], "unknown");
        let capped = json_value(&abstraction_pairs(py, &profiles, true, 1, 80));
        assert_eq!(capped["coverage"]["pair_limit_hit"], true);
        assert_eq!(capped["coverage"]["complete"], false);
        let buckets = json_value(&abstraction_pairs(py, &profiles, false, 100, 2));
        assert!(buckets["coverage"]["oversized_buckets"].as_u64().unwrap() > 0);
        assert!(buckets["coverage"]["refined_buckets"].as_u64().unwrap() > 0);
        assert_eq!(
            buckets["coverage"]["candidate_generation_complete"],
            buckets["coverage"]["unresolved_oversized_buckets"] == 0
        );
        let kwargs = PyDict::new(py);
        kwargs.set_item("max_pairs", 0).unwrap();
        assert!(module(py, "slop_core")
            .getattr("audit_abstraction_pairs")
            .unwrap()
            .call((&profiles,), Some(&kwargs))
            .is_err());
    });
}

#[test]
fn indexed_structural_retrieval_does_not_require_shared_callees_or_identical_shape() {
    let case = Case::new();
    let a = "def compute(value):\n    first = value + 1\n    second = first * 2\n    third = second - 3\n    return third\n";
    let b = "def compute(value):\n    first = value + 4\n    second = first * 5\n    third = second - 6\n    normalized = third / 7\n    return normalized\n";
    Python::attach(|py| {
        let result = collect_profiles(py, &case, &[("a.py", a), ("b.py", b)]);
        let profiles = result.get_item(1).unwrap();
        let exhaustive = json_value(&abstraction_pairs(py, &profiles, true, 100, 80));
        let indexed = json_value(&abstraction_pairs(py, &profiles, false, 100, 80));
        assert_eq!(exhaustive["edges"].as_array().unwrap().len(), 1);
        assert_eq!(indexed["edges"], exhaustive["edges"]);
        assert_eq!(indexed["edges"][0]["evidence_kind"], "structural");
        assert_eq!(json_value(&profiles)[0]["calls"], serde_json::json!([]));
    });
}
