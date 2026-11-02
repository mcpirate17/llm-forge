//! Explicitly trusted captured facts, not an ambient filesystem or runtime identity claim.
use super::*;
use cap_std::fs::Dir;
use serde::Deserialize;
use std::collections::BTreeMap;
const MAX_BYTES: usize = 32 * 1024 * 1024;

pub fn parse_request(text: &str) -> Result<Value, String> {
    if text.len() > MAX_BYTES {
        return Err("mutation request exceeds 32 MiB".into());
    }
    let value: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
    admit(&value)?;
    Ok(value)
}
pub(super) fn admit(value: &Value) -> Result<(), String> {
    measure(value).map(|_| ())
}
pub(super) fn measure(value: &Value) -> Result<usize, String> {
    let mut pending = vec![(value, 0usize)];
    let mut nodes = 0usize;
    let mut bytes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if depth > 64 || nodes > 200_000 {
            return Err("mutation JSON exceeds depth/node limits".into());
        }
        let children = match value {
            Value::Array(rows) => rows.len(),
            Value::Object(map) => map.len(),
            _ => 0,
        };
        if children + nodes + pending.len() > 200_000 {
            return Err("mutation JSON exceeds node limit".into());
        }
        match value {
            Value::String(text) => bytes = bytes.saturating_add(text.len()),
            Value::Number(number) => bytes = bytes.saturating_add(number.as_str().len()),
            Value::Array(rows) => pending.extend(rows.iter().map(|row| (row, depth + 1))),
            Value::Object(map) => {
                for (key, row) in map {
                    bytes = bytes.saturating_add(key.len());
                    pending.push((row, depth + 1));
                }
            }
            _ => bytes = bytes.saturating_add(32),
        }
        if bytes > MAX_BYTES {
            return Err("mutation JSON exceeds 32 MiB work budget".into());
        }
    }
    Ok(bytes)
}
pub(crate) fn read_bounded(path: &Path, maximum: usize) -> Result<Option<Vec<u8>>, String> {
    let parent = path.parent().ok_or("mutation input has no parent")?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("mutation input name must be UTF-8")?;
    let root = match Dir::open_ambient_dir(parent, cap_std::ambient_authority()) {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    crate::candidate_cargo_lint::read_optional(&root, name, maximum)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapturedGenerated {
    campaign: CampaignContract,
    receipt: Value,
    runner: RunnerState,
    binding_sha256: BTreeMap<String, String>,
    lineage: Option<Value>,
}
/// No filesystem, subprocess or interpreter access. The caller must have
/// authenticated the manifest-derived contract, source drift, runner map, file
/// hashes and lineage from its held capture. These facts are not attestations.
/// Legacy/reviewed anchor receipts deliberately require the ambient API instead.
pub fn validate_generated_captured(request: &Value) -> Result<Value, String> {
    admit(request)?;
    let input: CapturedGenerated =
        serde_json::from_value(request.clone()).map_err(|error| error.to_string())?;
    if !input.campaign.generated {
        return Err("captured verifier supports generated campaigns only; legacy anchors need separate verified Git input".into());
    }
    let components = input
        .runner
        .components
        .as_ref()
        .and_then(Value::as_object)
        .filter(|map| !map.is_empty())
        .ok_or("captured runner component map missing")?;
    if components
        .values()
        .any(|hash| hash.as_str().is_none_or(|hash| !sha256(hash)))
        || input
            .runner
            .mutation_testing_sha256
            .as_deref()
            .is_none_or(|hash| !sha256(hash))
    {
        return Err("captured runner hashes malformed or missing".into());
    }
    for hash in input.binding_sha256.values() {
        if !sha256(hash) {
            return Err("captured binding hash is malformed".into());
        }
    }
    let expanded = crate::receipt_slim::expand_receipt(&input.receipt)?;
    admit(&expanded)?;
    let payload = expanded
        .as_object()
        .ok_or("captured receipt must be an object")?;
    let errors = crate::mutation_receipt::captured_generated_errors(
        payload,
        &input.campaign,
        &input.runner,
        |path| input.binding_sha256.get(path).cloned(),
        |recorded| {
            input.lineage.as_ref().is_some_and(|lineage| {
                crate::mutation_receipt::lineage_payload_accepts(recorded, lineage)
            })
        },
    );
    let result = json!({"valid":errors.is_empty(),"errors":errors.iter().map(|error|
        json!({"kind":error.kind.as_str(),"detail":error.detail})).collect::<Vec<_>>(),
        "scope":"trusted-captured-generated-inputs","execution_verified":false,"approval":false});
    admit(&result)?;
    Ok(result)
}
fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
