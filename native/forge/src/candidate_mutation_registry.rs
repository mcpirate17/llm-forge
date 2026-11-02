//! Exhaustive candidate-tree registry/manifest/receipt capture; no ambient validator.
use super::*;
use conductor_native::mutation_evidence as engine;
pub(super) struct Registry {
    pub path: String,
    pub payload: Value,
    pub manifests: Vec<String>,
}
pub(super) struct Catalog {
    pub contracts: Vec<Value>,
    pub raw_manifests: BTreeMap<String, Value>,
    pub receipts: Vec<engine::CapturedReceipt>,
    pub broken: Vec<Value>,
    pub malformed: Vec<String>,
}
pub(super) fn registry(
    capture: &mut Capture<'_>,
    config: Option<&conductor_native::project_config::ProjectConfig>,
) -> Result<Option<Registry>> {
    let path = conductor_native::project_paths::mutation_registry_from_config(config)
        .map_err(anyhow::Error::msg)?;
    if !capture.inputs.entries.contains_key(&path) {
        capture.inputs.absent(&path)?;
        return Ok(None);
    }
    let text = capture.text(&path, 2 * 1024 * 1024)?;
    let directory = Path::new(&path)
        .parent()
        .unwrap_or(Path::new(""))
        .join("registry.d");
    let fragments = direct_json(
        &capture.inputs.entries,
        directory
            .to_str()
            .context("fragment directory must be UTF-8")?,
    )?;
    ensure!(
        fragments.len() <= 4096,
        "mutation registry fragments exceed4096"
    );
    let mut values = BTreeMap::new();
    for file in fragments {
        let text = capture.text(&file, 2 * 1024 * 1024)?;
        crate::tool_schema::parse_document_with_limit(&text, 2 * 1024 * 1024)?;
        values.insert(file, text);
    }
    // Strict duplicate-key JSON is checked in Forge before the shared native schema parser.
    crate::tool_schema::parse_document_with_limit(&text, 2 * 1024 * 1024)?;
    let selected = engine::load_registry_captured(&text, &values).map_err(anyhow::Error::msg)?;
    let manifests = strings(&selected["manifests"])?;
    ensure!(
        manifests.len() <= 4096,
        "mutation campaign inventory exceeds4096"
    );
    Ok(Some(Registry {
        path,
        payload: selected["payload"].clone(),
        manifests,
    }))
}
pub(super) fn catalog(
    capture: &mut Capture<'_>,
    registry: &Registry,
    selected: &[String],
) -> Result<Catalog> {
    let mut result = Catalog {
        contracts: Vec::new(),
        raw_manifests: BTreeMap::new(),
        receipts: Vec::new(),
        broken: Vec::new(),
        malformed: Vec::new(),
    };
    let mut contract_bytes = 0usize;
    for path in &registry.manifests {
        match campaign(capture, path, selected) {
            Ok((contract, raw)) => {
                contract_bytes = contract_bytes
                    .checked_add(crate::json_canon::encoded_size(
                        &contract,
                        32 * 1024 * 1024,
                    )?)
                    .context("mutation contract bytes overflow")?;
                ensure!(
                    contract_bytes <= 32 * 1024 * 1024,
                    "mutation contract amplification exceeds32MiB"
                );
                result.contracts.push(contract);
                result.raw_manifests.insert(path.clone(), raw);
            }
            Err(error) => {
                if capture.failed {
                    return Err(error);
                }
                result
                    .broken
                    .push(json!({"manifest":path,"error":format!("{error:#}")}));
            }
        }
    }
    let dirs = strings(&registry.payload["receipt_directories"])?;
    ensure!(dirs.len() <= 64, "mutation receipt directories exceed64");
    let mut seen = BTreeSet::new();
    for dir in dirs {
        let dir = engine::normalize_path(&dir).map_err(anyhow::Error::msg)?;
        for path in direct_json(&capture.inputs.entries, &dir)? {
            if !seen.insert(path.clone()) {
                continue;
            }
            ensure!(seen.len() <= 4096, "mutation receipt inventory exceeds4096");
            let bytes = capture.read(&path, 2 * 1024 * 1024)?;
            match std::str::from_utf8(&bytes)
                .map_err(anyhow::Error::from)
                .and_then(|text| {
                    crate::tool_schema::parse_document_with_limit(text, 2 * 1024 * 1024)
                        .map(|value| (text, value))
                }) {
                Ok((text, value)) if value.is_object() => {
                    result.receipts.push(engine::CapturedReceipt {
                        path,
                        text: text.into(),
                    })
                }
                Ok(_) => result
                    .malformed
                    .push(format!("{path}: receipt must be a JSON object")),
                Err(error) => result.malformed.push(format!("{path}: {error:#}")),
            }
        }
    }
    Ok(result)
}
fn campaign(capture: &mut Capture<'_>, path: &str, selected: &[String]) -> Result<(Value, Value)> {
    let path = engine::normalize_path(path).map_err(anyhow::Error::msg)?;
    if !capture.inputs.entries.contains_key(&path) {
        capture.inputs.absent(&path)?;
        anyhow::bail!("registered campaign is absent: {path}");
    }
    let raw = capture.json(&path)?;
    let mut wanted = BTreeSet::from([path.clone()]);
    for key in [
        "source_sha256",
        "test_sha256",
        "source_symbols",
        "test_scopes",
    ] {
        if let Some(rows) = raw[key].as_object() {
            for path in rows.keys() {
                wanted.insert(normalize(path)?);
            }
        }
    }
    if let Some(rows) = raw["ranked_tests"].as_array() {
        for row in rows {
            if let Some(node) = row["nodeid"].as_str() {
                wanted.insert(normalize(node.split("::").next().unwrap_or_default())?);
            }
        }
    }
    if let Some(rows) = raw["mutations"].as_array() {
        for row in rows {
            if let Some(patch) = row["patch_file"].as_str() {
                let patch = normalize(patch)?;
                let joined = Path::new(&path)
                    .parent()
                    .unwrap_or(Path::new(""))
                    .join(patch);
                wanted.insert(normalize(
                    joined
                        .to_str()
                        .context("mutation patch path must be UTF-8")?,
                )?);
            }
        }
    }
    ensure!(
        wanted.len() <= 4096,
        "campaign capture references exceed4096"
    );
    let mut files = BTreeMap::new();
    for input in wanted {
        if capture.inputs.entries.contains_key(&input) {
            files.insert(input.clone(), capture.read(&input, 2 * 1024 * 1024)?);
        } else {
            capture.inputs.absent(&input)?;
        }
    }
    let contract =
        engine::load_campaign_captured(&path, &files, selected).map_err(anyhow::Error::msg)?;
    Ok((contract, raw))
}
fn normalize(path: &str) -> Result<String> {
    let value = engine::normalize_path(path).map_err(anyhow::Error::msg)?;
    crate::candidate_source::safe_path(value.as_bytes())?;
    Ok(value)
}
fn direct_json(
    entries: &BTreeMap<String, crate::candidate_source::TreeEntry>,
    directory: &str,
) -> Result<Vec<String>> {
    ensure!(
        !entries.contains_key(directory),
        "mutation JSON inventory directory is a tracked non-directory"
    );
    let prefix = format!("{directory}/");
    let mut result = Vec::new();
    for path in entries.keys().filter(|path| path.starts_with(&prefix)) {
        let tail = &path[prefix.len()..];
        if let Some((first, _)) = tail.split_once('/') {
            ensure!(
                !Path::new(first)
                    .extension()
                    .is_some_and(|ext| ext == "json"),
                "mutation JSON entry is a directory"
            );
        } else if Path::new(tail).extension().is_some_and(|ext| ext == "json") {
            result.push(path.clone());
        }
    }
    Ok(result)
}
