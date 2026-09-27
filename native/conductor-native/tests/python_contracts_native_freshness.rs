#![cfg(feature = "python-compat-tests")]
//! Native freshness contracts over Rust-built synthetic checkouts.

#[path = "python_contracts/native_freshness_support.rs"]
mod freshness_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use freshness_support::{
    checkout, crates, freshness, install, lines, make_crate, standard_crate, NO_MODULE, PYPROJECT,
};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyTuple};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use support::{module, path, AttrPatch, Case};

const SERVER_BLOCK: &str = "GRAPH SERVER natives out of date in /x:\n- demo-native: 1 installed, 2 declared; `make crg-sync`";

fn finding_lines(py: Python<'_>, root: &Path, expected: &[&str]) {
    assert_eq!(lines(py, root), expected);
}

#[test]
fn a_crate_is_read_from_its_pyproject() {
    let case = Case::new();
    let crate_dir = standard_crate(case.root());
    Python::attach(|py| {
        let expected = freshness(py)
            .getattr("Crate")
            .unwrap()
            .call1((path(py, &crate_dir), "demo-native", "1.2.3", "demo_native"))
            .unwrap();
        assert!(crates(py, case.root())
            .eq(PyTuple::new(py, [expected]).unwrap())
            .unwrap());
    });
}

#[test]
fn only_a_crate_declaring_a_module_name_is_installed() {
    let case = Case::new();
    standard_crate(case.root());
    let binary = case.mkdir("tooling/native/demo-binary");
    fs::write(binary.join("pyproject.toml"), NO_MODULE).unwrap();
    let bare = case.mkdir("tooling/native/demo-bare");
    fs::write(bare.join("Cargo.toml"), "[package]\nname = \"demo-bare\"\n").unwrap();
    Python::attach(|py| {
        let names: Vec<String> = crates(py, case.root())
            .try_iter()
            .unwrap()
            .map(|one| {
                one.unwrap()
                    .getattr("distribution")
                    .unwrap()
                    .extract()
                    .unwrap()
            })
            .collect();
        assert_eq!(names, ["demo-native"]);
    });
}

#[test]
fn a_tree_with_no_native_directory_declares_nothing() {
    let case = Case::new();
    Python::attach(|py| assert!(crates(py, case.root()).eq(PyTuple::empty(py)).unwrap()));
}

#[test]
fn the_host_names_where_its_crates_are() {
    let case = Case::new();
    let crate_dir = case
        .mkdir("crates/demo-native/src")
        .parent()
        .unwrap()
        .to_path_buf();
    fs::write(crate_dir.join("pyproject.toml"), PYPROJECT).unwrap();
    case.write(
        "pyproject.toml",
        "[tool.conductor]\nnative_root = \"crates\"\n",
    );
    Python::attach(|py| {
        let names: Vec<String> = crates(py, case.root())
            .try_iter()
            .unwrap()
            .map(|one| {
                one.unwrap()
                    .getattr("distribution")
                    .unwrap()
                    .extract()
                    .unwrap()
            })
            .collect();
        assert_eq!(names, ["demo-native"]);
    });
}

#[test]
fn a_compiled_extension_is_read_from_the_record() {
    let case = Case::new();
    let info = install(case.root(), "1.2.3", None);
    fs::write(
        info.join("RECORD"),
        "demo_native/demo_native.cpython-312-x86_64-linux-gnu.so,,\ndemo_native/__init__.py,,\n",
    )
    .unwrap();
    Python::attach(|py| {
        let actual = freshness(py)
            .getattr("compiled_extensions")
            .unwrap()
            .call1((path(py, &info),))
            .unwrap();
        let expected = PyTuple::new(
            py,
            ["demo_native/demo_native.cpython-312-x86_64-linux-gnu.so"],
        )
        .unwrap();
        assert!(actual.eq(expected).unwrap());
    });
}

#[test]
fn a_distribution_from_an_index_records_no_origin() {
    let case = Case::new();
    let from_index = install(case.root(), "1.2.3", None);
    let direct = install(case.root(), "9.9.9", Some("file:///x"));
    Python::attach(|py| {
        let url = freshness(py).getattr("direct_url").unwrap();
        assert!(url
            .call1((path(py, &from_index),))
            .unwrap()
            .eq(PyDict::new(py))
            .unwrap());
        assert!(url
            .call1((path(py, &direct),))
            .unwrap()
            .get_item("url")
            .unwrap()
            .eq("file:///x")
            .unwrap());
    });
}

#[test]
fn the_make_target_names_the_directory_not_the_distribution() {
    let case = Case::new();
    make_crate(case.root(), "1.2.3", "demo-native-crate");
    Python::attach(|py| {
        let crate_obj = crates(py, case.root()).get_item(0).unwrap();
        assert!(crate_obj
            .getattr("distribution")
            .unwrap()
            .eq("demo-native")
            .unwrap());
        assert!(crate_obj
            .getattr("make_target")
            .unwrap()
            .eq("demo-native-crate")
            .unwrap());
    });
}

#[test]
fn a_dist_info_is_matched_by_escaped_name_not_by_prefix() {
    let case = Case::new();
    let packages = case.mkdir(".venv/lib/python3.12/site-packages");
    fs::create_dir(packages.join("demo_native_extras-9.9.dist-info")).unwrap();
    Python::attach(|py| {
        let info = freshness(py).getattr("dist_info").unwrap();
        assert!(info
            .call1((path(py, &packages), "demo-native"))
            .unwrap()
            .is_none());
        fs::create_dir(packages.join("demo_native-1.2.3.dist-info")).unwrap();
        let found = info.call1((path(py, &packages), "demo-native")).unwrap();
        assert!(!found.is_none());
        assert!(found
            .getattr("name")
            .unwrap()
            .eq("demo_native-1.2.3.dist-info")
            .unwrap());
    });
}

#[test]
fn a_crate_the_venv_never_installed_is_reported() {
    let case = Case::new();
    standard_crate(case.root());
    case.mkdir(".venv/lib/python3.12/site-packages");
    Python::attach(|py| {
        finding_lines(
            py,
            case.root(),
            &["demo-native: not installed in .venv; `make demo-native` builds it"],
        )
    });
}

#[test]
fn an_installed_version_behind_the_crate_is_reported() {
    let case = Case::new();
    let crate_dir = standard_crate(case.root());
    install(
        case.root(),
        "1.2.2",
        Some(&format!("file://{}", crate_dir.display())),
    );
    Python::attach(|py| {
        let crate_obj = crates(py, case.root()).get_item(0).unwrap();
        freshness(py)
            .getattr("write_stamp")
            .unwrap()
            .call1((path(py, case.root()), crate_obj))
            .unwrap();
        finding_lines(
            py,
            case.root(),
            &["demo-native: installed 1.2.2, this tree declares 1.2.3; `make demo-native`"],
        );
    });
}

#[test]
fn a_wheel_built_in_another_checkout_is_reported() {
    let case = Case::new();
    standard_crate(case.root());
    install(
        case.root(),
        "1.2.3",
        Some("file:///elsewhere/tooling/native/demo-native"),
    );
    Python::attach(|py| {
        let crate_obj = crates(py, case.root()).get_item(0).unwrap();
        freshness(py)
            .getattr("write_stamp")
            .unwrap()
            .call1((path(py, case.root()), crate_obj))
            .unwrap();
        finding_lines(py, case.root(), &["demo-native: built from /elsewhere/tooling/native/demo-native, not this checkout; `make demo-native`"]);
    });
}

#[test]
fn an_installer_that_recorded_no_source_is_not_accused() {
    let case = Case::new();
    standard_crate(case.root());
    install(case.root(), "1.2.3", None);
    Python::attach(|py| {
        let crate_obj = crates(py, case.root()).get_item(0).unwrap();
        freshness(py)
            .getattr("write_stamp")
            .unwrap()
            .call1((path(py, case.root()), crate_obj))
            .unwrap();
        finding_lines(py, case.root(), &[]);
    });
}

#[test]
fn sources_changed_since_the_build_are_reported() {
    let case = Case::new();
    let crate_dir = Python::attach(|py| checkout(&case, py));
    fs::write(crate_dir.join("src/lib.rs"), "pub fn one() -> u8 { 2 }\n").unwrap();
    Python::attach(|py| {
        finding_lines(
            py,
            case.root(),
            &["demo-native: crate sources changed since the last build; `make demo-native`"],
        )
    });
}

#[test]
fn a_build_that_left_no_stamp_is_not_accused_of_drift() {
    let case = Case::new();
    let crate_dir = standard_crate(case.root());
    install(
        case.root(),
        "1.2.3",
        Some(&format!("file://{}", crate_dir.display())),
    );
    fs::write(crate_dir.join("src/lib.rs"), "pub fn one() -> u8 { 2 }\n").unwrap();
    Python::attach(|py| finding_lines(py, case.root(), &[]));
}

#[test]
fn a_checkout_with_no_venv_has_nothing_to_compare() {
    let case = Case::new();
    standard_crate(case.root());
    Python::attach(|py| {
        let actual = freshness(py)
            .getattr("findings")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        assert!(actual.eq(PyTuple::empty(py)).unwrap());
    });
}

#[test]
fn a_venv_that_matches_the_tree_says_nothing() {
    let case = Case::new();
    Python::attach(|py| {
        checkout(&case, py);
        let actual = freshness(py)
            .getattr("findings")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        assert!(actual.eq(PyTuple::empty(py)).unwrap());
        let report = freshness(py)
            .getattr("report")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        assert!(report.eq("").unwrap());
    });
}

#[test]
fn cargos_own_build_output_is_not_a_source_change() {
    let case = Case::new();
    let crate_dir = Python::attach(|py| checkout(&case, py));
    let generated = crate_dir.join("target/release/build/dep/out");
    fs::create_dir_all(&generated).unwrap();
    fs::write(
        generated.join("generated_alias.rs"),
        "// regenerated every build\n",
    )
    .unwrap();
    Python::attach(|py| finding_lines(py, case.root(), &[]));
}

#[test]
fn the_digest_covers_the_manifests_as_well_as_the_rust() {
    let case = Case::new();
    let crate_dir = Python::attach(|py| checkout(&case, py));
    Python::attach(|py| {
        let digest = freshness(py).getattr("source_digest").unwrap();
        let before: String = digest
            .call1((path(py, &crate_dir),))
            .unwrap()
            .extract()
            .unwrap();
        fs::write(
            crate_dir.join("Cargo.toml"),
            "[package]\nname = \"demo-native\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let after: String = digest
            .call1((path(py, &crate_dir),))
            .unwrap()
            .extract()
            .unwrap();
        assert_ne!(after, before);
    });
}

#[test]
fn a_renamed_source_changes_the_digest() {
    let case = Case::new();
    let crate_dir = Python::attach(|py| checkout(&case, py));
    Python::attach(|py| {
        let digest = freshness(py).getattr("source_digest").unwrap();
        let before: String = digest
            .call1((path(py, &crate_dir),))
            .unwrap()
            .extract()
            .unwrap();
        fs::rename(crate_dir.join("src/lib.rs"), crate_dir.join("src/main.rs")).unwrap();
        let after: String = digest
            .call1((path(py, &crate_dir),))
            .unwrap()
            .extract()
            .unwrap();
        assert_ne!(after, before);
    });
}

#[test]
fn the_digest_reads_only_rust_and_manifests() {
    let case = Case::new();
    let crate_dir = Python::attach(|py| checkout(&case, py));
    Python::attach(|py| {
        let files = freshness(py)
            .getattr("source_files")
            .unwrap()
            .call1((path(py, &crate_dir),))
            .unwrap();
        let names: BTreeSet<String> = files
            .try_iter()
            .unwrap()
            .map(|file| file.unwrap().getattr("name").unwrap().extract().unwrap())
            .collect();
        assert_eq!(
            names,
            ["pyproject.toml", "Cargo.toml", "lib.rs"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
    });
}

#[test]
fn the_report_names_the_checkout_it_judged() {
    let case = Case::new();
    let crate_dir = Python::attach(|py| checkout(&case, py));
    fs::write(crate_dir.join("src/lib.rs"), "// changed\n").unwrap();
    Python::attach(|py| {
        let actual: String = freshness(py)
            .getattr("report")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap()
            .extract()
            .unwrap();
        assert!(actual.starts_with(&format!(
            "NATIVE TOOLING out of date in {}:",
            case.root().display()
        )));
    });
}

fn context<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("root", path(py, Path::new("/nonexistent")))
        .unwrap();
    kwargs.set_item("event", "SessionStart").unwrap();
    kwargs.set_item("payload", PyDict::new(py)).unwrap();
    module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn gate_patch(
    py: Python<'_>,
    adapters: &Bound<'_, pyo3::types::PyModule>,
    checkout: Option<&Path>,
) -> AttrPatch {
    let checkout = checkout.map(Path::to_path_buf);
    let session =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            assert_eq!(args.len(), 1);
            assert!(kwargs.is_none());
            match &checkout {
                Some(root) => Ok(path(args.py(), root).unbind()),
                None => Err(PyRuntimeError::new_err("no checkout")),
            }
        })
        .unwrap();
    let gate_kwargs = PyDict::new(py);
    gate_kwargs.set_item("session_checkout", session).unwrap();
    let gate = module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&gate_kwargs))
        .unwrap()
        .unbind();
    let body =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            assert_eq!(args.len(), 2);
            assert!(kwargs.is_none());
            Ok(gate.clone_ref(args.py()))
        })
        .unwrap();
    AttrPatch::replace(adapters.as_any(), "_body", body.as_any())
}

fn server_patch(py: Python<'_>, adapters: &Bound<'_, pyo3::types::PyModule>) -> AttrPatch {
    let server = adapters.getattr("crg_venv_sync").unwrap();
    let report = PyCFunction::new_closure(py, None, None, |args, kwargs| -> PyResult<String> {
        assert_eq!(args.len(), 1);
        assert!(kwargs.is_none());
        Ok(SERVER_BLOCK.to_owned())
    })
    .unwrap();
    AttrPatch::replace(&server, "session_report", report.as_any())
}

#[test]
fn the_adapter_injects_the_report_as_session_context() {
    let case = Case::new();
    let crate_dir = Python::attach(|py| checkout(&case, py));
    fs::write(crate_dir.join("src/lib.rs"), "// changed\n").unwrap();
    Python::attach(|py| {
        let adapters = module(py, "tooling.hooks.dispatch.adapters");
        let _gate = gate_patch(py, &adapters, Some(case.root()));
        let output = adapters
            .getattr("native_freshness_report")
            .unwrap()
            .call1((context(py),))
            .unwrap();
        assert!(!output.is_none());
        let specific = output.get_item("hookSpecificOutput").unwrap();
        assert!(specific
            .get_item("hookEventName")
            .unwrap()
            .eq("SessionStart")
            .unwrap());
        let detail: String = specific
            .get_item("additionalContext")
            .unwrap()
            .extract()
            .unwrap();
        assert!(detail.contains("crate sources changed since the last build"));
    });
}

#[test]
fn the_adapter_never_raises_into_a_session() {
    let _case = Case::new();
    Python::attach(|py| {
        let adapters = module(py, "tooling.hooks.dispatch.adapters");
        let _gate = gate_patch(py, &adapters, None);
        let output = adapters
            .getattr("native_freshness_report")
            .unwrap()
            .call1((context(py),))
            .unwrap();
        assert!(!output.is_none());
        let message: String = output.get_item("systemMessage").unwrap().extract().unwrap();
        assert!(message.contains("no checkout"));
        assert!(!output.contains("hookSpecificOutput").unwrap());
    });
}

#[test]
fn the_hook_is_registered_on_session_start() {
    let _case = Case::new();
    Python::attach(|py| {
        let registry = module(py, "tooling.hooks.dispatch.registry");
        let spec = registry
            .getattr("HOOKS")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(Result::unwrap)
            .find(|hook| {
                hook.getattr("name")
                    .unwrap()
                    .eq("native_freshness")
                    .unwrap()
            })
            .unwrap();
        assert!(spec.getattr("event").unwrap().eq("SessionStart").unwrap());
        assert!(spec
            .getattr("adapter")
            .unwrap()
            .eq("native_freshness_report")
            .unwrap());
        assert!(!spec
            .getattr("fail_closed")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let timeout: i64 = spec.getattr("timeout").unwrap().extract().unwrap();
        let event_timeout: i64 = registry
            .getattr("event_timeout")
            .unwrap()
            .call1(("SessionStart",))
            .unwrap()
            .extract()
            .unwrap();
        assert!(timeout <= event_timeout);
    });
}

#[test]
fn the_adapter_carries_both_interpreters() {
    let case = Case::new();
    let crate_dir = Python::attach(|py| checkout(&case, py));
    fs::write(crate_dir.join("src/lib.rs"), "// changed\n").unwrap();
    Python::attach(|py| {
        let adapters = module(py, "tooling.hooks.dispatch.adapters");
        let _gate = gate_patch(py, &adapters, Some(case.root()));
        let _server = server_patch(py, &adapters);
        let output = adapters
            .getattr("native_freshness_report")
            .unwrap()
            .call1((context(py),))
            .unwrap();
        let detail: String = output
            .get_item("hookSpecificOutput")
            .unwrap()
            .get_item("additionalContext")
            .unwrap()
            .extract()
            .unwrap();
        assert!(detail.contains("NATIVE TOOLING out of date"));
        assert!(detail.contains("GRAPH SERVER natives out of date"));
        assert!(detail.contains("\n\n"));
    });
}

#[test]
fn a_stale_graph_server_alone_is_worth_a_block() {
    let case = Case::new();
    Python::attach(|py| {
        checkout(&case, py);
        let adapters = module(py, "tooling.hooks.dispatch.adapters");
        let _gate = gate_patch(py, &adapters, Some(case.root()));
        let _server = server_patch(py, &adapters);
        let output = adapters
            .getattr("native_freshness_report")
            .unwrap()
            .call1((context(py),))
            .unwrap();
        assert!(!output.is_none());
        assert!(output
            .get_item("hookSpecificOutput")
            .unwrap()
            .get_item("additionalContext")
            .unwrap()
            .eq(SERVER_BLOCK)
            .unwrap());
    });
}
