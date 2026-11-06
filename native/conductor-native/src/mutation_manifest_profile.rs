use serde_json::{Map, Value};

pub fn execution_profile(
    generator: &Map<String, Value>,
    language: &str,
    engine: &str,
) -> Result<Option<String>, String> {
    let Some(raw) = generator.get("options") else {
        return Ok(None);
    };
    let options = raw
        .as_object()
        .ok_or("generator.options must be an object")?;
    crate::mutation_workspace_selector::parse(options)?;
    let Some(value) = options.get("execution_profile") else {
        return Ok(None);
    };
    let profile = value
        .as_str()
        .ok_or("generator.options.execution_profile must be a string")?;
    if !crate::mutation_workspace_selector::is_workspace(Some(profile))
        || language != "rust"
        || engine != "cargo-mutants"
    {
        return Err("unsupported generator execution profile for campaign".into());
    }
    Ok(Some(profile.to_owned()))
}

pub(super) fn generated_test_scopes(
    tests: &Map<String, Value>,
    selected: bool,
) -> Map<String, Value> {
    // Selected targets leave these pins as anchors, not complete per-file coverage.
    let scope = if selected {
        serde_json::json!({"mode": "partial", "selection": "workspace_selector"})
    } else {
        serde_json::json!({"mode": "complete", "selection": "test_argv"})
    };
    tests
        .keys()
        .map(|path| (path.clone(), scope.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload() -> Map<String, Value> {
        json!({"campaign_id":"fixture", "title":"fixture", "language":"rust",
            "mutation_engine":"cargo-mutants", "generator":{"source":["src/a.rs"],
                "run_timeout_seconds":60,"options":{"package":"fixture"}},
            "source_sha256":{"src/a.rs":"a".repeat(64)},
            "test_sha256":{"tests/a.rs":"b".repeat(64)},
            "test_argv":["cargo","test"], "survivor_baseline":[]})
        .as_object()
        .unwrap()
        .clone()
    }
    fn contract(
        payload: &Map<String, Value>,
        engine: &str,
    ) -> Result<super::super::CampaignContract, String> {
        super::super::generated_campaign_contract_with(
            payload,
            b"manifest".to_vec(),
            "campaigns/a.json",
            engine,
            |_| None,
        )
    }
    #[test]
    fn verified_manifest_selects_profile_and_old_shape_stays_unchanged() {
        let mut payload = payload();
        let old = contract(&payload, "cargo-mutants").unwrap();
        let old = serde_json::to_value(old).unwrap();
        assert!(old.get("execution_profile").is_none());
        assert_eq!(old["test_scopes"]["tests/a.rs"]["mode"], "complete");
        payload["generator"]["options"]["execution_profile"] = json!("cargo-workspace-v2");
        let selected = contract(&payload, "cargo-mutants").unwrap();
        assert_eq!(
            selected.execution_profile.as_deref(),
            Some("cargo-workspace-v2")
        );
        assert_eq!(
            serde_json::to_value(selected).unwrap()["execution_profile"],
            "cargo-workspace-v2"
        );
    }
    #[test]
    fn v3_selector_is_bound_and_v2_shape_remains_unchanged() {
        let mut payload = payload();
        payload["generator"]["options"]["execution_profile"] = json!("cargo-workspace-v3");
        payload["generator"]["options"]["workspace_selector"] =
            json!({"schema_version":1,"target":{"kind":"lib"},"filter":"module::tests::"});
        let selected = contract(&payload, "cargo-mutants").unwrap();
        let encoded = serde_json::to_value(selected).unwrap();
        assert_eq!(encoded["workspace_selector"]["target"]["kind"], "lib");
        assert_eq!(encoded["test_scopes"]["tests/a.rs"]["mode"], "partial");
        assert_eq!(
            encoded["test_scopes"]["tests/a.rs"]["selection"],
            "workspace_selector"
        );
        payload["generator"]["options"]["execution_profile"] = json!("cargo-workspace-v2");
        assert!(contract(&payload, "cargo-mutants").is_err());
    }
    #[test]
    fn malformed_unknown_or_wrong_engine_profiles_refuse() {
        for bad in [json!(null), json!(true), json!("unknown")] {
            let mut value = payload();
            value["generator"]["options"]["execution_profile"] = bad;
            assert!(contract(&value, "cargo-mutants").is_err());
        }
        let mut value = payload();
        value["generator"]["options"]["execution_profile"] = json!("cargo-workspace-v2");
        assert!(contract(&value, "fest").is_err());
        value["language"] = json!("python");
        assert!(contract(&value, "cargo-mutants").is_err());
        value["language"] = json!("rust");
        value.insert("generated_value".into(), json!({}));
        assert!(contract(&value, "cargo-mutants").is_err());
    }
}
