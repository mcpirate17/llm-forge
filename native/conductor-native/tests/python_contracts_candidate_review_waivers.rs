#![cfg(feature = "python-compat-tests")]
//! Rust-owned mutation-waiver policy and runtime contracts.

#[path = "python_contracts/candidate_review_support.rs"]
#[allow(dead_code)]
mod candidate_review_support;
#[path = "python_contracts/git_fixture_support.rs"]
#[allow(dead_code)]
mod git_fixture_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use candidate_review_support::{
    added_change, gate_context, isolated_case, minimal_policy_text, replace_fields, utc_date,
};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use support::{assert_error, module, path, AttrPatch, Case};

const WAIVED: &str = "conductor/test_waived_probe.py";
const OTHER: &str = "research/tests/test_other_probe.py";
const LEGACY: &str = "research/tests/test_legacy_probe.py";

fn policy_text(py: Python<'_>, entries: &[String], days: i32) -> String {
    let base = minimal_policy_text(&utc_date(py, days), "exceptions = []");
    base.replacen(
        "\n[classes]",
        &format!("\n{}\n\n[classes]", entries.join("\n\n")),
        1,
    )
}

fn policy_constants(py: Python<'_>) -> (String, String, String, String) {
    let policy = module(py, "conductor.candidate_review.policy");
    let get = |name: &str| policy.getattr(name).unwrap().extract::<String>().unwrap();
    (
        get("W7_TRIDENT_LINEAR_INTEGRATION_MILESTONE"),
        get("MUTATION_WAIVER_INTEGRATION_BASE"),
        get("MUTATION_WAIVER_SOURCE_ANCHOR"),
        get("MUTATION_WAIVER_BINDING_CLAUSE"),
    )
}

fn base_entry(py: Python<'_>) -> Map<String, Value> {
    let (milestone, base, anchor, clause) = policy_constants(py);
    let anchored = format!("{:x}", Sha256::digest(b"anchored bytes\n"));
    let source = format!("{:x}", Sha256::digest(b"pinned source\n"));
    let mut map = Map::new();
    map.insert(
        "sources".into(),
        json!([{"path":"research/tools/dep.py","sha256":source}]),
    );
    map.insert("id".into(), json!("waiver-probe"));
    map.insert("path".into(), json!(LEGACY));
    map.insert("owner".into(), json!("tim"));
    map.insert(
        "justification".into(),
        json!("bounded legacy lane stabilization before w7 linear integration lands"),
    );
    map.insert("expires".into(), json!(utc_date(py, 7)));
    map.insert("milestone".into(), json!(milestone));
    map.insert("integration_base".into(), json!(base));
    map.insert("source_anchor".into(), json!(anchor));
    map.insert("sha256".into(), json!(anchored));
    map.insert("binding_clause".into(), json!(clause));
    map
}

fn source_toml(value: &Value) -> String {
    if let Some(rows) = value.as_array() {
        let rendered: Vec<String> = rows
            .iter()
            .map(|row| {
                let fields: Vec<String> = row
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(key, value)| format!("{key} = {value}"))
                    .collect();
                format!("{{ {} }}", fields.join(", "))
            })
            .collect();
        format!("[{}]", rendered.join(", "))
    } else {
        value.to_string()
    }
}

fn waiver_table(entry: &Map<String, Value>) -> String {
    let mut lines = vec!["[[mutation_waivers]]".to_owned()];
    for (key, value) in entry {
        if value.is_null() {
            continue;
        }
        let rendered = match key.as_str() {
            "expires" => value.as_str().unwrap().to_owned(),
            "sources" => source_toml(value),
            _ => value.to_string(),
        };
        lines.push(format!("{key} = {rendered}"));
    }
    lines.join("\n")
}

fn load_waiver_policy<'py>(
    py: Python<'py>,
    case: &Case,
    entries: &[String],
    days: i32,
) -> PyResult<Bound<'py, PyAny>> {
    let file = case.write(
        &format!("policy-{}.toml", fs::read_dir(case.root()).unwrap().count()),
        &policy_text(py, entries, days),
    );
    module(py, "conductor.candidate_review.policy")
        .getattr("load_policy")
        .unwrap()
        .call1((path(py, &file),))
}

fn rejection(py: Python<'_>, case: &Case, entry: &Map<String, Value>, fragment: &str) {
    let error = load_waiver_policy(py, case, &[waiver_table(entry)], 35).unwrap_err();
    let class = module(py, "conductor.candidate_review.policy")
        .getattr("PolicyError")
        .unwrap();
    assert_error(py, error, &class, fragment);
}

fn single_field_variants(py: Python<'_>) -> Vec<(&'static str, Value, &'static str)> {
    let (_, _, _, clause) = policy_constants(py);
    let anchored = format!("{:x}", Sha256::digest(b"anchored bytes\n"));
    vec![
        ("path", json!("*.py"), "glob metacharacters"),
        ("path", json!("probe.py"), "exact repo-relative"),
        ("expires", json!(utc_date(py, -1)), "expired on"),
        ("expires", json!(utc_date(py, 91)), "more than 90 days out"),
        ("milestone", json!("other-milestone"), "milestone"),
        ("id", json!(""), "non-empty string"),
        ("sources", Value::Null, "requires sources"),
        ("sources", json!([]), "requires sources"),
        (
            "sources",
            json!([{"path":"research/test/helper.py","sha256":anchored}]),
            "test-shaped",
        ),
        (
            "integration_base",
            json!("4".repeat(40)),
            "integration base",
        ),
        ("source_anchor", json!("4".repeat(40)), "source anchor"),
        ("sha256", json!("not-a-hash"), "64 lowercase hex digits"),
        (
            "sha256",
            json!(anchored.to_uppercase()),
            "64 lowercase hex digits",
        ),
        (
            "binding_clause",
            json!(clause.replace("voids", "never voids")),
            "verbatim",
        ),
        (
            "path",
            json!("research/tests/../tests/test_x.py"),
            "traversal",
        ),
        (
            "path",
            json!("research\\tests\\test_broken.py"),
            "traversal",
        ),
        ("path", json!("research/tests/test_\u{1}x.py"), "traversal"),
        ("path", json!("research/tools/helper.py"), "test_*.py"),
        (
            "path",
            json!("research/tools/test_helper.py"),
            "tests/ directory",
        ),
        ("sources", json!("legacy-dep"), "requires sources"),
        (
            "sources",
            json!([{"path":"research/tools/dep.py"}]),
            "path and sha256",
        ),
        (
            "sources",
            json!([{"path":"research/tools/dep.py","sha256":anchored,"note":"extra"}]),
            "path and sha256",
        ),
        (
            "sources",
            json!([{"path":"research/tools/*.py","sha256":anchored}]),
            "non-test .py files",
        ),
        (
            "sources",
            json!([{"path":"research/tools/../dep.py","sha256":anchored}]),
            "non-test .py files",
        ),
        (
            "sources",
            json!([{"path":"research/tests/test_dep.py","sha256":anchored}]),
            "non-test .py files",
        ),
        (
            "sources",
            json!([{"path":"research/tests/lib.py","sha256":anchored}]),
            "non-test .py files",
        ),
        (
            "sources",
            json!([{"path":"research/tools/dep.py","sha256":"tooshort"}]),
            "source sha256 must be 64 lowercase hex digits",
        ),
    ]
}

#[test]
fn mutation_waiver_policy_accepts_bound_entry_and_rejects_malformed_variants() {
    let case = isolated_case();
    Python::attach(|py| {
        let base = base_entry(py);
        let loaded = load_waiver_policy(py, &case, &[waiver_table(&base)], 30).unwrap();
        let waivers = loaded.getattr("mutation_waivers").unwrap();
        assert_eq!(waivers.len().unwrap(), 1);
        let waiver = waivers.get_item(0).unwrap();
        let (milestone, integration_base, _, clause) = policy_constants(py);
        for (name, expected) in [
            ("path", LEGACY.to_owned()),
            ("milestone", milestone),
            ("integration_base", integration_base),
            (
                "sha256",
                format!("{:x}", Sha256::digest(b"anchored bytes\n")),
            ),
            ("binding_clause", clause),
        ] {
            assert_eq!(
                waiver.getattr(name).unwrap().extract::<String>().unwrap(),
                expected
            );
        }
        let sources = waiver.getattr("sources").unwrap();
        assert_eq!(sources.len().unwrap(), 1);
        assert_eq!(
            sources
                .get_item(0)
                .unwrap()
                .getattr("path")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "research/tools/dep.py"
        );
        assert_eq!(
            sources
                .get_item(0)
                .unwrap()
                .getattr("sha256")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            format!("{:x}", Sha256::digest(b"pinned source\n"))
        );
        for (key, replacement, fragment) in single_field_variants(py) {
            let mut entry = base.clone();
            entry.insert(key.to_owned(), replacement);
            rejection(py, &case, &entry, fragment);
        }
        let mut a = base.clone();
        a.insert("id".into(), json!("dup"));
        let mut b = a.clone();
        b.insert("path".into(), json!("research/tests/test_b.py"));
        let error =
            load_waiver_policy(py, &case, &[waiver_table(&a), waiver_table(&b)], 35).unwrap_err();
        assert_error(
            py,
            error,
            &module(py, "conductor.candidate_review.policy")
                .getattr("PolicyError")
                .unwrap(),
            "duplicated: dup",
        );
        let mut duplicate_path = base.clone();
        duplicate_path.insert("path".into(), json!("research/tests/test_c.py"));
        assert!(load_waiver_policy(
            py,
            &case,
            &[waiver_table(&duplicate_path), waiver_table(&duplicate_path)],
            35
        )
        .is_err());
        let digest = format!("{:x}", Sha256::digest(b"anchored bytes\n"));
        let mut duplicate_sources = base.clone();
        duplicate_sources.insert(
            "sources".into(),
            json!([{"path":"research/tools/dep.py","sha256":digest},
                {"path":"research/tools/dep.py","sha256":digest}]),
        );
        rejection(py, &case, &duplicate_sources, "duplicate source paths");
        let mut no_owner = base;
        no_owner.remove("owner");
        assert!(load_waiver_policy(py, &case, &[waiver_table(&no_owner)], 35).is_err());
    });
}

fn verifier(py: Python<'_>, paths: &[&str]) -> AttrPatch {
    let missing: Vec<Value> = paths
        .iter()
        .map(|relative| json!({"path":relative,"reason":"no campaign"}))
        .collect();
    let payload = json!({"status":"FAIL","checked_test_paths":paths,"evidence":[],
        "missing_evidence":missing,"malformed_receipts":[]});
    let kwargs = PyDict::new(py);
    kwargs
        .set_item(
            "return_value",
            module(py, "json")
                .getattr("loads")
                .unwrap()
                .call1((payload.to_string(),))
                .unwrap(),
        )
        .unwrap();
    let mock = module(py, "unittest.mock")
        .getattr("Mock")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap();
    AttrPatch::replace(
        module(py, "conductor.mutation_testing").as_any(),
        "verify_evidence",
        &mock,
    )
}

fn runtime_context<'py>(
    py: Python<'py>,
    root: &Path,
    base_oid: &str,
    anchored: &[u8],
    source: Option<(&str, &[u8])>,
) -> (Bound<'py, PyAny>, Vec<AttrPatch>) {
    let (context, guards) = gate_context(
        py,
        root,
        root,
        &[(LEGACY, &["test_frozen_legacy"])],
        base_oid,
    );
    let snapshot: PathBuf = context.getattr("snapshot").unwrap().extract().unwrap();
    fs::create_dir_all(snapshot.join("conductor")).unwrap();
    fs::write(snapshot.join(WAIVED), anchored).unwrap();
    fs::create_dir_all(snapshot.join("research/tests")).unwrap();
    fs::write(snapshot.join(OTHER), "def test_other():\n    assert True\n").unwrap();
    let (milestone, integration_base, anchor, clause) = policy_constants(py);
    let policy_module = module(py, "conductor.candidate_review.policy");
    let sources = match source {
        Some((relative, bytes)) => {
            let target = snapshot.join(relative);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, bytes).unwrap();
            let kwargs = PyDict::new(py);
            kwargs.set_item("path", relative).unwrap();
            kwargs
                .set_item("sha256", format!("{:x}", Sha256::digest(bytes)))
                .unwrap();
            vec![policy_module
                .getattr("WaiverSourceBinding")
                .unwrap()
                .call((), Some(&kwargs))
                .unwrap()]
        }
        None => Vec::new(),
    };
    let expires = module(py, "datetime")
        .getattr("date")
        .unwrap()
        .call_method1("fromisoformat", (utc_date(py, 7),))
        .unwrap();
    let kwargs = PyDict::new(py);
    for (key, value) in [
        ("waiver_id", "runtime-waiver"),
        ("path", WAIVED),
        ("owner", "tim"),
        (
            "justification",
            "bounded legacy lane stabilization before w7 linear integration lands",
        ),
        ("milestone", &milestone),
        ("integration_base", &integration_base),
        ("source_anchor", &anchor),
        ("binding_clause", &clause),
    ] {
        kwargs.set_item(key, value).unwrap();
    }
    kwargs.set_item("expires", expires).unwrap();
    kwargs
        .set_item("sha256", format!("{:x}", Sha256::digest(anchored)))
        .unwrap();
    kwargs
        .set_item("sources", PyTuple::new(py, sources).unwrap())
        .unwrap();
    let waiver = policy_module
        .getattr("MutationWaiverPolicy")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap();
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("mutation_waivers", PyTuple::new(py, [waiver]).unwrap())
        .unwrap();
    let policy = replace_fields(py, &context.getattr("policy").unwrap(), &kwargs);
    let kwargs = PyDict::new(py);
    kwargs
        .set_item(
            "changes",
            PyTuple::new(
                py,
                [
                    added_change(py, WAIVED, &["python", "source", "test"]),
                    added_change(py, OTHER, &["python", "source", "test"]),
                ],
            )
            .unwrap(),
        )
        .unwrap();
    let candidate = replace_fields(py, &context.getattr("candidate").unwrap(), &kwargs);
    let kwargs = PyDict::new(py);
    kwargs.set_item("candidate", candidate).unwrap();
    kwargs.set_item("policy", policy).unwrap();
    (replace_fields(py, &context, &kwargs), guards)
}

fn check<'py>(py: Python<'py>, context: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    module(py, "conductor.candidate_review.checks")
        .getattr("check_mutation_evidence")
        .unwrap()
        .call1((context,))
        .unwrap()
}

fn metric_paths(result: &Bound<'_, PyAny>, key: &str) -> Vec<String> {
    result
        .getattr("metrics")
        .unwrap()
        .get_item(key)
        .unwrap()
        .extract()
        .unwrap()
}

fn finding_pairs(result: &Bound<'_, PyAny>) -> Vec<(String, String, String)> {
    result
        .getattr("findings")
        .unwrap()
        .try_iter()
        .unwrap()
        .map(|finding| {
            let finding = finding.unwrap();
            (
                finding.getattr("rule_id").unwrap().extract().unwrap(),
                finding.getattr("path").unwrap().extract().unwrap(),
                finding.getattr("message").unwrap().extract().unwrap(),
            )
        })
        .collect()
}

#[test]
fn mutation_waiver_runtime_conditions() {
    let case = isolated_case();
    Python::attach(|py| {
        let _verifier = verifier(py, &[WAIVED, OTHER]);
        let (_, base, _, _) = policy_constants(py);
        let anchored = b"def test_stable():\n    assert True\n";
        let (context, _anchor) = runtime_context(py, case.root(), &base, anchored, None);
        let result = check(py, &context);
        assert_eq!(
            result
                .getattr("status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "failed"
        );
        let pairs = finding_pairs(&result);
        assert_eq!(
            pairs
                .iter()
                .map(|(_, file, _)| file.as_str())
                .collect::<Vec<_>>(),
            [OTHER, WAIVED, OTHER]
        );
        assert_eq!(metric_paths(&result, "mutation_waiver_applied"), [WAIVED]);
        let values: Vec<_> = pairs
            .iter()
            .filter(|(rule, _, _)| rule == "new-test-value-not-admitted")
            .collect();
        assert_eq!(
            values
                .iter()
                .map(|(_, file, _)| file.as_str())
                .collect::<Vec<_>>(),
            [WAIVED, OTHER]
        );
        assert!(values
            .iter()
            .any(|(_, _, message)| message.contains("::test_stable")));
        assert!(values
            .iter()
            .any(|(_, _, message)| message.contains("::test_other")));
        let snapshot: PathBuf = context.getattr("snapshot").unwrap().extract().unwrap();
        fs::write(
            snapshot.join(WAIVED),
            [anchored.as_slice(), b"# drift\n"].concat(),
        )
        .unwrap();
        let drift = check(py, &context);
        assert_eq!(
            finding_pairs(&drift)
                .iter()
                .map(|(_, file, _)| file.clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([WAIVED.to_owned(), OTHER.to_owned()])
        );
        assert_eq!(
            finding_pairs(&drift)
                .iter()
                .map(|(rule, _, _)| rule.clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "missing-mutation-receipt".to_owned(),
                "new-test-value-not-admitted".to_owned()
            ])
        );
        assert!(metric_paths(&drift, "mutation_waiver_applied").is_empty());
        let kwargs = PyDict::new(py);
        kwargs.set_item("base_commit_oid", "c".repeat(40)).unwrap();
        let candidate = replace_fields(py, &context.getattr("candidate").unwrap(), &kwargs);
        let kwargs = PyDict::new(py);
        kwargs.set_item("candidate", candidate).unwrap();
        let wrong_base = replace_fields(py, &context, &kwargs);
        fs::write(snapshot.join(WAIVED), anchored).unwrap();
        assert!(metric_paths(&check(py, &wrong_base), "mutation_waiver_applied").is_empty());
    });
}

#[test]
fn mutation_waiver_applies_across_candidate_kinds() {
    let case = isolated_case();
    Python::attach(|py| {
        let _verifier = verifier(py, &[WAIVED]);
        let (_, base, _, _) = policy_constants(py);
        for kind in ["index", "range", "commit"] {
            let root = case.root().join(kind);
            let (context, _anchor) = runtime_context(
                py,
                &root,
                &base,
                b"def test_kind():\n    assert True\n",
                None,
            );
            let kwargs = PyDict::new(py);
            kwargs.set_item("kind", kind).unwrap();
            kwargs
                .set_item(
                    "commit_oid",
                    if kind == "commit" {
                        Some("d".repeat(40))
                    } else {
                        None
                    },
                )
                .unwrap();
            let candidate = replace_fields(py, &context.getattr("candidate").unwrap(), &kwargs);
            let kwargs = PyDict::new(py);
            kwargs.set_item("candidate", candidate).unwrap();
            let swept = replace_fields(py, &context, &kwargs);
            assert_eq!(
                metric_paths(&check(py, &swept), "mutation_waiver_applied"),
                [WAIVED],
                "{kind}"
            );
        }
    });
}

#[test]
fn mutation_waiver_source_binding_requires_pinned_bytes() {
    let case = isolated_case();
    Python::attach(|py| {
        let _verifier = verifier(py, &[WAIVED]);
        let (_, base, _, _) = policy_constants(py);
        let relative = "research/tools/legacy_dep.py";
        let source = b"# legacy dependency frozen at the integration base\nVALUE = 1\n";
        let (context, _anchor) = runtime_context(
            py,
            case.root(),
            &base,
            b"def test_bound():\n    assert True\n",
            Some((relative, source)),
        );
        assert_eq!(
            metric_paths(&check(py, &context), "mutation_waiver_applied"),
            [WAIVED]
        );
        let snapshot: PathBuf = context.getattr("snapshot").unwrap().extract().unwrap();
        let pinned = snapshot.join(relative);
        fs::write(&pinned, "# drifted after integration\nVALUE = 2\n").unwrap();
        assert!(metric_paths(&check(py, &context), "mutation_waiver_applied").is_empty());
        fs::remove_file(&pinned).unwrap();
        assert!(metric_paths(&check(py, &context), "mutation_waiver_applied").is_empty());
    });
}
