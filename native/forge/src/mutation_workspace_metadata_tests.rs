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
            {"name":"demo","kind":["lib"],"src_path":root.join("src/lib.rs"),"test":true},
            {"name":"check","kind":["test"],"src_path":root.join("tests/check.rs"),"test":true}
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
        None,
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

fn with_dependency(root: &Path, data: &mut Value, kind: &str, target: &Path) {
    let dependency = root.join("dependency");
    fs::create_dir_all(&dependency).unwrap();
    fs::write(
        dependency.join("Cargo.toml"),
        "[package]\nname='dep'\nversion='0.1.0'\n",
    )
    .unwrap();
    data["packages"].as_array_mut().unwrap().push(json!({
        "id":"dep-id", "name":"dep", "version":"0.1.0", "source":null,
        "manifest_path":dependency.join("Cargo.toml"),
        "targets":[{"name":"sample", "kind":[kind], "src_path":target}]
    }));
    data["resolve"]["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"dep-id", "deps":[]}));
}

#[test]
fn missing_unused_dependency_example_is_allowed_but_build_and_selected_targets_are_required() {
    let (_temp, root, home, mut data, sources, tests) = fixture();
    let missing = root.join("dependency/examples/omitted.rs");
    with_dependency(&root, &mut data, "example", &missing);
    assert!(run(&root, &home, &data, &sources, &tests).is_ok());
    for kind in ["lib", "bin", "proc-macro", "custom-build", "unknown"] {
        data["packages"][1]["targets"][0]["kind"] = json!([kind]);
        let error = run(&root, &home, &data, &sources, &tests).unwrap_err();
        let text = format!("{error:#}");
        assert!(
            text.contains("dep-id") && text.contains("sample") && text.contains("omitted.rs"),
            "{text}"
        );
    }
    data["packages"][1]["targets"][0]["kind"] = json!(["example"]);
    data["packages"][0]["targets"][0]["src_path"] = json!(root.join("src/missing.rs"));
    assert!(run(&root, &home, &data, &sources, &tests).is_err());
}

#[test]
fn missing_dependency_dev_target_still_refuses_escape_alias_and_parent_after_missing() {
    use std::os::unix::fs::symlink;
    let (temp, root, home, mut data, sources, tests) = fixture();
    let dependency = root.join("dependency");
    let initial = dependency.join("examples/absent.rs");
    with_dependency(&root, &mut data, "example", &initial);
    symlink("missing-dir", dependency.join("alias")).unwrap();
    for path in [
        temp.path().join("outside/not-found.rs"),
        dependency.join("alias/not-found.rs"),
        dependency.join("missing/../not-found.rs"),
        root.join("../repo/dependency/examples/not-found.rs"),
    ] {
        data["packages"][1]["targets"][0]["src_path"] = json!(path);
        assert!(
            run(&root, &home, &data, &sources, &tests).is_err(),
            "accepted {path:?}"
        );
    }
}

fn selected_run(
    root: &Path,
    home: &Path,
    data: &Value,
    sources: &BTreeMap<String, String>,
    tests: &BTreeMap<String, String>,
    selector: &Selector,
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
        Some(selector),
    )
}
fn selector(target: Target, filter: Option<&str>) -> Selector {
    Selector {
        schema_version: 1,
        target,
        filter: filter.map(str::to_owned),
    }
}
#[test]
fn library_selection_binds_source_and_forwards_target_to_build_and_test() {
    let (_temp, root, home, data, sources, tests) = fixture();
    let selected = selected_run(
        &root,
        &home,
        &data,
        &sources,
        &tests,
        &selector(Target::Lib {}, Some("checks::")),
    )
    .unwrap();
    assert_eq!(
        selected.baseline_args,
        vec![
            "cargo",
            "test",
            "--manifest-path",
            "Cargo.toml",
            "--package",
            "demo",
            "--locked",
            "--offline",
            "--lib",
            "--",
            "checks::"
        ]
    );
    assert_eq!(
        selected.engine_args,
        vec![
            "--test-package",
            "demo",
            "--cargo-arg=--lib",
            "--cargo-test-arg=--",
            "--cargo-test-arg=checks::"
        ]
    );
    let target = selected.selected_target.unwrap();
    assert_eq!(
        (
            target.kind.as_str(),
            target.name.as_str(),
            target.source.as_str()
        ),
        ("lib", "demo", "src/lib.rs")
    );
    assert_eq!(selected.test_scope, "selected-target-full-source-snapshot");
}
#[test]
fn integration_source_resolves_from_held_manifest_not_caller_or_metadata_alone() {
    let (_temp, root, home, mut data, sources, tests) = fixture();
    fs::create_dir(root.join("custom")).unwrap();
    fs::write(root.join("custom/contract.rs"), "#[test] fn check() {}\n").unwrap();
    fs::write(root.join("Cargo.toml"),"[package]\nname='demo'\nversion='0.1.0'\n[[test]]\nname='check'\npath='custom/contract.rs'\n").unwrap();
    data["packages"][0]["targets"][1]["src_path"] = json!(root.join("custom/contract.rs"));
    let s = selector(
        Target::Test {
            name: "check".into(),
        },
        None,
    );
    let resolved = selected_run(&root, &home, &data, &sources, &tests, &s).unwrap();
    assert_eq!(
        resolved.selected_target.unwrap().source,
        "custom/contract.rs"
    );
    data["packages"][0]["targets"][1]["src_path"] = json!(root.join("tests/check.rs"));
    assert!(selected_run(&root, &home, &data, &sources, &tests, &s)
        .unwrap_err()
        .to_string()
        .contains("held manifest"));
}
#[test]
fn selected_custom_disabled_and_feature_gated_targets_refuse() {
    let (_temp, root, home, mut data, sources, tests) = fixture();
    let s = selector(Target::Lib {}, None);
    for field in ["harness=false", "test=false", "required-features=['extra']"] {
        fs::write(
            root.join("Cargo.toml"),
            format!("[package]\nname='demo'\nversion='0.1.0'\n[lib]\n{field}\n"),
        )
        .unwrap();
        assert!(
            selected_run(&root, &home, &data, &sources, &tests, &s).is_err(),
            "{field}"
        );
    }
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='demo'\nversion='0.1.0'\n",
    )
    .unwrap();
    data["packages"][0]["targets"][0]["required-features"] = json!(["extra"]);
    assert!(selected_run(&root, &home, &data, &sources, &tests, &s).is_err());
    data["packages"][0]["targets"][0]
        .as_object_mut()
        .unwrap()
        .remove("required-features");
    data["packages"][0]["targets"][0]["test"] = json!(false);
    assert!(selected_run(&root, &home, &data, &sources, &tests, &s).is_err());
}
#[test]
fn unselected_custom_harness_does_not_widen_or_block_library_selection() {
    let (_temp, root, home, data, sources, tests) = fixture();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='demo'\nversion='0.1.0'\n[[test]]\nname='check'\nharness=false\n",
    )
    .unwrap();
    let selected = selected_run(
        &root,
        &home,
        &data,
        &sources,
        &tests,
        &selector(Target::Lib {}, None),
    )
    .unwrap();
    assert_eq!(selected.baseline_args.last().unwrap(), "--lib");
    assert!(
        run(&root, &home, &data, &sources, &tests).is_err(),
        "v2 retains its old custom harness refusal"
    );
    assert!(selected_run(
        &root,
        &home,
        &data,
        &sources,
        &tests,
        &selector(
            Target::Test {
                name: "check".into()
            },
            None
        )
    )
    .is_err());
}
#[test]
fn selected_target_missing_duplicate_or_ambiguous_source_refuses() {
    let (_temp, root, home, mut data, sources, tests) = fixture();
    assert!(selected_run(
        &root,
        &home,
        &data,
        &sources,
        &tests,
        &selector(
            Target::Test {
                name: "absent".into()
            },
            None
        )
    )
    .is_err());
    let lib = data["packages"][0]["targets"][0].clone();
    data["packages"][0]["targets"]
        .as_array_mut()
        .unwrap()
        .push(lib);
    assert!(selected_run(
        &root,
        &home,
        &data,
        &sources,
        &tests,
        &selector(Target::Lib {}, None)
    )
    .is_err());
    data["packages"][0]["targets"].as_array_mut().unwrap().pop();
    fs::create_dir(root.join("tests/check")).unwrap();
    fs::write(
        root.join("tests/check/main.rs"),
        "#[test] fn another() {}\n",
    )
    .unwrap();
    assert!(selected_run(
        &root,
        &home,
        &data,
        &sources,
        &tests,
        &selector(
            Target::Test {
                name: "check".into()
            },
            None
        )
    )
    .unwrap_err()
    .to_string()
    .contains("ambiguous"));
}

#[test]
fn standard_multi_crate_type_library_is_selected_and_normalized() {
    let (_temp, root, home, mut data, sources, tests) = fixture();
    fs::write(root.join("Cargo.toml"),"[package]\nname='demo'\nversion='0.1.0'\n[lib]\nname='_conductor_native'\ncrate-type=['cdylib','rlib']\n").unwrap();
    data["packages"][0]["targets"][0]["name"] = json!("_conductor_native");
    data["packages"][0]["targets"][0]["kind"] = json!(["cdylib", "rlib"]);
    let selected = selected_run(
        &root,
        &home,
        &data,
        &sources,
        &tests,
        &selector(Target::Lib {}, None),
    )
    .unwrap();
    let target = selected.selected_target.unwrap();
    assert_eq!(target.kind, "lib");
    assert_eq!(target.name, "_conductor_native");
    assert!(selected
        .targets
        .contains(&"cdylib,rlib:_conductor_native".into()));
}
#[test]
fn implicit_target_discovery_flags_and_held_package_name_are_independent_guards() {
    let (_temp, root, home, data, sources, tests) = fixture();
    for (flag, target) in [
        ("autolib", Target::Lib {}),
        (
            "autotests",
            Target::Test {
                name: "check".into(),
            },
        ),
    ] {
        fs::write(
            root.join("Cargo.toml"),
            format!("[package]\nname='demo'\nversion='0.1.0'\n{flag}=false\n"),
        )
        .unwrap();
        assert!(selected_run(
            &root,
            &home,
            &data,
            &sources,
            &tests,
            &selector(target.clone(), None)
        )
        .unwrap_err()
        .to_string()
        .contains(flag));
        let explicit = match target {
            Target::Lib {} => "[lib]\n",
            Target::Test { .. } => "[[test]]\nname='check'\n",
        };
        fs::write(
            root.join("Cargo.toml"),
            format!("[package]\nname='demo'\nversion='0.1.0'\n{flag}=false\n{explicit}"),
        )
        .unwrap();
        assert!(selected_run(
            &root,
            &home,
            &data,
            &sources,
            &tests,
            &selector(target, None)
        )
        .is_ok());
    }
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='other'\nversion='0.1.0'\n",
    )
    .unwrap();
    assert!(selected_run(
        &root,
        &home,
        &data,
        &sources,
        &tests,
        &selector(Target::Lib {}, None)
    )
    .unwrap_err()
    .to_string()
    .contains("package name"));
}

#[test]
fn slashless_workspace_mutation_sources_refuse_recursive_basename_globs() {
    let (_temp, root, _home, _data, _, _) = fixture();
    for (workspace, source) in [
        (root.clone(), "lib.rs"),
        (root.join("nested"), "nested/lib.rs"),
    ] {
        let pins = BTreeMap::from([(source.to_owned(), "a".repeat(64))]);
        let error = source_mapping(&root, &workspace, &pins)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("recursive basename glob") && error.contains("lib.rs"),
            "{error}"
        );
    }
    let pins = BTreeMap::from([("src/lib.rs".into(), "a".repeat(64))]);
    assert_eq!(
        source_mapping(&root, &root, &pins).unwrap().0,
        ["src/lib.rs"]
    );
}
