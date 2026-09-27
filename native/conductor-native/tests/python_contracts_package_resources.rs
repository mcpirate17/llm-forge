#![cfg(feature = "python-compat-tests")]
//! Rust-owned parity for test_package_resources.py (23 expanded cases).

#[path = "python_contracts/package_resources_support.rs"]
mod fixtures;

use base64::Engine;
use fixtures::{
    digest_files, installed_tooling, run_reader, rust_test_temp, sha256_hex, BaselineLease, Hook,
    TempCase,
};
use serde_json::{json, Value};
use sha2::Digest;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

fn installed(
    label: &str,
) -> (
    BaselineLease,
    TempCase,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let baseline = BaselineLease::acquire();
    let case = rust_test_temp(label);
    let (python, site) = installed_tooling(&case, &baseline);
    (baseline, case, python, site)
}

fn record(site: &Path) -> std::path::PathBuf {
    site.join("conductor_tooling-9.9.9.dist-info/RECORD")
}

fn rewrite_record(site: &Path, path: &str, field: usize, replacement: &str) {
    let target = record(site);
    let mut lines = fs::read_to_string(&target)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut found = false;
    for line in &mut lines {
        if line.starts_with(&format!("{path},")) {
            let mut columns = line.split(',').map(str::to_owned).collect::<Vec<_>>();
            columns[field] = replacement.to_owned();
            *line = columns.join(",");
            found = true;
            break;
        }
    }
    assert!(found, "fixture RECORD omitted {path}");
    fs::write(target, format!("{}\n", lines.join("\n"))).unwrap();
}

fn code(result: &Value) -> &str {
    result["code"].as_str().expect("error result code")
}

#[test]
fn reads_verified_bytes_from_an_installed_unpacked_wheel() {
    let (_baseline, _case, python, site) = installed("verified-wheel");
    let before = digest_files(&site);
    let tooling = run_reader(&python, &site, "tooling", "fixture.txt", Hook::None);
    let conductor = run_reader(&python, &site, "conductor", "fixture.txt", Hook::None);
    assert_eq!(
        tooling,
        json!({
            "package": "tooling",
            "name": "fixture.txt",
            "version": "9.9.9",
            "data": "tooling-installed-resource\n",
            "sha256": sha256_hex(b"tooling-installed-resource\n"),
        })
    );
    assert_eq!(conductor["data"], "conductor-installed-resource\n");
    assert_eq!(digest_files(&site), before);
}

macro_rules! invalid_selector_case {
    ($name:ident, $package:expr, $asset:expr) => {
        #[test]
        fn $name() {
            let (_baseline, _case, python, site) = installed(stringify!($name));
            assert_eq!(
                code(&run_reader(&python, &site, $package, $asset, Hook::None)),
                "RESOURCE_NAME_INVALID"
            );
        }
    };
}
invalid_selector_case!(invalid_selector_other_package, "other", "fixture.txt");
invalid_selector_case!(invalid_selector_empty_name, "tooling", "");
invalid_selector_case!(invalid_selector_absolute_name, "tooling", "/fixture.txt");
invalid_selector_case!(
    invalid_selector_parent_component,
    "tooling",
    "../fixture.txt"
);
invalid_selector_case!(
    invalid_selector_empty_component,
    "tooling",
    "nested//fixture.txt"
);
invalid_selector_case!(invalid_selector_backslash, "tooling", "nested\\fixture.txt");
invalid_selector_case!(invalid_selector_nul, "tooling", "fixture\0.txt");

#[test]
fn refuses_foreign_namespace_pollution() {
    let (_baseline, case, python, site) = installed("foreign-namespace");
    let foreign = case.root().join("foreign");
    fs::create_dir_all(foreign.join("tooling")).unwrap();
    fs::write(
        foreign.join("tooling/fixture.txt"),
        b"foreign namespace bytes\n",
    )
    .unwrap();
    assert_eq!(
        run_reader(
            &python,
            &site,
            "tooling",
            "fixture.txt",
            Hook::AppendPath { path: foreign }
        ),
        json!({"code":"RESOURCE_LAYOUT_UNSUPPORTED", "field":"package"})
    );
}

#[test]
fn refuses_actual_project_shadow_before_resource_traversal() {
    let (_baseline, case, python, site) = installed("project-shadow");
    let foreign = case.root().join("foreign");
    fs::create_dir_all(foreign.join("tooling")).unwrap();
    fs::write(foreign.join("tooling/__init__.py"), b"").unwrap();
    assert_eq!(
        run_reader(
            &python,
            &site,
            "tooling",
            "fixture.txt",
            Hook::ShadowPath { path: foreign }
        ),
        json!({"code":"RESOURCE_LAYOUT_UNSUPPORTED", "field":"package"})
    );
}

#[test]
fn refuses_missing_record() {
    let (_baseline, _case, python, site) = installed("missing-record");
    fs::remove_file(record(&site)).unwrap();
    assert_eq!(
        code(&run_reader(
            &python,
            &site,
            "tooling",
            "fixture.txt",
            Hook::None
        )),
        "RESOURCE_LAYOUT_UNSUPPORTED"
    );
}

#[test]
fn requires_exact_record_path_membership() {
    let (_baseline, _case, python, site) = installed("exact-record-path");
    let root_asset = site.join("tooling/fixture.txt");
    let nested_asset = site.join("tooling/nested/fixture.txt");
    fs::write(&nested_asset, fs::read(&root_asset).unwrap()).unwrap();
    let rows = fs::read_to_string(record(&site)).unwrap();
    let mut lines = rows
        .lines()
        .filter(|line| !line.starts_with("tooling/fixture.txt,"))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let nested_row = lines
        .iter_mut()
        .find(|line| line.starts_with("tooling/nested/fixture.txt,"))
        .expect("nested path in RECORD");
    let replacement_row = format!(
        "tooling/nested/fixture.txt,sha256={},{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(fs::read(&root_asset).unwrap())),
        fs::metadata(&root_asset).unwrap().len()
    );
    *nested_row = replacement_row.clone();
    let matching = lines
        .iter()
        .filter(|line| {
            line.starts_with("tooling/")
                && Path::new(line.split(',').next().unwrap())
                    .file_name()
                    .unwrap()
                    == "fixture.txt"
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(matching, vec![replacement_row]);
    fs::write(record(&site), format!("{}\n", lines.join("\n"))).unwrap();
    assert_eq!(
        run_reader(&python, &site, "tooling", "fixture.txt", Hook::None),
        json!({"code":"RESOURCE_NOT_FOUND", "field":"name"})
    );
}

macro_rules! malformed_record_case {
    ($name:ident, $field:expr, $replacement:expr) => {
        #[test]
        fn $name() {
            let (_baseline, _case, python, site) = installed(stringify!($name));
            rewrite_record(&site, "tooling/fixture.txt", $field, $replacement);
            assert_eq!(
                code(&run_reader(
                    &python,
                    &site,
                    "tooling",
                    "fixture.txt",
                    Hook::None
                )),
                "RESOURCE_LAYOUT_UNSUPPORTED"
            );
        }
    };
}
malformed_record_case!(
    malformed_record_hash_is_refused,
    1,
    "md5=not-a-wheel-digest"
);
malformed_record_case!(malformed_record_size_is_refused, 2, "not-a-size");

#[test]
fn refuses_record_declared_over_limit_before_read() {
    let (_baseline, _case, python, site) = installed("record-over-limit");
    rewrite_record(&site, "tooling/fixture.txt", 2, "4194305");
    assert_eq!(
        code(&run_reader(
            &python,
            &site,
            "tooling",
            "fixture.txt",
            Hook::None
        )),
        "RESOURCE_TOO_LARGE"
    );
}

#[test]
fn refuses_duplicate_installed_distribution() {
    let (_baseline, _case, python, site) = installed("duplicate-distribution");
    let source = site.join("conductor_tooling-9.9.9.dist-info");
    let duplicate = site.join("conductor_tooling-8.8.8.dist-info");
    fs::create_dir_all(&duplicate).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            fs::copy(entry.path(), duplicate.join(entry.file_name())).unwrap();
        }
    }
    let metadata = duplicate.join("METADATA");
    let body = fs::read_to_string(&metadata)
        .unwrap()
        .replace("Version: 9.9.9", "Version: 8.8.8");
    fs::write(metadata, body).unwrap();
    assert_eq!(
        code(&run_reader(
            &python,
            &site,
            "tooling",
            "fixture.txt",
            Hook::None
        )),
        "RESOURCE_LAYOUT_UNSUPPORTED"
    );
}

#[test]
fn refuses_editable_and_malformed_direct_url_metadata() {
    let (_baseline, _case, python, site) = installed("direct-url");
    let direct_url = site.join("conductor_tooling-9.9.9.dist-info/direct_url.json");
    fs::write(&direct_url, r#"{"dir_info":{"editable":true}}"#).unwrap();
    assert_eq!(
        code(&run_reader(
            &python,
            &site,
            "tooling",
            "fixture.txt",
            Hook::None
        )),
        "RESOURCE_LAYOUT_UNSUPPORTED"
    );
    fs::write(&direct_url, "not valid JSON").unwrap();
    assert_eq!(
        code(&run_reader(
            &python,
            &site,
            "tooling",
            "fixture.txt",
            Hook::None
        )),
        "RESOURCE_LAYOUT_UNSUPPORTED"
    );
}

#[test]
fn refuses_symlink_oversize_and_record_drift() {
    let (_baseline, case, python, site) = installed("symlink-large-drift");
    let resource = site.join("tooling/fixture.txt");
    let target = case.root().join("target.txt");
    fs::write(&target, b"outside\n").unwrap();
    fs::remove_file(&resource).unwrap();
    symlink(&target, &resource).unwrap();
    assert_eq!(
        code(&run_reader(
            &python,
            &site,
            "tooling",
            "fixture.txt",
            Hook::None
        )),
        "RESOURCE_UNSAFE"
    );
    fs::remove_file(&resource).unwrap();
    fs::write(&resource, vec![b'x'; 4 * 1024 * 1024 + 1]).unwrap();
    assert_eq!(
        code(&run_reader(
            &python,
            &site,
            "tooling",
            "fixture.txt",
            Hook::None
        )),
        "RESOURCE_TOO_LARGE"
    );
    fs::write(&resource, b"drifted-installed-resource\n").unwrap();
    assert_eq!(
        code(&run_reader(
            &python,
            &site,
            "tooling",
            "fixture.txt",
            Hook::None
        )),
        "RESOURCE_CHANGED"
    );
}

#[test]
fn refuses_intermediate_symlink_and_requires_record_path_membership() {
    let (_baseline, _case, python, site) = installed("intermediate-symlink");
    let nested = site.join("tooling/nested");
    let target = site.join("tooling/foreign-nested");
    fs::create_dir(&target).unwrap();
    fs::write(
        target.join("fixture.txt"),
        fs::read(nested.join("fixture.txt")).unwrap(),
    )
    .unwrap();
    fs::remove_file(nested.join("fixture.txt")).unwrap();
    fs::remove_dir(&nested).unwrap();
    symlink(&target, &nested).unwrap();
    assert_eq!(
        code(&run_reader(
            &python,
            &site,
            "tooling",
            "nested/fixture.txt",
            Hook::None
        )),
        "RESOURCE_UNSAFE"
    );
}

#[test]
fn refuses_asset_path_replaced_after_open() {
    let (_baseline, _case, python, site) = installed("replace-after-open");
    let resource = site.join("tooling/fixture.txt");
    let replacement = site.join("tooling/replacement.txt");
    fs::write(&replacement, b"tooling-installed-resource\n").unwrap();
    assert_eq!(
        run_reader(
            &python,
            &site,
            "tooling",
            "fixture.txt",
            Hook::ReplaceAfterOpen {
                resource,
                replacement
            },
        ),
        json!({"code":"RESOURCE_CHANGED", "field":"name"})
    );
}

#[test]
fn refuses_fifo_swapped_before_open_without_blocking() {
    let (_baseline, _case, python, site) = installed("fifo-swap");
    let resource = site.join("tooling/fixture.txt");
    assert_eq!(
        run_reader(
            &python,
            &site,
            "tooling",
            "fixture.txt",
            Hook::FifoBeforeOpen { resource },
        ),
        json!({"code":"RESOURCE_UNSAFE", "field":"name"})
    );
}

#[test]
fn refuses_same_byte_symlink_swapped_between_validation_and_open() {
    let (_baseline, _case, python, site) = installed("same-byte-symlink");
    let resource = site.join("tooling/fixture.txt");
    let target = site.join("same-byte-symlink-target.txt");
    fs::write(&target, fs::read(&resource).unwrap()).unwrap();
    assert_eq!(
        run_reader(
            &python,
            &site,
            "tooling",
            "fixture.txt",
            Hook::SymlinkBeforeOpen { resource, target },
        ),
        json!({"code":"RESOURCE_UNSAFE", "field":"name"})
    );
}

#[test]
fn anchored_walk_refuses_intermediate_directory_swapped_to_external_same_bytes() {
    let (_baseline, case, python, site) = installed("anchored-intermediate-swap");
    let nested = site.join("tooling/nested");
    let external = case.root().join("external");
    fs::create_dir(&external).unwrap();
    fs::write(
        external.join("fixture.txt"),
        fs::read(nested.join("fixture.txt")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        run_reader(
            &python,
            &site,
            "tooling",
            "nested/fixture.txt",
            Hook::IntermediateBeforeOpen { nested, external },
        ),
        json!({"code":"RESOURCE_UNSAFE", "field":"name"})
    );
}
