//! Held live registry, manifest, patch and receipt capture for corpus audits.
use crate::{candidate_review_mutation::registry::Catalog, mutation_live_inputs::LiveInputs};
use anyhow::{ensure, Context, Result};
use conductor_native::mutation_evidence as engine;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
fn text(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).context("corpus input must be UTF-8")
}
fn normalize(path: &str) -> Result<String> {
    let path = engine::normalize_path(path).map_err(anyhow::Error::msg)?;
    crate::candidate_source::safe_path(path.as_bytes())?;
    Ok(path)
}
fn strings(value: &Value) -> Result<Vec<String>> {
    value
        .as_array()
        .context("expected array")?
        .iter()
        .map(|value| Ok(value.as_str().context("expected string")?.to_owned()))
        .collect()
}
pub(crate) fn registry(capture: &mut LiveInputs, path: &str) -> Result<Value> {
    let path = normalize(path)?;
    let data = capture
        .read(&path)?
        .context("mutation registry absent")?
        .to_vec();
    let directory = Path::new(&path)
        .parent()
        .unwrap_or(Path::new(""))
        .join("registry.d");
    let directory = directory
        .to_str()
        .context("registry directory must be UTF-8")?;
    let mut fragments = BTreeMap::new();
    for file in capture.json_files(directory)? {
        let bytes = capture
            .read(&file)?
            .context("registry fragment disappeared")?
            .to_vec();
        crate::tool_schema::parse_document_with_limit(text(&bytes)?, 2 * 1024 * 1024)?;
        fragments.insert(file, text(&bytes)?.to_owned());
    }
    crate::tool_schema::parse_document_with_limit(text(&data)?, 2 * 1024 * 1024)?;
    engine::load_registry_captured(text(&data)?, &fragments).map_err(anyhow::Error::msg)
}
fn references(manifest: &str, raw: &Value) -> Result<BTreeSet<String>> {
    let mut paths = BTreeSet::from([manifest.to_owned()]);
    for key in [
        "source_sha256",
        "test_sha256",
        "source_symbols",
        "test_scopes",
    ] {
        if let Some(rows) = raw[key].as_object() {
            for path in rows.keys() {
                paths.insert(normalize(path)?);
            }
        }
    }
    if let Some(rows) = raw["ranked_tests"].as_array() {
        for row in rows {
            if let Some(node) = row["nodeid"].as_str() {
                paths.insert(normalize(node.split("::").next().unwrap_or_default())?);
            }
        }
    }
    if let Some(rows) = raw["mutations"].as_array() {
        for row in rows {
            if let Some(patch) = row["patch_file"].as_str() {
                let joined = Path::new(manifest)
                    .parent()
                    .unwrap_or(Path::new(""))
                    .join(normalize(patch)?);
                paths.insert(normalize(
                    joined.to_str().context("patch path must be UTF-8")?,
                )?);
            }
        }
    }
    ensure!(
        paths.len() <= 4096,
        "campaign capture references exceed 4096"
    );
    Ok(paths)
}
fn campaign(
    capture: &mut LiveInputs,
    manifest: &str,
) -> Result<std::result::Result<(Value, Value), String>> {
    let manifest = normalize(manifest)?;
    let Some(bytes) = capture.read(&manifest)?.map(|bytes| bytes.to_vec()) else {
        return Ok(Err(format!("registered campaign is absent: {manifest}")));
    };
    let raw = match text(&bytes)
        .and_then(|text| crate::tool_schema::parse_document_with_limit(text, 2 * 1024 * 1024))
    {
        Ok(value) => value,
        Err(error) => return Ok(Err(format!("{error:#}"))),
    };
    let paths = match references(&manifest, &raw) {
        Ok(paths) => paths,
        Err(error) => return Ok(Err(format!("{error:#}"))),
    };
    let mut files = BTreeMap::from([(manifest.clone(), bytes)]);
    let cargo_path = match engine::cargo_value::declaration_path(&raw) {
        Ok(value) => value,
        Err(error) => return Ok(Err(error)),
    };
    let cargo = if let Some(declaration) = cargo_path {
        let sources = match crate::candidate_review_mutation::registry::cargo_sources(&raw) {
            Ok(value) => value,
            Err(error) => return Ok(Err(format!("{error:#}"))),
        };
        match capture.cargo_inventory(&declaration, &sources, &mut files) {
            Ok(held) => Some(held),
            Err(error) => return Ok(Err(format!("{error:#}"))),
        }
    } else {
        None
    };
    for path in paths {
        if files.contains_key(&path) {
            continue;
        }
        if let Some(bytes) = capture.read(&path)? {
            files.insert(path, bytes.to_vec());
        }
    }
    let contract = match &cargo {
        Some(held) => {
            engine::load_campaign_captured_with_cargo(&manifest, &files, &[], held.inventory())
        }
        None => engine::load_campaign_captured(&manifest, &files, &[]),
    };
    match contract {
        Ok(contract) => {
            if let Some(held) = cargo {
                capture.retain_cargo(held);
            }
            Ok(Ok((contract, raw)))
        }
        Err(error) => Ok(Err(error)),
    }
}
pub(crate) fn catalog(capture: &mut LiveInputs, registry: &Value) -> Result<Catalog> {
    let manifests = strings(&registry["manifests"])?;
    ensure!(manifests.len() <= 4096, "campaign inventory exceeds 4096");
    let mut result = Catalog {
        contracts: Vec::new(),
        raw_manifests: BTreeMap::new(),
        receipts: Vec::new(),
        has_native_receipts: false,
        broken: Vec::new(),
        malformed: Vec::new(),
    };
    for path in manifests {
        match campaign(capture, &path)? {
            Ok((contract, raw)) => {
                result.contracts.push(contract);
                result.raw_manifests.insert(path, raw);
            }
            Err(error) => result.broken.push(json!({"manifest":path,"error":error})),
        }
    }
    let dirs = strings(&registry["payload"]["receipt_directories"])?;
    ensure!(dirs.len() <= 64, "receipt directory inventory exceeds 64");
    let mut seen = BTreeSet::new();
    for directory in dirs {
        for path in capture.json_files(&normalize(&directory)?)? {
            if !seen.insert(path.clone()) {
                continue;
            }
            let bytes = capture
                .read(&path)?
                .context("receipt disappeared")?
                .to_vec();
            match text(&bytes).and_then(|text| {
                crate::tool_schema::parse_document_with_limit(text, 2 * 1024 * 1024)
            }) {
                Ok(value) if value.is_object() => {
                    result.has_native_receipts |= value
                        .as_object()
                        .is_some_and(conductor_native::mutation_native_runner::declared);
                    result.receipts.push(engine::CapturedReceipt {
                        path,
                        text: text(&bytes)?.to_owned(),
                    });
                }
                _ => result.malformed.push(path),
            }
        }
    }
    capture.verify()?;
    Ok(result)
}
