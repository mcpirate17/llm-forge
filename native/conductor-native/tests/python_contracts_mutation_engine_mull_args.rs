#![cfg(feature = "python-compat-tests")]
//! Mull timeout, coverage, build, and manifest argument contracts.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/mutation_mull_fixture.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use fixture::{campaign, campaign_error, mull, repo_src};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::path::Path;
use support::{path, Case};

fn engine_argv<'py>(
    py: Python<'py>,
    subject: &Bound<'py, pyo3::types::PyAny>,
    name: &str,
) -> Bound<'py, PyList> {
    mull(py)
        .getattr("_engine_argv")
        .unwrap()
        .call1((
            subject,
            "/bin/mull-runner-18",
            path(py, Path::new("/b/t")),
            path(py, Path::new("/b/t.profdata")),
            path(py, Path::new("/b/r")),
            name,
        ))
        .unwrap()
        .cast_into::<PyList>()
        .unwrap()
}

fn items(argv: &Bound<'_, PyList>) -> Vec<String> {
    argv.extract().unwrap()
}
fn after<'a>(argv: &'a [String], flag: &str) -> &'a str {
    &argv[argv.iter().position(|x| x == flag).unwrap() + 1]
}

#[test]
fn the_mutant_timeout_is_converted_from_seconds_to_milliseconds() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = campaign(py);
        subject.setattr("mutant_timeout_seconds", 7).unwrap();
        assert_eq!(
            after(&items(&engine_argv(py, &subject, "t")), "--timeout"),
            "7000"
        );
    });
}

#[test]
fn the_timeout_floor_is_pinned_because_these_suites_run_in_microseconds() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = campaign(py);
        subject.setattr("mutant_timeout_seconds", 7).unwrap();
        assert_eq!(
            after(&items(&engine_argv(py, &subject, "t")), "--minimum-timeout"),
            "7000"
        );
    });
}

#[test]
fn the_run_is_always_filtered_by_real_coverage_data() {
    let _case = Case::new();
    Python::attach(|py| {
        let argv = items(&engine_argv(py, &campaign(py), "t"));
        assert_eq!(after(&argv, "--coverage-info"), "/b/t.profdata");
        assert!(!argv.contains(&"--include-not-covered".to_owned()));
    });
}

#[test]
fn the_invocation_pins_every_other_bound_from_the_manifest() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = campaign(py);
        subject.setattr("jobs", 3).unwrap();
        subject
            .setattr("exclude", ("cxx_remove_void_call",))
            .unwrap();
        let argv = mull(py)
            .getattr("_engine_argv")
            .unwrap()
            .call1((
                &subject,
                "/bin/mull-runner-18",
                path(py, Path::new("/b/t")),
                path(py, Path::new("/b/t.profdata")),
                path(py, Path::new("/b/reports")),
                "kernels",
            ))
            .unwrap()
            .cast_into::<PyList>()
            .unwrap();
        let argv = items(&argv);
        assert_eq!(&argv[..2], ["/bin/mull-runner-18", "/b/t"]);
        for (flag, expected) in [
            ("--workers", "3"),
            ("--reporters", "Elements"),
            ("--report-dir", "/b/reports"),
            ("--report-name", "kernels"),
            ("--ignore-mutators", "cxx_remove_void_call"),
        ] {
            assert_eq!(after(&argv, flag), expected);
        }
    });
}

#[test]
fn the_build_carries_the_pass_plugin_and_both_instrumentations() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = campaign(py);
        let options = subject
            .getattr("options")
            .unwrap()
            .cast_into::<PyDict>()
            .unwrap();
        options
            .set_item(
                "cmake_args",
                ["-DENABLE_TESTS=ON", "-DBUILD_SHARED_LIBS=OFF"],
            )
            .unwrap();
        let raw = mull(py)
            .getattr("_configure_argv")
            .unwrap()
            .call1((
                &subject,
                path(py, &repo_src()),
                path(py, Path::new("/b")),
                "18",
            ))
            .unwrap()
            .cast_into::<PyList>()
            .unwrap();
        let argv = items(&raw);
        let source_dir: String = options
            .get_item("cmake_source_dir")
            .unwrap()
            .unwrap()
            .extract()
            .unwrap();
        let expected = repo_src().join(source_dir).to_string_lossy().into_owned();
        assert_eq!(
            &argv[..7],
            ["cmake", "-S", &expected, "-B", "/b", "-G", "Ninja"]
        );
        assert_eq!(
            &argv[argv.len() - 2..],
            ["-DENABLE_TESTS=ON", "-DBUILD_SHARED_LIBS=OFF"]
        );
        let flags: Vec<_> = argv
            .iter()
            .filter(|v| v.starts_with("-DCMAKE_CXX_FLAGS="))
            .collect();
        assert!(!flags.is_empty());
        for token in [
            "-fpass-plugin=/usr/lib/mull-ir-frontend-18",
            "-fprofile-instr-generate",
            "-fcoverage-mapping",
        ] {
            assert!(flags[0].contains(token));
        }
        assert!(argv
            .iter()
            .any(|v| v.starts_with("-DCMAKE_EXE_LINKER_FLAGS=")
                && v.contains("-fprofile-instr-generate")));
        assert!(argv.contains(&"-DCMAKE_CXX_COMPILER=clang++-18".to_owned()));
    });
}

#[test]
fn a_campaign_missing_its_toolchain_pins_is_refused() {
    let _case = Case::new();
    Python::attach(|py| {
        let api = mull(py);
        let subject = campaign(py);
        let options = subject
            .getattr("options")
            .unwrap()
            .cast_into::<PyDict>()
            .unwrap();
        options.set_item("llvm_version", py.None()).unwrap();
        campaign_error(
            py,
            api.getattr("_llvm_version")
                .unwrap()
                .call1((&subject,))
                .unwrap_err(),
            "llvm_version",
        );
        assert!(api
            .getattr("_llvm_version")
            .unwrap()
            .call1((campaign(py),))
            .unwrap()
            .eq("18")
            .unwrap());
        let subject = campaign(py);
        let options = subject
            .getattr("options")
            .unwrap()
            .cast_into::<PyDict>()
            .unwrap();
        options.del_item("cmake_source_dir").unwrap();
        campaign_error(
            py,
            api.getattr("_configure_argv")
                .unwrap()
                .call1((
                    &subject,
                    path(py, &repo_src()),
                    path(py, Path::new("/b")),
                    "18",
                ))
                .unwrap_err(),
            "cmake_source_dir",
        );
        let subject = campaign(py);
        let options = subject
            .getattr("options")
            .unwrap()
            .cast_into::<PyDict>()
            .unwrap();
        options.del_item("executables").unwrap();
        campaign_error(
            py,
            api.getattr("_executables")
                .unwrap()
                .call1((&subject,))
                .unwrap_err(),
            "executables",
        );
        let replacement = PyDict::new(py);
        replacement.set_item("executables", "test_kernels").unwrap();
        subject.setattr("options", replacement).unwrap();
        campaign_error(
            py,
            api.getattr("_executables")
                .unwrap()
                .call1((&subject,))
                .unwrap_err(),
            "executables",
        );
    });
}
