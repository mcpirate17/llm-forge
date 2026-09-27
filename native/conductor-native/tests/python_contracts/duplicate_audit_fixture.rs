//! Fixture inputs and fake analyzer reports for the duplicate-audit contracts.

use crate::audit_fixture::{git, isolated_case, py_json, write};
use crate::support::{module, path, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyCFunction, PyDict, PyModule};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub const SENTINEL: &str = "DUPLICATE_INDEX_SENTINEL = 1";
pub const CPD_NAMESPACE: &str = "https://pmd-code.org/schema/cpd-report";

pub fn audit<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.run_duplicate_audit")
}

pub fn repo() -> (Case, PathBuf) {
    let case = isolated_case();
    let root = case.mkdir("repo");
    git(&root, &["init", "-q"]);
    case.mkdir("repo/src");
    case.mkdir("repo/native");
    (case, root)
}

pub fn baseline_relative(py: Python<'_>, name: &str) -> String {
    audit(py)
        .getattr(name)
        .unwrap()
        .call_method0("as_posix")
        .unwrap()
        .extract()
        .unwrap()
}

pub fn write_baseline(path: &Path, entries: &[Value]) {
    let mut keyed = Map::new();
    for entry in entries {
        let key = entry["key"].as_str().unwrap();
        let mut files = vec![
            entry["firstFile"].as_str().unwrap(),
            entry["secondFile"].as_str().unwrap(),
        ];
        files.sort_unstable();
        keyed.insert(
            key.to_owned(),
            json!({"files":files,"lines":entry["lines"]}),
        );
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        format!(
            "{}\n",
            json!({"_comment":"test baseline","count":keyed.len(),"entries":keyed})
        ),
    )
    .unwrap();
}

pub fn configure_jscpd(py: Python<'_>, root: &Path, ignores: &[&str]) {
    let binary = root.join("node_modules/.bin/jscpd");
    fs::create_dir_all(binary.parent().unwrap()).unwrap();
    fs::write(&binary, "#!/bin/sh\n").unwrap();
    write(
        root,
        "package.json",
        &format!("{}\n", json!({"jscpd":{"ignore":ignores}})),
    );
    write(root, ".gitignore", "node_modules/\n");
    let baseline = baseline_relative(py, "JSCPD_BASELINE_RELATIVE");
    write_baseline(&root.join(&baseline), &[]);
    git(root, &["add", "package.json", ".gitignore", &baseline]);
}

pub fn configure_pmd(root: &Path) {
    let binary = root.join("node_modules/.bin/pmd");
    fs::create_dir_all(binary.parent().unwrap()).unwrap();
    fs::write(binary, "#!/bin/sh\n").unwrap();
}

pub fn stable_key(py: Python<'_>, first: &str, second: &str, fragment: &str) -> String {
    let normalized = fragment
        .trim_matches('\n')
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    module(py, "conductor._native")
        .getattr("stable_duplicate_key_native")
        .unwrap()
        .call1((first, second, PyBytes::new(py, normalized.as_bytes())))
        .unwrap()
        .extract()
        .unwrap()
}

pub fn dup_entry(py: Python<'_>, first: &str, second: &str, fragment: &str) -> Value {
    json!({"key":stable_key(py, first, second, fragment),"firstFile":first,"secondFile":second,"lines":10})
}

pub fn completed(py: Python<'_>, command: &Bound<'_, PyAny>, code: i32, stderr: &str) -> Py<PyAny> {
    module(py, "subprocess")
        .getattr("CompletedProcess")
        .unwrap()
        .call1((command, code, "", stderr))
        .unwrap()
        .unbind()
}

pub fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.exists() {
        return out;
    }
    let mut todo = vec![root.to_owned()];
    while let Some(dir) = todo.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                todo.push(path);
            } else if path.is_file() {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn arg_after(command: &[String], flag: &str) -> PathBuf {
    PathBuf::from(&command[command.iter().position(|arg| arg == flag).unwrap() + 1])
}

pub fn jscpd_emulator(py: Python<'_>, require_ignored: bool) -> AttrPatch {
    let callback =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            let py = args.py();
            let command: Vec<String> = args.get_item(0)?.extract()?;
            let kw = kwargs.expect("analyzer keyword args");
            let cwd: PathBuf = kw.get_item("cwd")?.unwrap().extract()?;
            let tool: String = kw.get_item("tool_name")?.unwrap().extract()?;
            assert_eq!(tool, "jscpd");
            if require_ignored {
                assert!(cwd
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("llm-index-sources-"));
                assert!(command.iter().any(|item| item == "src"));
                assert!(!command
                    .iter()
                    .any(|item| item == cwd.join("src").to_str().unwrap()));
            }
            let config: Value =
                serde_json::from_str(&fs::read_to_string(cwd.join("package.json")).unwrap())
                    .unwrap();
            let patterns = config["jscpd"]["ignore"].as_array().unwrap();
            let mut matching = Vec::new();
            for source in ["src", "native"] {
                for file in files_under(&cwd.join(source)) {
                    let relative = file
                        .strip_prefix(&cwd)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .replace('\\', "/");
                    let ignored = patterns.iter().any(|pattern| {
                        let pattern = pattern.as_str().unwrap();
                        pattern == "src/tests/**" && relative.starts_with("src/tests/")
                            || pattern == "**/src/tests/**" && relative.starts_with("src/tests/")
                    });
                    if ignored {
                        continue;
                    }
                    let body = fs::read_to_string(&file).unwrap();
                    if require_ignored {
                        assert!(!body.contains(SENTINEL));
                    }
                    if body.contains(SENTINEL) {
                        matching.push(relative);
                    }
                }
            }
            let duplicates = if matching.len() >= 2 {
                json!([{"firstFile":{"name":matching[0]},"secondFile":{"name":matching[1]},
                "fragment":SENTINEL,"lines":10}])
            } else {
                json!([])
            };
            let output = arg_after(&command, "--output");
            fs::create_dir_all(&output).unwrap();
            fs::write(
                output.join("jscpd-report.json"),
                json!({"duplicates":duplicates}).to_string(),
            )
            .unwrap();
            Ok(completed(py, &args.get_item(0)?, 0, ""))
        })
        .unwrap();
    AttrPatch::replace(audit(py).as_any(), "_run_report_command", callback.as_any())
}

pub fn pmd_emulator(py: Python<'_>) -> AttrPatch {
    let callback = PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
        let py = args.py();
        let command: Vec<String> = args.get_item(0)?.extract()?;
        let tool: String = kwargs.unwrap().get_item("tool_name")?.unwrap().extract()?;
        assert_eq!(tool, "pmd-cpd");
        let file_list = arg_after(&command, "--file-list");
        let files = fs::read_to_string(file_list).unwrap();
        let files: Vec<_> = files.lines().collect();
        assert!(files.len() >= 2);
        let report = arg_after(&command, "--report-file");
        fs::write(report, format!(
            "<pmd-cpd xmlns=\"{CPD_NAMESPACE}\"><duplication lines=\"10\" tokens=\"80\"><file path=\"{}\" line=\"1\" endline=\"10\" /><file path=\"{}\" line=\"1\" endline=\"10\" /><codefragment><![CDATA[{SENTINEL}]]></codefragment></duplication></pmd-cpd>",
            files[0], files[1]
        )).unwrap();
        Ok(completed(py, &args.get_item(0)?, 0, ""))
    }).unwrap();
    AttrPatch::replace(audit(py).as_any(), "_run_report_command", callback.as_any())
}

pub fn kwargs_root<'py>(py: Python<'py>, root: &Path) -> Bound<'py, PyDict> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("root", path(py, root)).unwrap();
    kwargs
}

pub fn call_baseline(
    py: Python<'_>,
    baseline: &Path,
    entries: &[Value],
    changed: Option<&[&str]>,
) -> i32 {
    let kwargs = PyDict::new(py);
    kwargs.set_item("tool_name", "test-analyzer").unwrap();
    if let Some(paths) = changed {
        let set = module(py, "builtins")
            .getattr("frozenset")
            .unwrap()
            .call1((paths.to_vec(),))
            .unwrap();
        kwargs.set_item("changed_files", set).unwrap();
    }
    let rows = py_json(py, &Value::Array(entries.to_vec()));
    audit(py)
        .getattr("_check_against_baseline")
        .unwrap()
        .call((path(py, baseline), rows), Some(&kwargs))
        .unwrap()
        .extract()
        .unwrap()
}
