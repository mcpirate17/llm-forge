#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for reuse consolidation cluster aggregation.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::py_json;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use serde_json::{json, Value};
use support::{module, Case};

const EXPECTED_CLUSTER_SUMMARY: &str = r#"[["exact",60,[["pkg/local/member_0.py",10,25,"shared","return value"],["pkg/local/member_1.py",11,21,"_shared","return value"],["pkg/local/member_2.py",12,22,"_shared","return value"]],0.99,63,"low","auto","exact, same-dir"],["near",48,[["pkg/near/member_0.py",10,20,"renamed","return renamed_0"],["pkg/near/member_1.py",11,21,"_renamed","return renamed_1"],["pkg/near/member_2.py",12,22,"_renamed","return renamed_2"]],0.9,46,"low","auto","near, same-dir"],["exact",55,[["pkg/local/member_0.py",10,20,"shared_two","return other"],["pkg/local/member_1.py",11,21,"_shared_two","return other"]],0.99,35,"low","auto","exact, same-dir"],["exact",90,[["research/synthesis/member_0.py",10,20,"tpl_lane","return x"],["research/synthesis/member_1.py",11,21,"_tpl_lane","return x"]],0.71,10,"high","ignore","exact, same-dir, semantic-family-risk"],["exact",72,[["pkg/member_0.py",10,20,"helper","return item"],["research/member_1.py",11,21,"_helper","return item"]],0.81,4,"high","ignore","exact, cross-dir"]]"#;

fn native(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "conductor.reuse.consolidation")
}

fn record<'py>(
    py: Python<'py>,
    (file, start, end, name, hash, tokens, source): (&str, i32, i32, &str, &str, i32, &str),
) -> Bound<'py, PyAny> {
    native(py)
        .getattr("FuncRecord")
        .unwrap()
        .call1((file, start, end, name, hash, tokens, source))
        .unwrap()
}

fn records(py: Python<'_>) -> Bound<'_, PyList> {
    let result = PyList::empty(py);
    let specs = [
        (
            "exact-local",
            Some("pkg/local"),
            "shared",
            Some("return value"),
            60,
            3,
        ),
        (
            "exact-local-two",
            Some("pkg/local"),
            "shared_two",
            Some("return other"),
            55,
            2,
        ),
        ("near-local", Some("pkg/near"), "renamed", None, 48, 3),
        (
            "exact-risk",
            Some("research/synthesis"),
            "tpl_lane",
            Some("return x"),
            90,
            2,
        ),
        ("cross-root", None, "helper", Some("return item"), 72, 2),
    ];
    for (group, directory, name, shared_source, tokens, count) in specs {
        for member in 0..count {
            let root = directory.unwrap_or(if member == 0 { "pkg" } else { "research" });
            let source = shared_source
                .map(str::to_owned)
                .unwrap_or_else(|| format!("return renamed_{member}"));
            let named = if member == 0 {
                name.to_owned()
            } else {
                format!("_{name}")
            };
            result
                .append(record(
                    py,
                    (
                        &format!("{root}/member_{member}.py"),
                        10 + member,
                        20 + member,
                        &named,
                        group,
                        tokens,
                        &source,
                    ),
                ))
                .unwrap();
        }
    }
    result
        .append(record(
            py,
            (
                "pkg/local/member_0.py",
                10,
                25,
                "shared",
                "exact-local",
                60,
                "return value",
            ),
        ))
        .unwrap();
    result
}

fn field<T: for<'a> FromPyObject<'a, 'a>>(value: &Bound<'_, PyAny>, name: &str) -> T {
    value.getattr(name).unwrap().extract().ok().unwrap()
}

fn summary(clusters: &Bound<'_, PyAny>) -> Vec<Value> {
    clusters
        .try_iter()
        .unwrap()
        .map(|cluster| {
            let cluster = cluster.unwrap();
            let sites: Vec<Value> = cluster
                .getattr("sites")
                .unwrap()
                .try_iter()
                .unwrap()
                .map(|site| {
                    let site = site.unwrap();
                    json!([
                        field::<String>(&site, "file"),
                        field::<i64>(&site, "line_start"),
                        field::<i64>(&site, "line_end"),
                        field::<String>(&site, "name"),
                        field::<String>(&site, "source"),
                    ])
                })
                .collect();
            json!([
                field::<String>(&cluster, "kind"),
                field::<i64>(&cluster, "tokens"),
                sites,
                field::<f64>(&cluster, "confidence"),
                field::<i64>(&cluster, "value_score"),
                field::<String>(&cluster, "risk"),
                field::<String>(&cluster, "disposition"),
                field::<String>(&cluster, "rationale"),
            ])
        })
        .collect()
}

#[test]
fn native_cluster_aggregation_matches_python() {
    let _case = Case::new();
    Python::attach(|py| {
        let native = native(py);
        let clusters = native
            .getattr("build_clusters")
            .unwrap()
            .call1((records(py),))
            .unwrap();
        let expected: Value = serde_json::from_str(EXPECTED_CLUSTER_SUMMARY).unwrap();
        assert_eq!(Value::Array(summary(&clusters)), expected);
        let kwargs = PyDict::new(py);
        kwargs.set_item("batch_size", 2).unwrap();
        native
            .getattr("_assign_ids")
            .unwrap()
            .call((clusters.clone(),), Some(&kwargs))
            .unwrap();
        let mut ids = Vec::new();
        for cluster in clusters.try_iter().unwrap() {
            let cluster = cluster.unwrap();
            let home = native
                .getattr("_suggested_home")
                .unwrap()
                .call1((cluster.getattr("sites").unwrap(),))
                .unwrap();
            let home: Option<String> = home.extract().unwrap();
            ids.push(json!([
                field::<String>(&cluster, "id"),
                field::<i64>(&cluster, "batch"),
                home
            ]));
        }
        assert_eq!(
            Value::Array(ids),
            json!([
                ["C001", 0, "pkg/local/_member.py"],
                ["C002", 1, "pkg/near/_member.py"],
                ["C003", 0, "pkg/local/_member.py"],
                ["C004", -1, "research/synthesis/_member.py"],
                ["C005", -1, null]
            ])
        );
    });
}

#[test]
fn native_token_clone_evidence_matches_python() {
    let _case = Case::new();
    Python::attach(|py| {
        let report = py_json(
            py,
            json!({"duplicates":[
                {"tokens":80,"firstFile":{"name":"pkg/one.py","start":3,"end":20},
                 "secondFile":{"name":"pkg/two.py","start":4,"end":21}},
                {"tokens":10,"firstFile":{},"secondFile":{}}
            ]}),
        );
        let clusters = native(py)
            .getattr("_clusters_from_jscpd")
            .unwrap()
            .call1((report,))
            .unwrap();
        assert_eq!(
            Value::Array(summary(&clusters)),
            json!([[
                "token",
                80,
                [
                    ["pkg/one.py", 3, 20, "<clone>", ""],
                    ["pkg/two.py", 4, 21, "<clone>", ""]
                ],
                0.9,
                47,
                "low",
                "auto",
                "token, same-dir"
            ]])
        );
    });
}
