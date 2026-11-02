use super::*;

const ID: &str = "python-ast";
fn policy(id: &str) -> Value {
    conductor_native::candidate_policy::fragment("check",&json!({"id":id,"value":{
        "kind":"command","profiles":["fast"],"severity":"low","max_output_chars":65536,
        "command":["forge","candidate","analyze","--check",id,"--base","{base}","--tree","{tree}","--execute","--","{files}"],
        "version_command":["forge","candidate","analyze","--check",id,"--tool-version"]}}),"2026-11-01").unwrap()
}
fn command(id: &str) -> Vec<String> {
    [
        "forge",
        "candidate",
        "analyze",
        "--check",
        id,
        "--base",
        &"c".repeat(40),
        "--tree",
        &"b".repeat(40),
        "--execute",
        "--",
        "src/a.py",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}
fn row(id: &str) -> Value {
    json!({"check_id":id,"rule_id":"dynamic-execution","severity":"critical","message":"unsafe dynamic execution",
        "path":"src/a.py","line":3,"column":0,"evidence":{"call":"eval"},
        "fingerprint":"untrusted-fingerprint","exception_id":"untrusted-exception","inherited":true})
}
fn packet(id: &str, rows: Vec<Value>) -> Value {
    json!({"schema_version":1,"check":id,"executed":true,"plan":{
        "base_tree_oid":"a".repeat(40),"candidate_tree_oid":"b".repeat(40),"selected_files":["src/a.py"],
        "included_files":["src/a.py"],"excluded_files":[],"policy_path":"policy.toml",
        "policy_sha256":"f".repeat(64),"manifest_paths":[],"provider_metadata":null,"source_bytes":128},
        "result":{"findings":rows,"files":["src/a.py"],"metrics":{"files_analyzed":1},
            "provider_scope":{"complete":false,"runtime_verified":false},"unresolved":[]}})
}
fn report(code: i32, text: &str) -> Report {
    Report {
        outcome: Outcome::Exited,
        exit_code: Some(code),
        signal: None,
        stdout: text.into(),
        stderr: String::new(),
        stdout_bytes: text.len() as u64,
        stderr_bytes: 0,
        duration_ms: 42,
        errors: vec![],
        cleanup_complete: true,
        resources: crate::cgroup::Report {
            backend: "fixture",
            path: "/unused/group".into(),
            cpu_quota_us: 100_000,
            cpu_period_us: 100_000,
            memory_max_bytes: 1024 * 1024,
            memory_swap_max_bytes: 0,
            processes_max: 8,
            verified: true,
        },
        cpu_seconds: Some(30),
        address_space_bytes: Some(1024 * 1024),
    }
}
fn candidate(files: &[String]) -> crate::candidate_source::Candidate {
    serde_json::from_value(json!({"kind":"index","tree_oid":"b".repeat(40),"base_tree_oid":"a".repeat(40),
        "base_commit_oid":"c".repeat(40),"commit_oid":null,"target_ref":null,"integration_base_oid":null,
        "integration_base_detail":"fixture","changes":files.iter().map(|path| json!({"status":"M","path":path,
            "old_path":null,"old_mode":"100644","new_mode":"100644","old_oid":"d".repeat(40),
            "new_oid":"e".repeat(40),"classes":["python"],"risk":"low"})).collect::<Vec<_>>()})).unwrap()
}
fn bound(
    normalized: &Value,
    files: &[String],
    command: &[String],
    version: Option<&VersionEvidence>,
    report: &Report,
) -> Result<CheckResult> {
    let candidate = candidate(files);
    let policy = crate::candidate_policy::Policy {
        path: "policy.toml".into(),
        digest: "f".repeat(64),
        document: json!({}),
    };
    map_result_bound(
        normalized,
        files,
        command,
        version,
        report,
        Some(AnalysisContext {
            candidate: &candidate,
            policy: &policy,
        }),
    )
}
fn mapped(report: &Report) -> CheckResult {
    bound(
        &policy(ID),
        &["src/a.py".into()],
        &command(ID),
        Some(&VersionEvidence::Available(format!(
            "forge-native-analysis 1 fixture {ID}"
        ))),
        report,
    )
    .unwrap()
}
fn rejected(report: &Report) {
    let mapped = mapped(report);
    assert_eq!(mapped.status, CheckStatus::Failed);
    assert_eq!(mapped.findings.len(), 1);
    assert_eq!(mapped.findings[0].rule_id, "analyzer-crash");
    assert_eq!(mapped.findings[0].severity, Severity::Critical);
    assert!(!mapped.findings[0].fingerprint.is_empty());
    assert!(!mapped.metrics.contains_key("analysis"));
}

#[test]
fn protocol_selection_requires_known_command_prefix_but_malformed_version_pairing_is_critical() {
    for id in IDS {
        assert!(selected(&policy(id)));
    }
    for (index, value) in [(0, "worker"), (2, "scan"), (4, "other")] {
        let mut value_policy = policy(ID);
        value_policy["command"][index] = json!(value);
        assert!(!selected(&value_policy));
    }
    for (index, value) in [(0, "worker"), (4, "other"), (5, "--version")] {
        let mut value_policy = policy(ID);
        value_policy["version_command"][index] = json!(value);
        assert!(selected(&value_policy));
        let output = bound(
            &value_policy,
            &["src/a.py".into()],
            &command(ID),
            Some(&VersionEvidence::Available(format!(
                "forge-native-analysis 1 fixture {ID}"
            ))),
            &report(2, ""),
        )
        .unwrap();
        assert_eq!(output.findings[0].rule_id, "analyzer-crash");
        assert_eq!(output.findings[0].severity, Severity::Critical);
    }
    let mut value_policy = policy(ID);
    value_policy["version_command"]
        .as_array_mut()
        .unwrap()
        .push(json!("unexpected"));
    assert!(selected(&value_policy));
    assert!(!selected(&policy("unrelated-command")));
}

#[test]
fn structured_critical_findings_keep_native_identity_location_and_fresh_fingerprints_under_low_policy(
) {
    let encoded = packet(ID, vec![row(ID)]).to_string();
    let first = mapped(&report(1, &encoded));
    let second = mapped(&report(1, &encoded));
    let finding = &first.findings[0];
    assert_eq!(first.status, CheckStatus::Failed);
    assert_eq!(finding.check_id, ID);
    assert_eq!(finding.rule_id, "dynamic-execution");
    assert_eq!(finding.severity, Severity::Critical);
    assert_eq!(finding.path.as_deref(), Some("src/a.py"));
    assert_eq!(finding.line, Some(3));
    assert_eq!(finding.column, Some(0));
    assert_eq!(finding.evidence["call"], "eval");
    assert_ne!(finding.fingerprint, "untrusted-fingerprint");
    assert!(!finding.fingerprint.is_empty());
    assert_eq!(finding.fingerprint, second.findings[0].fingerprint);
    assert_eq!(finding.exception_id, None);
    assert!(!finding.inherited);
}

#[test]
fn empty_clean_result_keeps_plan_metrics_and_scope_without_claiming_attestation() {
    let packet = packet(ID, vec![]);
    let output = mapped(&report(0, &packet.to_string()));
    assert_eq!(output.status, CheckStatus::Passed);
    assert!(output.findings.is_empty());
    assert_eq!(output.metrics["analysis"]["plan"], packet["plan"]);
    assert_eq!(
        output.metrics["analysis"]["result"]["metrics"],
        json!({"files_analyzed":1})
    );
    assert_eq!(
        output.metrics["analysis"]["result"]["files"],
        json!(["src/a.py"])
    );
    assert_eq!(
        output.metrics["analysis"]["result"]["provider_scope"]["runtime_verified"],
        false
    );
    assert!(output.metrics["analysis"]["result"]
        .get("findings")
        .is_none());
    assert_eq!(output.metrics["cleanup_complete"], true);
    // The argv base is a commit-shaped OID distinct from the emitted base tree.
    assert_ne!(
        command(ID)[6],
        packet["plan"]["base_tree_oid"].as_str().unwrap()
    );
}

#[test]
fn corrupted_json_duplicate_keys_and_trailing_documents_are_critical() {
    let text = packet(ID, vec![]).to_string();
    for text in [
        "not-json".into(),
        "[]".into(),
        format!("{text}\n{{}}"),
        format!("{{\"check\":\"wrong\",{}", &text[1..]),
        text.replace(
            "\"files_analyzed\":1",
            "\"files_analyzed\":1,\"files_analyzed\":2",
        ),
    ] {
        rejected(&report(0, &text));
    }
}

#[test]
fn envelope_schema_execution_check_selected_files_and_tree_must_match() {
    for (field, value) in [
        ("schema_version", json!(2)),
        ("check", json!("structure-audit")),
        ("executed", json!(false)),
        ("plan", Value::Null),
        ("result", Value::Null),
    ] {
        let mut value_packet = packet(ID, vec![]);
        value_packet[field] = value;
        rejected(&report(0, &value_packet.to_string()));
    }
    for (field, value) in [
        ("selected_files", json!(["elsewhere.py"])),
        ("selected_files", json!(["src/a.py", "src/a.py"])),
        ("candidate_tree_oid", json!("d".repeat(40))),
        ("base_tree_oid", json!("HEAD")),
    ] {
        let mut value_packet = packet(ID, vec![]);
        value_packet["plan"][field] = value;
        rejected(&report(0, &value_packet.to_string()));
    }
    let mut extra = packet(ID, vec![]);
    extra["unexpected"] = json!(true);
    rejected(&report(0, &extra.to_string()));
}

#[test]
fn result_inventory_metrics_and_exit_findings_contract_fail_closed() {
    for (field, value) in [
        ("files", json!(["outside.py"])),
        ("files", json!(["src/a.py", "src/a.py"])),
        ("metrics", Value::Null),
        ("findings", json!({})),
    ] {
        let mut packet = packet(ID, vec![]);
        packet["result"][field] = value;
        rejected(&report(0, &packet.to_string()));
    }
    rejected(&report(0, &packet(ID, vec![row(ID)]).to_string()));
    rejected(&report(1, &packet(ID, vec![]).to_string()));
    rejected(&report(2, ""));
}

#[test]
fn findings_outside_result_files_bad_locations_and_unrecognized_fields_are_critical() {
    for (field, value) in [
        ("check_id", json!("other")),
        ("rule_id", json!("")),
        ("path", json!("outside.py")),
        ("line", json!(0)),
        ("column", json!(-1)),
        ("severity", json!("approved")),
        ("unrecognized_field", json!(true)),
        ("evidence", json!([])),
    ] {
        let mut finding = row(ID);
        finding[field] = value;
        rejected(&report(1, &packet(ID, vec![finding]).to_string()));
    }
    let mut value_packet = packet(ID, vec![row(ID)]);
    value_packet["result"]["files"] = json!([]);
    rejected(&report(1, &value_packet.to_string()));
}

#[test]
fn matching_version_and_concrete_command_binding_are_required() {
    let observed = report(0, &packet(ID, vec![]).to_string());
    for version in [
        None,
        Some(VersionEvidence::Unavailable("no version".into())),
        Some(VersionEvidence::Available(
            "forge-native-analysis 2 fixture python-ast".into(),
        )),
        Some(VersionEvidence::Available(
            "forge-native-analysis 1 fixture structure-audit".into(),
        )),
    ] {
        let output = bound(
            &policy(ID),
            &["src/a.py".into()],
            &command(ID),
            version.as_ref(),
            &observed,
        )
        .unwrap();
        assert_eq!(output.findings[0].rule_id, "analyzer-crash");
        assert_eq!(output.findings[0].severity, Severity::Critical);
    }
    for argv in [
        {
            let mut v = command(ID);
            v[0] = "worker".into();
            v
        },
        {
            let mut v = command(ID);
            v[4] = "config-parse".into();
            v
        },
        {
            let mut v = command(ID);
            v[6] = "HEAD".into();
            v
        },
        {
            let mut v = command(ID);
            v[9] = "--tool-version".into();
            v
        },
        {
            let mut v = command(ID);
            v[11] = "other.py".into();
            v
        },
        {
            let mut v = command(ID);
            v.insert(9, "--execute".into());
            v
        },
    ] {
        let output = bound(
            &policy(ID),
            &["src/a.py".into()],
            &argv,
            Some(&VersionEvidence::Available(format!(
                "forge-native-analysis 1 fixture {ID}"
            ))),
            &observed,
        )
        .unwrap();
        assert_eq!(output.findings[0].rule_id, "analyzer-crash");
    }
}

#[test]
fn incomplete_stdout_byte_counts_diagnostics_and_replacement_decoding_are_critical() {
    let text = packet(ID, vec![]).to_string();
    let mut truncated = report(0, &text);
    truncated.stdout_bytes += 10;
    rejected(&truncated);
    let mut diagnostic = report(0, &text);
    diagnostic.stderr = "warning".into();
    diagnostic.stderr_bytes = 7;
    rejected(&diagnostic);
    let mut decoded = packet(ID, vec![row(ID)]);
    decoded["result"]["findings"][0]["message"] = json!("replacement \u{fffd}");
    rejected(&report(1, &decoded.to_string()));
}

#[test]
fn supervisor_failures_never_become_ordinary_low_severity_findings() {
    let text = packet(ID, vec![]).to_string();
    for outcome in [Outcome::TimedOut, Outcome::Cancelled, Outcome::Failed] {
        let mut failed = report(0, &text);
        failed.outcome = outcome;
        rejected(&failed);
    }
    for field in 0..4 {
        let mut failed = report(0, &text);
        match field {
            0 => failed.cleanup_complete = false,
            1 => failed.resources.verified = false,
            2 => failed.errors.push("supervisor error".into()),
            _ => failed.signal = Some(9),
        }
        rejected(&failed);
    }
}

#[test]
fn unrelated_command_protocol_keeps_existing_generic_mapping() {
    let mut unrelated = policy(ID);
    unrelated["command"][0] = json!("operator-tool");
    unrelated["version_command"] = json!(["operator-tool", "--version"]);
    let output = bound(
        &unrelated,
        &["src/a.py".into()],
        &command(ID),
        Some(&VersionEvidence::Available("some other tool".into())),
        &report(1, "ordinary diagnostic"),
    )
    .unwrap();
    assert_eq!(output.findings[0].rule_id, "analyzer-finding");
    assert_eq!(output.findings[0].severity, Severity::Low);
    assert!(!output.metrics.contains_key("analysis"));
}

#[test]
fn native_protocol_without_parent_context_is_critical() {
    let output = map_result(
        &policy(ID),
        &["src/a.py".into()],
        &command(ID),
        Some(&VersionEvidence::Available(format!(
            "forge-native-analysis 1 fixture {ID}"
        ))),
        &report(0, &packet(ID, vec![]).to_string()),
    )
    .unwrap();
    assert_eq!(output.findings[0].severity, Severity::Critical);
    assert_eq!(output.findings[0].rule_id, "analyzer-crash");
}

#[test]
fn partition_omissions_overlap_duplicates_and_forged_exclusion_reasons_are_critical() {
    for (included, excluded) in [
        (json!([]), json!([])),
        (
            json!(["src/a.py"]),
            json!([{"path":"src/a.py","reason":"deleted"}]),
        ),
        (json!(["src/a.py", "src/a.py"]), json!([])),
        (json!([]), json!([{"path":"src/a.py","reason":"approved"}])),
        (
            json!([]),
            json!([{"path":"src/a.py","reason":"test-class"}]),
        ),
        // Even a recognized reason cannot exclude an eligible parent-classified file.
        (
            json!([]),
            json!([{"path":"src/a.py","reason":"class-not-selected"}]),
        ),
    ] {
        let mut value = packet(ID, vec![]);
        value["plan"]["included_files"] = included;
        value["plan"]["excluded_files"] = excluded;
        value["result"]["files"] = json!([]);
        rejected(&report(0, &value.to_string()));
    }
    let files = vec!["src/a.py".into(), "src/b.py".into()];
    let mut value = packet(ID, vec![]);
    value["plan"]["selected_files"] = json!(files);
    value["plan"]["included_files"] = json!(files);
    let mut argv = command(ID);
    argv.push("src/b.py".into());
    let output = bound(
        &policy(ID),
        &files,
        &argv,
        Some(&VersionEvidence::Available(format!(
            "forge-native-analysis 1 fixture {ID}"
        ))),
        &report(0, &value.to_string()),
    )
    .unwrap();
    assert_eq!(output.findings[0].rule_id, "analyzer-crash");
}

#[test]
fn parent_policy_and_resolved_base_tree_are_bound_independently_of_child_claims() {
    for (field, value) in [
        ("base_tree_oid", json!("c".repeat(40))),
        ("policy_path", json!("other-policy.toml")),
        ("policy_sha256", json!("e".repeat(64))),
    ] {
        let mut packet = packet(ID, vec![]);
        packet["plan"][field] = value;
        rejected(&report(0, &packet.to_string()));
    }
    let mut argv = command(ID);
    argv[6] = "d".repeat(40);
    let output = bound(
        &policy(ID),
        &["src/a.py".into()],
        &argv,
        Some(&VersionEvidence::Available(format!(
            "forge-native-analysis 1 fixture {ID}"
        ))),
        &report(0, &packet(ID, vec![]).to_string()),
    )
    .unwrap();
    assert_eq!(output.findings[0].rule_id, "analyzer-crash");
}

fn import_packet() -> Value {
    let mut packet = packet("import-declaration", vec![]);
    packet["plan"]["provider_metadata"] =
        json!({"path":"/operator/providers.json","sha256":"1".repeat(64)});
    packet["result"]["metrics"] = json!({"unresolved_imports":1});
    packet["result"]["unresolved"] = json!([{"path":"src/a.py","module":"optional"}]);
    packet["result"]["provider_scope"] = json!({"stdlib_count":2,"module_count":1,"complete":false,
        "runtime_verified":false,"provenance":{"schema_version":1,"implementation":"cpython","version":"3.12",
            "platform":"linux","sha256":"1".repeat(64)}});
    packet
}
#[test]
fn import_scope_and_unresolved_inventory_cannot_disappear_or_claim_verification() {
    let id = "import-declaration";
    let version = VersionEvidence::Available(format!("forge-native-analysis 1 fixture {id}"));
    let evaluate = |packet: &Value| {
        bound(
            &policy(id),
            &["src/a.py".into()],
            &command(id),
            Some(&version),
            &report(0, &packet.to_string()),
        )
        .unwrap()
    };
    assert_eq!(evaluate(&import_packet()).status, CheckStatus::Passed);
    for mutation in 0..7 {
        let mut packet = import_packet();
        match mutation {
            0 => packet["result"]["provider_scope"] = Value::Null,
            1 => packet["result"]["provider_scope"]["complete"] = json!(true),
            2 => packet["result"]["provider_scope"]["runtime_verified"] = json!(true),
            3 => packet["result"]["unresolved"] = Value::Null,
            4 => packet["result"]["metrics"]["unresolved_imports"] = json!(0),
            5 => packet["result"]["unresolved"][0]["path"] = json!("outside.py"),
            _ => packet["result"]["provider_scope"]["provenance"]["sha256"] = json!("2".repeat(64)),
        }
        let output = evaluate(&packet);
        assert_eq!(output.findings[0].rule_id, "analyzer-crash");
        assert_eq!(output.findings[0].severity, Severity::Critical);
    }
}

#[test]
fn parent_classified_empty_import_inventory_needs_no_provider_observation() {
    let id = "import-declaration";
    let files = vec!["src/a.py".into()];
    let mut candidate = candidate(&files);
    candidate.changes[0].classes.push("test".into());
    let policy_parent = crate::candidate_policy::Policy {
        path: "policy.toml".into(),
        digest: "f".repeat(64),
        document: json!({}),
    };
    let mut packet = packet(id, vec![]);
    packet["plan"]["included_files"] = json!([]);
    packet["plan"]["excluded_files"] = json!([{"path":"src/a.py","reason":"test-class"}]);
    packet["result"] = json!({"files":[],"findings":[],"metrics":{"files_analyzed":0}});
    let output = map_result_bound(
        &policy(id),
        &files,
        &command(id),
        Some(&VersionEvidence::Available(format!(
            "forge-native-analysis 1 fixture {id}"
        ))),
        &report(0, &packet.to_string()),
        Some(AnalysisContext {
            candidate: &candidate,
            policy: &policy_parent,
        }),
    )
    .unwrap();
    assert_eq!(output.status, CheckStatus::Passed);
    assert_eq!(
        output.metrics["analysis"]["result"]["metrics"]["files_analyzed"],
        0
    );
}
