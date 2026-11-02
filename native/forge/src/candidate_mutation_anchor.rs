//! Historical receipt provenance over immutable Git blobs, never caller assertions.
use super::*;
pub(super) fn prove(
    git: &mut GitInputs<'_>,
    path: &str,
    bytes: &[u8],
    contract: &Value,
    baseline: Option<&crate::candidate_execution_workspace::MutationGrandfather>,
) -> Result<()> {
    let baseline = baseline
        .context("legacy receipt requires explicitly pinned legacy grandfather baseline")?;
    ensure!(
        definitions::legacy(baseline),
        "legacy receipt requires exact legacy grandfather pins"
    );
    ensure!(
        path.starts_with("conductor/mutation_campaigns/receipts/") && path.ends_with(".json"),
        "legacy receipt path is outside the anchored receipt directory"
    );
    git.anchor(definitions::ANCHOR_COMMIT, definitions::ANCHOR_TREE)?;
    let registry = conductor_native::mutation_evidence::legacy_anchor_registry_path();
    let raw = git.read(definitions::ANCHOR_TREE, &registry)?;
    let payload =
        crate::tool_schema::parse_document_with_limit(std::str::from_utf8(raw)?, 1024 * 1024)?;
    let rows = payload["campaigns"]
        .as_array()
        .context("legacy receipt anchor registry is malformed")?;
    let manifest = contract["manifest"]
        .as_str()
        .context("campaign manifest unavailable")?;
    ensure!(
        rows.iter()
            .any(|row| row["manifest"].as_str() == Some(manifest)),
        "legacy receipt campaign was not registered at the anchor"
    );
    let anchored_manifest = git.read(definitions::ANCHOR_TREE, manifest)?;
    ensure!(
        Some(hex(anchored_manifest).as_str()) == contract["manifest_sha256"].as_str(),
        "legacy receipt manifest differs from anchor"
    );
    let entry = git
        .entries(definitions::ANCHOR_TREE)?
        .get(path)
        .context("legacy receipt anchor blob unavailable")?;
    ensure!(
        entry.mode == "100644" && entry.object_type == "blob",
        "legacy receipt anchor must be a regular100644 blob"
    );
    ensure!(
        git.read(definitions::ANCHOR_TREE, path)? == bytes,
        "legacy receipt bytes differ from anchored blob"
    );
    Ok(())
}
