#[cfg(feature = "source-analysis")]
use conductor_native::mutation_value_inputs::parse_bound_cargo_libtest;
use conductor_native::mutation_value_inputs::{
    attribution_supported, collect_mutant_evidence, ctest_identity, parse_cargo_libtest,
    parse_junit_file, parse_junit_text, JunitAdapter,
};
use serde_json::json;

fn ranked(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn fully_qualified_libtest_names_preserve_real_module_identity() {
    let node = "src/lib.rs::tests::nested::test_sum";
    assert!(attribution_supported("cargo-libtest", &ranked(&[node])));
    let parsed = parse_cargo_libtest(
        "test tests::nested::test_sum ... ok\ntest other::test_sum ... FAILED\n",
        &ranked(&[node]),
    )
    .unwrap();
    assert_eq!(parsed["status"], "INCOMPLETE");
    assert!(parsed["tests"].get(node).is_none());
    for bad in [
        "src/lib.rs::tests::::test_sum",
        "src/lib.rs::tests::",
        "src/lib.rs::tests::test sum",
        "src/lib.rs::tests::1bad",
    ] {
        assert!(!attribution_supported("cargo-libtest", &ranked(&[bad])));
    }
}

#[test]
#[cfg(feature = "source-analysis")]
fn cargo_attribution_requires_one_verified_target_and_source() {
    use std::fs;
    let root = std::env::temp_dir().join(format!("cargo-bound-fixture-{}", std::process::id()));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(root.join("src/lib.rs"), "pub mod a;\n").unwrap();
    fs::write(
        root.join("src/a.rs"),
        "#[cfg(test)] mod tests { #[test] fn shared() {} }",
    )
    .unwrap();
    fs::write(
        root.join("tests/other.rs"),
        "mod a { mod tests { #[test] fn shared() {} } }\n",
    )
    .unwrap();
    let node = "src/a.rs::a::tests::shared";
    let args = ranked(&[
        "cargo",
        "test",
        "--manifest-path",
        "Cargo.toml",
        "--package",
        "fixture",
        "--lib",
    ]);
    let executed = std::process::Command::new("cargo")
        .args(&args[1..])
        .current_dir(&root)
        .env("CARGO_TARGET_DIR", root.join("target"))
        .output()
        .unwrap();
    assert!(
        executed.status.success(),
        "{}",
        String::from_utf8_lossy(&executed.stderr)
    );
    let output = String::from_utf8(executed.stdout).unwrap();
    let ok = parse_bound_cargo_libtest(&output, &ranked(&[node]), &root, &args).unwrap();
    assert_eq!(ok["tests"][node]["outcome"], "PASSED");
    let partial = parse_bound_cargo_libtest(
        "test a::tests::shared ... FAILED\n",
        &ranked(&[node]),
        &root,
        &args,
    )
    .unwrap();
    assert_eq!(partial["status"], "INCOMPLETE");
    let wrong = ranked(&[
        "cargo",
        "test",
        "--manifest-path",
        "Cargo.toml",
        "--package",
        "fixture",
        "--test",
        "other",
    ]);
    let other = std::process::Command::new("cargo")
        .args(&wrong[1..])
        .current_dir(&root)
        .env("CARGO_TARGET_DIR", root.join("target"))
        .output()
        .unwrap();
    assert!(other.status.success());
    let collision = String::from_utf8(other.stdout).unwrap();
    assert!(collision.contains("test a::tests::shared ... ok"));
    assert!(parse_bound_cargo_libtest(&collision, &ranked(&[node]), &root, &wrong).is_err());
    assert_eq!(
        parse_cargo_libtest(&collision, &ranked(&[node])).unwrap()["status"],
        "INCOMPLETE"
    );
    assert!(parse_bound_cargo_libtest(
        "test a::tests::shared ... FAILED\n",
        &ranked(&[node]),
        &root,
        &ranked(&["sh", "-c", "cargo test"])
    )
    .is_err());
    let multiple = ranked(&[
        "cargo",
        "test",
        "--manifest-path",
        "Cargo.toml",
        "--package",
        "fixture",
        "--lib",
        "--test",
        "other",
    ]);
    assert!(parse_bound_cargo_libtest(&collision, &ranked(&[node]), &root, &multiple).is_err());
    let missing = parse_bound_cargo_libtest(
        "test other::shared ... FAILED\n",
        &ranked(&[node]),
        &root,
        &args,
    )
    .unwrap();
    assert_eq!(missing["status"], "INCOMPLETE");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pytest_parameterized_cases_keep_failure_priority_and_duration_sum() {
    let nodeid = "pkg/test_math.py::TestGroup::test_add";
    let report = r#"<testsuites><testsuite>
      <testcase classname="pkg.test_math.TestGroup" name="test_add[a]" time="0.2"><skipped/></testcase>
      <testcase classname="pkg.test_math.TestGroup" name="test_add[b]" time="0.3"/>
      <testcase classname="pkg.test_math.TestGroup" name="test_add[c]" time="bad"><failure/></testcase>
      <testcase classname="pkg.test_math.TestGroup" name="test_add[d]"><error/></testcase>
    </testsuite></testsuites>"#;
    let result = parse_junit_text(report, JunitAdapter::Pytest, &ranked(&[nodeid])).unwrap();
    assert_eq!(result["status"], "COMPLETE");
    assert_eq!(
        result["tests"][nodeid],
        json!({
            "outcome": "ERROR", "duration_seconds": 0.5, "cases": 4
        })
    );
    assert_eq!(result["failed_nodeids"], json!([nodeid]));
}

#[test]
fn pytest_unmapped_and_missing_cases_fail_closed_in_order() {
    let one = "pkg/test_math.py::test_one";
    let absent = "pkg/test_math.py::test_absent";
    let report = r#"<testsuite>
      <testcase classname="foreign" name="other[a]"/>
      <testcase classname="pkg.test_math" name="test_one"/>
    </testsuite>"#;
    let result = parse_junit_text(report, JunitAdapter::Pytest, &ranked(&[one, absent])).unwrap();
    assert_eq!(result["status"], "INCOMPLETE");
    assert_eq!(result["missing_nodeids"], json!([absent]));
    assert_eq!(
        result["unmapped_cases"],
        json!([{"classname": "foreign", "name": "other[a]"}])
    );
    assert_eq!(result["tests"][one]["outcome"], "PASSED");
}

#[test]
fn pytest_skipped_case_remains_skipped_with_unmapped_neighbor() {
    let nodeid = "pkg/test_math.py::test_one";
    let report = "<testsuite><testcase classname=\"pkg.test_math\" name=\"test_one[a]\" time=\"bad\"><skipped/></testcase>\
        <testcase classname=\"other\" name=\"extra\"><failure/></testcase></testsuite>";
    let result = parse_junit_text(report, JunitAdapter::Pytest, &ranked(&[nodeid])).unwrap();
    assert_eq!(result["status"], "INCOMPLETE");
    assert_eq!(
        result["tests"][nodeid],
        json!({"outcome": "SKIPPED", "duration_seconds": 0.0, "cases": 1})
    );
    assert_eq!(
        result["unmapped_cases"],
        json!([{"classname": "other", "name": "extra"}])
    );
}

#[test]
fn pytest_status_precedence_and_default_attributes_match_junit_contract() {
    let nodeid = "pkg/test_math.py::test_one";
    for (first, second, expected) in [
        ("error", "failure", "ERROR"),
        ("failure", "error", "ERROR"),
        ("skipped", "failure", "FAILED"),
        ("skipped", "passed", "PASSED"),
        ("failure", "skipped", "FAILED"),
    ] {
        let marker = |kind: &str| {
            if kind == "passed" {
                String::new()
            } else {
                format!("<{kind}/>")
            }
        };
        let report = format!(
            "<testsuite><testcase classname=\"pkg.test_math\" name=\"test_one[a]\">{}</testcase>\
             <testcase classname=\"pkg.test_math\" name=\"test_one[b]\">{}</testcase></testsuite>",
            marker(first),
            marker(second)
        );
        let result = parse_junit_text(&report, JunitAdapter::Pytest, &ranked(&[nodeid])).unwrap();
        assert_eq!(
            result["tests"][nodeid]["outcome"], expected,
            "{first} then {second}"
        );
    }
    let report = "<testsuite><testcase classname=\"pkg.test_math\" name=\"test_one\"/><testcase/></testsuite>";
    let result = parse_junit_text(report, JunitAdapter::Pytest, &ranked(&[nodeid])).unwrap();
    assert_eq!(
        result["tests"][nodeid],
        json!({"outcome": "PASSED", "duration_seconds": 0.0, "cases": 1})
    );
    assert_eq!(
        result["unmapped_cases"],
        json!([{"classname": "", "name": ""}])
    );
}

#[test]
fn ctest_last_registration_wins_and_unranked_failure_is_visible() {
    let nodeid = "tests/reset.c::test_reset";
    let report = r#"<testsuite>
      <testcase name="reset.test_reset" time="1.25"><error/></testcase>
      <testcase name="reset.test_reset" status="disabled" time="bad"/>
      <testcase name="other.test_fail" status="fail"/>
    </testsuite>"#;
    let result = parse_junit_text(report, JunitAdapter::Ctest, &ranked(&[nodeid])).unwrap();
    assert_eq!(result["status"], "COMPLETE");
    assert_eq!(
        result["tests"][nodeid],
        json!({
            "outcome": "SKIPPED", "duration_seconds": 0.0, "cases": 1
        })
    );
    assert_eq!(result["failed_nodeids"], json!([]));
    assert_eq!(result["unranked_failures"], json!(["other.test_fail"]));
}

#[test]
fn ctest_error_failure_status_and_missing_names_preserve_contract() {
    let failed = "tests/reset.c::test_failed";
    let errored = "tests/reset.c::test_error";
    let absent = "tests/reset.c::test_absent";
    let report = r#"<testsuite>
      <testcase name="reset.test_failed" status="fail" time="1.1234567"/>
      <testcase name="reset.test_error" time="bad"><error/></testcase>
      <testcase><failure/></testcase>
    </testsuite>"#;
    let result = parse_junit_text(
        report,
        JunitAdapter::Ctest,
        &ranked(&[failed, errored, absent]),
    )
    .unwrap();
    assert_eq!(result["status"], "INCOMPLETE");
    assert_eq!(result["tests"][failed]["duration_seconds"], 1.123457);
    assert_eq!(result["tests"][errored]["outcome"], "ERROR");
    assert_eq!(result["failed_nodeids"], json!([failed, errored]));
    assert_eq!(result["missing_nodeids"], json!([absent]));
    assert_eq!(result["unranked_failures"], json!([""]));
}

#[test]
fn ctest_disabled_notrun_and_missing_attributes_keep_distinct_outcomes() {
    let disabled = "tests/reset.c::test_disabled";
    let notrun = "tests/reset.c::test_notrun";
    let defaulted = "tests/reset.c::test_default";
    let report = "<testsuite><testcase name=\"reset.test_disabled\" status=\"disabled\"/>\
        <testcase name=\"reset.test_notrun\" status=\"notrun\"/>\
        <testcase name=\"reset.test_default\"/><testcase><error/></testcase></testsuite>";
    let result = parse_junit_text(
        report,
        JunitAdapter::Ctest,
        &ranked(&[disabled, notrun, defaulted]),
    )
    .unwrap();
    assert_eq!(result["tests"][disabled]["outcome"], "SKIPPED");
    assert_eq!(result["tests"][notrun]["outcome"], "SKIPPED");
    assert_eq!(
        result["tests"][defaulted],
        json!({"outcome": "PASSED", "duration_seconds": 0.0, "cases": 1})
    );
    assert_eq!(result["unranked_failures"], json!([""]));
}

#[test]
fn xml_rejects_dtd_malformed_and_oversized_documents() {
    let ranked = ranked(&["pkg/test_math.py::test_one"]);
    let dtd = "<!DOCTYPE testsuite [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><testsuite/>";
    assert!(parse_junit_text(dtd, JunitAdapter::Pytest, &ranked)
        .unwrap_err()
        .contains("DTD"));
    assert!(parse_junit_text("<testsuite><testcase>", JunitAdapter::Pytest, &ranked).is_err());
    let oversized = " ".repeat(64 * 1024 * 1024 + 1);
    assert!(parse_junit_text(&oversized, JunitAdapter::Pytest, &ranked)
        .unwrap_err()
        .contains("exceeds"));
    assert!(
        parse_junit_text("<testsuite/><testsuite/>", JunitAdapter::Pytest, &ranked)
            .unwrap_err()
            .contains("more than one")
    );
    assert!(parse_junit_text(
        "<testsuite>&unknown;</testsuite>",
        JunitAdapter::Pytest,
        &ranked
    )
    .unwrap_err()
    .contains("undefined entity"));
}

#[test]
fn junit_file_decodes_declared_latin1_attribute_bytes() {
    let path = std::env::temp_dir().join(format!(
        "conductor-native-latin1-junit-{}-{:?}.xml",
        std::process::id(),
        std::thread::current().id()
    ));
    let report = b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><testsuite>\
        <testcase classname=\"pkg.test_math\" name=\"test_caf\xe9\"/></testsuite>";
    std::fs::write(&path, report).unwrap();
    let nodeid = "pkg/test_math.py::test_caf\u{e9}";
    let result = parse_junit_file(&path, JunitAdapter::Pytest, &ranked(&[nodeid]));
    std::fs::remove_file(&path).unwrap();
    let result = result.unwrap();
    assert_eq!(result["status"], "COMPLETE");
    assert_eq!(result["tests"][nodeid]["outcome"], "PASSED");
}

#[test]
fn libtest_preserves_failure_without_fabricating_time_and_rejects_collision() {
    let alpha = "src/a.rs::unit::test_alpha";
    let beta = "src/b.rs::unit::test_beta";
    let output = "test other::test_blunt ... FAILED\n\
                  test unit::test_alpha ... ok\n\
                  test unit::test_alpha ... FAILED\n\
                  test unit::test_beta ... ignored\n";
    let result = parse_cargo_libtest(output, &ranked(&[alpha, beta])).unwrap();
    assert_eq!(result["status"], "INCOMPLETE");
    assert!(result["tests"].as_object().unwrap().is_empty());
    let collided = parse_cargo_libtest(
        "test a::test_alpha ... ok\ntest b::test_alpha ... FAILED\n",
        &ranked(&[alpha]),
    )
    .unwrap();
    assert_eq!(collided["status"], "INCOMPLETE");
    assert_eq!(collided["ambiguous_nodeids"], json!([]));
    assert!(collided["tests"].get(alpha).is_none());
    let retained = parse_cargo_libtest(
        "test unit::test_alpha ... FAILED\ntest unit::test_alpha ... ok\n",
        &ranked(&[alpha]),
    )
    .unwrap();
    assert_eq!(retained["status"], "INCOMPLETE");
}

#[test]
fn libtest_json_lines_preserve_ranked_and_unranked_failure_provenance() {
    let ranked = ranked(&["src/lib.rs::mod::test_alpha"]);
    let output = r#"{"type":"suite","event":"started","test_count":2}
{"type":"test","event":"started","name":"mod::test_alpha"}
{"type":"test","event":"failed","name":"mod::test_alpha"}
{"type":"test","event":"failed","name":"other::test_blunt"}
{"type":"suite","event":"failed"}
"#;
    let result = parse_cargo_libtest(output, &ranked).unwrap();
    assert_eq!(result["status"], "INCOMPLETE");
    assert!(result["tests"].as_object().unwrap().is_empty());
}

#[test]
fn identity_checks_refuse_unseparable_ranked_tests() {
    assert!(attribution_supported(
        "pytest-junit",
        &ranked(&["pkg/test_math.py::test_one"])
    ));
    assert!(!attribution_supported("pytest-junit", &[]));
    assert_eq!(
        ctest_identity("tests/reset.cpp::test_run").unwrap(),
        "reset.test_run"
    );
    for invalid in [
        "tests/reset.c",
        "tests/reset.py::test_run",
        "tests/reset.c::x::test_run",
        "tests/reset.c::",
    ] {
        assert!(ctest_identity(invalid).is_err(), "{invalid}");
    }
    assert!(!attribution_supported(
        "ctest-junit",
        &ranked(&["a/reset.c::test_run", "b/reset.cpp::test_run"])
    ));
    assert!(!attribution_supported(
        "cargo-libtest",
        &ranked(&["a.rs::same", "b.rs::same"])
    ));
    assert!(!attribution_supported(
        "pytest-junit",
        &ranked(&["src/a.rs::test_run"])
    ));
    assert!(!attribution_supported("ctest-junit", &[]));
    assert!(!attribution_supported("cargo-libtest", &[]));
}

#[test]
fn mutant_evidence_prefers_explicit_failed_nodeids_and_preserves_order() {
    let ranked = ranked(&["t.py::a", "t.py::b"]);
    let contracts = vec![
        ("m2".to_owned(), "c".to_owned()),
        ("m1".to_owned(), "c".to_owned()),
    ];
    let reports = json!({
        "m2": {"status": "COMPLETE", "tests": {"t.py::a": {"outcome": "PASSED"}},
            "failed_nodeids": ["t.py::b"]},
        "m1": {"status": "COMPLETE", "tests": {"t.py::a": {"outcome": "ERROR"}}}
    });
    let evidence = collect_mutant_evidence(&ranked, &contracts, &reports, &json!({"m2": "KILLED"}));
    assert_eq!(
        evidence[0],
        json!({"mutation_id": "m2", "outcome": "KILLED",
        "report_state": "COMPLETE", "killers": ["t.py::b"]})
    );
    assert_eq!(
        evidence[1],
        json!({"mutation_id": "m1", "outcome": null,
        "report_state": "COMPLETE", "killers": ["t.py::a"]})
    );
}
