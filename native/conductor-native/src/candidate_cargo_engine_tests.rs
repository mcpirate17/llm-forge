use super::*;
use serde_json::{json, Value};

fn advisory(id: &str, extra: &str, patched: &str) -> Value {
    json!({"path":format!("crates/example/{id}.md"), "text":format!("```toml\n[advisory]\nid = \"{id}\"\npackage = \"example\"\ndate = \"2021-01-01\"\n{extra}\n[versions]\npatched = [\"{patched}\"]\n```\n\n# Example advisory 雪\n\nFixture description.\n")})
}
fn bundle() -> Value {
    json!({"schema_version":1,"captured_at":123,"advisory_commit":"a".repeat(40),"advisories":[
        advisory("RUSTSEC-2021-0001", "", ">= 2.0.0"),
        advisory("RUSTSEC-2021-0002", "informational = \"unmaintained\"", ">= 2.0.0"),
        advisory("RUSTSEC-2021-0003", "withdrawn = \"2022-01-01\"", ">= 2.0.0")],
        "registry":[{"name":"example","version":"1.0.0","checksum":"1".repeat(64),"yanked":true},
        {"name":"example","version":"2.0.0","checksum":"2".repeat(64),"yanked":false}]})
}
fn auditor(value: &Value) -> Result<Auditor, String> {
    Auditor::parse(&serde_json::to_vec(value).unwrap())
}
fn lock(version: &str, checksum: &str) -> String {
    format!("version = 3\n\n[[package]]\nname = \"example\"\nversion = \"{version}\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{checksum}\"\n")
}

#[test]
fn rustsec_versions_informational_withdrawn_yanked_and_scope() {
    let a = auditor(&bundle()).unwrap();
    assert_eq!(a.captured_at(), 123);
    assert_eq!(a.advisory_count(), 3);
    assert_eq!(a.registry_count(), 2);
    let report = a.audit(&lock("1.0.0", &"1".repeat(64))).unwrap();
    assert_eq!(report.crates_io_packages, 1);
    assert_eq!(report.yanked.len(), 1);
    assert_eq!(report.matches.len(), 2);
    assert_eq!(report.matches[0].advisory_id, "RUSTSEC-2021-0001");
    assert_eq!(
        report.matches[1].informational.as_deref(),
        Some("unmaintained")
    );
    let fixed = a.audit(&lock("2.0.0", &"2".repeat(64))).unwrap();
    assert!(fixed.matches.is_empty());
    assert!(fixed.yanked.is_empty());
    let workspace = a
        .audit("version = 3\n[[package]]\nname = \"local\"\nversion = \"1.0.0\"\n")
        .unwrap();
    assert_eq!(workspace.other_sources[0].source, "workspace");
}

#[test]
fn missing_wrong_checksum_and_unknown_source_fail_or_report_scope() {
    let a = auditor(&bundle()).unwrap();
    assert!(a.audit(&lock("1.0.0", &"2".repeat(64))).is_err());
    assert!(a.audit(&lock("3.0.0", &"3".repeat(64))).is_err());
    let external = "version = 3\n[[package]]\nname = \"example\"\nversion = \"1.0.0\"\nsource = \"git+https://example.com/repo#0123456789012345678901234567890123456789\"\n";
    let report = a.audit(external).unwrap();
    assert_eq!(report.crates_io_packages, 0);
    assert_eq!(report.other_sources.len(), 1);
    assert!(report.other_sources[0].source.starts_with("git+"));
}

#[test]
fn bundle_rejects_malformed_duplicates_and_path_mismatch() {
    let mut p = bundle();
    p["schema_version"] = json!(2);
    assert!(auditor(&p).is_err());
    p = bundle();
    p["advisory_commit"] = json!("0".repeat(40));
    assert!(auditor(&p).is_err());
    p = bundle();
    p["advisories"][0]["path"] = json!("crates/wrong/RUSTSEC-2021-0001.md");
    assert!(auditor(&p).is_err());
    p = bundle();
    p["advisories"][1] = p["advisories"][0].clone();
    assert!(auditor(&p).is_err());
    p = bundle();
    p["registry"][1] = p["registry"][0].clone();
    assert!(auditor(&p).is_err());
    p = bundle();
    p["extra"] = json!(true);
    assert!(auditor(&p).is_err());
    let text = serde_json::to_string(&bundle()).unwrap().replacen(
        "\"schema_version\":1",
        "\"schema_version\":1,\"schema_version\":1",
        1,
    );
    assert!(Auditor::parse(text.as_bytes()).is_err());
    p = bundle();
    p["advisories"][0]["text"] = json!("雪".repeat(10));
    assert!(auditor(&p).is_err());
}

#[test]
fn bounds_shared_work_and_unicode_title() {
    let a = auditor(&bundle()).unwrap();
    assert!(a.audit(&"x".repeat(MAX_LOCK + 1)).is_err());
    let mut remaining = 3;
    assert!(a
        .audit_with_budget(&lock("1.0.0", &"1".repeat(64)), &mut remaining)
        .is_err());
    assert_eq!(bounded_title(&"雪".repeat(2000)).len(), 4095);
    assert!(Auditor::parse(&vec![b' '; MAX_BUNDLE + 1]).is_err());
}

#[test]
fn shared_budget_crosses_locks_and_output_counts_json_escaping() {
    let a = auditor(&bundle()).unwrap();
    let mut budget = 1 + a.advisories["example"]
        .iter()
        .map(|item| 1 + item.ranges.len() * 34)
        .sum::<usize>();
    a.audit_with_budget(&lock("1.0.0", &"1".repeat(64)), &mut budget)
        .unwrap();
    assert_eq!(budget, 0);
    assert!(a
        .audit_with_budget(&lock("2.0.0", &"2".repeat(64)), &mut budget)
        .is_err());
    let value = "\u{1}\"雪";
    let size = crate::candidate_cargo_audit::serialized_size(&value, MAX_OUTPUT).unwrap();
    let mut output = OutputBudget(MAX_OUTPUT - size);
    assert!(output.add(&value).is_err());
    let mut output = OutputBudget(MAX_OUTPUT - size - 1);
    assert!(output.add(&value).is_ok());
}

#[test]
fn malformed_or_missing_registry_checksum_refuses() {
    let a = auditor(&bundle()).unwrap();
    let missing = "version = 3\n[[package]]\nname = \"example\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
    assert!(a.audit(missing).is_err());
    let invalid = "version = 3\n[[package]]\nname = \"example\"\nversion = \"1.0.0\"\nsource = \"unsupported+bad\"\n";
    assert!(a.audit(invalid).is_err());
    let mut value = bundle();
    value["registry"][0]["checksum"] = json!("0".repeat(64));
    assert!(auditor(&value).is_err());
}

#[test]
fn advisory_ids_require_exact_rustsec_shape() {
    assert!(advisory_id("RUSTSEC-2021-1234"));
    for value in [
        "CVE-2021-1234",
        "RUSTSEC-0000-0000",
        "RUSTSEC-2021-1234-extra",
        "RUSTSEC-2021-12xx",
    ] {
        assert!(!advisory_id(value));
    }
}

#[test]
fn cached_scope_and_ranges_match_rustsec_queries() {
    let mut data = bundle();
    for row in data["advisories"].as_array_mut().unwrap() {
        let text = row["text"]
            .as_str()
            .unwrap()
            .replace("[versions]\n", "[versions]\nunaffected = [\"< 0.5.0\"]\n");
        row["text"] = json!(text);
    }
    let a = auditor(&data).unwrap();
    for version in [
        "0.1.0",
        "0.5.0-alpha.1",
        "0.5.0",
        "1.0.0",
        "2.0.0-alpha.1",
        "2.0.0",
        "3.0.0",
    ] {
        let parsed: Lockfile = lock(version, &"1".repeat(64)).parse().unwrap();
        let package = &parsed.packages[0];
        let direct = Query::crate_scope().package(package);
        let informational = direct.clone().informational(true);
        let expected: Vec<_> = a.advisories["example"]
            .iter()
            .filter(|item| direct.matches(&item.advisory) || informational.matches(&item.advisory))
            .map(|item| item.advisory.id().to_string())
            .collect();
        let mut found = Vec::new();
        let mut output = OutputBudget(0);
        add_matches(package, &a.advisories["example"], &mut found, &mut output).unwrap();
        assert_eq!(
            found
                .iter()
                .map(|item| item.advisory_id.clone())
                .collect::<Vec<_>>(),
            expected,
            "{version}"
        );
    }
}

#[test]
fn preparse_rejects_quadratic_range_sets_and_version_overflow() {
    let mut data = bundle();
    let text = data["advisories"][0]["text"].as_str().unwrap().to_owned();
    let ranges = (0..65)
        .map(|n| format!("\"= 0.0.{n}\""))
        .collect::<Vec<_>>()
        .join(",");
    data["advisories"][0]["text"] =
        json!(text.replace("patched = [\">= 2.0.0\"]", &format!("patched = [{ranges}]")));
    assert!(auditor(&data).is_err());
    data = bundle();
    let text = data["advisories"][0]["text"]
        .as_str()
        .unwrap()
        .replace(">= 2.0.0", "= 1.2.18446744073709551615");
    data["advisories"][0]["text"] = json!(text);
    assert!(auditor(&data).is_err());
}

#[test]
fn preparse_range_count_and_shared_parse_budget_boundaries() {
    let make = |count| {
        let ranges = (0..count)
            .map(|n| format!("\"= 0.0.{n}\""))
            .collect::<Vec<_>>()
            .join(",");
        advisory("RUSTSEC-2021-0001", "", ">= 2.0.0")["text"]
            .as_str()
            .unwrap()
            .replace("patched = [\">= 2.0.0\"]", &format!("patched = [{ranges}]"))
    };
    let text = make(64);
    let mut remaining = 4096;
    assert!(preflight_ranges(&text, &mut remaining).is_ok());
    assert_eq!(remaining, 0);
    assert!(preflight_ranges(&text, &mut remaining).is_err());
    let mut fresh = MAX_PARSE_WORK;
    assert!(preflight_ranges(&make(65), &mut fresh).is_err());
    let mut data = bundle();
    data["advisories"][0]["text"] = json!(text);
    assert!(auditor(&data).is_ok());
}

#[test]
fn preparse_requirement_length_and_numeric_run_bounds() {
    let prefix = ">= 1.0.0-";
    let requirement = format!("{prefix}{}", "a".repeat(256 - prefix.len()));
    let text = |req: &str| {
        advisory("RUSTSEC-2021-0001", "", req)["text"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let mut remaining = MAX_PARSE_WORK;
    assert!(preflight_ranges(&text(&requirement), &mut remaining).is_ok());
    let mut data = bundle();
    data["advisories"][0]["text"] = json!(text(&requirement));
    assert!(auditor(&data).is_ok());
    assert!(preflight_ranges(&text(&format!("{requirement}a")), &mut remaining).is_err());
    assert!(
        preflight_ranges(&text(">= 1.0.0-alpha.18446744073709551616"), &mut remaining).is_err()
    );
    assert!(preflight_ranges(&text("= 1.2.18446744073709551615"), &mut remaining).is_err());
}

#[test]
fn rejects_excessively_long_advisory_source() {
    let mut data = bundle();
    let text = data["advisories"][0]["text"].as_str().unwrap().replace(
        "date = \"2021-01-01\"",
        &format!(
            "date = \"2021-01-01\"\nsource = \"registry+https://example.com/{}\"",
            "a".repeat(4097)
        ),
    );
    data["advisories"][0]["text"] = json!(text);
    assert!(auditor(&data).is_err());
}
