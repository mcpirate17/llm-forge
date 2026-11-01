use forge::tool_schema::{CompiledSchema, ToolRegistry, MAX_DEPTH, MAX_INPUT_BYTES};
use serde_json::json;

fn object() -> CompiledSchema {
    CompiledSchema::compile(&json!({"type":"object", "additionalProperties":false,
        "properties":{"path":{"type":"string","minLength":1},"limit":{"type":"integer","minimum":1,"maximum":100}},
        "required":["path"]})).unwrap()
}

#[test]
fn closed_objects_reject_missing_unknown_duplicate_and_mistyped_parameters() {
    let schema = object();
    for value in [
        r#"{"path":"a"}"#,
        r#"{"limit":10,"path":"a"}"#,
        r#"{"p\u0061th":"a"}"#,
    ] {
        schema.validate(value).unwrap();
    }
    for (input, code) in [
        (r#"{}"#, "required_property"),
        (r#"{"path":""}"#, "string_length"),
        (r#"{"path":"a","extra":0}"#, "unknown_property"),
        (r#"{"path":"a","p\u0061th":"b"}"#, "duplicate_property"),
        (r#"{"path":"a","limit":"10"}"#, "type"),
        (r#"{"path":"a","limit":0}"#, "number_range"),
    ] {
        assert_eq!(schema.validate(input).unwrap_err().code, code, "{input}");
    }
}

#[test]
fn exact_decimals_and_mathematical_integers_do_not_round_through_floats() {
    let schema = CompiledSchema::from_json(
        r#"{"type":"integer","minimum":9007199254740993,"maximum":9007199254740993}"#,
    )
    .unwrap();
    for value in [
        "9007199254740993",
        "9007199254740993.000",
        "90071992547409930e-1",
    ] {
        schema.validate(value).unwrap();
    }
    for value in ["9007199254740992", "9007199254740994", "9007199254740993.1"] {
        assert!(schema.validate(value).is_err(), "{value}");
    }
    let integers = CompiledSchema::from_json(r#"{"type":"integer"}"#).unwrap();
    for value in ["-0.0", "1e1000000", "0e-1000000", "1.5e1", "123000e-3"] {
        integers.validate(value).unwrap();
    }
    for value in ["1e-1000000", "1.00000000000000001", "123000e-4"] {
        assert_eq!(integers.validate(value).unwrap_err().code, "integer");
    }
    let negative =
        CompiledSchema::from_json(r#"{"type":"number","minimum":-1.01,"maximum":-0.9}"#).unwrap();
    for value in ["-1.01", "-1", "-0.9", "-99e-2"] {
        negative.validate(value).unwrap();
    }
    for value in ["-1.0101", "-0.89", "0", "-0"] {
        assert!(negative.validate(value).is_err());
    }
}

#[test]
fn strings_count_unicode_scalars_and_decode_surrogate_pairs() {
    let one =
        CompiledSchema::from_json(r#"{"type":"string","minLength":1,"maxLength":1}"#).unwrap();
    for input in [
        r#""a""#,
        r#""é""#,
        r#""😀""#,
        r#""\ud83d\ude00""#,
        r#""\n""#,
        r#""\u0000""#,
    ] {
        one.validate(input).unwrap();
    }
    for input in [
        r#""""#,
        r#""ab""#,
        r#""\ud800""#,
        r#""\udc00""#,
        r#""\ud800\u0041""#,
        r#""\x20""#,
        "\"a\n\"",
        r#""\u00""#,
    ] {
        assert!(one.validate(input).is_err(), "{input:?}");
    }
    let choices = CompiledSchema::from_json(r#"{"type":"string","enum":["😀","a\nb"]}"#).unwrap();
    choices.validate(r#""\ud83d\ude00""#).unwrap();
    choices.validate(r#""a\u000ab""#).unwrap();
    assert!(choices.validate(r#""a""#).is_err());
}

#[test]
fn containers_whitespace_and_trailing_input_are_strict() {
    let array = CompiledSchema::from_json(
        r#"{"type":"array","minItems":1,"maxItems":2,"items":{"type":"boolean"}}"#,
    )
    .unwrap();
    for input in ["[true]", " \n[true, false]\t"] {
        array.validate(input).unwrap();
    }
    for input in [
        "[]",
        "[true,false,true]",
        "[true,]",
        "[true false]",
        "[null]",
        "[true]false",
        "[True]",
        "[true\u{b}]",
    ] {
        assert!(array.validate(input).is_err(), "{input:?}");
    }
    let schema = object();
    for input in [
        r#"{"path":"a",}"#,
        r#"{'path':'a'}"#,
        r#"{"path":"a"}{}"#,
        "",
        "\u{feff}{}",
    ] {
        assert!(schema.validate(input).is_err(), "{input:?}");
    }
}

#[test]
fn number_scanner_rejects_non_json_and_resource_exhausting_exponents() {
    let schema = CompiledSchema::from_json(r#"{"type":"number"}"#).unwrap();
    for input in ["0", "-0", "1.1", "1e+3", "1E-3", "1e309", "1e-309"] {
        schema.validate(input).unwrap();
    }
    for input in [
        "+1",
        "01",
        "-01",
        ".1",
        "1.",
        "1e",
        "1e+",
        "--1",
        "NaN",
        "Infinity",
        "1 2",
        "1e1000001",
        "1e99999999999999999999",
    ] {
        assert!(schema.validate(input).is_err(), "{input}");
    }
}

#[test]
fn unsupported_ambiguous_and_oversized_schemas_fail_registration() {
    for input in [
        r#"{}"#,
        r#"true"#,
        r#"{"type":["null","string"]}"#,
        r#"{"type":"object"}"#,
        r#"{"type":"object","additionalProperties":true}"#,
        r#"{"type":"string","pattern":".*"}"#,
        r#"{"type":"string","maxLength":1048577}"#,
        r#"{"type":"string","minLength":2,"maxLength":1}"#,
        r#"{"type":"string","enum":[]}"#,
        r#"{"type":"string","enum":["x","x"]}"#,
        r#"{"type":"string","enum":[1]}"#,
        r#"{"type":"number","maximum":0,"minimum":1}"#,
        r#"{"type":"array"}"#,
        r#"{"type":"null","oneOf":[]}"#,
        r#"{"type":"object","additionalProperties":false,"required":["missing"]}"#,
        r#"{"type":"number","type":"string"}"#,
        r#"{"type":"object","additionalProperties":false,"properties":{"a":{"type":"string"},"\u0061":{"type":"number"}}}"#,
        r#"{"type":"number","$ref":"https://example.invalid/schema"}"#,
    ] {
        assert!(CompiledSchema::from_json(input).is_err(), "{input}");
    }
    let mut nested = json!({"type":"null"});
    for _ in 0..=MAX_DEPTH {
        nested = json!({"type":"array","items":nested});
    }
    assert!(CompiledSchema::compile(&nested).is_err());
    let properties = (0..129)
        .map(|i| (format!("p{i}"), json!({"type":"null"})))
        .collect::<serde_json::Map<_, _>>();
    assert!(CompiledSchema::compile(
        &json!({"type":"object","properties":properties,"additionalProperties":false})
    )
    .is_err());
}

#[test]
fn large_string_bounds_remain_exact_with_compact_grammar() {
    let schema =
        CompiledSchema::from_json(r#"{"type":"string","minLength":8191,"maxLength":524288}"#)
            .unwrap();
    assert!(schema.grammar().len() < 10000);
    for length in [8190, 8191, 8192, 524288, 524289] {
        let input = format!("\"{}\"", "x".repeat(length));
        assert_eq!(
            schema.validate(&input).is_ok(),
            (8191..=524288).contains(&length)
        );
    }
}

#[test]
fn repairs_are_explicit_revalidated_and_never_change_parameter_values() {
    let schema = object();
    let input = " \n```json\n{\"path\":\"a\"}\n```\n";
    assert!(schema.validate_with_repair(input, false).is_err());
    let repaired = schema.validate_with_repair(input, true).unwrap();
    assert!(repaired.repaired);
    assert_eq!(repaired.json, r#"{"path":"a"}"#);
    assert!(
        !schema
            .validate_with_repair(repaired.json, true)
            .unwrap()
            .repaired
    );
    for input in [
        "```json\n{}\n```",
        "prefix\n```json\n{\"path\":\"a\"}\n```",
        "{\"path\":\"a\",}",
    ] {
        assert!(schema.validate_with_repair(input, true).is_err());
    }
    let huge = "x".repeat(MAX_INPUT_BYTES + 1);
    assert_eq!(
        schema.validate_with_repair(&huge, true).unwrap_err().code,
        "input_limit"
    );
}

fn catalog() -> String {
    r#"{"tools":[{"name":"inspect","inputSchema":{"type":"object","additionalProperties":false,"properties":{"path":{"type":"string"}},"required":["path"]}}]}"#.to_owned()
}

#[test]
fn registry_accepts_only_known_calls_and_validated_arguments() {
    let registry = ToolRegistry::from_json(&catalog()).unwrap();
    let call = registry
        .validate_call(
            r#"{"arguments":{"path":"main.rs"},"name":"ins\u0070ect"}"#,
            false,
        )
        .unwrap();
    assert_eq!(call.name(), "inspect");
    assert_eq!(call.arguments(), r#"{"path":"main.rs"}"#);
    for input in [
        r#"{"name":"exec","arguments":{"path":"a"}}"#,
        r#"{"name":"inspect","arguments":{}}"#,
        r#"{"name":"inspect","arguments":{},"extra":0}"#,
        r#"{"name":"inspect","name":"inspect","arguments":{}}"#,
        r#"{"name":"inspect","arguments":{},"arguments":{"path":"a"}}"#,
        r#"{"name":"inspect","arguments":"{\"path\":\"a\"}"}"#,
    ] {
        assert!(registry.validate_call(input, false).is_err(), "{input}");
    }
    assert!(ToolRegistry::from_json(
        &catalog().replace("\"tools\":", "\"nextCursor\":\"x\",\"tools\":")
    )
    .is_err());
    let mut duplicate: serde_json::Value = serde_json::from_str(&catalog()).unwrap();
    let tool = duplicate["tools"][0].clone();
    duplicate["tools"].as_array_mut().unwrap().push(tool);
    let error = ToolRegistry::from_json(&duplicate.to_string()).unwrap_err();
    assert!(error.to_string().contains("duplicate tool name"));
}

#[test]
fn full_property_bitset_catalog_and_depth_limits_accept_the_boundary() {
    let properties: serde_json::Map<_, _> = (0..128)
        .map(|i| (format!("p{i}"), json!({"type":"null"})))
        .collect();
    let required: Vec<_> = properties.keys().cloned().collect();
    let value: serde_json::Map<_, _> = properties
        .keys()
        .map(|name| (name.clone(), serde_json::Value::Null))
        .collect();
    let schema = CompiledSchema::compile(&json!({"type":"object","properties":properties,
        "required":required,"additionalProperties":false}))
    .unwrap();
    schema
        .validate(&serde_json::Value::Object(value).to_string())
        .unwrap();
    let tools: Vec<_> = (0..256)
        .rev()
        .map(|i| {
            json!({"name":format!("tool{i:03}"),
        "inputSchema":{"type":"object","additionalProperties":false}})
        })
        .collect();
    let registry = ToolRegistry::from_json(&json!({"tools":tools}).to_string()).unwrap();
    for i in [0, 1, 128, 255] {
        registry
            .validate_call(
                &format!(r#"{{"name":"tool{i:03}","arguments":{{}}}}"#),
                false,
            )
            .unwrap();
    }
    let mut nested = json!({"type":"null"});
    let mut input = "null".to_owned();
    for _ in 0..MAX_DEPTH {
        nested = json!({"type":"array","items":nested});
        input = format!("[{input}]");
    }
    CompiledSchema::from_json(&nested.to_string())
        .unwrap()
        .validate(&input)
        .unwrap();
}

#[test]
fn decimal_order_matches_exact_scaled_integer_oracle() {
    let schema =
        CompiledSchema::from_json(r#"{"type":"number","minimum":-1.25,"maximum":2.75}"#).unwrap();
    for scaled in -1000..=1000 {
        let expected = (-125..=275).contains(&scaled);
        for text in [
            format!("{scaled}e-2"),
            format!("{}e-3", scaled * 10),
            format!("{scaled}.00e-2"),
        ] {
            assert_eq!(schema.validate(&text).is_ok(), expected, "{text}");
        }
    }
}

#[test]
fn schema_bounds_preserve_decimals_beyond_ieee_and_u64() {
    let schema = CompiledSchema::from_json(r#"{"type":"number","minimum":0.1000000000000000000000001,"maximum":0.1000000000000000000000002}"#).unwrap();
    for value in [
        "0.1000000000000000000000001",
        "0.10000000000000000000000015",
    ] {
        schema.validate(value).unwrap();
    }
    for value in ["0.1", "0.1000000000000000000000003"] {
        assert_eq!(schema.validate(value).unwrap_err().code, "number_range");
    }
    let huge =
        CompiledSchema::from_json(r#"{"type":"number","minimum":1e400,"maximum":1e400}"#).unwrap();
    huge.validate("10e399").unwrap();
    assert!(huge.validate("1e399").is_err());
    assert!(huge.validate("1e401").is_err());
}

#[test]
fn immutable_schema_handles_concurrent_validation_and_arbitrary_utf8_without_panics() {
    let schema = object();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..5000 {
                    schema.validate(r#"{"path":"main.rs","limit":10}"#).unwrap();
                }
            });
        }
    });
    // Deterministic malformed-input stress. Compare string syntax against serde
    // on generated ASCII documents, including every possible control byte.
    let string = CompiledSchema::from_json(r#"{"type":"string"}"#).unwrap();
    let mut random = 0x4d595df4d0f33173u64;
    for _ in 0..20_000 {
        let mut input = String::from("\"");
        for _ in 0..12 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            input.push((random as u8 % 128) as char);
        }
        input.push('"');
        assert_eq!(
            string.validate(&input).is_ok(),
            serde_json::from_str::<String>(&input).is_ok(),
            "{input:?}"
        );
    }
}

#[test]
fn grammar_is_deterministic_bounded_and_declares_runtime_numeric_checks() {
    let schema = object();
    let grammar = schema.grammar();
    assert!(grammar.starts_with("# forge tool schema:"));
    assert!(grammar.contains("numeric bounds are runtime checks"));
    assert!(grammar.contains("root ::= ws n-0"));
    assert_eq!(grammar, object().grammar());
    let mut properties = serde_json::Map::new();
    for i in 0..128 {
        properties.insert(format!("p{i}"), json!({"type":"boolean"}));
    }
    let large = CompiledSchema::compile(
        &json!({"type":"object","properties":properties,"additionalProperties":false}),
    )
    .unwrap();
    assert!(
        large.grammar().len() < 40_000,
        "optional fields must not expand permutations"
    );
}
#[test]
fn storage_size_override_preserves_duplicate_checks_and_tool_input_limit() {
    let text = "x".repeat(1024 * 1024);
    let input = format!("{{\"text\":\"{text}\"}}");
    assert!(forge::tool_schema::parse_document(&input).is_err());
    let parsed = forge::tool_schema::parse_document_with_limit(&input, 2 * 1024 * 1024).unwrap();
    assert_eq!(parsed["text"], text);
    let duplicate = format!("{{\"text\":\"{text}\",\"nested\":{{\"key\":1,\"k\\u0065y\":2}}}}");
    assert!(
        forge::tool_schema::parse_document_with_limit(&duplicate, 2 * 1024 * 1024)
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
    assert!(forge::tool_schema::parse_document_with_limit("{}", 33 * 1024 * 1024).is_err());
    assert!(forge::tool_schema::parse_document_with_limit("{}", 0).is_err());
}
