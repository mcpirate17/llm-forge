//! Held ordinary Cargo discovery inputs; no compiler/runtime closure claim.
use crate::candidate_inputs::{BoundFile, DirectoryBinding};
use anyhow::{ensure, Result};
use conductor_native::{
    mutation_cargo_supporting, mutation_cargo_supporting_binding::SupportingInputs,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub(crate) struct Held {
    named: PathBuf,
    root: PathBuf,
    directory: DirectoryBinding,
    files: Vec<(String, BoundFile)>,
    inventory: SupportingInputs,
    max_files: usize,
    required: BTreeMap<String, String>,
}
impl Held {
    pub(crate) fn capture(
        named: &Path,
        max_files: usize,
        required: &BTreeMap<String, String>,
    ) -> Result<Self> {
        ensure!(
            (1..=4096).contains(&max_files),
            "invalid supporting-input file budget"
        );
        let root = named.canonicalize()?;
        let directory = DirectoryBinding::repository(&root, None)?;
        let (entries, files) = crate::mutation_cargo_capture::read_entries(&root, "", max_files)?;
        let inventory =
            mutation_cargo_supporting::build(&entries, required).map_err(anyhow::Error::msg)?;
        let held = Self {
            named: named.to_owned(),
            root,
            directory,
            files,
            inventory,
            max_files,
            required: required.clone(),
        };
        held.verify()?;
        Ok(held)
    }
    pub(crate) fn capture_peer(&self, root: &Path) -> Result<Self> {
        Self::capture(root, self.max_files, &self.required)
    }
    pub(crate) fn inventory(&self) -> &SupportingInputs {
        &self.inventory
    }
    pub(crate) fn verify(&self) -> Result<()> {
        ensure!(
            self.named.canonicalize()? == self.root,
            "named supporting-input root changed"
        );
        self.directory.verify()?;
        for (_, file) in &self.files {
            file.verify()?;
        }
        let (entries, _) =
            crate::mutation_cargo_capture::read_entries(&self.root, "", self.max_files)?;
        let current = mutation_cargo_supporting::build(&entries, &self.required)
            .map_err(anyhow::Error::msg)?;
        ensure!(current == self.inventory, "Cargo supporting inputs changed");
        self.directory.verify()
    }
}

/// A stale ordinary contract rejects only native rows; Python compatibility is separate.
pub(crate) fn stale_native_contract(
    contract: &serde_json::Value,
    row: &serde_json::Value,
    ordinary: bool,
) -> bool {
    ordinary
        && contract["source_drifted"] == true
        && row
            .as_object()
            .is_some_and(conductor_native::mutation_native_runner::declared)
}

fn eligible_contract(contract: &serde_json::Value) -> bool {
    contract["source_drifted"] != true
        && contract["generated"] == true
        && contract["generated_value"].is_null()
        && (contract["mutation_engine"] == "cargo-mutants"
            || confined_fest(contract)
            || confined_mull(contract))
}

/// Only a profile from the verified campaign selects Fest's snapshot hash domain.
pub(crate) fn confined_fest(contract: &serde_json::Value) -> bool {
    contract["generated"] == true
        && contract["generated_value"].is_null()
        && contract["language"] == "python"
        && contract["mutation_engine"] == "fest"
        && contract["execution_profile"] == "fest-confined-v1"
}

pub(crate) fn confined_mull(contract: &serde_json::Value) -> bool {
    contract["generated"] == true
        && contract["generated_value"].is_null()
        && contract["language"] == "cpp"
        && contract["mutation_engine"] == "mull"
        && contract["execution_profile"] == "mull-confined-v1"
}

fn eligible_receipt(
    receipt: &conductor_native::mutation_evidence::CapturedReceipt,
    contracts: &[serde_json::Value],
    native_fingerprint: &mut impl FnMut() -> Result<Option<String>>,
    installed: Option<&conductor_native::mutation_native_runner::InstalledIdentity>,
) -> Result<Option<String>> {
    use conductor_native::mutation_native_runner as native;
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&receipt.text) else {
        return Ok(None);
    };
    let Ok(expanded) = conductor_native::receipt_slim::expand_receipt(&value) else {
        return Ok(None);
    };
    let Some(object) = expanded.as_object().filter(|row| native::declared(row)) else {
        return Ok(None);
    };
    if !matches!(
        object.get("status").and_then(serde_json::Value::as_str),
        Some("PASS" | "RATCHET_HELD")
    ) || object
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
        != Some("llm.mutation-testing.receipt.v3")
        || object.get("mutants_are_generated") != Some(&serde_json::Value::Bool(true))
        || native::owned_cleanup_error(object).is_some()
    {
        return Ok(None);
    }
    let Some(identity) = object.get("supporting_inputs") else {
        return Ok(None);
    };
    let Ok(identity) = serde_json::from_value::<
        conductor_native::mutation_cargo_supporting_binding::SupportingInputs,
    >(identity.clone()) else {
        return Ok(None);
    };
    if identity.validate().is_err() {
        return Ok(None);
    }
    let Some(id) = object
        .get("campaign_id")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(None);
    };
    let Some(contract) = contracts
        .iter()
        .find(|c| c["campaign_id"].as_str() == Some(id))
    else {
        return Ok(None);
    };
    if [
        "manifest",
        "manifest_sha256",
        "source_sha256",
        "test_sha256",
    ]
    .iter()
    .any(|key| object.get(*key) != contract.get(*key))
    {
        return Ok(None);
    }
    if !eligible_contract(contract) {
        return Ok(None);
    }
    let current = native_fingerprint()?;
    if !native::errors_with_installed(object, |_| current.clone(), |_| false, installed).is_empty()
    {
        return Ok(None);
    }
    Ok(Some(id.to_owned()))
}

/// Only the currently admitted ordinary root Cargo profile uses this binding.
pub(crate) fn required_for(
    contracts: &[serde_json::Value],
    receipts: &[conductor_native::mutation_evidence::CapturedReceipt],
    mut native_fingerprint: impl FnMut() -> Result<Option<String>>,
    installed: Option<&conductor_native::mutation_native_runner::InstalledIdentity>,
) -> Result<Option<BTreeMap<String, String>>> {
    if !contracts.iter().any(eligible_contract) {
        return Ok(None);
    }
    let mut native_ids = std::collections::BTreeSet::new();
    for receipt in receipts {
        if let Some(id) = eligible_receipt(receipt, contracts, &mut native_fingerprint, installed)?
        {
            native_ids.insert(id);
        }
    }
    let mut required = BTreeMap::new();
    let mut found = false;
    for contract in contracts {
        if !contract["execution_profile"].is_null() {
            continue;
        }
        if !contract["campaign_id"]
            .as_str()
            .is_some_and(|id| native_ids.contains(id))
            || !eligible_contract(contract)
        {
            continue;
        }
        found = true;
        for field in ["source_sha256", "test_sha256"] {
            let pins = contract[field]
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("missing {field} pins"))?;
            for (path, value) in pins {
                let digest = value
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("invalid {field} pin"))?;
                required.insert(path.clone(), digest.to_owned());
            }
        }
    }
    Ok(found.then_some(required))
}

pub(crate) struct WorkspaceRequired {
    pub(crate) pins: BTreeMap<String, String>,
    pub(crate) manifest: String,
}
/// Identify complete-snapshot profiles from verified contracts, never receipt fields.
pub(crate) fn workspace_required_for(
    contracts: &[serde_json::Value],
    receipts: &[conductor_native::mutation_evidence::CapturedReceipt],
    mut native_fingerprint: impl FnMut() -> Result<Option<String>>,
    installed: Option<&conductor_native::mutation_native_runner::InstalledIdentity>,
) -> Result<BTreeMap<String, Result<WorkspaceRequired, String>>> {
    if !contracts.iter().any(eligible_contract) {
        return Ok(BTreeMap::new());
    }
    let mut eligible = std::collections::BTreeSet::new();
    for receipt in receipts {
        if let Some(id) = eligible_receipt(receipt, contracts, &mut native_fingerprint, installed)?
        {
            eligible.insert(id);
        }
    }
    let mut result = BTreeMap::new();
    for contract in contracts {
        let Some(id) = contract["campaign_id"].as_str() else {
            continue;
        };
        if !conductor_native::mutation_cargo_supporting_binding::uses_snapshot_inputs(
            contract["execution_profile"].as_str(),
        ) || !eligible.contains(id)
        {
            continue;
        }
        ensure!(
            result
                .insert(id.to_owned(), workspace_pins(contract))
                .is_none(),
            "duplicate workspace campaign ID"
        );
    }
    Ok(result)
}
pub(crate) fn capture_workspace_profiles(
    required: BTreeMap<String, Result<WorkspaceRequired, String>>,
    mut capture: impl FnMut(
        &WorkspaceRequired,
    ) -> Result<
        conductor_native::mutation_cargo_supporting_binding::SupportingInputs,
    >,
) -> BTreeMap<
    String,
    Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs, String>,
> {
    required
        .into_iter()
        .map(|(id, pins)| {
            let result = pins.and_then(|pins| capture(&pins).map_err(|error| format!("{error:#}")));
            (id, result)
        })
        .collect()
}
fn workspace_pins(contract: &serde_json::Value) -> Result<WorkspaceRequired, String> {
    let mut pins = BTreeMap::new();
    for field in ["source_sha256", "test_sha256"] {
        let map = contract[field]
            .as_object()
            .ok_or_else(|| format!("missing {field} pins"))?;
        for (path, value) in map {
            let hash = value
                .as_str()
                .ok_or_else(|| format!("invalid {field} pin"))?;
            if let Some(old) = pins.insert(path.clone(), hash.to_owned()) {
                if old != hash {
                    return Err(format!(
                        "conflicting workspace source and test pins: {path}"
                    ));
                }
            }
        }
    }
    let manifest = contract["manifest"]
        .as_str()
        .ok_or("workspace campaign manifest is missing")?
        .to_owned();
    Ok(WorkspaceRequired { pins, manifest })
}

#[cfg(test)]
#[path = "mutation_cargo_supporting_tests.rs"]
mod tests;
