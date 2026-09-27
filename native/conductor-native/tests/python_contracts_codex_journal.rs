#![cfg(feature = "python-compat-tests")]
//! Journal filtering and presentation contracts, with all Git calls mocked.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::exceptions::PyAssertionError;
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyTuple};
use std::sync::{Arc, Mutex};
use support::{module, AttrPatch, Case};

#[test]
fn status_lines_passes_pathspecs_and_filters_sensitive_paths() {
    let _case = Case::new();
    let calls = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    Python::attach(|py| {
        let journal = module(py, "conductor.codex_journal");
        let observed = calls.clone();
        let fake = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>, _: Option<&Bound<'_, PyDict>>| -> PyResult<&str> {
                let (argv,): (Vec<String>,) = args.extract()?;
                observed.lock().unwrap().push(argv);
                Ok(" M AGENTS.md\n M secret_token.txt\n M research/lab_notebook.db")
            },
        )
        .unwrap();
        let _git = AttrPatch::replace(journal.as_any(), "_run_git", fake.as_any());
        let result: Vec<String> = journal
            .call_method1("_status_lines", (vec!["AGENTS.md"],))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(result, vec![" M AGENTS.md"]);
    });
    assert_eq!(
        *calls.lock().unwrap(),
        vec![vec!["status", "--short", "--", "AGENTS.md"]]
    );
}

#[test]
fn capped_status_lines_reports_omitted_count() {
    let _case = Case::new();
    Python::attach(|py| {
        let status = vec![" M a.py", " M b.py", " M c.py"];
        let result: Vec<String> = module(py, "conductor.codex_journal")
            .call_method1("_capped_status_lines", (status, 2))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(result, vec![" M a.py", " M b.py", "... 1 more non-protected changes omitted; use --path or --max-status to include them."]);
    });
}

#[test]
fn build_entry_accepts_path_scope_and_status_cap() {
    let _case = Case::new();
    Python::attach(|py| {
        let journal = module(py, "conductor.codex_journal");
        let fake = PyCFunction::new_closure(
            py,
            None,
            None,
            |args: &Bound<'_, PyTuple>, _: Option<&Bound<'_, PyDict>>| -> PyResult<&str> {
                let (argv,): (Vec<String>,) = args.extract()?;
                match argv
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice()
                {
                    ["branch", "--show-current"] => Ok("master"),
                    ["rev-parse", "--short", "HEAD"] => Ok("abc1234"),
                    ["status", "--short", "--", "AGENTS.md", "Makefile"] => {
                        Ok(" M AGENTS.md\n M Makefile\n M conductor/codex_journal.py")
                    }
                    _ => Err(PyAssertionError::new_err(format!(
                        "unexpected git args: {argv:?}"
                    ))),
                }
            },
        )
        .unwrap();
        let _git = AttrPatch::replace(journal.as_any(), "_run_git", fake.as_any());
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("paths", vec!["AGENTS.md", "Makefile"])
            .unwrap();
        kwargs.set_item("max_status", 2).unwrap();
        let test_command = "pytest conductor/tests/test_codex_journal.py -q";
        let entry: String = journal
            .getattr("build_entry")
            .unwrap()
            .call(("Scoped journal", vec![test_command]), Some(&kwargs))
            .unwrap()
            .extract()
            .unwrap();
        for expected in [
            "- Branch: `master`",
            "- HEAD: `abc1234`",
            "- ` M AGENTS.md`",
            "- ` M Makefile`",
            "1 more non-protected changes omitted",
            test_command,
        ] {
            assert!(
                entry.contains(expected),
                "missing {expected:?} in {entry:?}"
            );
        }
    });
}
