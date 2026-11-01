#![cfg(target_os = "linux")]
#[path = "support/candidate_clang_fixture.rs"]
#[allow(dead_code)]
mod candidate_clang_fixture;
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_clang_fixture::{assert_code, run, Private};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::PathBuf,
    process::{Command, Output},
};
const OLD: &str = "pub fn value() -> u32 { 1 }\n";
const NEW: &str = "pub fn value() -> u32 { 2 }\n";
const BUMP: &str = "source-changed-without-version-bump";
fn manifest(version: &str) -> String {
    format!("[package]\nname='Example_Pkg'\nversion='{version}'\n")
}
fn fixture(
    base_version: Option<&str>,
    version: &str,
    base_source: Option<&str>,
    source: Option<&str>,
) -> Private {
    let old = base_version.map(manifest);
    let current = manifest(version);
    let mut base = Vec::new();
    let mut target = vec![("crate/Cargo.toml", current.as_bytes())];
    if let Some(old) = &old {
        base.push(("crate/Cargo.toml", old.as_bytes()));
    }
    if let Some(source) = base_source {
        base.push(("crate/src/lib.rs", source.as_bytes()));
    }
    if let Some(source) = source {
        target.push(("crate/src/lib.rs", source.as_bytes()));
    }
    Private::new(&base, &target)
}
fn command(f: &Private, execute: bool, files: &[&str]) -> Command {
    let options: &[&str] = if execute { &["--execute"] } else { &[] };
    let mut command = f.tool_command_options("crate-version", options, files);
    command
        .env("FORGE_CRATE_INSTALLED_ROOTS", "[]")
        .env("PYO3_PYTHON", "/forbidden-python")
        .env("CONDUCTOR_PYTHON", "/forbidden-python");
    command
}
fn report(output: Output, code: i32, execute: bool) -> Value {
    assert_code(&output, code);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["check"], "crate-version");
    assert_eq!(value["executed"], execute);
    assert!(value["plan"]["selected_files"].is_array());
    assert!(value["plan"]["manifest_paths"].is_array());
    assert!(value["plan"]["installed_metadata"].is_array());
    if execute {
        assert!(value["result"]["findings"].is_array());
    } else {
        assert!(value["result"].is_null());
    }
    value
}
fn rules(value: &Value) -> Vec<&str> {
    value["result"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["rule_id"].as_str().unwrap())
        .collect()
}
#[test]
fn version_probe_has_no_git_or_installed_roots_requirement() {
    let host = support::Host::new();
    let mut command = host.command();
    command
        .env_clear()
        .env("PATH", "")
        .env("HOME", &host.0)
        .env("FORGE_LEDGER_DISABLE", "1")
        .args(["candidate", "crate-version", "--repo"])
        .arg(&host.0)
        .arg("--tool-version");
    let output = run(command);
    assert_code(&output, 0);
    assert!(!String::from_utf8(output.stdout).unwrap().trim().is_empty());
}
#[test]
fn dry_and_execute_use_captured_candidate_bytes_despite_dirty_originals() {
    let f = fixture(Some("1.0.0"), "1.0.1", Some(OLD), Some(NEW));
    let before = f.case.source_state();
    let repo = f.case.fixture.root.join("repo");
    fs::write(repo.join("crate/Cargo.toml"), b"dirty invalid manifest\n").unwrap();
    fs::write(repo.join("crate/src/lib.rs"), b"dirty invalid source\n").unwrap();
    let dry = report(run(command(&f, false, &["crate/src/lib.rs"])), 0, false);
    assert_eq!(dry["plan"]["candidate_tree_oid"], f.tree);
    assert_eq!(dry["plan"]["selected_files"], json!(["crate/src/lib.rs"]));
    assert_eq!(dry["plan"]["manifest_paths"], json!(["crate/Cargo.toml"]));
    assert_eq!(dry["plan"]["installed_metadata"], json!([]));
    let actual = report(run(command(&f, true, &["crate/src/lib.rs"])), 0, true);
    assert_eq!(actual["result"]["files"], json!(["crate/Cargo.toml"]));
    assert_eq!(
        actual["result"]["metrics"],
        json!({"crates_indexed":1,"crates_touched":1})
    );
    assert!(rules(&actual).is_empty());
    assert_eq!(
        fs::read(repo.join("crate/Cargo.toml")).unwrap(),
        b"dirty invalid manifest\n"
    );
    assert_eq!(
        fs::read(repo.join("crate/src/lib.rs")).unwrap(),
        b"dirty invalid source\n"
    );
    f.case.assert_preserved(&before);
}
#[test]
fn same_version_deleted_source_and_invalid_rust_still_demand_a_bump() {
    for source in [Some(NEW), None, Some("pub fn broken( {\n")] {
        let f = fixture(Some("1.0.0"), "1.0.0", Some(OLD), source);
        let before = f.case.source_state();
        let actual = report(run(command(&f, true, &["crate/src/lib.rs"])), 1, true);
        assert_eq!(rules(&actual), [BUMP]);
        assert_eq!(actual["result"]["findings"][0]["severity"], "critical");
        f.case.assert_preserved(&before);
    }
}
#[test]
fn new_crate_manifest_only_and_test_only_changes_do_not_require_another_bump() {
    let new = fixture(None, "1.0.0", None, Some(NEW));
    let before = new.case.source_state();
    assert!(rules(&report(
        run(command(&new, true, &["crate/src/lib.rs"])),
        0,
        true
    ))
    .is_empty());
    new.case.assert_preserved(&before);
    let f = fixture(Some("1.0.0"), "1.0.1", Some(OLD), Some(OLD));
    let before = f.case.source_state();
    let actual = report(run(command(&f, true, &["crate/Cargo.toml"])), 0, true);
    assert_eq!(actual["result"]["metrics"]["crates_touched"], 0);
    f.case.assert_preserved(&before);
    let old = "pub fn value() {}\n#[cfg(test)] mod tests { #[test] fn first() {} }\n";
    let current = "pub fn value() {}\n#[cfg(test)] mod tests { #[test] fn first() {} #[test] fn second() {} }\n";
    let f = fixture(Some("1.0.0"), "1.0.0", Some(old), Some(current));
    let before = f.case.source_state();
    let actual = report(run(command(&f, true, &["crate/src/lib.rs"])), 0, true);
    assert_eq!(actual["result"]["metrics"]["crates_touched"], 0);
    assert!(rules(&actual).is_empty());
    f.case.assert_preserved(&before);
}
#[test]
fn deepest_crate_owns_build_inputs_and_nonartifact_or_outside_paths_are_ignored() {
    let root = "[workspace]\nmembers=['crate']\n";
    let crate_manifest = manifest("1.0.0");
    let base = [
        ("Cargo.toml", root.as_bytes()),
        ("crate/Cargo.toml", crate_manifest.as_bytes()),
        ("crate/src/lib.rs", OLD.as_bytes()),
        ("crate/tests/it.rs", OLD.as_bytes()),
        ("crate/examples/ex.c", b"old\n"),
    ];
    let target = [
        ("Cargo.toml", root.as_bytes()),
        ("crate/Cargo.toml", crate_manifest.as_bytes()),
        ("crate/src/lib.rs", NEW.as_bytes()),
        ("crate/tests/it.rs", NEW.as_bytes()),
        ("crate/examples/ex.c", b"new\n"),
    ];
    let f = Private::new(&base, &target);
    let before = f.case.source_state();
    let actual = report(
        run(command(
            &f,
            true,
            &[
                "crate/tests/it.rs",
                "crate/examples/ex.c",
                "crate/src/lib.rs",
            ],
        )),
        1,
        true,
    );
    assert_eq!(
        actual["plan"]["manifest_paths"],
        json!(["Cargo.toml", "crate/Cargo.toml"])
    );
    assert_eq!(actual["result"]["files"], json!(["crate/Cargo.toml"]));
    assert_eq!(
        actual["result"]["metrics"],
        json!({"crates_indexed":2,"crates_touched":1})
    );
    assert_eq!(rules(&actual), [BUMP]);
    f.case.assert_preserved(&before);
    let f = Private::new(
        &[
            ("crate/Cargo.toml", crate_manifest.as_bytes()),
            ("outside/x.rs", OLD.as_bytes()),
        ],
        &[
            ("crate/Cargo.toml", crate_manifest.as_bytes()),
            ("outside/x.rs", NEW.as_bytes()),
        ],
    );
    let before = f.case.source_state();
    let actual = report(run(command(&f, true, &["outside/x.rs"])), 0, true);
    assert_eq!(actual["result"]["metrics"]["crates_touched"], 0);
    assert!(rules(&actual).is_empty());
    f.case.assert_preserved(&before);
}
#[test]
fn inherited_and_malformed_versions_are_findings_instead_of_a_clean_pass() {
    for current in [
        "[package]\nname='Example_Pkg'\nversion.workspace=true\n",
        "[package malformed\n",
    ] {
        let base = manifest("1.0.0");
        let f = Private::new(
            &[
                ("crate/Cargo.toml", base.as_bytes()),
                ("crate/src/lib.rs", OLD.as_bytes()),
            ],
            &[
                ("crate/Cargo.toml", current.as_bytes()),
                ("crate/src/lib.rs", NEW.as_bytes()),
            ],
        );
        let before = f.case.source_state();
        let actual = report(run(command(&f, true, &["crate/src/lib.rs"])), 1, true);
        assert_eq!(rules(&actual), ["unresolvable-crate-version"]);
        f.case.assert_preserved(&before);
    }
}
fn site(f: &Private, label: &str, version: &str) -> (PathBuf, PathBuf, Vec<u8>) {
    let root = f.case.fixture.root.join(label);
    let directory = root.join(format!("example_pkg-{version}.dist-info"));
    fs::create_dir_all(&directory).unwrap();
    for path in [&root, &directory] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path = directory.join("METADATA");
    let bytes =
        format!("Metadata-Version: 2.1\nName: Example-Pkg\nVersion: {version}\n\nfixture\n")
            .into_bytes();
    fs::write(&path, &bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    (root, path, bytes)
}
fn with_roots(f: &Private, roots: &[PathBuf]) -> Command {
    let mut command = command(f, true, &["crate/src/lib.rs"]);
    command.env(
        "FORGE_CRATE_INSTALLED_ROOTS",
        serde_json::to_string(roots).unwrap(),
    );
    command
}
#[test]
fn explicit_ordered_metadata_roots_record_hashes_and_compare_only_metadata_versions() {
    let f = fixture(Some("1.0.0"), "1.0.1", Some(OLD), Some(NEW));
    let before = f.case.source_state();
    let (old, old_path, old_bytes) = site(&f, "old-site", "1.0.0");
    let (current, current_path, current_bytes) = site(&f, "current-site", "1.0.1");
    let drift = report(run(with_roots(&f, std::slice::from_ref(&old))), 1, true);
    assert_eq!(rules(&drift), ["installed-version-drift"]);
    assert_eq!(
        drift["plan"]["installed_metadata"],
        json!([{"path":old_path,"sha256":format!("{:x}",Sha256::digest(&old_bytes))}])
    );
    let actual = report(run(with_roots(&f, &[current, old])), 0, true);
    assert!(rules(&actual).is_empty());
    assert_eq!(
        actual["plan"]["installed_metadata"],
        json!([{"path":current_path,"sha256":format!("{:x}",Sha256::digest(&current_bytes))}])
    );
    assert_eq!(fs::read(old_path).unwrap(), old_bytes);
    assert_eq!(fs::read(current_path).unwrap(), current_bytes);
    f.case.assert_preserved(&before);
}
#[test]
fn roots_are_explicit_owned_and_nofollow_and_git_context_is_required() {
    let f = fixture(Some("1.0.0"), "1.0.1", Some(OLD), Some(NEW));
    let before = f.case.source_state();
    let mut missing = command(&f, true, &["crate/src/lib.rs"]);
    missing.env_remove("FORGE_CRATE_INSTALLED_ROOTS");
    assert_code(&run(missing), 2);
    let mut git = command(&f, true, &["crate/src/lib.rs"]);
    git.env_remove("GIT_DIR");
    assert_code(&run(git), 2);
    assert_code(&run(with_roots(&f, &[f.snapshot.path().to_path_buf()])), 2);
    let (root, metadata, bytes) = site(&f, "site", "1.0.1");
    let alias = f.case.fixture.root.join("site-alias");
    symlink(&root, &alias).unwrap();
    assert_code(&run(with_roots(&f, &[alias])), 2);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).unwrap();
    assert_code(&run(with_roots(&f, std::slice::from_ref(&root))), 2);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let outside = f.case.fixture.root.join("outside-metadata");
    fs::write(&outside, &bytes).unwrap();
    fs::remove_file(&metadata).unwrap();
    symlink(&outside, &metadata).unwrap();
    assert_code(&run(with_roots(&f, &[root])), 2);
    assert_eq!(fs::read(outside).unwrap(), bytes);
    f.case.assert_preserved(&before);
}
#[test]
fn changed_snapshot_manifest_or_source_cannot_reuse_git_evidence() {
    for path in ["crate/Cargo.toml", "crate/src/lib.rs"] {
        let f = fixture(Some("1.0.0"), "1.0.1", Some(OLD), Some(NEW));
        let before = f.case.source_state();
        fs::write(f.snapshot.path().join(path), b"replacement bytes\n").unwrap();
        assert_code(&run(command(&f, true, &["crate/src/lib.rs"])), 2);
        f.case.assert_preserved(&before);
    }
}
fn assert_detected_change(f: &Private, path: &str, old_path: &str, code: char) {
    use forge::candidate_source::{Kind, Source};
    use git2::{Oid, Repository, Signature, Time};
    let root = f.case.fixture.root.join("repo");
    let repo = Repository::open(&root).unwrap();
    let tree = repo.find_tree(Oid::from_str(&f.tree).unwrap()).unwrap();
    let base = repo.find_commit(Oid::from_str(&f.base).unwrap()).unwrap();
    let sig = Signature::new("Fixture", "fixture@invalid", &Time::new(0, 0)).unwrap();
    // Commit an owned fixture object only; no HEAD/ref or index is changed.
    let target = repo
        .commit(None, &sig, &sig, "detection witness", &tree, &[&base])
        .unwrap();
    let candidate = Source::open(&root)
        .unwrap()
        .resolve(Kind::Range, &target.to_string(), Some(&f.base))
        .unwrap();
    assert_eq!(candidate.tree_oid, f.tree);
    let change = candidate
        .changes
        .iter()
        .find(|change| change.path == path)
        .unwrap();
    assert_eq!(change.old_path.as_deref(), Some(old_path));
    assert!(
        change.status.starts_with(code),
        "expected {code} detection, got {}",
        change.status
    );
}
#[test]
fn moving_production_source_into_tests_still_requires_the_old_crate_bump() {
    let cargo = manifest("1.0.0");
    let f = Private::new(
        &[
            ("crate/Cargo.toml", cargo.as_bytes()),
            ("crate/src/old.rs", OLD.as_bytes()),
        ],
        &[
            ("crate/Cargo.toml", cargo.as_bytes()),
            ("crate/tests/old.rs", OLD.as_bytes()),
        ],
    );
    let before = f.case.source_state();
    assert_detected_change(&f, "crate/tests/old.rs", "crate/src/old.rs", 'R');
    let actual = report(run(command(&f, true, &["crate/tests/old.rs"])), 1, true);
    assert_eq!(actual["result"]["files"], json!(["crate/Cargo.toml"]));
    assert_eq!(actual["result"]["metrics"]["crates_touched"], 1);
    assert_eq!(rules(&actual), [BUMP]);
    assert_eq!(actual["result"]["findings"][0]["severity"], "critical");
    f.case.assert_preserved(&before);
}
#[test]
fn cross_crate_rename_requires_bumps_for_both_departure_and_destination() {
    let a = "[package]\nname='crate_a'\nversion='1.0.0'\n";
    let b = "[package]\nname='crate_b'\nversion='1.0.0'\n";
    let f = Private::new(
        &[
            ("A/Cargo.toml", a.as_bytes()),
            ("B/Cargo.toml", b.as_bytes()),
            ("A/src/old.rs", OLD.as_bytes()),
        ],
        &[
            ("A/Cargo.toml", a.as_bytes()),
            ("B/Cargo.toml", b.as_bytes()),
            ("B/src/old.rs", OLD.as_bytes()),
        ],
    );
    let before = f.case.source_state();
    assert_detected_change(&f, "B/src/old.rs", "A/src/old.rs", 'R');
    let actual = report(run(command(&f, true, &["B/src/old.rs"])), 1, true);
    assert_eq!(
        actual["result"]["files"],
        json!(["A/Cargo.toml", "B/Cargo.toml"])
    );
    assert_eq!(actual["result"]["metrics"]["crates_touched"], 2);
    assert_eq!(rules(&actual), [BUMP, BUMP]);
    let findings = actual["result"]["findings"].as_array().unwrap();
    assert_eq!(findings[0]["path"], "A/Cargo.toml");
    assert_eq!(findings[1]["path"], "B/Cargo.toml");
    f.case.assert_preserved(&before);
}
#[test]
fn detected_copy_does_not_charge_the_retained_source_crate_as_a_deletion() {
    let a = "[package]\nname='crate_a'\nversion='1.0.0'\n";
    let b = "[package]\nname='crate_b'\nversion='1.0.0'\n";
    let source: String = (0..24)
        .map(|i| format!("pub fn value{i}() -> u32 {{ {i} }}\n"))
        .collect();
    // libgit2 copies(true) considers modified sources; unchanged sources are
    // not requested by current detection options. A test-only edit creates a
    // candidate source without changing its production projection.
    let retained = format!("{source}#[cfg(test)] mod tests {{ #[test] fn added() {{}} }}\n");
    let f = Private::new(
        &[
            ("A/Cargo.toml", a.as_bytes()),
            ("B/Cargo.toml", b.as_bytes()),
            ("A/src/old.rs", source.as_bytes()),
        ],
        &[
            ("A/Cargo.toml", a.as_bytes()),
            ("B/Cargo.toml", b.as_bytes()),
            ("A/src/old.rs", retained.as_bytes()),
            ("B/src/copied.rs", source.as_bytes()),
        ],
    );
    let before = f.case.source_state();
    assert_detected_change(&f, "B/src/copied.rs", "A/src/old.rs", 'C');
    let actual = report(run(command(&f, true, &["B/src/copied.rs"])), 1, true);
    assert_eq!(actual["result"]["files"], json!(["B/Cargo.toml"]));
    assert_eq!(actual["result"]["metrics"]["crates_touched"], 1);
    assert_eq!(rules(&actual), [BUMP]);
    assert_eq!(actual["result"]["findings"][0]["path"], "B/Cargo.toml");
    f.case.assert_preserved(&before);
}
