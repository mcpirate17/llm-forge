#![cfg(feature = "python-compat-tests")]
//! Anchored mutant patch application: drift tolerance, precise refusal, and dry run.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyModule};
use std::fs;
use support::{assert_error, module, path, Case};

const PATCH: &str = "diff --git a/m.py b/m.py\n--- a/m.py\n+++ b/m.py\n@@ -2,5 +2,5 @@\n def f(x):\n     y = x + 1\n-    return y\n+    return -y\n \n \n";
const ORIGINAL: &str = "import os\ndef f(x):\n    y = x + 1\n    return y\n\n\n";

fn target(case: &Case, body: &str) -> std::path::PathBuf {
    case.write("m.py", body)
}

fn apply<'py>(
    py: Python<'py>,
    code: &Bound<'py, PyModule>,
    patch: &str,
    case: &Case,
) -> PyResult<Bound<'py, PyAny>> {
    code.getattr("apply_patch_text")
        .unwrap()
        .call1((patch, path(py, case.root())))
}

fn check<'py>(
    py: Python<'py>,
    code: &Bound<'py, PyModule>,
    patch: &str,
    case: &Case,
) -> PyResult<Bound<'py, PyAny>> {
    code.getattr("check_patch_text")
        .unwrap()
        .call1((patch, path(py, case.root())))
}

fn refusal(
    py: Python<'_>,
    code: &Bound<'_, PyModule>,
    call: PyResult<Bound<'_, PyAny>>,
    message: &str,
) {
    assert_error(
        py,
        call.unwrap_err(),
        &code.getattr("PatchApplyError").unwrap(),
        message,
    );
}

#[test]
fn content_anchor_survives_line_shift_neighbour_edit_and_eof_append() {
    let case = Case::new();
    Python::attach(|py| {
        let code = module(py, "conductor.mutation_patch_apply");
        let file = target(&case, &format!("{}{}", "# pad\n".repeat(20), ORIGINAL));
        apply(py, &code, PATCH, &case).unwrap();
        let body = fs::read_to_string(&file).unwrap();
        assert!(body.contains("return -y"));
        assert!(!body.contains("return y\n"));

        let expected = "import os\ndef f(x):\n    y = x + 1  # note\n    return -y\n\n\n";
        target(&case, &ORIGINAL.replace("y = x + 1", "y = x + 1  # note"));
        apply(py, &code, PATCH, &case).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), expected);

        target(&case, &format!("{ORIGINAL}def appended():\n    return 2\n"));
        apply(py, &code, PATCH, &case).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(),
            "import os\ndef f(x):\n    y = x + 1\n    return -y\n\n\ndef appended():\n    return 2\n");
    });
}

#[test]
fn ambiguity_refuses_tie_but_header_disambiguates_and_gone_edit_refuses() {
    let case = Case::new();
    Python::attach(|py| {
        let code = module(py, "conductor.mutation_patch_apply");
        let block = "def f(x):\n    y = x + 1\n    return y\n\n\n";
        target(
            &case,
            &format!("# a\n# b\n{block}{}{block}", "# pad\n".repeat(5)),
        );
        let tie = PATCH.replace("@@ -2,5 +2,5 @@", "@@ -8,5 +8,5 @@");
        refusal(
            py,
            &code,
            apply(py, &code, &tie, &case),
            "refusing to guess",
        );
        let file = target(&case, &format!("{block}{}{block}", "# pad\n".repeat(40)));
        let second = PATCH.replace("@@ -2,5 +2,5 @@", "@@ -46,5 +46,5 @@");
        apply(py, &code, &second, &case).unwrap();
        let body = fs::read_to_string(&file).unwrap();
        assert!(body.find("return -y").unwrap() > body.find("# pad").unwrap());
        target(
            &case,
            &ORIGINAL.replace("    return y", "    return abs(y)"),
        );
        refusal(
            py,
            &code,
            apply(py, &code, PATCH, &case),
            "does not match anywhere",
        );
        target(&case, "def g(z):\n    w = z * 3\n    return y\n\n\n");
        apply(py, &code, PATCH, &case).unwrap();
        let body = fs::read_to_string(&file).unwrap();
        assert!(body.contains("return -y"));
        assert!(body.contains("w = z * 3"));
    });
}

#[test]
fn newline_and_pure_insertion_preserve_all_neighbouring_bytes() {
    let case = Case::new();
    Python::attach(|py| {
        let code = module(py, "conductor.mutation_patch_apply");
        let file = target(&case, "value = 1");
        let newline_patch = "diff --git a/m.py b/m.py\n--- a/m.py\n+++ b/m.py\n@@ -1,1 +1,1 @@\n-value = 1\n\\ No newline at end of file\n+value = 2\n\\ No newline at end of file\n";
        apply(py, &code, newline_patch, &case).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "value = 2");
        let insertion = "diff --git a/m.py b/m.py\n--- a/m.py\n+++ b/m.py\n@@ -1,3 +1,4 @@\n import os\n def f(x):\n+    assert x\n     y = x + 1\n";
        target(&case, ORIGINAL);
        apply(py, &code, insertion, &case).unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "import os\ndef f(x):\n    assert x\n    y = x + 1\n    return y\n\n\n"
        );
        let leading = "diff --git a/m.py b/m.py\n--- a/m.py\n+++ b/m.py\n@@ -1,4 +1,5 @@\n import os\n+import sys\n def f(x):\n     y = x + 1\n     return y\n";
        target(&case, &ORIGINAL.replace("import os", "import io"));
        refusal(
            py,
            &code,
            apply(py, &code, leading, &case),
            "does not match anywhere",
        );
    });
}

#[test]
fn creation_deletion_rename_and_dev_null_forms_are_refused() {
    let _case = Case::new();
    Python::attach(|py| {
        let code = module(py, "conductor.mutation_patch_apply");
        let parse = code.getattr("parse_unified_diff").unwrap();
        for marker in [
            "new file mode 100644",
            "deleted file mode 100644",
            "rename from x",
        ] {
            let patch = format!("diff --git a/m.py b/m.py\n{marker}\n--- a/m.py\n+++ b/m.py\n@@ -1,1 +1,1 @@\n-import os\n+import sys\n");
            refusal(
                py,
                &code,
                parse.call1((patch,)),
                "unsupported patch directive",
            );
        }
        for (patch, expected) in [
            ("diff --git a/m.py b/m.py\n--- /dev/null\n+++ b/m.py\n@@ -0,0 +1,1 @@\n+import sys\n", "creates a file"),
            ("diff --git a/m.py b/m.py\n--- a/m.py\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-import os\n", "deletes a file"),
        ] {
            refusal(py, &code, parse.call1((patch,)), expected);
        }
    });
}

#[test]
fn dry_run_matches_apply_without_writing_and_reports_touched_paths() {
    let case = Case::new();
    Python::attach(|py| {
        let code = module(py, "conductor.mutation_patch_apply");
        refusal(py, &code, check(py, &code, PATCH, &case), "missing file");
        let file = target(&case, ORIGINAL);
        check(py, &code, PATCH, &case).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), ORIGINAL);
        let written = apply(py, &code, PATCH, &case)
            .unwrap()
            .extract::<Vec<String>>()
            .unwrap();
        assert_eq!(written, vec!["m.py"]);
        assert_ne!(fs::read_to_string(&file).unwrap(), ORIGINAL);
    });
}
