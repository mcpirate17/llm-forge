use super::*;
fn input(path: &str, text: &str, notebook: bool) -> ConfigInput {
    ConfigInput {
        path: path.into(),
        text: text.into(),
        notebook,
    }
}
fn run(path: &str, text: &str, notebook: bool) -> Value {
    evaluate(&[input(path, text, notebook)]).unwrap()
}
fn rules(value: &Value) -> Vec<&str> {
    value["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["rule_id"].as_str().unwrap())
        .collect()
}
fn malformed(path: &str, text: &str) -> Value {
    let result = run(path, text, false);
    assert_eq!(rules(&result), ["malformed-config"], "{path}: {result}");
    assert_eq!(result["findings"][0]["severity"], "critical");
    assert_eq!(result["metrics"]["malformed_files"], 1);
    result
}
#[test]
fn valid_configs_include_json_scalars_arrays_toml_dates_and_workflow_yaml() {
    for text in [
        "null",
        "true",
        "4",
        "\"雪\"",
        "[1, {\"key\": [false, null]}]",
    ] {
        assert!(rules(&run("a.json", text, false)).is_empty());
    }
    let records = [
        input(
            "z.TOML",
            "name='雪'\nstamp=2026-11-01T12:34:56Z\n[table]\nitems=[1,2]\n",
            false,
        ),
        input(
            "a.YAML",
            "on:\n  push: {}\njobs:\n  build:\n    steps:\n      - run: echo valid\n",
            false,
        ),
    ];
    let result = evaluate(&records).unwrap();
    assert!(rules(&result).is_empty());
    assert_eq!(result["files"], json!(["a.YAML", "z.TOML"]));
    assert_eq!(result["metrics"]["files_parsed"], 2);
    for text in ["", "# comment only\n", "[1, true, null]", "value"] {
        assert!(rules(&run("scalar.yml", text, false)).is_empty());
    }
}
#[test]
fn parser_failures_and_duplicate_keys_are_explicit_without_source_echo() {
    for (path, text) in [
        ("a.json", "{\"secret-key\": private-token}"),
        ("a.json", r#"{"x":1,"\u0078":2}"#),
        ("a.json", "[1,]"),
        ("a.toml", "secret='private-token'\nsecret='second'\n"),
        ("a.toml", "[broken"),
        ("a.yaml", "key: [unterminated"),
    ] {
        let result = malformed(path, text);
        let message = result["findings"][0]["message"].as_str().unwrap();
        assert!(!message.contains("private-token") && !message.contains("secret-key"));
    }
    malformed("unknown.cfg", "valid=1");
}
#[test]
fn notebooks_preserve_cell_index_and_python_truthiness_without_executing_cells() {
    let book = json!({"nbformat":4,"cells":[
        {"cell_type":"code","source":["must_not_execute()"],"outputs":[{"text":"output"}],"execution_count":null},
        {"cell_type":"markdown","outputs":[1],"execution_count":5},
        {"cell_type":"code","outputs":[],"execution_count":7},
        {"cell_type":"code","outputs":[],"execution_count":0},
        {"cell_type":"code","outputs":null,"execution_count":false}
    ]});
    let result = run("book.ipynb", &book.to_string(), false);
    assert_eq!(rules(&result), ["notebook-output", "notebook-output"]);
    assert_eq!(result["findings"][0]["line"], 1);
    assert_eq!(result["findings"][1]["line"], 3);
    assert_eq!(result["findings"][0]["severity"], "high");
    assert_eq!(result["metrics"]["notebooks"], 1);
    let clean =
        json!({"nbformat":4.0,"cells":[{"cell_type":"code","outputs":[],"execution_count":0}]});
    assert!(rules(&run("book.data", &clean.to_string(), true)).is_empty());
    assert!(rules(&run("book.IPYNB", &clean.to_string(), false)).is_empty());
}
#[test]
fn notebook_schema_errors_remain_critical_after_prior_output_findings() {
    for value in [
        json!([]),
        json!({"nbformat":4}),
        json!({"nbformat":3,"cells":[]}),
        json!({"nbformat":"4","cells":[]}),
        json!({"nbformat":4,"cells":[3]}),
    ] {
        malformed("book.ipynb", &value.to_string());
    }
    let partial = json!({"nbformat":4,"cells":[{"cell_type":"code","execution_count":1},false]});
    let result = run("book.ipynb", &partial.to_string(), false);
    assert_eq!(rules(&result), ["notebook-output", "malformed-config"]);
    assert_eq!(result["metrics"]["malformed_files"], 1);
    assert_eq!(rules(&run("data.json", "{}", true)), ["malformed-config"]);
}
#[test]
fn yaml_duplicate_and_implicitly_equal_keys_are_valid_syntax() {
    for text in [
        "key: 1\nkey: 2\n",
        "key: 1\n'key': 2\n",
        "on: 1\ntrue: 2\n",
        "yes: 1\n1: 2\n",
        "1: first\n1.0: second\n",
        "null: a\n~: b\n",
        "010: a\n",
        "2026-11-01: a\n",
    ] {
        assert!(rules(&run("key.yml", text, false)).is_empty(), "{text}");
    }
    malformed("key.yml", "? [a,b]\n: value\n");
    assert!(rules(&run("key.yml", "'on': 1\ntrue: 2\n", false)).is_empty());
    assert!(rules(&run(
        "key.yml",
        "'010': value\n'2026-11-01': value\n",
        false
    ))
    .is_empty());
}
#[test]
fn yaml_safe_aliases_tags_merges_and_dates_are_admitted() {
    for text in [
        "a: &id 1\nb: *id\n",
        "a: !!str 1\n",
        "a: &empty []\n",
        "<<: {a: 1}\na: 2\n",
        "date: 2026-11-01\n",
        "a: 2026-latest\n12-service: 01\n",
        "a: !!float .inf\nb: .NaN\n",
    ] {
        assert!(rules(&run("a.yaml", text, false)).is_empty(), "{text}");
    }
    for text in [
        "a: *missing\n",
        "a: !custom value\n",
        "<<: 5\n",
        "---\na: 1\n---\nb: 2\n",
        "---\n---\n",
        "date: 2026-99-99\n",
    ] {
        malformed("a.yaml", text);
    }
    assert!(rules(&run("a.yaml", "date: '2026-11-01'\n'<<': value\n", false)).is_empty());
}
#[test]
fn json_nonfinite_is_refused_while_toml_and_yaml_keep_syntax_semantics() {
    for (path, text) in [("a.json", "1e9999"), ("a.json", "NaN")] {
        malformed(path, text);
    }
    for (path, text) in [
        ("a.json", "\"NaN\""),
        ("a.toml", "value='nan'\n"),
        ("a.yml", "value: '.inf'\n"),
        ("a.toml", "value=nan\n"),
        ("a.toml", "value=+inf\n"),
        ("a.yml", "value: .inf\n"),
        ("a.yml", "value: .NaN\n"),
    ] {
        assert!(rules(&run(path, text, false)).is_empty());
    }
}
fn small() -> Limits {
    Limits {
        files: 4,
        source: 1024,
        total: 4096,
        nodes: 32,
        depth: 3,
        scalar: 32,
        findings: 4,
        output: 4096,
    }
}
#[test]
fn per_file_structure_limits_emit_malformed_findings_not_partial_clean_results() {
    for (path, text) in [
        ("a.json", "[[[[0]]]]"),
        ("a.toml", "[a.b.c.d]\nx=1\n"),
        ("a.yml", "a: {b: {c: {d: 1}}}\n"),
    ] {
        let result = evaluate_with_limits(&[input(path, text, false)], small()).unwrap();
        assert_eq!(rules(&result), ["malformed-config"]);
    }
    for (path, text) in [
        ("a.json", "\"12345\""),
        ("a.toml", "a='12345'"),
        ("a.yml", "a: '12345'"),
    ] {
        let result = evaluate_with_limits(
            &[input(path, text, false)],
            Limits {
                scalar: 4,
                ..small()
            },
        )
        .unwrap();
        assert_eq!(rules(&result), ["malformed-config"]);
    }
    let result = evaluate_with_limits(
        &[input("a.json", "[0,1,2,3]", false)],
        Limits {
            nodes: 4,
            ..small()
        },
    )
    .unwrap();
    assert_eq!(rules(&result), ["malformed-config"]);
}
#[test]
fn global_input_admission_is_separate_from_per_file_parser_findings() {
    assert!(
        evaluate(&vec![input("a.json", "null", false); LIMITS.files + 1])
            .unwrap_err()
            .to_string()
            .contains("input count")
    );
    assert!(evaluate(&[input("large.json", &"x".repeat(LIMITS.source + 1), false)]).is_err());
    assert!(evaluate_with_limits(
        &[
            input("a.json", "null", false),
            input("b.json", "null", false)
        ],
        Limits {
            files: 1,
            ..small()
        }
    )
    .is_err());
    assert!(evaluate_with_limits(
        &[input("a.json", "[123456789]", false)],
        Limits {
            source: 10,
            ..small()
        }
    )
    .is_err());
    assert!(evaluate_with_limits(
        &[input("a.json", "null", false)],
        Limits {
            total: 9,
            ..small()
        }
    )
    .is_err());
    assert!(evaluate(&[
        input("a.json", "null", false),
        input("a.json", "null", false)
    ])
    .is_err());
    for path in ["../a.json", "/a.json", "a//b.json", "a\nb.json"] {
        assert!(evaluate(&[input(path, "null", false)]).is_err());
    }
}
#[test]
fn finding_and_encoded_output_limits_fail_instead_of_truncating() {
    let book = json!({"nbformat":4,"cells":[{"cell_type":"code","execution_count":1},
        {"cell_type":"code","execution_count":2}]});
    let book_input = input("book.ipynb", &book.to_string(), false);
    assert!(evaluate_with_limits(
        &[book_input],
        Limits {
            findings: 1,
            ..small()
        }
    )
    .unwrap_err()
    .to_string()
    .contains("findings"));
    assert!(evaluate_with_limits(
        &[input("a.json", "null", false)],
        Limits {
            output: 16,
            ..small()
        }
    )
    .is_err());
}
#[test]
fn order_is_deterministic_and_empty_batch_is_complete_without_findings() {
    let a = input("a.json", "broken", false);
    let z = input("z.toml", "value=1", false);
    assert_eq!(
        evaluate(&[a.clone(), z.clone()]).unwrap(),
        evaluate(&[z, a]).unwrap()
    );
    let empty = evaluate(&[]).unwrap();
    assert_eq!(empty["files"], json!([]));
    assert_eq!(empty["findings"], json!([]));
    assert_eq!(empty["metrics"]["files_scanned"], 0);
}
