use super::*;
use serde_json::json;
use std::fs;

fn fixture() -> (
    tempfile::TempDir,
    PathBuf,
    PathBuf,
    Value,
    BTreeMap<String, String>,
    BTreeMap<String, String>,
) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    let home = temp.path().join("home");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='demo'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(root.join("Cargo.lock"), "version = 4\n").unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    fs::write(root.join("tests/check.rs"), "#[test] fn check() {}\n").unwrap();
    let digest = |p: &str| format!("{:x}", Sha256::digest(fs::read(root.join(p)).unwrap()));
    let sources = BTreeMap::from([("src/lib.rs".into(), digest("src/lib.rs"))]);
    let tests = BTreeMap::from([("tests/check.rs".into(), digest("tests/check.rs"))]);
    let data = json!({"version":1,"workspace_root":root,"workspace_members":["demo-id"],"resolve":{"nodes":[{"id":"demo-id","deps":[]}]},"packages":[{
        "id":"demo-id","name":"demo","version":"0.1.0","source":null,"manifest_path":root.join("Cargo.toml"),"targets":[
            {"name":"demo","kind":["lib"],"src_path":root.join("src/lib.rs")},
            {"name":"check","kind":["test"],"src_path":root.join("tests/check.rs")}
        ]
    }]});
    (temp, root, home, data, sources, tests)
}
fn run(
    root: &Path,
    home: &Path,
    data: &Value,
    sources: &BTreeMap<String, String>,
    tests: &BTreeMap<String, String>,
) -> Result<Selection> {
    validate(
        &serde_json::to_vec(data)?,
        root,
        home,
        "Cargo.lock",
        "Cargo.toml",
        "demo",
        sources,
        tests,
    )
}
#[test]
fn package_wide_selection_is_explicit_and_pinned() {
    let (_temp, root, home, data, sources, tests) = fixture();
    let selection = run(&root, &home, &data, &sources, &tests).unwrap();
    assert_eq!(selection.engine_args, ["--test-package", "demo"]);
    assert_eq!(selection.package_id, "demo-id");
    assert!(selection.baseline_args.contains(&"--locked".to_string()));
}
#[test]
fn changed_source_or_test_pin_and_duplicate_ids_refuse() {
    let (_temp, root, home, mut data, sources, tests) = fixture();
    fs::write(root.join("src/lib.rs"), "changed").unwrap();
    assert!(run(&root, &home, &data, &sources, &tests).is_err());
    fs::write(root.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    fs::write(root.join("tests/check.rs"), "changed").unwrap();
    assert!(run(&root, &home, &data, &sources, &tests).is_err());
    fs::write(root.join("tests/check.rs"), "#[test] fn check() {}\n").unwrap();
    let item = data["packages"][0].clone();
    data["packages"].as_array_mut().unwrap().push(item);
    assert!(run(&root, &home, &data, &sources, &tests).is_err());
}
#[test]
fn target_escape_and_custom_harness_refuse() {
    let (temp, root, home, mut data, sources, tests) = fixture();
    let outside = temp.path().join("outside.rs");
    fs::write(&outside, "fn main() {}\n").unwrap();
    data["packages"][0]["targets"][0]["src_path"] = json!(outside);
    assert!(run(&root, &home, &data, &sources, &tests).is_err());
    data["packages"][0]["targets"][0]["src_path"] = json!(root.join("src/lib.rs"));
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='demo'\nversion='0.1.0'\n[[test]]\nname='check'\nharness=false\n",
    )
    .unwrap();
    assert!(run(&root, &home, &data, &sources, &tests).is_err());
}
#[test]
fn disabled_features_and_external_bin_inside_snapshot_are_admitted() {
    let (_temp, root, home, mut data, sources, tests) = fixture();
    fs::create_dir_all(root.join("sibling/src")).unwrap();
    let external = root.join("sibling/src/main.rs");
    fs::write(&external, "fn main() {}\n").unwrap();
    data["packages"][0]["targets"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name":"cli", "kind":["bin"], "src_path":external, "required-features":["optional"]
        }));
    let selected = run(&root, &home, &data, &sources, &tests).unwrap();
    assert!(selected.targets.contains(&"bin:cli".to_owned()));
    assert_eq!(selected.test_scope, "package-wide-full-source-snapshot");
}
#[test]
fn external_and_unknown_sources_refuse() {
    let (_temp, root, home, mut data, sources, tests) = fixture();
    data["packages"][0]["source"] = json!("git+https://example.invalid/repo");
    assert!(run(&root, &home, &data, &sources, &tests).is_err());
}

#[test]
fn selected_sources_map_to_workspace_relative_without_broadening() {
    let (_temp, root, _home, _data, sources, _) = fixture();
    let (engine, report) = source_mapping(&root, &root, &sources).unwrap();
    assert_eq!(engine, ["src/lib.rs"]);
    assert_eq!(report, "");
    fs::create_dir_all(root.join("native/workspace/src")).unwrap();
    let nested = BTreeMap::from([("native/workspace/src/lib.rs".into(), "a".into())]);
    let (engine, report) = source_mapping(&root, &root.join("native/workspace"), &nested).unwrap();
    assert_eq!(engine, ["src/lib.rs"]);
    assert_eq!(report, "native/workspace");
    assert!(source_mapping(&root, &root.join("native/workspace"), &sources).is_err());
    let glob = BTreeMap::from([("native/workspace/src/*.rs".into(), "a".into())]);
    assert!(source_mapping(&root, &root.join("native/workspace"), &glob).is_err());
}

#[test]
fn metadata_accepts_confined_parent_components_in_targets_and_manifest() {
    let (_temp, root, home, mut data, sources, tests) = fixture();
    fs::create_dir(root.join("sibling")).unwrap();
    data["packages"][0]["manifest_path"] = json!(root.join("sibling/../Cargo.toml"));
    data["packages"][0]["targets"][0]["src_path"] = json!(root.join("sibling/../src/lib.rs"));
    let selection = run(&root, &home, &data, &sources, &tests).unwrap();
    assert_eq!(selection.package_name, "demo");
}

#[test]
fn metadata_rejects_escape_reentry_and_every_symlink_component() {
    use std::os::unix::fs::symlink;
    let (temp, root, home, mut data, sources, tests) = fixture();
    fs::create_dir(root.join("sibling")).unwrap();
    symlink("sibling", root.join("alias")).unwrap();
    symlink("src/lib.rs", root.join("final-link.rs")).unwrap();
    symlink("missing.rs", root.join("dangling.rs")).unwrap();
    let candidates = [
        root.join("../repo/src/lib.rs"),
        root.join("alias/../src/lib.rs"),
        root.join("final-link.rs"),
        root.join("dangling.rs"),
        temp.path().join("outside.rs"),
    ];
    fs::write(temp.path().join("outside.rs"), "fn outside() {}\n").unwrap();
    for path in candidates {
        data["packages"][0]["targets"][0]["src_path"] = json!(path);
        assert!(
            run(&root, &home, &data, &sources, &tests).is_err(),
            "accepted {path:?}"
        );
    }
}
