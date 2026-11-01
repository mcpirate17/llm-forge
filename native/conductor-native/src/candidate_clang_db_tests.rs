use super::*;
use serde_json::{json, Value};
fn record(arguments: Value) -> Value {
    json!({"directory":"/snapshot/build","file":"../src/雪.c","arguments":arguments})
}
fn parse(value: Value) -> Result<Database, String> {
    parse_database(&serde_json::to_vec(&value).unwrap())
}
#[test]
fn all_configurations_and_order_survive_without_path_rebasing() {
    let a = record(json!(["clang", "-DNAME=#tag", "-c", "../src/雪.c"]));
    let b = json!({"directory":"relative","file":"../src/雪.c",
        "command": "clang '-DNAME=with spaces' -c ../src/雪.c", "output":"雪.o"});
    let db = parse(json!([b, a.clone(), a])).unwrap();
    assert_eq!(db.entries.len(), 3);
    assert_eq!(db.entries[0].directory, "relative");
    assert_eq!(db.entries[0].file, "../src/雪.c");
    assert_eq!(
        db.entries[0].arguments,
        ["clang", "-DNAME=with spaces", "-c", "../src/雪.c"]
    );
    assert_eq!(db.entries[0].output.as_deref(), Some("雪.o"));
    assert_eq!(db.entries[1], db.entries[2]);
    assert!(parse(json!([])).unwrap().entries.is_empty());
    assert_eq!(
        serde_json::to_value(db).unwrap()["entries"][0]["file"],
        "../src/雪.c"
    );
}
#[test]
fn dual_representations_must_agree_including_empty_args_and_llvm_backslashes() {
    let mut valid = record(json!(["clang", "", "aqb", "a b.c"]));
    valid["command"] = json!(r#"clang '' "a\qb" 'a b.c'"#);
    assert_eq!(
        parse(json!([valid.clone()])).unwrap().entries[0].arguments[2],
        "aqb"
    );
    valid["arguments"][2] = json!(r"a\qb");
    assert!(parse(json!([valid])).unwrap_err().contains("disagree"));
    for command in ["", "   ", "'' -c x.c", "clang 'broken", "clang \\"] {
        assert!(parse(json!([{"directory":".","file":"x.c","command":command}])).is_err());
    }
}
#[test]
fn strict_json_duplicate_unknown_and_typed_shape_refusals() {
    let duplicates = [
        r#"[{"directory":".","directory":"other","file":"x.c","arguments":["clang"]}]"#,
        r#"[{"directory":".","file":"x.c","f\u0069le":"other.c","arguments":["clang"]}]"#,
        r#"[{"directory":".","file":"x.c","arguments":["clang"],"arguments":["clang"]}]"#,
        r#"[{"directory":".","file":"x.c","command":"clang","command":"other"}]"#,
        r#"[{"directory":".","file":"x.c","command":"clang","output":"","output":"x.o"}]"#,
    ];
    for raw in duplicates {
        assert!(parse_database(raw.as_bytes())
            .unwrap_err()
            .contains("duplicate"));
    }
    for value in [
        json!({}),
        json!([null]),
        json!([{}]),
        json!([{"directory":".","file":"x.c","command":"clang","extra":1}]),
        json!([{"directory":".","file":"x.c","arguments":null}]),
        json!([{"directory":".","file":"x.c","arguments":[3]}]),
        json!([{"directory":".","file":"x.c","arguments":[]}]),
        json!([{"directory":".","file":"x.c","arguments":[""]}]),
        json!([{"directory":".","file":"x.c","command":"clang","output":null}]),
    ] {
        assert!(parse(value).is_err());
    }
    assert!(parse_database(b"[] []").is_err());
    assert!(parse_database(&[b'[', 0xff, b']'])
        .unwrap_err()
        .contains("UTF8"));
}
#[test]
fn controls_are_refused_without_echoing_untrusted_values() {
    for field in ["directory", "file", "output", "command"] {
        for control in ['\0', '\n', '\r', '\t', '\u{7f}', '\u{85}'] {
            let mut entry = json!({"directory":".","file":"x.c","command":"clang"});
            entry[field] = json!(format!("private-token{control}value"));
            let error = parse(json!([entry])).unwrap_err();
            assert!(!error.contains("private-token"));
        }
    }
    assert!(parse(json!([record(json!(["clang", "-DVALUE=\n"]))])).is_err());
    let error = parse(json!("private-secret-wrong-shape")).unwrap_err();
    assert!(!error.contains("private-secret"));
}
fn small() -> Limits {
    Limits {
        input: 4096,
        decoded: 128,
        records: 2,
        arguments: 3,
        token: 8,
        command: 64,
        path: 8,
        total_arguments: 12,
    }
}
fn limited(value: Value, limits: Limits) -> Result<Database, String> {
    parse_with_limits(&serde_json::to_vec(&value).unwrap(), limits)
}
#[test]
fn exact_record_argument_path_and_token_boundaries() {
    let entry = json!({"directory":"12345678","file":"x.c","arguments":["clang","雪山ab",""]});
    assert!(limited(json!([entry.clone(), entry.clone()]), small()).is_ok());
    assert!(limited(
        json!([entry.clone(), entry.clone(), entry.clone()]),
        small()
    )
    .is_err());
    let mut over = entry.clone();
    over["arguments"].as_array_mut().unwrap().push(json!(""));
    assert!(limited(json!([over]), small()).is_err());
    let mut over = entry.clone();
    over["file"] = json!("123456789");
    assert!(limited(json!([over]), small()).is_err());
    let mut over = entry;
    over["arguments"][1] = json!("雪山abc");
    assert!(limited(json!([over]), small()).is_err());
}
#[test]
fn decoded_aggregate_counts_all_representations_even_when_they_agree() {
    let entry = json!({"directory":".","file":"x","arguments":["cc","ab"],"command":"cc ab"});
    // 1+1 path bytes +4 supplied argv +5 command +4 derived argv =15.
    let limits = Limits {
        decoded: 15,
        ..small()
    };
    assert!(limited(json!([entry.clone()]), limits).is_ok());
    assert!(limited(
        json!([entry]),
        Limits {
            decoded: 14,
            ..limits
        }
    )
    .is_err());
    let raw = b"[]";
    assert!(parse_with_limits(
        raw,
        Limits {
            input: 2,
            ..small()
        }
    )
    .is_ok());
    assert!(parse_with_limits(
        raw,
        Limits {
            input: 1,
            ..small()
        }
    )
    .is_err());
}
#[test]
fn command_emission_is_bounded_even_for_many_empty_quoted_arguments() {
    let mut args = vec!["clang".to_owned()];
    args.resize(MAX_ARGUMENTS, String::new());
    assert!(parse(json!([record(json!(args))])).is_ok());
    let command = format!("clang{}", " ''".repeat(MAX_ARGUMENTS));
    let error = parse(json!([{"directory":".","file":"x.c","command":command}])).unwrap_err();
    assert!(error.contains("compilation command exceeds"));
    let mut args = vec!["clang".to_owned()];
    args.resize(MAX_ARGUMENTS + 1, String::new());
    assert!(parse(json!([record(json!(args))])).is_err());
}
#[test]
fn large_record_count_is_rejected_during_stream_decode() {
    let entry = br#"{"directory":".","file":"x.c","arguments":["cc"]}"#;
    let mut raw = Vec::with_capacity((entry.len() + 1) * (MAX_RECORDS + 1) + 2);
    raw.push(b'[');
    for i in 0..=MAX_RECORDS {
        if i > 0 {
            raw.push(b',');
        }
        raw.extend_from_slice(entry);
    }
    raw.push(b']');
    assert!(raw.len() < MAX_INPUT);
    assert!(parse_database(&raw)
        .unwrap_err()
        .contains("compilation database exceeds"));
}
#[test]
fn empty_argument_metadata_cannot_evade_aggregate_count_admission() {
    let entry = json!({"directory":".","file":"x","arguments":["cc","",""]});
    let limits = Limits {
        total_arguments: 5,
        ..small()
    };
    assert!(limited(json!([entry.clone()]), limits).is_ok());
    let error = limited(json!([entry.clone(), entry]), limits).unwrap_err();
    assert!(error.contains("total argument count"), "{error}");
}
#[test]
fn actual_public_byte_limits_include_unicode_and_command_padding() {
    let token = format!("{}雪", "a".repeat(MAX_TOKEN - 3));
    assert!(parse(json!([record(json!(["cc", token.clone()]))])).is_ok());
    assert!(parse(json!([record(json!(["cc", format!("{token}a")]))])).is_err());
    let mut entry = json!({"directory":"a".repeat(MAX_PATH),"file":"x.c","arguments":["cc"]});
    assert!(parse(json!([entry.clone()])).is_ok());
    entry["directory"] = json!("a".repeat(MAX_PATH + 1));
    assert!(parse(json!([entry])).is_err());
    let command = format!("cc{}", " ".repeat(MAX_COMMAND - 2));
    let entry = json!({"directory":".","file":"x.c","command":command});
    assert!(parse(json!([entry.clone()])).is_ok());
    let mut entry = entry;
    entry["command"] = json!(format!("{} ", entry["command"].as_str().unwrap()));
    assert!(parse(json!([entry])).is_err());
    assert!(parse_database(&vec![b' '; MAX_INPUT + 1])
        .unwrap_err()
        .contains("input byte limit"));
}
