use super::tests::{roster, Fixture};
use super::*;
fn write(f: &Fixture, path: &str, text: &str) {
    f.file(path);
    std::fs::write(f.path.join(path), text).unwrap();
}
#[test]
fn virtual_workspace_expands_members_before_unstyled_and_linted_filters() {
    let f = Fixture::new();
    write(
        &f,
        "Cargo.toml",
        "[workspace]\nmembers=['crates/*']\nexclude=['crates/excluded']\n",
    );
    for name in ["a", "b", "excluded"] {
        f.file(&format!("crates/{name}/Cargo.toml"));
    }
    let r = roster(
        &[".", "crates/a", "crates/b", "crates/excluded"],
        "unstyled=['crates/b']\nlinted=['crates/a']",
        &["Cargo.toml", "crates/*/Cargo.toml"],
    );
    let fmt = plan(&f.root, &r, &["Cargo.toml"], Mode::Fmt).unwrap();
    assert_eq!(fmt.manifests, ["crates/a/Cargo.toml"]);
    assert_eq!(fmt.input_owners[0].crate_path, ".");
    assert_eq!(fmt.skips[0].crate_path, "crates/b");
    assert_eq!(fmt.skips[0].kind, SkipKind::Unstyled);
    assert_eq!(
        fmt.scope_expansions
            .iter()
            .map(|v| v.manifest.as_str())
            .collect::<Vec<_>>(),
        ["crates/a/Cargo.toml", "crates/b/Cargo.toml"]
    );
    assert!(fmt
        .scope_expansions
        .iter()
        .all(|v| v.reason == ScopeReason::Workspace && v.trigger == "Cargo.toml"));
    let clippy = plan(&f.root, &r, &["Cargo.toml"], Mode::Clippy).unwrap();
    assert_eq!(clippy.manifests, ["crates/a/Cargo.toml"]);
    assert_eq!(clippy.skips[0].kind, SkipKind::NotLinted);
}
#[test]
fn package_workspace_keeps_itself_and_confined_parent_members_without_all() {
    let f = Fixture::new();
    write(
        &f,
        "native/forge/Cargo.toml",
        "[package]\n[workspace]\nmembers=['../forge-cli']\n",
    );
    f.file("native/forge-cli/Cargo.toml");
    f.file("native/forge/nested/Cargo.toml");
    let r = roster(
        &["native/forge", "native/forge-cli", "native/forge/nested"],
        "",
        &["native/**/Cargo.toml"],
    );
    let result = plan(&f.root, &r, &["native/forge/Cargo.toml"], Mode::Fmt).unwrap();
    assert_eq!(
        result.manifests,
        [
            "native/forge-cli/Cargo.toml",
            "native/forge/Cargo.toml",
            "native/forge/nested/Cargo.toml"
        ]
    );
    assert_eq!(result.scope_expansions.len(), 2);
}
#[test]
fn borrowed_bin_lib_test_example_bench_and_build_paths_expand_owner_coverage() {
    for declaration in [
        "[lib]\npath='../source/main.rs'\n",
        "[[bin]]\npath='../source/main.rs'\n",
        "[[test]]\npath='../source/main.rs'\n",
        "[[example]]\npath='../source/main.rs'\n",
        "[[bench]]\npath='../source/main.rs'\n",
        "build='../source/main.rs'\n",
    ] {
        let f = Fixture::new();
        f.file("source/Cargo.toml");
        f.file("source/main.rs");
        f.file("source/sibling/mod.rs");
        let text = format!("[package]\n{declaration}");
        write(&f, "borrower/Cargo.toml", &text);
        let r = roster(&["source", "borrower"], "", &["*/Cargo.toml"]);
        let result = plan(&f.root, &r, &["source/sibling/mod.rs"], Mode::Fmt).unwrap();
        assert_eq!(
            result.manifests,
            ["borrower/Cargo.toml", "source/Cargo.toml"]
        );
        assert_eq!(result.input_owners[0].crate_path, "source");
        assert_eq!(
            result.scope_expansions[0].reason,
            ScopeReason::BorrowedTarget
        );
        assert_eq!(result.scope_expansions[0].trigger, "source/sibling/mod.rs");
    }
}
#[test]
fn selected_directory_contains_borrowed_target_parent_without_prefix_collisions() {
    let f = Fixture::new();
    f.file("source/Cargo.toml");
    f.file("source/src/main.rs");
    f.file("source/src-other/selected.rs");
    write(
        &f,
        "borrower/Cargo.toml",
        "[package]\n[lib]\npath='../source/src/main.rs'\n",
    );
    let r = roster(&["source", "borrower"], "", &["*/Cargo.toml"]);
    for selected in ["source", "source/src"] {
        let result = plan(&f.root, &r, &[selected], Mode::Fmt).unwrap();
        assert_eq!(
            result.manifests,
            ["borrower/Cargo.toml", "source/Cargo.toml"]
        );
        assert_eq!(
            result.scope_expansions[0].reason,
            ScopeReason::BorrowedTarget
        );
    }
    let result = plan(&f.root, &r, &["source/src-other"], Mode::Fmt).unwrap();
    assert_eq!(result.manifests, ["source/Cargo.toml"]);
}
#[test]
fn missing_inventory_member_or_explicit_path_escape_never_narrows_scope_silently() {
    let f = Fixture::new();
    write(&f, "Cargo.toml", "[workspace]\nmembers=['member']\n");
    f.file("member/Cargo.toml");
    let r = roster(&["."], "", &["Cargo.toml"]);
    assert!(plan(&f.root, &r, &["Cargo.toml"], Mode::Fmt)
        .unwrap_err()
        .contains("missing from roster manifest inventory"));
    write(&f, "Cargo.toml", "[package]\n[lib]\npath='../escape.rs'\n");
    assert!(plan(&f.root, &r, &["Cargo.toml"], Mode::Fmt)
        .unwrap_err()
        .contains("escapes"));
    write(&f, "Cargo.toml", "[package]\n[lib]\npath=17\n");
    assert!(plan(&f.root, &r, &["Cargo.toml"], Mode::Fmt)
        .unwrap_err()
        .contains("must be a string"));
}
#[test]
fn parent_normalization_checks_literal_prefix_before_popping_it() {
    let f = Fixture::new();
    f.file("pkg/Cargo.toml");
    f.file("source/main.rs");
    std::os::unix::fs::symlink("../source", f.path.join("pkg/link")).unwrap();
    let r = roster(&["pkg"], "", &["pkg/Cargo.toml"]);
    for path in ["link/../main.rs", "missing/../main.rs"] {
        write(
            &f,
            "pkg/Cargo.toml",
            &format!("[package]\n[lib]\npath='{path}'\n"),
        );
        assert!(plan(&f.root, &r, &["pkg/Cargo.toml"], Mode::Fmt).is_err());
    }
}
#[test]
fn manifest_capture_bytes_and_zero_package_virtual_scope_are_bounded() {
    let f = Fixture::new();
    write(&f, "Cargo.toml", "[workspace]\nmembers=[]\n");
    let r = roster(&["."], "", &["Cargo.toml"]);
    assert!(plan(&f.root, &r, &["Cargo.toml"], Mode::Fmt)
        .unwrap_err()
        .contains("no inventoried packages"));
    std::fs::write(f.path.join("Cargo.toml"), vec![b' '; 1024 * 1024 + 1]).unwrap();
    assert!(plan(&f.root, &r, &["Cargo.toml"], Mode::Fmt)
        .unwrap_err()
        .contains("byte limit"));
}

#[test]
fn excluded_member_need_not_be_in_scope_inventory_and_bad_workspace_types_fail() {
    let f = Fixture::new();
    write(
        &f,
        "Cargo.toml",
        "[workspace]\nmembers=['a','excluded']\nexclude=['excluded']\n",
    );
    f.file("a/Cargo.toml");
    f.file("excluded/Cargo.toml");
    let r = roster(&[".", "a"], "", &["Cargo.toml", "a/Cargo.toml"]);
    assert_eq!(
        plan(&f.root, &r, &["Cargo.toml"], Mode::Fmt)
            .unwrap()
            .manifests,
        ["a/Cargo.toml"]
    );
    write(&f, "Cargo.toml", "[workspace]\nmembers='a'\n");
    assert!(plan(&f.root, &r, &["Cargo.toml"], Mode::Fmt)
        .unwrap_err()
        .contains("arrays"));
}

#[test]
fn captured_manifests_share_the_eight_mib_read_budget() {
    let f = Fixture::new();
    let mut paths = Vec::new();
    let mut bytes = b"[package]\n".to_vec();
    bytes.resize(1024 * 1024, b' ');
    for i in 0..9 {
        let crate_path = format!("pkg{i}");
        f.file(&format!("{crate_path}/Cargo.toml"));
        std::fs::write(f.path.join(&crate_path).join("Cargo.toml"), &bytes).unwrap();
        paths.push(crate_path);
    }
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let r = roster(&refs, "", &["*/Cargo.toml"]);
    assert!(plan(&f.root, &r, &["pkg0/Cargo.toml"], Mode::Fmt)
        .unwrap_err()
        .contains("8 MiB"));
}

#[test]
fn explicit_package_workspace_is_validated_without_expanding_member_siblings() {
    let f = Fixture::new();
    write(&f, "Cargo.toml", "[workspace]\nmembers=['a','b']\n");
    write(&f, "a/Cargo.toml", "[package]\nworkspace='..'\n");
    f.file("b/Cargo.toml");
    let r = roster(&[".", "a", "b"], "", &["Cargo.toml", "*/Cargo.toml"]);
    let result = plan(&f.root, &r, &["a/Cargo.toml"], Mode::Fmt).unwrap();
    assert_eq!(result.manifests, ["a/Cargo.toml"]);
    assert!(result.scope_expansions.is_empty());
    write(&f, "a/Cargo.toml", "[package]\nworkspace='../b'\n");
    assert!(plan(&f.root, &r, &["a/Cargo.toml"], Mode::Fmt)
        .unwrap_err()
        .contains("contain workspace table"));
    write(&f, "a/Cargo.toml", "[package]\nworkspace='../../escape'\n");
    assert!(plan(&f.root, &r, &["a/Cargo.toml"], Mode::Fmt)
        .unwrap_err()
        .contains("escapes"));
}

#[test]
fn known_local_dependency_tables_are_captured_confined_and_not_scheduled_as_owners() {
    for table in [
        "dependencies",
        "dev-dependencies",
        "build-dependencies",
        "target.'cfg(unix)'.dependencies",
        "target.'cfg(unix)'.dev-dependencies",
        "target.'cfg(unix)'.build-dependencies",
        "workspace.dependencies",
        "patch.crates-io",
        "replace",
    ] {
        let f = Fixture::new();
        f.file("dep/Cargo.toml");
        write(
            &f,
            "a/Cargo.toml",
            &format!("[package]\n[{table}]\nlocal={{path='../dep'}}\n"),
        );
        let r = roster(&["a", "dep"], "", &["*/Cargo.toml"]);
        assert_eq!(
            plan(&f.root, &r, &["a/Cargo.toml"], Mode::Fmt)
                .unwrap()
                .manifests,
            ["a/Cargo.toml"]
        );
        let narrow = roster(&["a"], "", &["a/Cargo.toml"]);
        assert!(plan(&f.root, &narrow, &["a/Cargo.toml"], Mode::Fmt)
            .unwrap_err()
            .contains("local dependency missing"));
        write(
            &f,
            "a/Cargo.toml",
            &format!("[package]\n[{table}]\nlocal={{path='../../escape'}}\n"),
        );
        assert!(plan(&f.root, &r, &["a/Cargo.toml"], Mode::Fmt)
            .unwrap_err()
            .contains("escapes"));
    }
}

#[test]
fn arbitrary_metadata_paths_are_data_but_typed_dependency_paths_are_validated() {
    let f = Fixture::new();
    write(
        &f,
        "a/Cargo.toml",
        "[package]\n[package.metadata.custom]\npath='../../not-authority'\n",
    );
    let r = roster(&["a"], "", &["a/Cargo.toml"]);
    assert_eq!(
        plan(&f.root, &r, &["a/Cargo.toml"], Mode::Fmt)
            .unwrap()
            .manifests,
        ["a/Cargo.toml"]
    );
    write(
        &f,
        "a/Cargo.toml",
        "[package]\n[dependencies]\nlocal={path=17}\n",
    );
    assert!(plan(&f.root, &r, &["a/Cargo.toml"], Mode::Fmt)
        .unwrap_err()
        .contains("path must be a string"));
}

#[test]
fn borrowed_parent_matching_uses_components_not_string_prefixes() {
    let f = Fixture::new();
    f.file("source/main.rs");
    f.file("source/Cargo.toml");
    f.file("sourceish/Cargo.toml");
    write(
        &f,
        "borrower/Cargo.toml",
        "[package]\n[lib]\npath='../source/main.rs'\n",
    );
    let r = roster(&["source", "sourceish", "borrower"], "", &["*/Cargo.toml"]);
    let result = plan(&f.root, &r, &["sourceish/deleted.rs"], Mode::Fmt).unwrap();
    assert_eq!(result.manifests, ["sourceish/Cargo.toml"]);
    assert!(result.scope_expansions.is_empty());
}
