#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the Python surface of the native repository test index.

#[path = "python_contracts/repo_index_support.rs"]
mod repo_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyModule};
use repo_support::{
    cli, expected_tests, fixture, index, old_matcher_targets, paths, repo_root, tests_under,
};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use support::{module, path, Case};

fn old_matcher_superset(py: Python<'_>, root: &Path, index: &Bound<'_, PyAny>) -> (usize, usize) {
    let ast = PyModule::import(py, "ast").unwrap();
    let syntax_error = py.get_type::<pyo3::exceptions::PySyntaxError>();
    let mut parsed = 0;
    let mut checked = 0;
    for relative in tests_under(root) {
        let bytes = match fs::read(root.join(&relative)) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let source = String::from_utf8(bytes)
            .unwrap_or_else(|error| panic!("decode {}: {error}", relative.display()));
        let targets = match old_matcher_targets(&ast, &source) {
            Ok(targets) => targets,
            Err(error) if error.matches(py, &syntax_error).unwrap() => continue,
            Err(error) => panic!("parse {}: {error}", relative.display()),
        };
        parsed += 1;
        let relative = relative.to_string_lossy().into_owned();
        for dotted in targets {
            checked += 1;
            assert!(
                paths(index, "drivers_for", &dotted).contains(&relative),
                "{relative} imports {dotted}; the index does not resolve it"
            );
        }
    }
    (parsed, checked)
}

fn args(root: &Path, question: &[&str]) -> Vec<String> {
    let mut args = vec!["--root".to_owned(), root.to_string_lossy().into_owned()];
    args.extend(question.iter().map(|part| (*part).to_owned()));
    args
}

fn lines(output: &str) -> Vec<String> {
    output.lines().map(str::to_owned).collect()
}

#[test]
fn the_index_resolves_every_import_the_ast_matcher_did() {
    let case = Case::new();
    let fixture_root = fixture(&case);
    let live_root = repo_root();
    Python::attach(|py| {
        let fixed = index(py, fixture_root);
        assert_eq!(tests_under(fixture_root).len(), expected_tests().len());
        assert_eq!(old_matcher_superset(py, fixture_root, &fixed), (4, 4));
        let ast = PyModule::import(py, "ast").unwrap();
        let imports = fs::read_to_string(fixture_root.join("pkg/test_imports.py")).unwrap();
        assert_eq!(
            old_matcher_targets(&ast, &imports).unwrap(),
            ["pkg", "pkg.direct", "pkg.from_module", "pkg.multiple"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        let relative = fs::read_to_string(fixture_root.join("pkg/sub/test_relative.py")).unwrap();
        assert!(old_matcher_targets(&ast, &relative).unwrap().is_empty());
        assert!(paths(&fixed, "drivers_for", "pkg/quoted_only.py").is_empty());

        let live_files = tests_under(&live_root);
        assert!(!live_files.is_empty(), "wrong live Python source root");
        let live = index(py, &live_root);
        let indexed: usize = live.getattr("file_count").unwrap().extract().unwrap();
        assert_eq!(indexed, live_files.len(), "live index inventory differs");
        let (parsed, checked) = old_matcher_superset(py, &live_root, &live);
        assert!(
            parsed > 0 && checked > 0,
            "live AST oracle checked no imports"
        );
    });
}

#[test]
fn from_package_import_module_is_resolved() {
    let case = Case::new();
    fixture(&case);
    Python::attach(|py| {
        let index = index(py, case.root());
        assert_eq!(
            paths(&index, "drivers_for", "pkg/from_package.py"),
            ["pkg/test_imports.py"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert_eq!(
            paths(&index, "drivers_for", "pkg/from_module.py"),
            ["pkg/test_imports.py"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        for source in ["pkg/direct.py", "pkg/multiple.py"] {
            assert_eq!(
                paths(&index, "drivers_for", source),
                ["pkg/test_imports.py"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            );
        }
        assert_eq!(
            paths(&index, "drivers_for", "pkg/nested.py"),
            ["pkg/test_imports.py"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert_eq!(
            paths(&index, "drivers_for", "pkg/sub/local.py"),
            ["pkg/sub/test_relative.py"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert_eq!(
            paths(&index, "drivers_for", "pkg/target.py"),
            ["pkg/sub/test_relative.py"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
    });
}

fn live_function_sample(py: Python<'_>, root: &Path) -> Vec<String> {
    let ast = PyModule::import(py, "ast").unwrap();
    let function = ast.getattr("FunctionDef").unwrap();
    let async_function = ast.getattr("AsyncFunctionDef").unwrap();
    let mut names = Vec::<String>::new();
    for entry in fs::read_dir(root.join("conductor")).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".py") || name.starts_with("test_") {
            continue;
        }
        let source = fs::read_to_string(entry.path()).unwrap();
        let tree = ast.call_method1("parse", (source,)).unwrap();
        for node in ast
            .call_method1("walk", (tree,))
            .unwrap()
            .try_iter()
            .unwrap()
        {
            let node = node.unwrap();
            if node.is_instance(&function).unwrap() || node.is_instance(&async_function).unwrap() {
                names.push(node.getattr("name").unwrap().extract().unwrap());
            }
        }
    }
    names.sort();
    assert!(names.len() >= 25, "wrong live function inventory");
    PyModule::import(py, "random")
        .unwrap()
        .getattr("Random")
        .unwrap()
        .call1((11,))
        .unwrap()
        .call_method1("sample", (names, 25))
        .unwrap()
        .extract()
        .unwrap()
}

fn sampled_live_named_by_reference(py: Python<'_>, root: &Path) {
    let sample = live_function_sample(py, root);
    let re = PyModule::import(py, "re").unwrap();
    let escaped: Vec<String> = sample
        .iter()
        .map(|name| {
            re.call_method1("escape", (name,))
                .unwrap()
                .extract()
                .unwrap()
        })
        .collect();
    let pattern = format!(r"(?<![A-Za-z0-9_])({})(?![A-Za-z0-9_])", escaped.join("|"));
    let word = re.call_method1("compile", (pattern,)).unwrap();
    let tests: Vec<(String, String)> = tests_under(root)
        .into_iter()
        .map(|relative| {
            let bytes = fs::read(root.join(&relative)).unwrap();
            (
                relative.to_string_lossy().into_owned(),
                String::from_utf8_lossy(&bytes).into_owned(),
            )
        })
        .collect();
    let inventory: BTreeSet<String> = tests.iter().map(|(relative, _)| relative.clone()).collect();
    let live = index(py, root);
    for name in sample {
        let expected: BTreeSet<String> = tests
            .iter()
            .filter(|(_, source)| {
                let hits: Vec<String> = word
                    .call_method1("findall", (source,))
                    .unwrap()
                    .extract()
                    .unwrap();
                hits.contains(&name)
            })
            .map(|(relative, _)| relative.clone())
            .collect();
        let actual: BTreeSet<String> = paths(&live, "named_by", &name)
            .intersection(&inventory)
            .cloned()
            .collect();
        assert_eq!(actual, expected, "whole-word reference differs for {name}");
    }
}

#[test]
fn named_by_agrees_with_the_whole_word_reference() {
    let case = Case::new();
    fixture(&case);
    Python::attach(|py| {
        let fixed = index(py, case.root());
        let re = PyModule::import(py, "re").unwrap();
        let word = re
            .call_method1(
                "compile",
                (r"(?<![A-Za-z0-9_])refine_unexercised(?![A-Za-z0-9_])",),
            )
            .unwrap();
        let expected: BTreeSet<String> = tests_under(case.root())
            .into_iter()
            .filter(|relative| {
                let source = fs::read_to_string(case.root().join(relative)).unwrap();
                word.call_method1("search", (source,))
                    .unwrap()
                    .is_truthy()
                    .unwrap()
            })
            .map(|relative| relative.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            expected,
            ["pkg/test_words.py"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert_eq!(paths(&fixed, "named_by", "refine_unexercised"), expected);
        assert_eq!(
            paths(&fixed, "named_by", "refine_unexercised_extra"),
            ["pkg/test_suffix_only.py"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        sampled_live_named_by_reference(py, &repo_root());
    });
}

#[test]
fn a_package_is_named_by_its_directory() {
    let case = Case::new();
    Python::attach(|py| {
        let repo_index = module(py, "conductor.repo_index");
        for (source, expected) in [
            ("conductor/__init__.py", "conductor"),
            ("conductor/slop_gate.py", "conductor.slop_gate"),
            ("pkg/nested/__init__.py", "pkg.nested"),
            ("pkg/nested/target.py", "pkg.nested.target"),
        ] {
            let actual: String = repo_index
                .getattr("dotted_for")
                .unwrap()
                .call1((source,))
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(actual, expected);
        }
    });
    drop(case);
}

#[test]
fn the_index_is_not_degenerate() {
    let case = Case::new();
    fixture(&case);
    let live_root = repo_root();
    Python::attach(|py| {
        let fixed = index(py, case.root());
        let files = tests_under(case.root())
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(files, expected_tests());
        let count: usize = fixed.getattr("file_count").unwrap().extract().unwrap();
        assert_eq!(count, files.len());
        assert!(!paths(&fixed, "drivers_for", "pkg/from_package.py").is_empty());
        assert!(!paths(&fixed, "named_by", "hidden_helper").is_empty());
        for key in ["import_key_count", "name_key_count"] {
            let count: usize = fixed.getattr(key).unwrap().extract().unwrap();
            assert!(count > files.len(), "fixture index lost {key}");
        }
        let live_files = tests_under(&live_root);
        assert!(!live_files.is_empty());
        let live = index(py, &live_root);
        let count: usize = live.getattr("file_count").unwrap().extract().unwrap();
        assert_eq!(count, live_files.len());
        let imports: usize = live.getattr("import_key_count").unwrap().extract().unwrap();
        let names: usize = live.getattr("name_key_count").unwrap().extract().unwrap();
        assert!(imports > count, "live import index is unexpectedly sparse");
        assert!(names > count * 10, "live name index is unexpectedly sparse");
    });
}

#[test]
fn the_gate_asks_the_index_and_gets_the_same_answer() {
    let case = Case::new();
    fixture(&case);
    Python::attach(|py| {
        let fixed = index(py, case.root());
        let gate = module(py, "conductor.slop_gate");
        let actual: Vec<String> = gate
            .getattr("drivers_for")
            .unwrap()
            .call1(("pkg/from_package.py", path(py, case.root()), &fixed))
            .unwrap()
            .extract()
            .unwrap();
        let expected: Vec<String> = fixed
            .call_method1("drivers_for", ("pkg/from_package.py",))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(actual, expected);
    });
}

#[test]
fn the_cli_reports_the_root_it_actually_resolved() {
    let case = Case::new();
    fixture(&case);
    Python::attach(|py| {
        let requested = case.root().join("pkg/..");
        let (status, output) = cli(py, &args(&requested, &[]));
        assert_eq!(status, 0);
        assert!(output.contains(case.root().to_str().unwrap()));
        assert!(!output.contains("pkg/.."));
        assert!(output.contains("TestIndex"));
    });
}

fn git(root: &Path, arguments: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(arguments)
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .output()
        .expect("run isolated fixture git")
}

fn grep_args(name: &str) -> Vec<&str> {
    vec![
        "grep",
        "-l",
        "-w",
        "-F",
        name,
        "--",
        "*/test_*.py",
        "test_*.py",
    ]
}

#[test]
fn the_index_sees_untracked_tests_and_git_grep_does_not() {
    let case = Case::new();
    fixture(&case);
    assert!(git(case.root(), &["init", "-q"]).status.success());
    case.write("pkg/test_tracked.py", "def stable_word():\n    pass\n");
    assert!(git(case.root(), &["add", "--", "pkg/test_tracked.py"])
        .status
        .success());
    let tracked = git(case.root(), &grep_args("stable_word"));
    assert!(tracked.status.success());
    assert_eq!(
        String::from_utf8(tracked.stdout).unwrap().trim(),
        "pkg/test_tracked.py"
    );

    let marker = ["zz_untracked", "_marker_", "name"].concat();
    let scratch = "pkg/test_zz_index_untracked_probe.py";
    case.write(scratch, &format!("def {marker}():\n    pass\n"));
    Python::attach(|py| {
        let fresh = index(py, case.root());
        assert!(paths(&fresh, "named_by", &marker).contains(scratch));
    });
    let untracked = git(case.root(), &grep_args(&marker));
    assert_eq!(untracked.status.code(), Some(1));
    assert!(
        untracked.stdout.is_empty(),
        "Git searched an untracked file"
    );
}

#[test]
fn the_cli_answers_the_driver_question_it_was_asked() {
    let case = Case::new();
    fixture(&case);
    Python::attach(|py| {
        let (status, output) = cli(
            py,
            &args(case.root(), &["--drivers-for", "pkg/from_package.py"]),
        );
        assert_eq!(status, 0);
        assert_eq!(lines(&output), vec!["pkg/test_imports.py".to_owned()]);
    });
}

#[test]
fn the_cli_answers_the_naming_question_it_was_asked() {
    let case = Case::new();
    fixture(&case);
    Python::attach(|py| {
        let (status, output) = cli(
            py,
            &args(case.root(), &["--named-by", "refine_unexercised"]),
        );
        assert_eq!(status, 0);
        assert_eq!(lines(&output), vec!["pkg/test_words.py".to_owned()]);
        assert!(!output.contains("pkg/test_imports.py"));
    });
}

#[test]
fn an_impossible_root_is_refused_not_answered_emptily() {
    let case = Case::new();
    Python::attach(|py| {
        let repo_index = module(py, "conductor.repo_index");
        let bad = case.root().join("no-such-tree");
        let error = repo_index
            .getattr("build")
            .unwrap()
            .call1((path(py, &bad),))
            .unwrap_err();
        assert!(error
            .matches(py, &py.get_type::<pyo3::exceptions::PyNotADirectoryError>())
            .unwrap());
    });
}
