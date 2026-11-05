use super::*;
use crate::mutation_run_test_support::{campaign, change, repository, write};
use regex::Regex;
use serde_json::json;
use std::collections::BTreeMap;
use std::os::unix::fs::symlink;

/// A campaign over `sources`, each pinned, with no test the diff can touch.
fn scoped(sources: &[&str], engine: &str) -> (tempfile::TempDir, Campaign) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut subject = campaign(dir.path(), json!({"mutation_engine": engine}));
    set_sources(&mut subject, sources);
    subject.source_sha256 = pins(sources);
    subject.test_sha256 = BTreeMap::new();
    (dir, subject)
}

fn set_sources(subject: &mut Campaign, sources: &[&str]) {
    subject.source = sources.iter().map(|source| source.to_string()).collect();
}

fn pins(paths: &[&str]) -> BTreeMap<String, String> {
    paths
        .iter()
        .map(|path| (path.to_string(), "pin".to_string()))
        .collect()
}

fn validate(subject: &Campaign, root: &Path, only: &[&str]) -> Result<Vec<String>, String> {
    let only: Vec<String> = only.iter().map(|path| path.to_string()).collect();
    validate_run_scope(subject, root, "lane", "main", &only)
}

fn refused(subject: &Campaign, root: &Path, only: &[&str], part: &str) {
    let error = validate(subject, root, only).expect_err("refused");
    assert!(error.contains(part), "{error}");
}

#[test]
fn agent_scope_and_explicit_selection_cannot_admit_a_neighbour() {
    let repo = repository(&["mine.py", "other.py"]);
    let root = repo.path();
    change(root, &["mine.py"]);
    let (_dir, mut subject) = scoped(&["mine.py", "other.py"], "fest");

    refused(&subject, root, &[], "outside this agent's changes");
    refused(&subject, root, &["other.py"], "--only names files outside");
    set_sources(&mut subject, &["mine.py"]);
    subject.source_sha256 = pins(&["mine.py"]);
    assert_eq!(
        validate(&subject, root, &["mine.py"]),
        Ok(vec!["mine.py".to_string()])
    );

    change(root, &["other.py"]);
    set_sources(&mut subject, &["mine.py", "other.py"]);
    subject.source_sha256 = pins(&["mine.py", "other.py"]);
    assert_eq!(
        validate(&subject, root, &[]).expect("both changed").len(),
        2
    );
    refused(&subject, root, &["mine.py"], "other.py");
}

#[test]
fn a_changed_test_admits_only_its_named_source() {
    let repo = repository(&["subject.py", "other.py", "tests/test_subject.py"]);
    let root = repo.path();
    change(root, &["tests/test_subject.py"]);
    let (_dir, mut subject) = scoped(&["subject.py", "other.py"], "fest");
    subject.test_sha256 = pins(&["tests/test_subject.py"]);

    refused(&subject, root, &[], "other.py");
    set_sources(&mut subject, &["subject.py"]);
    subject.source_sha256 = pins(&["subject.py"]);
    assert_eq!(
        validate(&subject, root, &[]),
        Ok(vec!["subject.py".to_string()])
    );

    subject.test_sha256 = BTreeMap::new();
    refused(&subject, root, &[], "subject.py");
}

#[test]
fn a_test_named_by_two_sources_admits_neither_beyond_its_own_directory() {
    let repo = repository(&["a/subject.py", "b/subject.py", "a/test_subject.py"]);
    let root = repo.path();
    change(root, &["a/test_subject.py"]);
    let (_dir, mut subject) = scoped(&["a/subject.py", "b/subject.py"], "fest");
    subject.test_sha256 = pins(&["a/test_subject.py"]);

    refused(&subject, root, &[], "b/subject.py");
    set_sources(&mut subject, &["a/subject.py"]);
    subject.source_sha256 = pins(&["a/subject.py"]);
    assert_eq!(
        validate(&subject, root, &[]),
        Ok(vec!["a/subject.py".to_string()])
    );
}

#[test]
fn a_rust_package_relative_path_is_checked_against_the_repo_relative_diff() {
    let repo = repository(&["crate/src/lib.rs", "src/lib.rs"]);
    let root = repo.path();
    change(root, &["crate/src/lib.rs"]);
    let (_dir, mut subject) = scoped(&["crate/src/lib.rs"], "cargo-mutants");
    set_sources(&mut subject, &["src/lib.rs"]);
    subject
        .options
        .insert("package_root".into(), json!("crate"));

    assert_eq!(
        validate(&subject, root, &[]),
        Ok(vec!["crate/src/lib.rs".to_string()])
    );

    set_sources(&mut subject, &["src/**/*.rs"]);
    refused(&subject, root, &[], "exact repository files");

    let (_dir, unrooted) = scoped(&["src/lib.rs"], "cargo-mutants");
    refused(&unrooted, root, &[], "outside this agent's changes");
}

#[test]
fn a_symlink_escape_and_an_unpinned_source_refuse() {
    let outside = tempfile::tempdir().expect("tempdir");
    write(outside.path(), "outside.py", "x = 2\n");
    let repo = repository(&["mine.py", "nested/kept.py"]);
    let root = repo.path();
    change(root, &["mine.py"]);
    symlink(outside.path().join("outside.py"), root.join("link.py")).expect("symlink");
    let (_dir, mut subject) = scoped(&["mine.py"], "fest");

    set_sources(&mut subject, &["nested/../mine.py"]);
    refused(&subject, root, &[], "exact repository files");
    set_sources(&mut subject, &["link.py"]);
    refused(&subject, root, &[], "outside the checkout");
    set_sources(&mut subject, &["mine.py"]);
    subject.source_sha256 = BTreeMap::new();
    refused(&subject, root, &[], "pinned");
    set_sources(&mut subject, &[]);
    refused(&subject, root, &[], "pinned");
    for bad in [
        "*.py",
        "?.py",
        "[a].py",
        "../escape.py",
        "/tmp/escape.py",
        "missing.py",
    ] {
        set_sources(&mut subject, &[bad]);
        refused(&subject, root, &[], "mutation scope");
    }
}

fn config_lines(subject: &Campaign, root: &Path) -> Vec<String> {
    let path = mull_scope_config(subject, root).expect("config");
    fs::read_to_string(path)
        .expect("read")
        .lines()
        .map(str::to_string)
        .collect()
}

fn include_pattern(line: &str) -> Regex {
    let quoted = line.trim().strip_prefix("- ").expect("list item");
    let pattern: String = serde_json::from_str(quoted).expect("json scalar");
    Regex::new(&pattern).expect("regex")
}

#[test]
fn the_mull_config_filters_before_execution_and_escapes_regex() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical");
    write(&root, "src/a+.cc", "int a;\n");
    let (_manifest, mut subject) = scoped(&["src/a+.cc"], "mull");
    subject.operators = vec!["cxx_add_to_sub".to_string()];

    let lines = config_lines(&subject, &root);

    assert_eq!(lines[0], "includePaths:");
    let pattern = include_pattern(&lines[1]);
    let expected = root.join("src/a+.cc").to_string_lossy().into_owned();
    assert!(pattern.is_match(&expected));
    for other in [
        root.join("src/aa.cc").to_string_lossy().into_owned(),
        format!("{expected}.extra"),
        format!("prefix{expected}"),
    ] {
        assert!(!pattern.is_match(&other), "{other}");
    }
    assert_eq!(&lines[2..], ["mutators:", "  - \"cxx_add_to_sub\""]);

    subject.operators = Vec::new();
    assert!(!config_lines(&subject, &root)
        .join("\n")
        .contains("mutators"));

    set_sources(&mut subject, &[]);
    let error = mull_scope_config(&subject, &root).expect_err("no source");
    assert!(error.contains("at least one"), "{error}");
}

#[test]
fn mull_globs_are_expanded_to_pins_before_the_ownership_check() {
    let repo = repository(&["src/one.cc", "src/two.cc"]);
    let root = repo.path();
    change(root, &["src/one.cc"]);
    let (_dir, mut subject) = scoped(&["src/one.cc", "src/two.cc"], "mull");
    set_sources(&mut subject, &["src/**"]);

    refused(&subject, root, &[], "src/two.cc");

    change(root, &["src/two.cc"]);
    let expected = vec!["src/one.cc".to_string(), "src/two.cc".to_string()];
    assert_eq!(validate(&subject, root, &[]), Ok(expected));
    assert_eq!(config_lines(&subject, root).len(), 3);

    set_sources(&mut subject, &["missing/**"]);
    let error = mull_scope_config(&subject, root).expect_err("no pin");
    assert!(error.contains("no pinned target"), "{error}");
}

#[test]
fn python_escape_matches_re_escape() {
    assert_eq!(python_escape("/a-b.c d"), "/a\\-b\\.c\\ d");
    assert_eq!(python_escape("plain_name/1"), "plain_name/1");
}

#[test]
fn unselected_supporting_source_pins_are_refused() {
    let repo = repository(&["mine.py", "helper.py"]);
    let root = repo.path();
    change(root, &["mine.py"]);
    let (_dir, mut subject) = scoped(&["mine.py"], "fest");
    subject.source_sha256 = pins(&["mine.py", "helper.py"]);
    refused(&subject, root, &[], "supporting pins are unsupported");
}
