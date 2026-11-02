pub(crate) fn load_campaign_contract(
    root: &Path,
    relative_manifest: &str,
    python_test_nodeids: &HashMap<String, Vec<String>>,
    symbol_hashes: &HashMap<String, HashMap<String, String>>,
    candidate_paths: &BTreeSet<String>,
    inventory_from_source: bool,
) -> Result<CampaignContract, String> {
    let reader = capture::FsCampaignReader::new();
    legacy::load(
        root,
        relative_manifest,
        python_test_nodeids,
        symbol_hashes,
        candidate_paths,
        inventory_from_source,
        &reader,
    )
}
