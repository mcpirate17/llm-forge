use super::*;

fn state() -> Value {
    json!({"schema_version":1,"last_updated":"2026-01-01T00:00:00+00:00",
        "standing_mandates":["M-1: preserve approval"],"active_headings":["Current task"],"active_claims":[]})
}
fn envelope() -> Envelope {
    prepare(
        Preparation {
            task: "Continue 🦀",
            paths: vec!["src/main.rs".into()],
            runtime: BTreeMap::new(),
            context: "Original context",
            parent: None,
        },
        state(),
        instant::parse("2026-01-01T00:00:01Z").unwrap(),
    )
    .unwrap()
}

#[test]
fn canonical_integrity_and_state_fingerprint_are_stable_and_independent() {
    let e = envelope();
    let payload = e.payload().unwrap();
    let encoded = serde_json::to_string(&payload).unwrap();
    let decoded = Envelope::from_value(serde_json::from_str(&encoded).unwrap()).unwrap();
    assert_eq!(decoded.payload().unwrap(), payload);
    let mut reordered = state();
    reordered["last_updated"] = json!("2026-01-01T00:00:10+00:00");
    assert_eq!(
        state_fingerprint(&state()).unwrap(),
        state_fingerprint(&reordered).unwrap()
    );
    reordered["standing_mandates"] = json!(["M-2"]);
    assert_ne!(
        state_fingerprint(&state()).unwrap(),
        state_fingerprint(&reordered).unwrap()
    );
    let mut bad = payload;
    bad["context"] = json!("modified");
    assert!(Envelope::from_value(bad)
        .unwrap_err()
        .to_string()
        .contains("integrity"));
    assert_eq!(
        digest(&json!({"b":2,"a":1})),
        "43258cff783fe7036d8a43033f830adfc60ec037382473548ac742b888292777"
    );
}

#[test]
fn invalid_envelope_states_are_rejected_even_with_a_recomputed_integrity_hash() {
    let original = envelope();
    for (key, value) in [
        ("schema_version", json!(2)),
        ("task", json!(" ")),
        ("task", json!("a".repeat(501))),
        ("parent_handoff_id", json!(original.handoff_id)),
        ("paths", json!(["../escape"])),
        ("runtime", json!({"nested":{}})),
        ("runtime", json!({"float":1.5})),
        ("context", json!("a".repeat(3501))),
        ("created_at", json!("2026-01-01T00:00:01")),
        ("status", json!("active")),
        ("activated_at", json!("2026-01-01T00:00:02Z")),
        ("active_state_digest", json!("0".repeat(64))),
        ("unknown", json!(true)),
    ] {
        let mut malformed = serde_json::to_value(&original).unwrap();
        malformed[key] = value;
        malformed["integrity_sha256"] = json!(digest(&malformed));
        assert!(
            Envelope::from_value(malformed).is_err(),
            "accepted invalid {key}"
        );
    }
}

#[test]
fn redaction_and_character_caps_preserve_utf8() {
    let text = "sk-1234567890123456 Authorization: Bearer secret\napi_key=other; next";
    let safe = redact(text);
    assert!(!safe.contains("1234567890123456"));
    assert!(!safe.contains("secret"));
    assert!(!safe.contains("other"));
    assert_eq!(safe.matches("[REDACTED]").count(), 3);
    let text = "🦀".repeat(4000);
    let fitted = fit_text(&text, 3500);
    assert_eq!(fitted.chars().count(), 3500);
    assert!(fitted.ends_with('…'));
    assert!(envelope().live_context().contains("non-authoritative"));
}
