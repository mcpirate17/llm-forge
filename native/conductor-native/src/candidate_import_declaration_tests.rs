use super::*;

fn input(source: &str, manifest: &str) -> Value {
    json!({"files":[{"path":"conductor/a.py","text":source,"manifest":"pyproject.toml"}],
        "manifests":[{"path":"pyproject.toml","text":manifest}],"strict_trees":["conductor","tooling"],
        "providers":{"stdlib":["sys"],"modules":[{"name":"foo","distributions":["foo-dist"]},
        {"name":"other","distributions":["other-dist"]},{"name":"pytest","distributions":["pytest"]}],
        "provenance":{"schema_version":1,"implementation":"cpython","version":"3.12","platform":"linux","sha256":"a".repeat(64)}}})
}

#[test]
fn declarations_type_only_and_unresolved_are_distinct() {
    let value=input("import foo\nimport other\nimport sys\nimport _private\nimport unknown\nfrom . import local\nif TYPE_CHECKING:\n    import foo\n", "[project]\nname='fixture'\ndependencies=[]\n[project.optional-dependencies]\ndev=['foo-dist>=1']\n");
    let report = evaluate(&value).unwrap();
    let findings = report["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 2);
    assert_eq!(findings[0]["rule_id"], "undeclared-dependency");
    assert_eq!(findings[1]["rule_id"], "base-dependency-required");
    assert_eq!(report["unresolved"][0]["module"], "unknown");
    assert_eq!(report["provider_scope"]["complete"], false);
}

#[test]
fn type_checking_else_and_declared_provider_union() {
    let mut value=input("if typing.TYPE_CHECKING:\n    import foo\nelse:\n    import other\n", "[project]\nname='fixture'\ndependencies=['other-dist']\n[project.optional-dependencies]\ndev=['foo-dist']\n");
    assert!(evaluate(&value).unwrap()["findings"]
        .as_array()
        .unwrap()
        .is_empty());
    value["files"][0]["text"] =
        json!("if other_condition:\n    pass\nelif TYPE_CHECKING:\n    import foo\n");
    assert!(evaluate(&value).unwrap()["findings"]
        .as_array()
        .unwrap()
        .is_empty());
    value["providers"]["modules"][0]["distributions"] = json!(["foo-dist", "other-dist"]);
    value["files"][0]["text"] = json!("import foo\n");
    assert!(evaluate(&value).unwrap()["findings"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn nearest_manifest_and_input_provenance_fail_closed() {
    let mut value = input("import foo\n", "[project]\nname='fixture'\nversion='1'\n");
    value["manifests"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path":"conductor/pyproject.toml","text":"[project]\nname='nested'"}));
    assert!(evaluate(&value).is_err());
    value["files"][0]["manifest"] = json!("conductor/pyproject.toml");
    assert!(evaluate(&value).is_ok());
    value["providers"]["provenance"]["sha256"] = json!("invalid");
    assert!(evaluate(&value).is_err());
}

#[test]
fn exact_exemption_and_staleness_are_preserved() {
    let mut value = input(
        "import pytest\n",
        "[project]\nname='fixture'\n[project.optional-dependencies]\ndev=['pytest']\n",
    );
    value["files"][0]["path"] = json!("conductor/equivalence_probe.py");
    assert!(evaluate(&value).unwrap()["findings"]
        .as_array()
        .unwrap()
        .is_empty());
    value["files"][0]["text"] = json!("x = 1\n");
    assert_eq!(
        evaluate(&value).unwrap()["findings"][0]["rule_id"],
        "stale-exemption"
    );
}

#[test]
fn invalid_source_duplicates_and_bounds_are_incomplete() {
    let mut value = input("def f(:\n", "[project]\nname='fixture'\n");
    assert!(evaluate(&value).is_err());
    value["files"][0]["text"] = json!("x = 1\n");
    value["providers"]["modules"][0]["distributions"] = json!(["foo-dist", "foo_dist"]);
    assert!(evaluate(&value).is_err());
    value["providers"]["modules"][0]["distributions"] = json!(["foo-dist"]);
    value["files"][0]["text"] = json!("x".repeat(MAX_FILE + 1));
    assert!(evaluate(&value).is_err());
}

#[test]
fn groups_build_requirements_and_test_infrastructure() {
    let mut value=input("import foo\nimport other\n", "[project]\nname='fixture'\n[dependency-groups]\ndev=['foo-dist']\n[build-system]\nrequires=['other-dist']\n");
    value["files"][0]["path"] = json!("conductor/conftest.py");
    assert!(evaluate(&value).unwrap()["findings"]
        .as_array()
        .unwrap()
        .is_empty());
    value["files"][0]["path"] = json!("conductor/a.py");
    assert_eq!(
        evaluate(&value).unwrap()["findings"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn no_manifest_is_counted_and_empty_provider_is_not_guessed() {
    let mut value = input("import missing\n", "[project]\nname='fixture'\n");
    value["manifests"] = json!([]);
    value["files"][0]["manifest"] = Value::Null;
    let report = evaluate(&value).unwrap();
    assert_eq!(report["metrics"]["files_without_manifest"], 1);
    assert_eq!(report["provider_scope"]["runtime_verified"], false);
    value["manifests"] = json!([{"path":"pyproject.toml","text":"[project]\nname='fixture'"}]);
    value["files"][0]["manifest"] = json!("pyproject.toml");
    let report = evaluate(&value).unwrap();
    assert_eq!(report["unresolved"][0]["module"], "missing");
}

#[test]
fn provider_identifiers_include_unicode_combining_marks_without_normalizing_evidence() {
    assert!(module_name("a\u{301}").is_ok());
    assert!(module_name("δ_2").is_ok());
    assert!(module_name("\u{301}a").is_err());
    assert!(module_name("a.b").is_err());
    assert!(module_name("a;import os").is_err());
    let mut value = input(
        "import a\u{301}\n",
        "[project]\nname='fixture'\ndependencies=['foo-dist']\n",
    );
    value["providers"]["modules"][0]["name"] = json!("á");
    assert!(evaluate(&value).unwrap()["findings"]
        .as_array()
        .unwrap()
        .is_empty());
}
#[test]
fn requirement_names_accept_pep503_extras_markers_and_direct_references() {
    for requirement in [
        "Foo_Dist[http]>=1.1",
        "foo.dist; python_version >= '3.12'",
        "FOO-DIST @ file:///wheel",
    ] {
        let manifest = format!("[project]\nname='fixture'\ndependencies=[\"{requirement}\"]\n");
        assert!(
            evaluate(&input("import foo\n", &manifest)).unwrap()["findings"]
                .as_array()
                .unwrap()
                .is_empty(),
            "{requirement}"
        );
    }
}

#[test]
fn nearest_manifest_controls_the_declaration_rather_than_root() {
    let mut value = input(
        "import foo\n",
        "[project]\nname='root'\ndependencies=['foo-dist']\n",
    );
    value["files"][0]["path"] = json!("conductor/nested/a.py");
    value["files"][0]["manifest"] = json!("conductor/pyproject.toml");
    value["manifests"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path":"conductor/pyproject.toml","text":"[project]\nname='nested'\n"}));
    let report = evaluate(&value).unwrap();
    assert_eq!(report["findings"][0]["rule_id"], "undeclared-dependency");
    assert_eq!(
        report["findings"][0]["evidence"]["manifest"],
        "conductor/pyproject.toml"
    );
}

#[test]
fn relative_imports_are_excluded_but_type_only_undeclared_imports_are_not() {
    let value=input("from .foo import member\nfrom ..other import value\nfrom foo.submodule import thing\nif TYPE_CHECKING:\n    import other\n", "[project]\nname='fixture'\ndependencies=['foo-dist']\n");
    let report = evaluate(&value).unwrap();
    assert_eq!(report["findings"].as_array().unwrap().len(), 1);
    assert_eq!(report["findings"][0]["rule_id"], "undeclared-dependency");
    assert_eq!(report["findings"][0]["evidence"]["module"], "other");
}

#[test]
fn optional_dependency_is_allowed_outside_strict_tree() {
    let mut value = input(
        "import foo\n",
        "[project]\nname='fixture'\n[project.optional-dependencies]\ndev=['foo-dist']\n",
    );
    value["files"][0]["path"] = json!("research/a.py");
    let report = evaluate(&value).unwrap();
    assert!(report["findings"].as_array().unwrap().is_empty());
    assert_eq!(report["metrics"]["base_dependency_files"], 0);
}

#[test]
fn pytest_exemption_never_waives_undeclared_dependency() {
    let mut value = input("import pytest\n", "[project]\nname='fixture'\n");
    value["files"][0]["path"] = json!("conductor/equivalence_probe.py");
    let result = evaluate(&value).unwrap();
    assert_eq!(result["findings"][0]["rule_id"], "undeclared-dependency");
    assert_eq!(result["findings"][1]["rule_id"], "stale-exemption");
}

#[test]
fn malformed_manifest_is_incomplete_not_an_empty_declaration_set() {
    assert!(evaluate(&input("import foo\n", "[project\nname=")).is_err());
}

#[test]
fn supplied_stdlib_wins_over_provider_and_namespace_evidence_is_sorted() {
    let mut value = input("import sys\nimport foo\n", "[project]\nname='fixture'\n");
    value["providers"]["modules"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"sys","distributions":["sys-package"]}));
    value["providers"]["modules"][0]["distributions"] = json!(["Z-Package", "a-package"]);
    let result = evaluate(&value).unwrap();
    assert_eq!(result["findings"].as_array().unwrap().len(), 1);
    assert_eq!(result["findings"][0]["evidence"]["module"], "foo");
    assert_eq!(
        result["findings"][0]["evidence"]["distributions"],
        json!(["Z-Package", "a-package"])
    );
}
