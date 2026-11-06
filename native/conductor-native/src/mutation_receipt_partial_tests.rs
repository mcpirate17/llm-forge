use super::*;

#[test]
fn generated_pass_cannot_carry_partial_engine_diagnostics() {
    for key in ["engine_partial_progress", "engine_partial_progress_error"] {
        for value in [
            Value::Null,
            json!(false),
            json!({"schema_version": 1, "complete": false}),
        ] {
            for native in [false, true] {
                let mut overrides = json!({});
                if native {
                    overrides["runner_kind"] = json!("forge-native");
                }
                let mut payload = receipt(overrides);
                // The existing override helper uses null to remove a key.
                // Insert directly so null still tests diagnostic presence.
                payload.insert(key.into(), value.clone());
                let runner = RunnerState {
                    components: None,
                    error: None,
                    mutation_testing_sha256: None,
                };
                let found = crate::mutation_receipt::captured_generated_errors(
                    &payload,
                    &campaign(),
                    &runner,
                    |_| None,
                    |_| false,
                );
                assert!(
                    found
                        .iter()
                        .any(|row| row.kind == RejectionKind::SchemaError
                            && row.detail.contains("partial engine diagnostics")),
                    "{found:?}"
                );
            }
        }
    }
}

#[test]
fn stopped_generated_runs_keep_their_debt_classification_with_diagnostics() {
    for status in ["ERROR", "CANCELLED", "RATCHET_HELD"] {
        let found = rejections(json!({"status":status,
            "engine_partial_progress":{"schema_version":1,"complete":false},
            "engine_partial_progress_error":"incomplete report"}));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].kind, RejectionKind::NotPass);
        assert!(found[0].detail.contains(status));
    }
}
