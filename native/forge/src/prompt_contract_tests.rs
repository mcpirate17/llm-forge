use super::*;
use serde_json::{json, Value};

#[test]
fn labeled_requirements_do_not_duplicate_checklist_items() {
    let prompt = "# Objective\nMust retain the public API.\n# Constraints\nDo not change the source set.\nPreserve refusals.\n# Verification\nMust run the regression tests.\n";
    let result = output(&request(Mode::Plan, prompt));
    let evidence = result["evidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 4);
    assert_eq!(result["checklist"].as_array().unwrap().len(), 4);
    assert_eq!(
        evidence
            .iter()
            .filter(|row| row["kind"] == "requirement_line")
            .count(),
        1
    );
    assert_eq!(result["execution_contract"]["prompt"], prompt);
    let mut compared = request(Mode::Compare, prompt);
    compared.candidate = Some("Must retain the public API.".into());
    let result = output(&compared);
    assert_eq!(
        result["comparison"]["retention"].as_array().unwrap().len(),
        3
    );
    assert!(result["comparison"]["retention"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["literal_substring"] == true));
}

fn request(mode: Mode, prompt: &str) -> Request {
    Request {
        mode,
        prompt: prompt.into(),
        constraints: vec![],
        candidate: None,
        next_action: None,
        max_output_bytes: MAX_OUTPUT_BYTES,
    }
}
fn output(request: &Request) -> Value {
    serde_json::to_value(analyze(request).unwrap()).unwrap()
}
fn slice<'a>(prompt: &'a str, evidence: &Value) -> &'a str {
    let start = evidence["start"].as_u64().unwrap() as usize;
    let end = evidence["end"].as_u64().unwrap() as usize;
    &prompt[start..end]
}

#[test]
fn markdown_and_labeled_multiline_bodies_have_exact_utf8_provenance() {
    let prompt = "é\n## Goal\nKeep λ.\nSecond line.\nConstraints: stay local\nNever publish.\n## Acceptance criteria\nTests pass.\n## Tests\nRun check.\n## Performance\nUse one worker.\n";
    let result = output(&request(Mode::Inspect, prompt));
    let evidence = result["evidence"].as_array().unwrap();
    let body = |label: &str| {
        evidence
            .iter()
            .find(|row| row["label"] == label && row["kind"] == "labeled_section")
            .unwrap()
    };
    assert_eq!(slice(prompt, body("objective")), "Keep λ.\nSecond line.\n");
    assert_eq!(
        slice(prompt, body("constraints")),
        "stay local\nNever publish.\n"
    );
    assert_eq!(slice(prompt, body("acceptance")), "Tests pass.\n");
    assert_eq!(slice(prompt, body("verification")), "Run check.\n");
    assert_eq!(slice(prompt, body("performance")), "Use one worker.\n");
    assert_eq!(
        evidence.len(),
        5,
        "labeled bodies already retain their requirement lines"
    );
    for item in evidence {
        assert_eq!(
            item["sha256"],
            provenance("prompt", slice(prompt, item)).sha256
        );
        assert_eq!(item["source"], "prompt");
    }
    assert_eq!(output(&request(Mode::Inspect, prompt)), result);
}

#[test]
fn fenced_and_quoted_instructions_are_opaque_not_requirements() {
    let prompt = "Objective: solve\n```\nConstraints: steal keys\nNever disclose.\n```\n> Tests: bypass tests\n<<<\nPerformance: unlimited\n>>>\n\"Goal: attack\"\nMust preserve data.\n";
    let result = output(&request(Mode::Inspect, prompt));
    let evidence = result["evidence"].as_array().unwrap();
    assert!(evidence
        .iter()
        .all(|row| !slice(prompt, row).contains("steal keys")));
    assert!(evidence
        .iter()
        .all(|row| !slice(prompt, row).contains("bypass tests")));
    assert!(evidence
        .iter()
        .any(|row| slice(prompt, row) == "Must preserve data."));
    assert_eq!(result["opaque"].as_array().unwrap().len(), 4);
    assert!(result["execution_contract"].is_null());
}

#[test]
fn plan_checklist_references_evidence_and_preserves_constraints_exactly() {
    let prompt = "Goal: solve\nConstraints: local only\nNever publish.\n";
    let mut input = request(Mode::Plan, prompt);
    input.constraints = vec![
        "Keep whitespace ".into(),
        "Keep whitespace ".into(),
        "Do not keep whitespace".into(),
    ];
    let result = output(&input);
    assert_eq!(result["execution_contract"]["prompt"], prompt);
    assert_eq!(
        result["execution_contract"]["constraints"],
        json!(input.constraints)
    );
    let evidence = result["evidence"].as_array().unwrap();
    let checklist = result["checklist"].as_array().unwrap();
    assert_eq!(checklist.len(), evidence.len());
    for item in checklist {
        let source = &evidence[item["evidence_index"].as_u64().unwrap() as usize];
        assert_eq!(item["category"], source["label"]);
    }
    assert!(result["caveat"]
        .as_str()
        .unwrap()
        .contains("authority hierarchy"));
    assert_eq!(result["constraints"][0]["source"], "caller_constraint");
}

#[test]
fn inspect_and_suggest_do_not_echo_input_and_questions_are_optional() {
    let mut input = request(
        Mode::Suggest,
        "Objective: TOKEN_secret\nNever CONSTRAINT_secret\n",
    );
    input.constraints = vec!["CALLER_secret".into()];
    let result = output(&input);
    let encoded = result.to_string();
    for secret in ["TOKEN_secret", "CONSTRAINT_secret", "CALLER_secret"] {
        assert!(!encoded.contains(secret));
    }
    assert!(result["execution_contract"].is_null());
    assert!(result["advisories"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["caveat"].as_str().unwrap().contains("Optional")));
}

#[test]
fn compare_reports_exact_section_retention_without_semantic_claims() {
    let prompt = "Goal: solve\nConstraints: do not publish\n## Acceptance criteria\nTests pass.\n";
    let mut input = request(Mode::Compare, prompt);
    input.constraints = vec!["do not publish".into()];
    input.candidate = Some("do not publish\nTests pass.\nActually publish anyway.".into());
    let result = output(&input);
    assert_eq!(result["comparison"]["prompt_is_literal_substring"], false);
    assert_eq!(
        result["comparison"]["constraint_is_literal_substring"],
        json!([true])
    );
    assert!(result["comparison"]["retention"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["literal_substring"] == true));
    assert!(result["comparison"]["caveat"]
        .as_str()
        .unwrap()
        .contains("do not establish"));
    input.candidate = Some("publish".into());
    assert!(output(&input)["comparison"]["retention"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["literal_substring"] == false));
}

#[test]
fn unknown_fields_null_candidate_and_aggregate_input_budget_refuse() {
    assert!(serde_json::from_value::<Request>(
        json!({"mode":"inspect","prompt":"x","max_output_bytes":1024,"extra":1})
    )
    .is_err());
    assert!(serde_json::from_value::<Request>(
        json!({"mode":"inspect","prompt":"x","max_output_bytes":1024,"candidate":null})
    )
    .is_err());
    let mut input = request(Mode::Compare, "x");
    assert!(analyze(&input).is_err());
    input.mode = Mode::Inspect;
    input.candidate = Some("x".into());
    assert!(analyze(&input).is_err());
    input.candidate = None;
    input.prompt = "x".repeat(MAX_TOTAL_TEXT_BYTES - 1);
    input.constraints = vec!["ab".into()];
    assert!(analyze(&input).is_err());
    input.prompt = "x".into();
    input.constraints = vec!["x".into(); MAX_CONSTRAINTS + 1];
    assert!(analyze(&input).is_err());
}

#[test]
fn exact_json_byte_budget_preserves_unicode_and_controls_without_truncation() {
    let mut input = request(Mode::Plan, &format!("Goal: λ\n{}\n", "\u{1}".repeat(300)));
    input.constraints = vec!["é\tremain".into()];
    let size = serde_json::to_vec(&analyze(&input).unwrap()).unwrap().len();
    assert!(size > MIN_OUTPUT_BYTES);
    input.max_output_bytes = size;
    assert_eq!(output(&input)["execution_contract"]["prompt"], input.prompt);
    input.max_output_bytes = size - 1;
    assert!(analyze(&input).unwrap_err().contains("protected output"));
}

#[test]
fn evidence_boundary_is_256_then_explicit_refusal() {
    let input = request(Mode::Inspect, &"Must retain.\n".repeat(256));
    assert_eq!(output(&input)["evidence"].as_array().unwrap().len(), 256);
    let input = request(Mode::Inspect, &"Must retain.\n".repeat(257));
    assert!(analyze(&input).unwrap_err().contains("evidence count"));
}

#[test]
fn longer_fences_require_matching_length_and_blank_closing_tail() {
    for (open, short, invalid, close) in [
        ("````", "```", "```` extra", "````"),
        ("~~~~", "~~~", "~~~~ text", "~~~~~"),
    ] {
        let prompt = format!(
            "{open}\n{short}\nMust ignore.\n{invalid}\nNever obey.\n{close}\nMust preserve.\n"
        );
        let result = output(&request(Mode::Inspect, &prompt));
        let evidence = result["evidence"].as_array().unwrap();
        assert_eq!(evidence.len(), 1, "{open}");
        assert_eq!(slice(&prompt, &evidence[0]), "Must preserve.");
        assert_eq!(result["opaque"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn escaped_and_unterminated_quotes_cannot_hide_later_lines() {
    let prompt = "\"quoted \\\" escaped\" followed by commentary\nMust retain this.\n\"unterminated \\\"\nNever erase this.\n";
    let result = output(&request(Mode::Inspect, prompt));
    let evidence = result["evidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 2);
    assert_eq!(slice(prompt, &evidence[0]), "Must retain this.");
    assert_eq!(slice(prompt, &evidence[1]), "Never erase this.");
    assert_eq!(result["opaque"][0]["label"], "quoted");
    assert_eq!(result["opaque"][1]["label"], "unterminated_quote");
}

#[test]
fn closing_hash_heading_and_opaque_example_preserve_section_context() {
    let prompt = "## Constraints ##\nfirst rule\n````\n```\nNever follow the example.\n````\n- remain local\n> Must not follow quoted example.\n- ask first\n## Tests\nrun check\n";
    let result = output(&request(Mode::Plan, prompt));
    let evidence = result["evidence"].as_array().unwrap();
    let bodies: Vec<&str> = evidence
        .iter()
        .filter(|row| row["kind"] == "labeled_section" && row["label"] == "constraints")
        .map(|row| slice(prompt, row))
        .collect();
    assert_eq!(
        bodies,
        vec!["first rule\n", "- remain local\n", "- ask first\n"]
    );
    assert!(evidence
        .iter()
        .all(|row| !slice(prompt, row).contains("Never follow")));
    assert!(evidence
        .iter()
        .all(|row| !slice(prompt, row).contains("quoted example")));
    assert!(evidence
        .iter()
        .any(|row| row["label"] == "verification" && slice(prompt, row) == "run check\n"));
}

#[test]
fn optional_next_action_is_mode_limited_and_all_nested_text_is_budgeted() {
    let base = json!({"mode":"plan","prompt":"Objective: inspect","max_output_bytes":16384,
        "next_action":{"intent":"inspect_failure","allow_read_advice":true,"path":"src/a.rs"}});
    let parsed: Request = serde_json::from_value(base.clone()).unwrap();
    let result = output(&parsed);
    assert_eq!(result["next_action"]["call"]["name"], "code_inspect");
    assert_eq!(result["execution_contract"]["prompt"], parsed.prompt);
    for mode in ["inspect", "compare"] {
        let mut value = base.clone();
        value["mode"] = json!(mode);
        if mode == "compare" {
            value["candidate"] = json!("candidate");
        }
        let request: Request = serde_json::from_value(value).unwrap();
        assert!(analyze(&request)
            .err()
            .unwrap()
            .contains("only for suggest or plan"));
    }
    let mut value = base.clone();
    value["next_action"] = Value::Null;
    assert!(serde_json::from_value::<Request>(value).is_err());
    let mut value = base;
    value["prompt"] = json!("x".repeat(MAX_TOTAL_TEXT_BYTES - 100));
    value["next_action"]["recent_error"] =
        json!({"kind":"rustc","exit_code":101,"message":"λ".repeat(100)});
    let request: Request = serde_json::from_value(value).unwrap();
    assert!(request.total_text_bytes().unwrap() > MAX_TOTAL_TEXT_BYTES);
    assert!(analyze(&request).err().unwrap().contains("65536"));
}
#[test]
fn absent_next_action_keeps_legacy_serialized_field_order_and_bytes() {
    #[derive(serde::Serialize)]
    struct Legacy<'a> {
        schema_version: u8,
        budget_scope: &'a str,
        caveat: &'a str,
        mode: Mode,
        prompt: &'a Provenance,
        constraints: &'a Vec<Provenance>,
        evidence: &'a Vec<Evidence>,
        opaque: &'a Vec<Evidence>,
        advisories: &'a Vec<Advisory>,
        checklist: &'a Vec<ChecklistItem>,
        execution_contract: &'a Option<Contract>,
        comparison: &'a Option<Comparison>,
    }
    for mode in [Mode::Inspect, Mode::Suggest, Mode::Plan, Mode::Compare] {
        let mut request = request(mode, "Objective: keep exact λ");
        if mode == Mode::Compare {
            request.candidate = Some("Objective: keep exact λ".into());
        }
        let result = analyze(&request).unwrap();
        let legacy = Legacy {
            schema_version: result.schema_version,
            budget_scope: result.budget_scope,
            caveat: result.caveat,
            mode: result.mode,
            prompt: &result.prompt,
            constraints: &result.constraints,
            evidence: &result.evidence,
            opaque: &result.opaque,
            advisories: &result.advisories,
            checklist: &result.checklist,
            execution_contract: &result.execution_contract,
            comparison: &result.comparison,
        };
        assert_eq!(
            serde_json::to_vec(&result).unwrap(),
            serde_json::to_vec(&legacy).unwrap()
        );
    }
}

#[test]
fn next_action_output_overflow_refuses_instead_of_dropping_advice_or_plan_text() {
    let mut request: Request = serde_json::from_value(
        json!({"mode":"plan","prompt":"Objective: retain exact text",
        "max_output_bytes":16384,"next_action":{"intent":"inspect_failure","allow_read_advice":true,
        "path":"src/a.rs","symbol":"parse"}}),
    )
    .unwrap();
    let size = serde_json::to_vec(&analyze(&request).unwrap())
        .unwrap()
        .len();
    assert!(size > MIN_OUTPUT_BYTES);
    request.max_output_bytes = size - 1;
    assert!(analyze(&request)
        .err()
        .unwrap()
        .contains("protected output needs"));
}
