use super::*;
#[test]
fn string_byte_admission_matches_json_for_controls_and_unicode() {
    let values = (0..=127u8).map(|code| char::from(code).to_string()).chain(
        [
            "",
            "é/雪/🦀",
            "\"\\\0\u{1f}\t\r\n\u{2028}\u{2029}",
            "ASCII/path.rs",
        ]
        .map(str::to_owned),
    );
    for value in values {
        let payload = json!({"files":[{"path":format!("p{value}"),"text":value}]});
        let input = serde_json::to_vec(&payload).unwrap().len();
        assert!(scan(
            &payload,
            Scan::Secret,
            Limits {
                input,
                ..Limits::default()
            }
        )
        .is_ok());
        assert!(scan(
            &payload,
            Scan::Secret,
            Limits {
                input: input - 1,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .contains("input"));
    }
}
#[test]
fn secret_order_and_redaction_are_unchanged() {
    let generic = format!("token = '{}';", "x".repeat(24));
    let aws = format!("AKIA{}", "A".repeat(16));
    let key = "-----BEGIN PRIVATE KEY-----";
    let text = format!("{generic}\n{aws}\n{key}\n");
    let output = secret_scan(&json!({"files":[{"path":"file.txt","text":text}]})).unwrap();
    let rows = output["findings"].as_array().unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| r["rule_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["private-key", "aws-access-key", "generic-api-key"]
    );
    assert_eq!(
        rows.iter()
            .map(|r| r["line"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [3, 2, 1]
    );
    let encoded = output.to_string();
    assert!(!encoded.contains(&generic) && !encoded.contains(&aws) && !encoded.contains(key));
    for row in rows {
        assert_eq!(
            row["message"],
            "candidate contains secret-like credential material"
        );
    }
}
#[test]
fn lf_index_and_native_match_order_are_stable() {
    let source = "é\n\nstrcpy (x); gets( y );\npopen\n (x)";
    let output = native_source(&json!({"files":[{"path":"first.c","text":source},{"path":"second.c","text":"system (x)"}]})).unwrap();
    let rows = output["findings"].as_array().unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| r["line"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [3, 3, 4, 1]
    );
    assert_eq!(rows[2]["message"], "unsafe native API admitted: popen\n (");
    assert_eq!(rows[3]["path"], "second.c");
}
#[test]
fn indexed_lines_match_original_prefix_semantics_at_boundaries() {
    let text = "α\n\nβ\n";
    let mut index = Vec::new();
    newline_offsets(text, &mut index);
    for offset in 0..=text.len() {
        let old = text.as_bytes()[..offset]
            .iter()
            .filter(|b| **b == b'\n')
            .count()
            + 1;
        assert_eq!(line(&index, offset), old, "offset {offset}");
    }
    newline_offsets("x\n", &mut index);
    assert_eq!(index, [1]);
}
#[test]
fn malformed_input_and_admission_budgets_fail_explicitly() {
    assert!(secret_scan(&json!({"files":[{"path":"a","text":4}]})).is_err());
    assert!(native_source(&json!({"files":[{"path":false,"text":"a"}]})).is_err());
    let limit = Limits {
        files: 1,
        ..Limits::default()
    };
    assert!(scan(
        &json!({"files":[{"path":"a","text":""},{"path":"b","text":""}]}),
        Scan::Secret,
        limit
    )
    .unwrap_err()
    .contains("file count"));
    let limit = Limits {
        path: 3,
        ..Limits::default()
    };
    assert!(scan(
        &json!({"files":[{"path":"éé","text":""}]}),
        Scan::Secret,
        limit
    )
    .unwrap_err()
    .contains("path"));
    let limit = Limits {
        file: 2,
        ..Limits::default()
    };
    assert!(scan(
        &json!({"files":[{"path":"a","text":"abc"}]}),
        Scan::Native,
        limit
    )
    .unwrap_err()
    .contains("file"));
    let limit = Limits {
        input: 32,
        ..Limits::default()
    };
    assert!(scan(
        &json!({"files":[{"path":"a","text":"abc"}]}),
        Scan::Secret,
        limit
    )
    .unwrap_err()
    .contains("input"));
}
#[test]
fn dense_matches_and_output_bounds_refuse_without_partial_result() {
    let payload = json!({"files":[{"path":"dense.c","text":"gets()\n".repeat(4000)}]});
    let result = native_source(&payload).unwrap();
    let rows = result["findings"].as_array().unwrap();
    assert_eq!(rows.len(), 4000);
    assert_eq!(rows[3999]["line"], 4000);
    let limit = Limits {
        findings: 3,
        ..Limits::default()
    };
    assert!(scan(&payload, Scan::Native, limit)
        .unwrap_err()
        .contains("finding count"));
    let one = json!({"files":[{"path":"a.c","text":"gets()"}]});
    let size = serde_json::to_vec(&native_source(&one).unwrap())
        .unwrap()
        .len();
    let mut limit = Limits {
        output: size,
        ..Limits::default()
    };
    assert!(scan(&one, Scan::Native, limit).is_ok());
    limit.output = size - 1;
    assert!(scan(&one, Scan::Native, limit)
        .unwrap_err()
        .contains("output"));
}
#[test]
fn long_native_match_is_bounded_before_retention() {
    let payload = json!({"files":[{"path":"a.c","text":format!("gets{}(", " ".repeat(5000))}]});
    let limit = Limits {
        output: 1024,
        ..Limits::default()
    };
    assert!(scan(&payload, Scan::Native, limit)
        .unwrap_err()
        .contains("output"));
}

#[test]
fn input_budget_counts_exact_json_escaping_at_boundary() {
    for payload in [
        json!({"files":[]}),
        json!({"files":[{"path":"é\"\\\n.c","text":"🦀\0\t\"\\"}]}),
        json!({"files":[{"path":"a.c","text":"plain"},{"path":"b\n.c","text":"\0\"雪"}]}),
    ] {
        let exact = serde_json::to_vec(&payload).unwrap().len();
        let limit = Limits {
            input: exact,
            ..Limits::default()
        };
        assert!(scan(&payload, Scan::Secret, limit).is_ok());
        let limit = Limits {
            input: exact - 1,
            ..Limits::default()
        };
        assert!(scan(&payload, Scan::Secret, limit)
            .unwrap_err()
            .contains("input"));
    }
}
