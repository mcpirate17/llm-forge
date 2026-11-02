use super::*;
pub(crate) fn load_campaign_value(request: &Value) -> Result<Value, String> {
    let request: CampaignLoadRequest =
        serde_json::from_value(request.clone()).map_err(|error| error.to_string())?;
    let root = lexical_absolute(Path::new(&request.repo_root))?;
    let manifest = safe_relative(&request.manifest_path, "campaign manifest")?;
    let campaign = load_campaign_contract(
        &root,
        &manifest,
        &request.python_test_nodeids,
        &request.symbol_hashes,
        &BTreeSet::new(),
        true,
    )?;
    serde_json::to_value(campaign).map_err(|error| error.to_string())
}
pub(crate) fn load_registry_value(request: &Value) -> Result<Value, String> {
    let request: RegistryLoadRequest =
        serde_json::from_value(request.clone()).map_err(|error| error.to_string())?;
    let root = lexical_absolute(Path::new(&request.repo_root))?;
    let registry = safe_relative(&request.registry_path, "mutation registry")?;
    let (mut payload, manifests) = load_registry_manifest_paths(
        &root,
        &root.join(registry),
        Some(&request.canonical_test_patterns),
    )?;
    payload.insert(
        "campaigns".into(),
        Value::Array(
            manifests
                .into_iter()
                .map(|manifest| json!({"manifest":manifest}))
                .collect(),
        ),
    );
    Ok(Value::Object(payload))
}
