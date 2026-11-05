use super::{manifests, Tree};
use serde_json::json;

fn split_layout() -> Tree {
    let tree = Tree::new();
    tree.write(
        "core/Cargo.toml",
        "[package]\nname = 'core'\nversion = '0.1.0'\nautobins = false\n",
    );
    tree.write(
        "core/src/lib.rs",
        "pub mod shared;\n#[test] fn library_test() {}\n",
    );
    tree.write("core/src/main.rs", "mod cli;\nmod shared;\nfn main() {}\n");
    tree.write("core/src/cli.rs", "pub fn run() {}\n");
    tree.write("core/src/shared.rs", "pub fn shared() {}\n");
    tree.write("cli/Cargo.toml", "[package]\nname = 'widget-cli'\nversion = '0.1.0'\nautobins = false\n[[bin]]\nname = 'widget'\npath = '../core/src/main.rs'\n");
    tree
}

#[test]
fn external_binary_modules_are_refused_instead_of_assigned_to_the_library() {
    let tree = split_layout();
    for source in ["core/src/main.rs", "core/src/cli.rs"] {
        let error = tree.plan("rust", Some(&[source])).unwrap_err();
        assert!(error.contains(source), "{error}");
        assert!(
            error.contains("external Cargo target package widget-cli, not core"),
            "{error}"
        );
    }
    assert!(tree
        .plan("rust", None)
        .unwrap_err()
        .contains("external Cargo target package widget-cli"));
}

#[test]
fn local_library_and_shared_modules_keep_the_correct_package_and_pins() {
    let tree = split_layout();
    for source in ["core/src/lib.rs", "core/src/shared.rs"] {
        let plan = tree.plan("rust", Some(&[source])).unwrap();
        let manifest = &manifests(&plan)[0];
        assert_eq!(manifest["generator"]["options"]["package"], "core");
        assert_eq!(
            manifest["generator"]["options"]["manifest_path"],
            "core/Cargo.toml"
        );
        assert_eq!(
            manifest["generator"]["source"],
            json!([source.strip_prefix("core/").unwrap()])
        );
        assert_eq!(
            manifest["test_argv"],
            json!([
                "cargo",
                "test",
                "--manifest-path",
                "core/Cargo.toml",
                "--package",
                "core"
            ])
        );
        assert!(manifest["test_sha256"].get("core/src/lib.rs").is_some());
        assert!(manifest["source_sha256"].get(source).is_some());
        assert_eq!(manifest["source_sha256"].as_object().unwrap().len(), 1);
    }
}

#[test]
fn inline_module_includes_resolve_beside_the_physical_source_file() {
    let tree = split_layout();
    tree.write(
        "core/src/cli.rs",
        "pub fn run() {}\n#[cfg(test)] mod tests { mod nested { include!(\"cli_cases.rs\"); } }\n",
    );
    tree.write("core/src/cli_cases.rs", "#[test] fn cli_case() {}\n");

    let plan = tree
        .plan("rust", Some(&["core/src/lib.rs"]))
        .expect("an unrelated library plan can traverse the external binary's inline include");
    assert_eq!(
        manifests(&plan)[0]["generator"]["options"]["package"],
        "core"
    );
    let error = tree
        .plan("rust", Some(&["core/src/cli_cases.rs"]))
        .expect_err("the included source still belongs to the external binary");
    assert!(
        error.contains("core/src/cli_cases.rs") && error.contains("widget-cli"),
        "{error}"
    );
}

#[test]
fn conditional_lint_levels_do_not_change_module_ownership() {
    let tree = split_layout();
    tree.write("core/src/lib.rs", "#[cfg_attr(not(feature = \"python\"), allow(dead_code))] pub mod shared;\n#[test] fn library_test() {}\n");
    tree.write("core/src/cli.rs", "#[cfg_attr(feature = \"diagnostics\", warn(dead_code), deny(unused_imports))] mod child;\npub fn run() {}\n");
    tree.write("core/src/cli/child.rs", "pub fn child() {}\n");
    let plan = tree
        .plan("rust", Some(&["core/src/shared.rs"]))
        .expect("conditional lint levels do not change source reachability");
    assert_eq!(
        manifests(&plan)[0]["generator"]["options"]["package"],
        "core"
    );
    let error = tree
        .plan("rust", Some(&["core/src/cli/child.rs"]))
        .expect_err("lint levels do not change the external target owner");
    assert!(error.contains("widget-cli"), "{error}");
}

#[test]
fn disabled_local_includes_and_crate_roots_cannot_claim_external_sources() {
    for library in [
        "#[cfg(any())] include!(\"cli_cases.rs\");\n#[test] fn library_test() {}\n",
        "#![cfg(any())]\ninclude!(\"cli_cases.rs\");\n#[test] fn library_test() {}\n",
    ] {
        let tree = split_layout();
        tree.write("core/src/lib.rs", library);
        tree.write("core/src/cli.rs", "include!(\"cli_cases.rs\");\n");
        tree.write("core/src/cli_cases.rs", "pub fn cli_case() {}\n");
        let error = tree
            .plan("rust", Some(&["core/src/cli_cases.rs"]))
            .unwrap_err();
        assert!(error.contains("widget-cli"), "{error}");
    }
}

#[test]
fn include_attributes_cannot_hide_conditional_paths_or_unknown_expansion() {
    for attribute in [
        "cfg_attr(feature = \"optional\", cfg(any()))",
        "cfg_attr(feature = \"optional\", path = \"other.rs\")",
        "cfg_attr(feature = \"optional\", unknown_macro)",
        "unknown_macro",
    ] {
        let tree = split_layout();
        tree.write(
            "core/src/cli.rs",
            &format!("#[{attribute}] include!(\"cli_cases.rs\");\n"),
        );
        tree.write("core/src/cli_cases.rs", "pub fn cli_case() {}\n");
        let error = tree.plan("rust", Some(&["core/src/lib.rs"])).unwrap_err();
        assert!(
            error.contains("core/src/cli.rs")
                && error.contains("require expanded Cargo target ownership"),
            "{error}"
        );
    }
}

#[test]
fn ordinary_modules_and_includes_visit_the_same_file_in_distinct_contexts() {
    let tree = split_layout();
    tree.write(
        "core/src/main.rs",
        "mod inc;\nmod a { include!(\"inc.rs\"); }\nfn main() {}\n",
    );
    tree.write("core/src/inc.rs", "mod child;\n");
    tree.write("core/src/inc/child.rs", "pub fn ordinary_child() {}\n");
    tree.write("core/src/child.rs", "pub fn included_child() {}\n");
    tree.plan("rust", Some(&["core/src/lib.rs"]))
        .expect("both compiler-selected child paths exist");
    for path in ["core/src/inc/child.rs", "core/src/child.rs"] {
        let error = tree.plan("rust", Some(&[path])).unwrap_err();
        assert!(
            error.contains(path) && error.contains("widget-cli"),
            "{error}"
        );
    }
}

#[test]
fn explicit_module_paths_use_the_physical_parent_for_child_modules() {
    let tree = split_layout();
    tree.write(
        "core/src/main.rs",
        "#[path = \"inc.rs\"] mod normal;\nfn main() {}\n",
    );
    tree.write("core/src/inc.rs", "mod child;\n");
    tree.write("core/src/child.rs", "pub fn explicit_child() {}\n");
    tree.plan("rust", Some(&["core/src/lib.rs"]))
        .expect("explicit paths use physical parent");
    let error = tree.plan("rust", Some(&["core/src/child.rs"])).unwrap_err();
    assert!(error.contains("widget-cli"), "{error}");
}

#[test]
fn inline_path_attributes_use_the_enclosing_module_directory() {
    let tree = split_layout();
    tree.write("core/src/main.rs", "mod a;\nfn main() {}\n");
    tree.write("core/src/a.rs", "mod b;\n");
    tree.write(
        "core/src/a/b.rs",
        "mod inline { #[path = \"other.rs\"] mod inner; }\n",
    );
    let child = "core/src/a/b/inline/other.rs";
    tree.write(child, "pub fn nested_child() {}\n");
    tree.plan("rust", Some(&["core/src/lib.rs"]))
        .expect("compiler-selected nested inline path exists");
    let error = tree.plan("rust", Some(&[child])).unwrap_err();
    assert!(
        error.contains(child) && error.contains("widget-cli"),
        "{error}"
    );
}

#[test]
fn unknown_item_macros_and_shadowed_includes_require_expansion() {
    for source in [
        "macro_rules! generate { () => { mod generated; } } generate!();",
        "macro_rules! include { ($path:literal) => { mod generated; } } include!(\"cases.rs\");",
    ] {
        let tree = split_layout();
        tree.write("core/src/cli.rs", source);
        let error = tree.plan("rust", Some(&["core/src/lib.rs"])).unwrap_err();
        assert!(
            error.contains("core/src/cli.rs")
                && error.contains("require")
                && error.contains("expanded Cargo target ownership"),
            "{error}"
        );
    }
}

#[test]
fn explicit_macro_imports_cannot_shadow_literal_include_ownership() {
    for import in [
        "use crate::macros::include;",
        "use crate::macros::replacement as include;",
        "use crate::{macros::{replacement as include}};",
        "use crate::{macros::{include}};",
    ] {
        let tree = split_layout();
        tree.write(
            "core/src/cli.rs",
            &format!("{import}\ninclude!(\"cli_cases.rs\");\n"),
        );
        tree.write("core/src/cli_cases.rs", "pub fn cli_case() {}\n");
        let error = tree.plan("rust", Some(&["core/src/lib.rs"])).unwrap_err();
        assert!(
            error.contains("core/src/cli.rs")
                && error.contains("imported include! requires expanded Cargo target ownership"),
            "{error}"
        );
    }
}

#[test]
fn module_reads_reject_files_above_the_byte_budget() {
    let tree = split_layout();
    tree.write("core/src/cli.rs", &" ".repeat(2 * 1024 * 1024 + 1));
    let error = tree.plan("rust", Some(&["core/src/lib.rs"])).unwrap_err();
    assert!(
        error.contains("core/src/cli.rs") && error.contains("2 MiB per file"),
        "{error}"
    );
}

#[test]
fn conditional_paths_and_unknown_attributes_still_require_expansion() {
    for applied in ["path = \"other.rs\"", "cfg(any())", "unknown_macro"] {
        let tree = split_layout();
        tree.write(
            "core/src/cli.rs",
            &format!(
                "#[cfg_attr(feature = \"optional\", {applied})] mod child;\npub fn run() {{}}\n"
            ),
        );
        tree.write("core/src/cli/child.rs", "pub fn child() {}\n");
        let error = tree.plan("rust", Some(&["core/src/lib.rs"])).unwrap_err();
        assert!(
            error.contains("core/src/cli.rs")
                && error.contains("require expanded Cargo target ownership"),
            "{error}"
        );
    }
}

#[test]
fn conditional_local_modules_do_not_prove_default_package_ownership() {
    let tree = split_layout();
    tree.write(
        "core/src/lib.rs",
        "#[cfg(any())] mod cli;\n#[test] fn library_test() {}\n",
    );
    assert!(tree
        .plan("rust", Some(&["core/src/cli.rs"]))
        .unwrap_err()
        .contains("widget-cli"));
}

#[test]
fn missing_external_target_is_an_error_even_for_an_unrelated_scope() {
    let tree = split_layout();
    tree.write(
        "cli/Cargo.toml",
        "[package]\nname = 'widget-cli'\n[[bin]]\nname = 'widget'\npath = '../missing.rs'\n",
    );
    let error = tree.plan("rust", Some(&["core/src/lib.rs"])).unwrap_err();
    assert!(
        error.contains("cannot resolve Rust target/module") && error.contains("missing.rs"),
        "{error}"
    );
}

#[test]
fn a_pathless_target_prevents_skipping_a_mixed_package() {
    let tree = split_layout();
    tree.write("cli/Cargo.toml", "[package]\nname = 'widget-cli'\nautobins = false\n[[bin]]\nname = 'widget'\npath = '../core/src/main.rs'\n[[bin]]\nname = 'missing'\n");
    let error = tree.plan("rust", None).unwrap_err();
    assert_eq!(error, "crate cli declares a package but has no src/*.rs");
}

#[test]
fn parent_path_that_resolves_back_inside_the_package_is_not_external() {
    let tree = Tree::new();
    tree.write(
        "cli/Cargo.toml",
        "[package]\nname = 'widget-cli'\n[[bin]]\nname = 'widget'\npath = '../cli/main.rs'\n",
    );
    tree.write("cli/main.rs", "fn main() {}\n");
    assert_eq!(
        tree.plan("rust", None).unwrap_err(),
        "crate cli declares a package but has no src/*.rs"
    );
}

#[test]
fn an_external_target_without_a_containing_package_is_explicitly_refused() {
    let tree = Tree::new();
    tree.write(
        "cli/Cargo.toml",
        "[package]\nname = 'widget-cli'\n[[bin]]\nname = 'widget'\npath = '../shared/main.rs'\n",
    );
    tree.write("shared/main.rs", "fn main() {}\n");
    let error = tree.plan("rust", Some(&["shared/main.rs"])).unwrap_err();
    assert!(
        error.contains("shared/main.rs") && error.contains("widget-cli"),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn a_target_symlink_outside_the_repository_is_refused() {
    let tree = split_layout();
    let outside = Tree::new();
    outside.write("main.rs", "fn main() {}\n");
    std::os::unix::fs::symlink(outside.0.join("main.rs"), tree.0.join("cli/escaped.rs")).unwrap();
    tree.write(
        "cli/Cargo.toml",
        "[package]\nname = 'widget-cli'\n[[bin]]\nname = 'widget'\npath = 'escaped.rs'\n",
    );
    assert!(tree
        .plan("rust", None)
        .unwrap_err()
        .contains("outside the repository"));
}
