use super::*;
const OLD: &str = "pub fn value() -> u32 { 1 }\n";
const NEW: &str = "pub fn value() -> u32 { 2 }\n";
const BUMP: &str = "source-changed-without-version-bump";
fn manifest(name: &str, version: &str) -> String {
    format!("[package]\nname='{name}'\nversion='{version}'\n")
}
fn file(path: &str, current: Option<&str>, base: Option<&str>) -> Value {
    json!({"path":path,"current":current,"base":base})
}
fn payload(manifests: Vec<Value>, sources: Vec<Value>, installed: Value) -> Value {
    json!({"manifests":manifests,"sources":sources,"installed":installed})
}
fn run(
    current: Option<&str>,
    base: Option<&str>,
    source: &str,
    head: Option<&str>,
    old: Option<&str>,
) -> Value {
    evaluate(&payload(
        vec![file("crate/Cargo.toml", current, base)],
        vec![file(source, head, old)],
        json!([]),
    ))
    .unwrap()
}
fn rules(result: &Value) -> Vec<&str> {
    result["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["rule_id"].as_str().unwrap())
        .collect()
}
#[test]
fn changed_source_requires_a_bump_but_new_crate_and_manifest_only_do_not() {
    let old = manifest("example", "0.1.0");
    let new = manifest("example", "0.1.1");
    let unchanged = run(
        Some(&old),
        Some(&old),
        "crate/src/lib.rs",
        Some(NEW),
        Some(OLD),
    );
    assert_eq!(rules(&unchanged), [BUMP]);
    assert_eq!(unchanged["findings"][0]["severity"], "critical");
    assert_eq!(unchanged["files"], json!(["crate/Cargo.toml"]));
    assert_eq!(
        unchanged["metrics"],
        json!({"crates_indexed":1,"crates_touched":1})
    );
    assert!(rules(&run(
        Some(&new),
        Some(&old),
        "crate/src/lib.rs",
        Some(NEW),
        Some(OLD)
    ))
    .is_empty());
    assert!(rules(&run(Some(&old), None, "crate/src/lib.rs", Some(NEW), None)).is_empty());
    let only = run(
        Some(&new),
        Some(&old),
        "crate/Cargo.toml",
        Some(&new),
        Some(&old),
    );
    assert_eq!(only["metrics"]["crates_touched"], 0);
    assert!(rules(&only).is_empty());
}
#[test]
fn deepest_component_owner_wins_and_files_sort_independently_of_input_order() {
    let text = manifest("example", "1.0.0");
    let result = evaluate(&payload(
        vec![
            file("nested/Cargo.toml", Some(&text), Some(&text)),
            file("Cargo.toml", Some(&text), Some(&text)),
            file("a/Cargo.toml", Some(&text), Some(&text)),
        ],
        vec![
            file("nested/src/lib.rs", Some(NEW), Some(OLD)),
            file("a/src/lib.rs", Some(NEW), Some(OLD)),
        ],
        json!([]),
    ))
    .unwrap();
    assert_eq!(
        result["files"],
        json!(["a/Cargo.toml", "nested/Cargo.toml"])
    );
    assert_eq!(
        result["metrics"],
        json!({"crates_indexed":3,"crates_touched":2})
    );
    assert_eq!(result["findings"][0]["path"], "a/Cargo.toml");
    let outside = run(
        Some(&text),
        Some(&text),
        "crate_other/src/lib.rs",
        Some(NEW),
        Some(OLD),
    );
    assert_eq!(outside["metrics"]["crates_touched"], 0);
}
#[test]
fn build_suffixes_and_artifact_directory_exclusions_are_precise() {
    let text = manifest("example", "1.0.0");
    for suffix in ["rs", "c", "CC", "Cpp", "cu", "H", "hpp"] {
        let source = format!("crate/src/code.{suffix}");
        assert_eq!(
            rules(&run(
                Some(&text),
                Some(&text),
                &source,
                Some(NEW),
                Some(OLD)
            )),
            [BUMP]
        );
    }
    for source in [
        "crate/README.md",
        "crate/.rs",
        "crate/tests/a.rs",
        "crate/benches/a.c",
        "crate/examples/a.cpp",
    ] {
        let result = run(Some(&text), Some(&text), source, Some(NEW), Some(OLD));
        assert_eq!(result["metrics"]["crates_touched"], 0, "{source}");
    }
    // A nested module directory named tests is not Cargo's crate-root tests dir.
    assert_eq!(
        rules(&run(
            Some(&text),
            Some(&text),
            "crate/src/tests/a.rs",
            Some(NEW),
            Some(OLD)
        )),
        [BUMP]
    );
    assert!(rules(&run(
        Some(&text),
        Some(&text),
        "crate/src/a.c",
        Some("same"),
        Some("same")
    ))
    .is_empty());
}
#[test]
fn source_deletion_addition_and_rust_parse_failure_are_conservative_build_changes() {
    let text = manifest("example", "1.0.0");
    for source in ["crate/src/lib.rs", "crate/src/a.c"] {
        assert_eq!(
            rules(&run(Some(&text), Some(&text), source, None, Some(OLD))),
            [BUMP]
        );
        assert_eq!(
            rules(&run(Some(&text), Some(&text), source, Some(NEW), None)),
            [BUMP]
        );
    }
    assert_eq!(
        rules(&run(
            Some(&text),
            Some(&text),
            "crate/src/lib.rs",
            Some("pub fn broken( {"),
            Some(OLD)
        )),
        [BUMP]
    );
    assert_eq!(
        rules(&run(
            Some(&text),
            Some(&text),
            "crate/src/lib.rs",
            Some("not Rust"),
            Some("not Rust")
        )),
        [BUMP]
    );
}
fn inline(body: &str, extra: &str) -> String {
    format!("pub fn shipped() -> u32 {{ {body} }}\n#[cfg(test)]\nmod tests {{\n#[test]\nfn check() {{ let brace = r#\"}}\"quoted\"#; /* }} */ assert_eq!(shipped(), 1); }}\n{extra}\n}}\n#[cfg(test)] use super::unused;\n")
}
#[test]
fn inline_cfg_tests_do_not_hide_adjacent_production_changes() {
    let text = manifest("example", "1.0.0");
    let old = inline("1", "");
    let new = inline("1", "#[test] fn added() { assert!(true); }");
    let result = run(
        Some(&text),
        Some(&text),
        "crate/src/lib.rs",
        Some(&new),
        Some(&old),
    );
    assert_eq!(result["metrics"]["crates_touched"], 0);
    let shipped = inline("2", "");
    assert_eq!(
        rules(&run(
            Some(&text),
            Some(&text),
            "crate/src/lib.rs",
            Some(&shipped),
            Some(&old)
        )),
        [BUMP]
    );
}
#[test]
fn unreadable_malformed_inherited_and_malformed_base_manifests_are_not_clean() {
    let text = manifest("example", "1.0.0");
    let unavailable = run(None, Some(&text), "crate/src/lib.rs", Some(NEW), Some(OLD));
    assert_eq!(rules(&unavailable), ["unreadable-manifest"]);
    assert_eq!(unavailable["findings"][0]["severity"], "critical");
    for current in [
        "[package",
        "[package]\nname='example'\nversion.workspace=true\n",
        "[workspace]\nmembers=[]\n",
    ] {
        let result = run(
            Some(current),
            Some(&text),
            "crate/src/lib.rs",
            Some(NEW),
            Some(OLD),
        );
        assert_eq!(rules(&result), ["unresolvable-crate-version"]);
        assert_eq!(result["findings"][0]["severity"], "high");
    }
    let malformed_base = run(
        Some(&text),
        Some("malformed ["),
        "crate/src/lib.rs",
        Some(NEW),
        Some(OLD),
    );
    assert_eq!(rules(&malformed_base), ["unresolvable-crate-version"]);
}
#[test]
fn installed_metadata_normalizes_names_but_is_not_runtime_attestation() {
    let current = manifest("Example_Pkg", "1.0.1");
    let base = manifest("Example_Pkg", "1.0.0");
    let evidence = |installed| {
        payload(
            vec![file("crate/Cargo.toml", Some(&current), Some(&base))],
            vec![file("crate/src/lib.rs", Some(NEW), Some(OLD))],
            installed,
        )
    };
    assert!(rules(&evaluate(&evidence(json!([]))).unwrap()).is_empty());
    assert!(rules(
        &evaluate(&evidence(
            json!([{"name":"EXAMPLE..pkg","version":"1.0.1"}])
        ))
        .unwrap()
    )
    .is_empty());
    let drift = evaluate(&evidence(json!([{"name":"example-pkg","version":"9.9.9"}]))).unwrap();
    assert_eq!(rules(&drift), ["installed-version-drift"]);
    assert_eq!(drift["findings"][0]["severity"], "high");
    assert!(drift["findings"][0]["message"]
        .as_str()
        .unwrap()
        .contains("does not establish runtime artifact identity"));
    assert!(evaluate(&evidence(
        json!([{"name":"example_pkg","version":"1"},{"name":"EXAMPLE.PKG","version":"1"}])
    ))
    .is_err());
    assert_eq!(normalize_distribution_name("A._--B2").unwrap(), "a-b2");
    for bad in ["", "-name", "name_", "snow雪", "a/b", "a\0b"] {
        assert!(normalize_distribution_name(bad).is_err());
    }
}
#[test]
fn payload_shape_duplicate_paths_and_absent_source_evidence_fail() {
    let text = manifest("example", "1");
    let valid = payload(
        vec![file("Cargo.toml", Some(&text), None)],
        vec![],
        json!([]),
    );
    for key in ["manifests", "sources", "installed"] {
        let mut bad = valid.clone();
        bad.as_object_mut().unwrap().remove(key);
        assert!(evaluate(&bad).is_err());
    }
    let mut bad = valid.clone();
    bad["unknown"] = json!(true);
    assert!(evaluate(&bad).is_err());
    let mut bad = valid.clone();
    bad["manifests"][0]["extra"] = json!(true);
    assert!(evaluate(&bad).is_err());
    let mut bad = valid.clone();
    bad["sources"] = json!([file("a.rs", None, None)]);
    assert!(evaluate(&bad).is_err());
    let mut bad = valid.clone();
    bad["manifests"] = json!([valid["manifests"][0].clone(), valid["manifests"][0].clone()]);
    assert!(evaluate(&bad).is_err());
    for path in ["/absolute.rs", "a/../x.rs", "./x.rs", "a//x.rs", "a\nx.rs"] {
        let mut bad = valid.clone();
        bad["sources"] = json!([file(path, Some(NEW), None)]);
        assert!(evaluate(&bad).is_err());
    }
}
#[test]
fn admission_limits_fail_instead_of_dropping_evidence_or_findings() {
    let mut large = payload(vec![], vec![], json!([]));
    large["sources"] = json!(vec![file("a.rs", Some(NEW), None); MAX_RECORDS + 1]);
    assert!(evaluate(&large).unwrap_err().contains("record count"));
    large["sources"] = json!([file("a.rs", Some(&"x".repeat(MAX_TEXT + 1)), None)]);
    assert!(evaluate(&large).unwrap_err().contains("individual"));
    let sources: Vec<_> = (0..33)
        .map(|i| file(&format!("a{i}.c"), Some(&"x".repeat(MAX_TEXT)), None))
        .collect();
    large["sources"] = json!(sources);
    assert!(evaluate(&large).unwrap_err().contains("32 MiB"));
    let text = manifest("example", "1");
    let manifests: Vec<_> = (0..2049)
        .map(|i| file(&format!("crate{i}/Cargo.toml"), Some(&text), Some(&text)))
        .collect();
    let sources: Vec<_> = (0..2049)
        .map(|i| file(&format!("crate{i}/src/a.c"), Some("new"), Some("old")))
        .collect();
    let installed = json!([{"name":"example","version":"2"}]);
    assert!(evaluate(&payload(manifests, sources, installed))
        .unwrap_err()
        .contains("findings exceed"));
}
#[test]
fn missing_or_nonstring_current_package_name_is_unresolvable_even_after_bump() {
    let old = manifest("example", "1.0.0");
    for current in [
        "[package]\nversion='1.0.1'\n",
        "[package]\nname=7\nversion='1.0.1'\n",
    ] {
        let result = run(
            Some(current),
            Some(&old),
            "crate/src/lib.rs",
            Some(NEW),
            Some(OLD),
        );
        assert_eq!(rules(&result), ["unresolvable-crate-version"]);
        assert_eq!(result["findings"][0]["severity"], "high");
        assert_eq!(result["files"], json!(["crate/Cargo.toml"]));
        assert_eq!(result["metrics"]["crates_touched"], 1);
    }
}
