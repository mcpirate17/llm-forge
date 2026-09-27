#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the crate version candidate check.

#[path = "python_contracts/candidate_structure_support.rs"]
#[allow(dead_code)]
mod candidate_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use candidate_support::{context, git, ChangeKind};
use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyTuple};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use support::{module, text, AttrPatch, Case};

const SOURCE_OLD: &str = "pub fn old() {}\n";
const SOURCE_NEW: &str = "pub fn new() {}\n";
const RULE_BUMP: &str = "source-changed-without-version-bump";

fn manifest(name: &str, version: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2021\"\n")
}

fn write_files(root: &Path, files: &[(&str, &str)]) {
    for (relative, contents) in files {
        let file = root.join(relative);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, contents).unwrap();
    }
}

struct Repo {
    _case: Case,
    root: PathBuf,
    snapshot: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let case = Case::new();
        let root = case.mkdir("repo");
        let snapshot = case.root().join("snapshot");
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "t@example.invalid"]);
        git(&root, &["config", "user.name", "t"]);
        Self {
            _case: case,
            root,
            snapshot,
        }
    }

    fn commit_base(&self, files: &[(&str, &str)]) -> String {
        write_files(&self.root, files);
        git(&self.root, &["add", "-A"]);
        git(&self.root, &["commit", "-qm", "base"]);
        git(&self.root, &["rev-parse", "HEAD^{tree}"])
    }

    fn run<'py>(
        &self,
        py: Python<'py>,
        base_tree: &str,
        files: &[(&str, &str)],
        changed: &[&str],
        listed_entries: Option<&[&str]>,
    ) -> Bound<'py, PyAny> {
        write_files(&self.snapshot, files);
        let mut entries = listed_entries
            .map(|listed| listed.to_vec())
            .unwrap_or_else(|| files.iter().map(|(path, _)| *path).collect());
        entries.sort_unstable();
        let ctx = context(
            py,
            &self.root,
            &self.snapshot,
            base_tree,
            changed,
            &entries,
            ChangeKind::NativeModified,
        );
        module(py, "conductor.candidate_review.crate_version")
            .getattr("check_crate_version")
            .unwrap()
            .call1((ctx,))
            .unwrap()
    }
}

fn findings<'py>(result: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    result.getattr("findings").unwrap()
}

fn rules(result: &Bound<'_, PyAny>) -> HashSet<String> {
    findings(result)
        .try_iter()
        .unwrap()
        .map(|finding| text(&finding.unwrap().getattr("rule_id").unwrap()))
        .collect()
}

fn assert_no_findings(result: &Bound<'_, PyAny>) {
    assert!(findings(result).eq(PyList::empty(result.py())).unwrap());
}

fn touched(result: &Bound<'_, PyAny>) -> i64 {
    result
        .getattr("metrics")
        .unwrap()
        .get_item("crates_touched")
        .unwrap()
        .extract()
        .unwrap()
}

fn severity<'py>(py: Python<'py>, name: &str) -> Bound<'py, PyAny> {
    module(py, "conductor.candidate_review.model")
        .getattr("Severity")
        .unwrap()
        .getattr(name)
        .unwrap()
}

fn lib_with_tests(body: &str, extra: &str) -> String {
    format!(
        "pub fn shipped() -> u32 {{\n    {body}\n}}\n\n#[cfg(test)]\nmod tests {{\n    use super::*;\n\n    #[test]\n    fn it_works() {{\n        assert_eq!(shipped(), {body});\n        let brace = \"}}\";\n        let _ = brace;\n    }}\n{extra}}}\n"
    )
}

#[test]
fn a_bumped_crate_passes() {
    let fixture = Repo::new();
    let old = manifest("unshipped-crate", "0.1.0");
    let new = manifest("unshipped-crate", "0.1.1");
    let base = fixture.commit_base(&[("crate/Cargo.toml", &old), ("crate/src/lib.rs", SOURCE_OLD)]);
    Python::attach(|py| {
        let result = fixture.run(
            py,
            &base,
            &[("crate/Cargo.toml", &new), ("crate/src/lib.rs", SOURCE_NEW)],
            &["crate/src/lib.rs", "crate/Cargo.toml"],
            None,
        );
        assert_no_findings(&result);
    });
}

#[test]
fn the_missing_bump_is_blocking() {
    let fixture = Repo::new();
    let manifest = manifest("unshipped-crate", "0.1.0");
    let base = fixture.commit_base(&[
        ("crate/Cargo.toml", &manifest),
        ("crate/src/lib.rs", SOURCE_OLD),
    ]);
    Python::attach(|py| {
        let result = fixture.run(
            py,
            &base,
            &[
                ("crate/Cargo.toml", &manifest),
                ("crate/src/lib.rs", SOURCE_NEW),
            ],
            &["crate/src/lib.rs"],
            None,
        );
        let missing = findings(&result)
            .try_iter()
            .unwrap()
            .map(Result::unwrap)
            .filter(|item| text(&item.getattr("rule_id").unwrap()) == RULE_BUMP)
            .collect::<Vec<_>>();
        assert!(!missing.is_empty());
        let critical = severity(py, "CRITICAL");
        assert!(missing
            .iter()
            .all(|item| item.getattr("severity").unwrap().is(&critical)));
        assert_eq!(
            text(&missing[0].getattr("path").unwrap()),
            "crate/Cargo.toml"
        );
    });
}

#[test]
fn a_manifest_only_change_needs_no_bump() {
    let fixture = Repo::new();
    let manifest = manifest("unshipped-crate", "0.1.0");
    let base = fixture.commit_base(&[
        ("crate/Cargo.toml", &manifest),
        ("crate/src/lib.rs", SOURCE_OLD),
    ]);
    let description = format!("{manifest}description = \"x\"\n");
    Python::attach(|py| {
        let result = fixture.run(
            py,
            &base,
            &[
                ("crate/Cargo.toml", &description),
                ("crate/src/lib.rs", SOURCE_OLD),
            ],
            &["crate/Cargo.toml"],
            None,
        );
        assert_no_findings(&result);
        assert_eq!(touched(&result), 0);
    });
}

#[test]
fn a_new_crate_is_not_asked_to_bump() {
    let fixture = Repo::new();
    let base = fixture.commit_base(&[("README.md", "x\n")]);
    let manifest = manifest("unshipped-crate", "0.1.0");
    Python::attach(|py| {
        let result = fixture.run(
            py,
            &base,
            &[
                ("crate/Cargo.toml", &manifest),
                ("crate/src/lib.rs", SOURCE_NEW),
            ],
            &["crate/src/lib.rs", "crate/Cargo.toml"],
            None,
        );
        assert_no_findings(&result);
    });
}

#[test]
fn a_workspace_member_is_charged_not_its_root() {
    let fixture = Repo::new();
    let root_old = manifest("unshipped-root", "0.1.0");
    let root_new = manifest("unshipped-root", "0.2.0");
    let member = manifest("unshipped-member", "0.1.0");
    let base = fixture.commit_base(&[
        ("Cargo.toml", &root_old),
        ("member/Cargo.toml", &member),
        ("member/src/lib.rs", SOURCE_OLD),
    ]);
    Python::attach(|py| {
        let result = fixture.run(
            py,
            &base,
            &[
                ("Cargo.toml", &root_new),
                ("member/Cargo.toml", &member),
                ("member/src/lib.rs", SOURCE_NEW),
            ],
            &["member/src/lib.rs", "Cargo.toml"],
            None,
        );
        let paths = findings(&result)
            .try_iter()
            .unwrap()
            .map(|item| text(&item.unwrap().getattr("path").unwrap()))
            .collect::<HashSet<_>>();
        assert_eq!(paths, HashSet::from(["member/Cargo.toml".to_owned()]));
    });
}

#[test]
fn a_non_rust_change_in_a_crate_is_ignored() {
    let fixture = Repo::new();
    let manifest = manifest("unshipped-crate", "0.1.0");
    let base = fixture.commit_base(&[
        ("crate/Cargo.toml", &manifest),
        ("crate/src/lib.rs", SOURCE_OLD),
    ]);
    Python::attach(|py| {
        let result = fixture.run(
            py,
            &base,
            &[
                ("crate/Cargo.toml", &manifest),
                ("crate/src/lib.rs", SOURCE_OLD),
                ("crate/NOTES.md", "x\n"),
            ],
            &["crate/NOTES.md"],
            None,
        );
        assert_no_findings(&result);
    });
}

#[test]
fn a_c_source_change_counts_as_a_build_input() {
    let fixture = Repo::new();
    let manifest = manifest("unshipped-crate", "0.1.0");
    let base = fixture.commit_base(&[
        ("crate/Cargo.toml", &manifest),
        ("crate/src/shim.c", "int old(void) { return 0; }\n"),
    ]);
    Python::attach(|py| {
        let result = fixture.run(
            py,
            &base,
            &[
                ("crate/Cargo.toml", &manifest),
                ("crate/src/shim.c", "int new(void) { return 1; }\n"),
            ],
            &["crate/src/shim.c"],
            None,
        );
        assert!(rules(&result).contains(RULE_BUMP));
    });
}

#[test]
fn an_inherited_workspace_version_is_reported_not_passed() {
    let fixture = Repo::new();
    let inherited = "[package]\nname = \"unshipped-crate\"\nversion.workspace = true\n";
    let base = fixture.commit_base(&[
        ("crate/Cargo.toml", inherited),
        ("crate/src/lib.rs", SOURCE_OLD),
    ]);
    Python::attach(|py| {
        let result = fixture.run(
            py,
            &base,
            &[
                ("crate/Cargo.toml", inherited),
                ("crate/src/lib.rs", SOURCE_NEW),
            ],
            &["crate/src/lib.rs"],
            None,
        );
        assert_eq!(
            rules(&result),
            HashSet::from(["unresolvable-crate-version".to_owned()])
        );
    });
}

#[test]
fn an_unreadable_manifest_is_reported_not_skipped() {
    let fixture = Repo::new();
    let manifest = manifest("unshipped-crate", "0.1.0");
    let base = fixture.commit_base(&[
        ("crate/Cargo.toml", &manifest),
        ("crate/src/lib.rs", SOURCE_OLD),
    ]);
    Python::attach(|py| {
        let result = fixture.run(
            py,
            &base,
            &[("crate/src/lib.rs", SOURCE_NEW)],
            &["crate/src/lib.rs"],
            Some(&["crate/Cargo.toml", "crate/src/lib.rs"]),
        );
        assert_eq!(
            rules(&result),
            HashSet::from(["unreadable-manifest".to_owned()])
        );
    });
}

#[test]
fn installed_drift_is_reported_against_the_running_interpreter() {
    let fixture = Repo::new();
    let module_name = "conductor.candidate_review.crate_version";
    let old = manifest("unshipped-crate", "0.1.0");
    let new = manifest("unshipped-crate", "0.1.1");
    let base = fixture.commit_base(&[("crate/Cargo.toml", &old), ("crate/src/lib.rs", SOURCE_OLD)]);
    Python::attach(|py| {
        let review = module(py, module_name);
        let installed = PyCFunction::new_closure(
            py,
            None,
            None,
            |args: &Bound<'_, PyTuple>, kw: Option<&Bound<'_, PyDict>>| -> PyResult<String> {
                let named = kw.map(|kw| kw.get_item("name")).transpose()?.flatten();
                if args.len() > 1
                    || kw.is_some_and(|kw| kw.len() != usize::from(named.is_some()))
                    || (!args.is_empty() && named.is_some())
                    || (args.is_empty() && named.is_none())
                {
                    return Err(PyTypeError::new_err("installed_version takes one name"));
                }
                Ok("0.9.9".to_owned())
            },
        )
        .unwrap();
        let _patch = AttrPatch::replace(&review, "installed_version", installed.as_any());
        let result = fixture.run(
            py,
            &base,
            &[("crate/Cargo.toml", &new), ("crate/src/lib.rs", SOURCE_NEW)],
            &["crate/src/lib.rs", "crate/Cargo.toml"],
            None,
        );
        let drift = findings(&result)
            .try_iter()
            .unwrap()
            .map(Result::unwrap)
            .filter(|item| text(&item.getattr("rule_id").unwrap()) == "installed-version-drift")
            .collect::<Vec<_>>();
        assert!(!drift.is_empty());
        assert!(drift[0]
            .getattr("severity")
            .unwrap()
            .is(severity(py, "HIGH")));
        assert!(text(&drift[0].getattr("message").unwrap()).contains("0.9.9"));
    });
}

#[test]
fn an_uninstalled_crate_reports_no_drift() {
    let fixture = Repo::new();
    let old = manifest("unshipped-crate", "0.1.0");
    let new = manifest("unshipped-crate", "0.1.1");
    let base = fixture.commit_base(&[("crate/Cargo.toml", &old), ("crate/src/lib.rs", SOURCE_OLD)]);
    Python::attach(|py| {
        let result = fixture.run(
            py,
            &base,
            &[("crate/Cargo.toml", &new), ("crate/src/lib.rs", SOURCE_NEW)],
            &["crate/src/lib.rs", "crate/Cargo.toml"],
            None,
        );
        assert!(!rules(&result).contains("installed-version-drift"));
    });
}

#[test]
fn the_deepest_crate_owns_a_nested_source() {
    let _case = Case::new();
    Python::attach(|py| {
        let review = module(py, "conductor.candidate_review.crate_version");
        let owning = review.getattr("_owning_crate").unwrap();
        for (source, expected) in [
            ("member/inner/src/lib.rs", "member/inner"),
            ("member/src/lib.rs", "member"),
            ("top.rs", ""),
        ] {
            let actual: String = owning
                .call1((source, vec!["", "member", "member/inner"]))
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(actual, expected);
        }
    });
}

#[test]
fn a_source_outside_every_crate_is_charged_to_nobody() {
    let _case = Case::new();
    Python::attach(|py| {
        let review = module(py, "conductor.candidate_review.crate_version");
        let actual: Option<String> = review
            .getattr("_owning_crate")
            .unwrap()
            .call1(("elsewhere/src/lib.rs", vec!["member"]))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(actual, None);
    });
}

#[test]
fn adding_an_inline_test_does_not_demand_a_bump() {
    let fixture = Repo::new();
    let manifest = manifest("unshipped-crate", "0.1.0");
    let old = lib_with_tests("1", "");
    let new = lib_with_tests("1", "\n    #[test]\n    fn also() { assert!(true); }\n");
    let base = fixture.commit_base(&[("crate/Cargo.toml", &manifest), ("crate/src/lib.rs", &old)]);
    Python::attach(|py| {
        let result = fixture.run(
            py,
            &base,
            &[("crate/Cargo.toml", &manifest), ("crate/src/lib.rs", &new)],
            &["crate/src/lib.rs"],
            None,
        );
        assert_no_findings(&result);
        assert_eq!(touched(&result), 0);
    });
}

#[test]
fn shipped_code_beside_a_test_module_still_demands_a_bump() {
    let fixture = Repo::new();
    let manifest = manifest("unshipped-crate", "0.1.0");
    let old = lib_with_tests("1", "");
    let new = lib_with_tests("2", "");
    let base = fixture.commit_base(&[("crate/Cargo.toml", &manifest), ("crate/src/lib.rs", &old)]);
    Python::attach(|py| {
        let result = fixture.run(
            py,
            &base,
            &[("crate/Cargo.toml", &manifest), ("crate/src/lib.rs", &new)],
            &["crate/src/lib.rs"],
            None,
        );
        assert!(rules(&result).contains(RULE_BUMP));
    });
}

#[test]
fn an_integration_test_directory_is_not_a_build_input() {
    let fixture = Repo::new();
    let manifest = manifest("unshipped-crate", "0.1.0");
    let old = "#[test]\nfn a() {}\n";
    let new = "#[test]\nfn a() { assert!(true); }\n";
    let base = fixture.commit_base(&[("crate/Cargo.toml", &manifest), ("crate/tests/it.rs", old)]);
    Python::attach(|py| {
        let result = fixture.run(
            py,
            &base,
            &[("crate/Cargo.toml", &manifest), ("crate/tests/it.rs", new)],
            &["crate/tests/it.rs"],
            None,
        );
        assert_no_findings(&result);
    });
}

#[test]
fn an_unparseable_source_demands_the_bump() {
    let fixture = Repo::new();
    let manifest = manifest("unshipped-crate", "0.1.0");
    let base = fixture.commit_base(&[
        ("crate/Cargo.toml", &manifest),
        ("crate/src/lib.rs", "#[cfg(test)]\nmod t { fn a() {}\n"),
    ]);
    Python::attach(|py| {
        let result = fixture.run(
            py,
            &base,
            &[
                ("crate/Cargo.toml", &manifest),
                ("crate/src/lib.rs", "#[cfg(test)]\nmod t { fn b() {}\n"),
            ],
            &["crate/src/lib.rs"],
            None,
        );
        assert!(rules(&result).contains(RULE_BUMP));
    });
}

#[test]
fn a_new_source_file_is_a_build_input() {
    let fixture = Repo::new();
    let manifest = manifest("unshipped-crate", "0.1.0");
    let old = "pub fn a() {}\n";
    let base = fixture.commit_base(&[("crate/Cargo.toml", &manifest), ("crate/src/lib.rs", old)]);
    Python::attach(|py| {
        let result = fixture.run(
            py,
            &base,
            &[
                ("crate/Cargo.toml", &manifest),
                ("crate/src/lib.rs", old),
                ("crate/src/added.rs", "#[cfg(test)]\nmod t {}\n"),
            ],
            &["crate/src/added.rs"],
            None,
        );
        assert!(rules(&result).contains(RULE_BUMP));
    });
}

#[test]
fn a_raw_string_holding_a_brace_does_not_derail_the_stripper() {
    let _case = Case::new();
    let source = "pub fn a() {}\n#[cfg(test)]\nmod t {\n  const S: &str = r#\"a \" b }\"#;\n}\npub fn b() {}\n";
    Python::attach(|py| {
        let review = module(py, "conductor.candidate_review.crate_version");
        let actual: String = review
            .getattr("_strip_test_modules")
            .unwrap()
            .call1((source,))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(actual, "pub fn a() {}\n\npub fn b() {}\n");
    });
}

#[test]
fn a_commented_brace_does_not_derail_the_stripper() {
    let _case = Case::new();
    let source = "pub fn a() {}\n#[cfg(test)]\nmod t {\n  // }\n  /* } */\n}\npub fn b() {}\n";
    Python::attach(|py| {
        let review = module(py, "conductor.candidate_review.crate_version");
        let actual: String = review
            .getattr("_strip_test_modules")
            .unwrap()
            .call1((source,))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(actual, "pub fn a() {}\n\npub fn b() {}\n");
    });
}

#[test]
fn a_non_block_cfg_test_item_is_stripped_at_its_semicolon() {
    let _case = Case::new();
    let source = "#[cfg(test)]\nuse std::fmt;\npub fn a() {}\n";
    Python::attach(|py| {
        let review = module(py, "conductor.candidate_review.crate_version");
        let actual: String = review
            .getattr("_strip_test_modules")
            .unwrap()
            .call1((source,))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(actual, "\npub fn a() {}\n");
    });
}
