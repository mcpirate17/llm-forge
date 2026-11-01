use super::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn fixture() -> Value {
    serde_json::from_slice(include_bytes!(
        "../../tests/fixtures/candidate_receipt/reference.json"
    ))
    .unwrap()
}
fn signed(mut value: Value) -> Vec<u8> {
    let fields = value.as_object_mut().unwrap();
    fields.insert("receipt_id".into(), json!(""));
    fields.insert("receipt_digest".into(), json!(""));
    let bytes = canonical::encode(&value).unwrap();
    value["receipt_digest"] = json!(format!("{:x}", Sha256::digest(bytes.as_bytes())));
    serde_json::to_vec(&value).unwrap()
}

#[test]
fn cpython_oracle_cases_and_digests_match() {
    let raw = include_bytes!("../../tests/fixtures/candidate_receipt/reference.json");
    assert_eq!(
        format!("{:x}", Sha256::digest(raw)),
        "75484806648914f02abf47135d60b3454403e4e86ee95d7a5f88163c607cad37"
    );
    let data = fixture();
    let cases = data["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 26);
    for case in cases {
        let input = case["input"].as_str().unwrap();
        let value = crate::tool_schema::parse_document(input).unwrap();
        let text = canonical::encode(&value).unwrap();
        assert_eq!(text, case["canonical"].as_str().unwrap(), "input {input}");
        assert_eq!(
            format!("{:x}", Sha256::digest(text.as_bytes())),
            case["sha256"].as_str().unwrap()
        );
    }
}

#[test]
fn finite_float_corpus_matches_independent_cpython_reference() {
    let raw = include_bytes!("../../tests/fixtures/candidate_receipt/float-reference.json");
    let data: Value = serde_json::from_slice(raw).unwrap();
    let cases = data["cases"].as_array().unwrap();
    assert!(cases.len() > 7000);
    for case in cases {
        let input = case[0].as_str().unwrap();
        let value = crate::tool_schema::parse_document(input).unwrap();
        assert_eq!(
            canonical::encode(&value).unwrap(),
            case[1].as_str().unwrap(),
            "input {input}"
        );
    }
}

#[test]
fn digest_valid_failure_is_not_an_approval_and_id_is_unbound() {
    let data = fixture();
    let mut receipt = data["digest_valid_failed_receipt"].clone();
    let verified = parse_and_verify(&serde_json::to_vec(&receipt).unwrap()).unwrap();
    assert_eq!(verified.decision, "fail");
    assert_eq!(verified.receipt_id, "ignored identifier");
    receipt["receipt_id"] = json!("changed");
    assert_eq!(
        parse_and_verify(&serde_json::to_vec(&receipt).unwrap())
            .unwrap()
            .receipt_id,
        "changed"
    );
    receipt["message"] = json!("tampered");
    assert!(parse_and_verify(&serde_json::to_vec(&receipt).unwrap()).is_err());
}

#[test]
fn absent_or_nonstring_display_fields_do_not_imply_invalid_digest() {
    for value in [json!({}), json!({"decision":17})] {
        let bytes = signed(value);
        let mut payload: Value = serde_json::from_slice(&bytes).unwrap();
        payload.as_object_mut().unwrap().remove("receipt_id");
        let receipt = parse_and_verify(&serde_json::to_vec(&payload).unwrap()).unwrap();
        assert!(receipt.decision.is_empty() && receipt.receipt_id.is_empty());
        payload["receipt_id"] = json!(false);
        assert!(parse_and_verify(&serde_json::to_vec(&payload).unwrap()).is_ok());
    }
}

#[test]
fn rejects_duplicate_keys_nonfinite_numbers_and_excessive_limits() {
    for input in [
        r#"{"receipt_digest":"a","receipt_digest":"b"}"#,
        r#"{"receipt_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","n":1e999}"#,
        r#"{"receipt_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","n":NaN}"#,
        r#"{"receipt_digest":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#,
    ] {
        assert!(parse_and_verify(input.as_bytes()).is_err());
    }
    let large = format!(
        "{{\"receipt_digest\":\"{}\",\"n\":{}}}",
        "a".repeat(64),
        "1".repeat(4301)
    );
    assert!(parse_and_verify(large.as_bytes()).is_err());
    assert!(parse_and_verify(&vec![b' '; MAX_BYTES + 1]).is_err());
    let deep = format!(
        "{{\"receipt_digest\":\"{}\",\"n\":{}0{}}}",
        "a".repeat(64),
        "[".repeat(80),
        "]".repeat(80)
    );
    assert!(parse_and_verify(deep.as_bytes()).is_err());
}

#[test]
fn sealing_matches_independent_legacy_digest_and_only_replaces_identity_fields() {
    let original = fixture()["digest_valid_failed_receipt"].clone();
    let receipt = seal(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(
        receipt.digest(),
        original["receipt_digest"].as_str().unwrap()
    );
    assert_eq!(
        receipt.receipt_id(),
        format!("gr-{}", &receipt.digest()[..24])
    );
    let mut expected = original;
    expected["receipt_id"] = json!(receipt.receipt_id());
    assert_eq!(receipt.payload(), &expected);
    assert_eq!(receipt.decision(), "fail");
    let again = seal(&serde_json::to_vec(receipt.payload()).unwrap()).unwrap();
    assert_eq!(again.payload(), receipt.payload());
}

#[test]
fn sealing_preserves_unicode_numbers_and_records_unverified_pass_without_authority() {
    let input = br#"{"message":"\u00e9\ud83d\ude80","decision":"pass","numbers":[-0.0,9007199254740993,1e-999,1.000]}"#;
    let receipt = seal(input).unwrap();
    assert_eq!(receipt.payload()["message"], "é🚀");
    assert_eq!(receipt.decision(), "pass");
    assert!(receipt.payload().get("execution_verified").is_none());
    assert!(receipt.payload().get("review_complete").is_none());
    assert_eq!(
        receipt.payload()["numbers"][1].as_u64(),
        Some(9007199254740993)
    );
    let roundtrip = parse_and_verify(&serde_json::to_vec(receipt.payload()).unwrap()).unwrap();
    assert_eq!(roundtrip.digest(), receipt.digest());
    let reordered = br#"{"numbers":[-0.0,9007199254740993,1e-999,1.000],"decision":"pass","message":"\u00e9\ud83d\ude80","receipt_digest":"ignored","receipt_id":17}"#;
    assert_eq!(seal(reordered).unwrap().digest(), receipt.digest());
}

#[test]
fn sealing_refuses_invalid_inputs_and_reserves_space_for_integrity_fields() {
    for input in [
        b"[]".as_slice(),
        b"{\"x\":1,\"x\":2}",
        b"{\"x\":1e999}",
        b"{\"x\":\"\xff\"}",
    ] {
        assert!(seal(input).is_err());
    }
    let padding = format!("{{\"padding\":\"{}\"}}", "x".repeat(MAX_BYTES - 20));
    assert!(padding.len() < MAX_BYTES);
    assert!(seal(padding.as_bytes()).is_err());
    let deep = format!("{{\"x\":{}0{}}}", "[".repeat(80), "]".repeat(80));
    assert!(seal(deep.as_bytes()).is_err());
}
