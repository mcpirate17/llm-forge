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
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList};
use serde_json::{json, Value};
use support::{module, AttrPatch, Case};

#[path = "python_contracts/source_dirs_support.rs"]
mod source_dirs_support;

#[test]
fn resolve_settings_defaults_to_the_pyproject_source_dirs() {
    let case = Case::new();
    let root = source_dirs_support::host(&case);
    Python::attach(|py| {
        let tool = native(py);
        let argv = vec!["--repo".to_owned(), root.to_string_lossy().into_owned()];
        let args = tool
            .getattr("build_parser")
            .unwrap()
            .call0()
            .unwrap()
            .call_method1("parse_args", (argv,))
            .unwrap();
        let (_repo, targets, _exclude): (std::path::PathBuf, Vec<String>, Vec<String>) = tool
            .getattr("resolve_settings")
            .unwrap()
            .call1((args,))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(targets, ["pkg", "lib/pkg"]);
    });
}

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

fn assert_token_counts(metadata: &Bound<'_, PyDict>) {
    for (name, expected) in [
        ("excluded_non_code_files", 1),
        ("excluded_generated_files", 1),
        ("selected_files", 4),
    ] {
        assert_eq!(
            metadata
                .get_item(name)
                .unwrap()
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            expected
        );
    }
}

#[test]
fn token_clone_ingest_uses_packaged_collector_and_unique_scoped_files() {
    let case = Case::new();
    case.write("pkg/a.py", "value = 1\n");
    case.write("pkg/sub/b.rs", "fn value() {}\n");
    case.write("pkg/kernel.cu", "__global__ void kernel() {}\n");
    case.write("pkg/kernel.cuh", "__global__ void kernel();\n");
    case.write("pkg/results.json", "{\"generated\":true}\n");
    case.write(
        "pkg/generated.c",
        "/* Generated by Cython 3.0 */\nvoid helper() {}\n",
    );
    case.write("pkg/cache/ignored.py", "value = 1\n");
    case.write("pkg/tests/ignored.py", "value = 1\n");
    let root = case.root().to_path_buf();
    Python::attach(|py| {
        let callback = PyCFunction::new_closure(py, None, None, move |args, kwargs| {
            let py = args.py();
            let paths: Vec<String> = args.get_item(0)?.extract()?;
            assert_eq!(
                paths,
                [
                    "pkg/a.py",
                    "pkg/kernel.cu",
                    "pkg/kernel.cuh",
                    "pkg/sub/b.rs"
                ]
            );
            let kwargs = kwargs.unwrap();
            assert_eq!(
                kwargs
                    .get_item("cwd")?
                    .unwrap()
                    .extract::<std::path::PathBuf>()?,
                root
            );
            assert!(kwargs.get_item("full_files")?.unwrap().extract::<bool>()?);
            Ok::<_, PyErr>(
                py_json(
                    py,
                    json!({"statistics":{"total":{"sources":4},"formats":{
                        "cpp":{"total":{"sources":2},"sources":{"do_not_copy":"large details"}}
                    }},"duplicates":[{
                        "tokens":80,"lines":10,"fragment":"value",
                        "firstFile":{"name":root.join("pkg/a.py"),"start":3,"end":12},
                        "secondFile":{"name":root.join("pkg/sub/b.rs"),"start":4,"end":13}
                    }]}),
                )
                .unbind(),
            )
        })
        .unwrap();
        let tool = native(py);
        let _collector =
            AttrPatch::replace(tool.as_any(), "collect_jscpd_report", callback.as_any());
        let kwargs = PyDict::new(py);
        kwargs
            .set_item(
                "exclude",
                ["cache"]
                    .into_iter()
                    .collect::<std::collections::HashSet<_>>(),
            )
            .unwrap();
        kwargs.set_item("full_files", true).unwrap();
        let metadata = PyDict::new(py);
        kwargs.set_item("metadata", &metadata).unwrap();
        let clusters = tool
            .getattr("ingest_token_clones")
            .unwrap()
            .call(
                (support::path(py, case.root()), vec!["pkg", "pkg/sub"]),
                Some(&kwargs),
            )
            .unwrap();
        assert_eq!(clusters.len().unwrap(), 1);
        assert_token_counts(&metadata);
        let statistics = metadata.get_item("statistics").unwrap().unwrap();
        let cpp = statistics
            .get_item("formats")
            .unwrap()
            .get_item("cpp")
            .unwrap();
        assert_eq!(
            cpp.get_item("sources").unwrap().extract::<i32>().unwrap(),
            2
        );
        assert_eq!(cpp.len().unwrap(), 1);
        let sites = clusters.get_item(0).unwrap().getattr("sites").unwrap();
        assert_eq!(
            field::<String>(&sites.get_item(0).unwrap(), "file"),
            "pkg/a.py"
        );
        assert_eq!(field::<i32>(&sites.get_item(0).unwrap(), "line_start"), 3);
        assert!(!case.root().join("tasks/audit").exists());
    });
}

#[test]
fn token_clone_failure_is_explicit_and_preserves_external_ast_report() {
    let case = Case::new();
    let root = case.root().join("host");
    std::fs::create_dir_all(root.join("pkg")).unwrap();
    std::fs::write(root.join("pkg/a.py"), "value = 1\n").unwrap();
    let out = case.root().join("outside/result.json");
    let markdown = case.root().join("outside/result.md");
    Python::attach(|py| {
        let callback = PyCFunction::new_closure(py, None, None, |args, _kwargs| {
            let error = native(args.py())
                .getattr("DuplicateAuditError")?
                .call1(("detector unavailable",))?;
            Err::<(), _>(PyErr::from_value(error))
        })
        .unwrap();
        let tool = native(py);
        let _collector =
            AttrPatch::replace(tool.as_any(), "collect_jscpd_report", callback.as_any());
        let argv = vec![
            "--repo".to_owned(),
            root.to_string_lossy().into_owned(),
            "--targets".to_owned(),
            "pkg".to_owned(),
            "--with-token-clones".to_owned(),
            "--token-clone-full-files".to_owned(),
            "--out".to_owned(),
            out.to_string_lossy().into_owned(),
            "--md".to_owned(),
            markdown.to_string_lossy().into_owned(),
        ];
        assert_eq!(
            tool.getattr("main")
                .unwrap()
                .call1((argv,))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            2
        );
    });
    let data: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(data["complete"], false);
    assert_eq!(data["detectors"]["token_clones"]["status"], "failed");
    assert_eq!(
        data["detectors"]["token_clones"]["error"],
        "detector unavailable"
    );
    assert!(std::fs::read_to_string(markdown)
        .unwrap()
        .contains("**failed**"));
    assert!(!root.join("tasks").exists());
}

#[test]
fn full_file_sources_preserve_ignored_tracked_and_hidden_owned_paths() {
    let case = Case::new();
    case.write(".gitignore", "ignored/\n.claude/\n");
    case.write("ignored/tracked.py", "value = 1\n");
    case.write("ignored/untracked.py", "value = 1\n");
    case.write(".claude/hooks/tracked.py", "value = 1\n");
    case.write("ordinary.py", "value = 1\n");
    for args in [
        vec!["init", "--quiet"],
        vec![
            "add",
            "-f",
            ".gitignore",
            "ignored/tracked.py",
            ".claude/hooks/tracked.py",
        ],
    ] {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(case.root())
            .status()
            .unwrap()
            .success());
    }
    Python::attach(|py| {
        let kwargs = PyDict::new(py);
        let metadata = PyDict::new(py);
        kwargs.set_item("full_files", true).unwrap();
        kwargs.set_item("metadata", &metadata).unwrap();
        let paths: Vec<String> = native(py)
            .getattr("_token_source_paths")
            .unwrap()
            .call(
                (support::path(py, case.root()), vec![".", "ignored"]),
                Some(&kwargs),
            )
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            paths,
            [
                ".claude/hooks/tracked.py",
                "ignored/tracked.py",
                "ordinary.py"
            ]
        );
        assert_eq!(
            metadata
                .get_item("excluded_unowned_files")
                .unwrap()
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            1
        );
        assert_eq!(
            metadata
                .get_item("ownership")
                .unwrap()
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "git-tracked-and-nonignored-untracked"
        );
    });
}

#[test]
fn token_coverage_separates_threshold_skips_from_missing_source_evidence() {
    let case = Case::new();
    case.write("tiny.py", "value = 1\n");
    case.write("missing.py", &"value = 1\n".repeat(12));
    Python::attach(|py| {
        let report = py_json(
            py,
            json!({
                "audit_config":{"min_lines":10,"min_tokens":80},
                "statistics":{"formats":{"python":{"sources":{"reported.py":{}}}}}
            }),
        );
        let coverage = native(py)
            .getattr("_token_coverage")
            .unwrap()
            .call1((
                support::path(py, case.root()),
                vec!["reported.py", "tiny.py", "missing.py"],
                report,
            ))
            .unwrap();
        assert_eq!(
            coverage
                .get_item("reported_files")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            1
        );
        assert_eq!(
            coverage
                .get_item("below_min_lines_paths")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            ["tiny.py"]
        );
        assert_eq!(
            coverage
                .get_item("unexplained_missing_paths")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            ["missing.py"]
        );
    });
}

#[test]
fn missing_token_statistics_fail_closed_and_keep_ast_clusters() {
    let _case = Case::new();
    Python::attach(|py| {
        let globals = PyDict::new(py);
        globals.set_item("tool", native(py)).unwrap();
        py.run(
            &std::ffi::CString::new(
                r#"
import json
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

with TemporaryDirectory() as directory:
    root = Path(directory)
    source = root / 'pkg'
    source.mkdir()
    body = 'def helper(value):\n    first = value + 1\n    second = first * 2\n    third = second - 3\n    fourth = third + 4\n    return fourth\n'
    for name in ('a.py', 'b.py'):
        (source / name).write_text(body)
    argv = ['--repo', str(root), '--targets', 'pkg', '--with-token-clones',
            '--token-clone-full-files', '--out', str(root / 'result.json'), '--md', str(root / 'result.md')]
    for statistics, missing in ((None, ['pkg/a.py', 'pkg/b.py']),
                                 ({'formats': {'python': {'sources': {'pkg/a.py': {}}}}}, ['pkg/b.py'])):
        report = {'duplicates': [], 'audit_config': {'min_lines': 5, 'min_tokens': 50}}
        if statistics is not None:
            report['statistics'] = statistics
        with patch.object(tool, 'collect_jscpd_report', return_value=report):
            assert tool.main(argv) == 2
        data = json.loads((root / 'result.json').read_text())
        assert not data['complete']
        assert data['clusters'], 'partial token evidence discarded AST clusters'
        status = data['detectors']['token_clones']
        assert status['status'] == 'incomplete'
        assert status['coverage']['unexplained_missing_paths'] == missing
        assert '**incomplete**' in (root / 'result.md').read_text()
    # Explained below-threshold files alone do not invalidate a complete scan.
    (source / 'tiny.py').write_text('value = 1\n')
    report['statistics'] = {'formats': {'python': {'sources': {'pkg/a.py': {}, 'pkg/b.py': {}}}}}
    with patch.object(tool, 'collect_jscpd_report', return_value=report):
        assert tool.main(argv) == 0
    data = json.loads((root / 'result.json').read_text())
    assert data['complete']
    assert data['detectors']['token_clones']['coverage']['below_min_lines_paths'] == ['pkg/tiny.py']
"#,
            )
            .unwrap(),
            Some(&globals),
            None,
        )
        .unwrap();
    });
}
