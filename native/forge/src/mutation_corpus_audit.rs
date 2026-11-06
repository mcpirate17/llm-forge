//! Native corpus observations over a held immutable candidate and runner bundle.
use crate::{
    candidate_mutation_runner::RunnerBundle,
    candidate_review_mutation::{inputs::Capture, registry::Catalog},
    mutation_corpus_patch::{Checker, PatchObservation},
};
use anyhow::{Context, Result};
use conductor_native::{
    mutation_corpus_delta as delta, mutation_corpus_receipts as receipts,
    mutation_corpus_territory::{self as territory, CampaignScope},
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(crate) trait CorpusInputs {
    fn hash(&mut self, path: &str) -> Result<Option<String>>;
    fn present(&mut self, path: &str) -> Result<bool>;
    fn verify_inputs(&self) -> Result<()>;
    fn native_inventory_absent(&mut self) -> Result<bool>;
    fn supporting_inventory(
        &mut self,
        required: &BTreeMap<String, String>,
    ) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs>;
    fn workspace_inventory(
        &mut self,
        required: &crate::mutation_cargo_supporting::WorkspaceRequired,
    ) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs>;
    fn workspace_receipt_root(&mut self) -> Result<String>;
}
impl CorpusInputs for Capture<'_> {
    fn hash(&mut self, path: &str) -> Result<Option<String>> {
        if path == conductor_native::mutation_native_runner::FINGERPRINT_KEY {
            return self.native_source_fingerprint();
        }
        self.optional_hash(path)
    }
    fn present(&mut self, path: &str) -> Result<bool> {
        crate::candidate_source::safe_path(path.as_bytes())?;
        if self.inputs.entries.contains_key(path) {
            Ok(true)
        } else {
            self.inputs.absent(path)?;
            Ok(false)
        }
    }
    fn verify_inputs(&self) -> Result<()> {
        self.verify()
    }
    fn native_inventory_absent(&mut self) -> Result<bool> {
        self.native_inventory_absent()
    }
    fn supporting_inventory(
        &mut self,
        required: &BTreeMap<String, String>,
    ) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs> {
        self.supporting_inventory(required)
    }
    fn workspace_inventory(
        &mut self,
        required: &crate::mutation_cargo_supporting::WorkspaceRequired,
    ) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs> {
        self.workspace_inventory(required)
    }
    fn workspace_receipt_root(&mut self) -> Result<String> {
        let config = self.config()?;
        conductor_native::project_paths::mutation_receipt_root_from_config(config.as_ref())
            .map_err(anyhow::Error::msg)
    }
}
impl CorpusInputs for crate::mutation_live_inputs::LiveInputs {
    fn hash(&mut self, path: &str) -> Result<Option<String>> {
        if path == conductor_native::mutation_native_runner::FINGERPRINT_KEY {
            return self.native_source_fingerprint();
        }
        use sha2::{Digest, Sha256};
        Ok(self
            .read(path)?
            .map(|bytes| format!("{:x}", Sha256::digest(bytes))))
    }
    fn present(&mut self, path: &str) -> Result<bool> {
        Ok(self.read(path)?.is_some())
    }
    fn verify_inputs(&self) -> Result<()> {
        self.verify()
    }
    fn native_inventory_absent(&mut self) -> Result<bool> {
        self.native_inventory_absent()
    }
    fn supporting_inventory(
        &mut self,
        required: &BTreeMap<String, String>,
    ) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs> {
        self.supporting_inventory(required)
    }
    fn workspace_inventory(
        &mut self,
        required: &crate::mutation_cargo_supporting::WorkspaceRequired,
    ) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs> {
        self.workspace_inventory(required)
    }
    fn workspace_receipt_root(&mut self) -> Result<String> {
        let config = self.workspace_config()?;
        conductor_native::project_paths::mutation_receipt_root_from_config(config.as_ref())
            .map_err(anyhow::Error::msg)
    }
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("missing corpus string {key}"))
}
fn keys(value: &Value) -> Result<Vec<String>> {
    match value {
        Value::Null => Ok(vec![]),
        Value::Object(rows) => Ok(rows.keys().cloned().collect()),
        _ => anyhow::bail!("corpus scope must be an object"),
    }
}
fn patch_rows(catalog: &Catalog, checker: &Checker<'_>, root: &Path) -> Result<Value> {
    let mut stale = Vec::new();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut mutations = 0usize;
    for campaign in &catalog.contracts {
        checker.poll()?;
        let id = string(campaign, "campaign_id")?;
        for mutation in campaign["mutations"]
            .as_array()
            .context("missing mutations")?
        {
            mutations += 1;
            anyhow::ensure!(mutations <= 100_000, "mutations exceed 100000");
            let path = string(mutation, "patch_file")?.trim_start_matches('/');
            let mutation_id = string(mutation, "id")?;
            let verdict = checker.check(path, string(mutation, "patch_sha256")?)?;
            let (reason, detail) = match verdict {
                PatchObservation::Missing => (
                    "MISSING",
                    format!("no patch file at {}", root.join(path).display()),
                ),
                PatchObservation::HashDrift { expected, actual } => {
                    ("HASH_DRIFT", format!("expected {expected}, got {actual}"))
                }
                PatchObservation::DoesNotApply { detail } => ("DOES_NOT_APPLY", detail),
                PatchObservation::GitApplies | PatchObservation::AnchoredApplies => continue,
            };
            *counts.entry(id.to_owned()).or_default() += 1;
            stale.push(
                json!({"campaign_id":id,"mutation_id":mutation_id,"reason":reason,"detail":detail}),
            );
        }
    }
    stale.sort_by_key(|row| {
        (
            row["campaign_id"].as_str().unwrap_or_default().to_owned(),
            row["mutation_id"].as_str().unwrap_or_default().to_owned(),
        )
    });
    let mut broken: Vec<Value> = catalog.broken.iter().map(|row| json!({
        "manifest":row["manifest"], "detail":row["error"].as_str().unwrap_or_default().chars().take(400).collect::<String>()
    })).collect();
    broken.sort_by_key(|row| row["manifest"].as_str().unwrap_or_default().to_owned());
    Ok(
        json!({"status": if stale.is_empty() && broken.is_empty() {"CLEAN"} else {"STALE"},
        "repo_root":root,"campaigns":catalog.contracts.len(),"mutations":mutations,
        "stale_mutations":stale.len(),"stale_campaigns":counts,"stale":stale,"unloadable":broken}),
    )
}
#[derive(PartialEq, Eq)]
pub(crate) struct Interpreter {
    path: String,
    identity: Option<(u64, u64)>,
}
impl Interpreter {
    fn observe(path: &str) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let identity = match std::fs::metadata(path) {
            Ok(metadata) => Some((metadata.dev(), metadata.ino())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error).context("inspect campaign interpreter"),
        };
        Ok(Self {
            path: path.into(),
            identity,
        })
    }
    pub(crate) fn verify(&self) -> Result<()> {
        anyhow::ensure!(
            *self == Self::observe(&self.path)?,
            "campaign interpreter existence or identity changed"
        );
        Ok(())
    }
}
fn interpreter(campaign: &Value, root: &Path) -> Result<Option<(Value, Interpreter)>> {
    let id = string(campaign, "campaign_id")?;
    let argv = campaign["test_argv"]
        .as_array()
        .context("missing test argv")?;
    let Some(path) = argv
        .first()
        .and_then(Value::as_str)
        .filter(|path| path.starts_with('/'))
    else {
        return Ok(None);
    };
    let observation = Interpreter::observe(path)?;
    let candidate = Path::new(path);
    let (reason, detail) = if observation.identity.is_none() {
        (
            "INTERPRETER_ABSENT",
            "argv[0] is an absolute path that does not exist on this host".to_owned(),
        )
    } else {
        let inside = candidate
            .canonicalize()
            .is_ok_and(|path| path.starts_with(root));
        ("INTERPRETER_HOST_PINNED", format!("argv[0] is an absolute path, so the campaign binds to this host's {} instead of the runner's own interpreter; use a bare `python`", if inside {"checkout"} else {"filesystem"}))
    };
    Ok(Some((
        json!({"campaign_id":id,"interpreter":path,"reason":reason,"detail":detail}),
        observation,
    )))
}
fn receipt_rows(catalog: &Catalog) -> Result<BTreeMap<String, Vec<(String, Value)>>> {
    let mut grouped = BTreeMap::<String, Vec<(String, Value)>>::new();
    for receipt in &catalog.receipts {
        let payload = crate::tool_schema::parse_document(&receipt.text)?;
        if let Some(id) = payload["campaign_id"].as_str() {
            grouped
                .entry(id.into())
                .or_default()
                .push((receipt.path.clone(), payload));
        }
    }
    Ok(grouped)
}
struct Supporting<'a> {
    root: Option<&'a conductor_native::mutation_cargo_supporting_binding::SupportingInputs>,
    root_error: Option<&'a str>,
    receipt_root: Result<&'a str, &'a str>,
    workspace: &'a BTreeMap<
        String,
        Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs, String>,
    >,
}
fn accepted_rows(
    rows: &[(String, Value)],
    workspace: bool,
    receipt_root: Result<&str, &str>,
    capture: &mut dyn CorpusInputs,
    reject: impl Fn(&Value, &mut dyn CorpusInputs) -> Result<Option<String>>,
) -> Result<(Vec<Value>, BTreeSet<String>)> {
    let mut reasons = BTreeSet::new();
    let mut accepted = Vec::new();
    for (path, row) in rows {
        if workspace
            && row
                .as_object()
                .is_some_and(conductor_native::mutation_native_runner::declared)
        {
            let allowed = receipt_root.as_ref().is_ok_and(|root| {
                conductor_native::mutation_snapshot_inputs::within_namespace(path, root)
            });
            if !allowed {
                reasons.insert(
                    "workspace receipt is outside independently configured receipt root".into(),
                );
                continue;
            }
        }
        match reject(row, capture)? {
            Some(reason) => {
                reasons.insert(reason);
            }
            None => accepted.push(row.clone()),
        }
    }
    Ok((accepted, reasons))
}
fn campaign_receipts(
    campaign: &Value,
    rows: &[(String, Value)],
    capture: &mut dyn CorpusInputs,
    bundle: Option<&RunnerBundle>,
    installed: Option<&conductor_native::mutation_native_runner::InstalledIdentity>,
    supporting: &Supporting<'_>,
) -> Result<(Option<Value>, Vec<Value>, Vec<Value>)> {
    let ordinary_cargo = campaign["generated"] == true
        && campaign["mutation_engine"] == "cargo-mutants"
        && campaign["generated_value"].is_null();
    let ordinary_native =
        ordinary_cargo || crate::mutation_cargo_supporting::confined_fest(campaign);
    let id = string(campaign, "campaign_id")?;
    let workspace = conductor_native::mutation_cargo_supporting_binding::uses_snapshot_inputs(
        campaign["execution_profile"].as_str(),
    );
    let (identity, supporting_error) = if workspace {
        match supporting.workspace.get(id) {
            Some(Ok(identity)) => (Some(identity), None),
            Some(Err(error)) => (None, Some(error.as_str())),
            None => (
                None,
                Some("independently captured workspace inputs unavailable"),
            ),
        }
    } else {
        (supporting.root, supporting.root_error)
    };
    let symbols: BTreeSet<_> = keys(&campaign["source_symbols"])?.into_iter().collect();
    let view = receipts::Campaign {
        campaign_id: id,
        generated: campaign["generated"] == true,
        ordinary_cargo,
        execution_profile: campaign["execution_profile"].as_str(),
        ordinary_contract: ordinary_native.then_some(campaign),
        declares_value_analysis: !campaign["value_analysis_payload"].is_null(),
        source_symbols: &symbols,
    };
    let reject = |row: &Value, capture: &mut dyn CorpusInputs| {
        if ordinary_native
            && row
                .as_object()
                .is_some_and(conductor_native::mutation_native_runner::declared)
        {
            if let Some(error) = supporting_error {
                return Ok(Some(format!(
                    "native supporting-input capture unavailable: {error}"
                )));
            }
        }
        if crate::mutation_cargo_supporting::stale_native_contract(campaign, row, ordinary_native) {
            return Ok(Some("current source or test hashes drifted".into()));
        }
        if bundle.is_none()
            && !row
                .as_object()
                .is_some_and(conductor_native::mutation_native_runner::declared)
        {
            return Ok(Some(
                "Python receipt requires an operator-pinned runner bundle".into(),
            ));
        }
        let empty = json!({});
        receipts::rejection_with_supporting(
            &view,
            row,
            bundle.map_or(&empty, |bundle| &bundle.runner["components"]),
            |path| capture.hash(path),
            |recorded| {
                Ok(bundle
                    .and_then(|bundle| bundle.lineage.as_ref())
                    .is_some_and(|lineage| {
                        conductor_native::mutation_evidence::captured_lineage_accepts(
                            recorded, lineage,
                        )
                    }))
            },
            installed,
            identity,
        )
    };
    let (accepted, reasons) =
        accepted_rows(rows, workspace, supporting.receipt_root, capture, reject)?;
    let evidence = if rows.is_empty() {
        Some(
            json!({"campaign_id":id,"reason":"NO_RECEIPT","receipts":0,"detail":"no receipt declares this campaign_id"}),
        )
    } else if accepted.is_empty() {
        Some(
            json!({"campaign_id":id,"reason":"NO_ACCEPTABLE_RECEIPT","receipts":rows.len(),"detail":reasons.into_iter().collect::<Vec<_>>().join("; ")}),
        )
    } else {
        None
    };
    let selected = receipts::newest_acceptable(&accepted, |_| Ok(None))?;
    let (unmeasured, inert) = receipts::value_findings(&view, selected)?;
    Ok((evidence, unmeasured, inert))
}
fn scope(catalog: &Catalog, campaign: &Value) -> Result<CampaignScope> {
    let manifest = string(campaign, "manifest")?.trim_start_matches('/');
    let raw = catalog
        .raw_manifests
        .get(manifest)
        .context("campaign raw manifest absent")?;
    Ok(CampaignScope {
        language: string(campaign, "language")?.to_owned(),
        source_paths: keys(&raw["source_sha256"])?,
        test_paths: keys(&raw["test_sha256"])?,
    })
}
fn workspace_receipt_root(capture: &mut dyn CorpusInputs, needed: bool) -> Result<String, String> {
    if needed {
        capture
            .workspace_receipt_root()
            .map_err(|error| format!("{error:#}"))
    } else {
        Err("independently captured workspace receipt root unavailable".into())
    }
}
fn reproduction(
    catalog: &Catalog,
    capture: &mut dyn CorpusInputs,
    bundle: Option<&RunnerBundle>,
    installed: Option<&conductor_native::mutation_native_runner::InstalledIdentity>,
    changed: &[String],
    root: &Path,
    checker: &Checker<'_>,
) -> Result<(Value, Vec<Interpreter>)> {
    let grouped = receipt_rows(catalog)?;
    let mut interpreters = Vec::new();
    let mut interpreter_bindings = Vec::new();
    let mut evidence = Vec::new();
    let mut unmeasured = Vec::new();
    let mut inert = Vec::new();
    let mut scopes = Vec::new();
    let supporting_capture = crate::mutation_cargo_supporting::required_for(
        &catalog.contracts,
        &catalog.receipts,
        || capture.hash(conductor_native::mutation_native_runner::FINGERPRINT_KEY),
        installed,
    )
    .and_then(|required| {
        required
            .map(|paths| capture.supporting_inventory(&paths))
            .transpose()
    });
    let (supporting, supporting_error) = match supporting_capture {
        Ok(value) => (value, None),
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    let workspace_required = crate::mutation_cargo_supporting::workspace_required_for(
        &catalog.contracts,
        &catalog.receipts,
        || capture.hash(conductor_native::mutation_native_runner::FINGERPRINT_KEY),
        installed,
    )?;
    let workspace =
        crate::mutation_cargo_supporting::capture_workspace_profiles(workspace_required, |pins| {
            capture.workspace_inventory(pins)
        });
    let receipt_root = workspace_receipt_root(capture, !workspace.is_empty());
    let supporting = Supporting {
        receipt_root: receipt_root
            .as_ref()
            .map(String::as_str)
            .map_err(String::as_str),
        root: supporting.as_ref(),
        root_error: supporting_error.as_deref(),
        workspace: &workspace,
    };
    for campaign in &catalog.contracts {
        checker.poll()?;
        let id = string(campaign, "campaign_id")?;
        if let Some((row, binding)) = interpreter(campaign, root)? {
            interpreters.push(row);
            interpreter_bindings.push(binding);
        }
        let rows = grouped.get(id).map_or(&[][..], Vec::as_slice);
        let (failure, missing, tests) =
            campaign_receipts(campaign, rows, capture, bundle, installed, &supporting)?;
        evidence.extend(failure);
        unmeasured.extend(missing);
        inert.extend(tests);
        scopes.push(scope(catalog, campaign)?);
    }
    for rows in [
        &mut interpreters,
        &mut evidence,
        &mut unmeasured,
        &mut inert,
    ] {
        rows.sort_by_key(|row| {
            (
                row["campaign_id"].as_str().unwrap_or_default().to_owned(),
                row["nodeid"].as_str().unwrap_or_default().to_owned(),
            )
        });
    }
    let mut present = BTreeSet::new();
    for path in changed {
        checker.poll()?;
        if capture.present(path)? {
            present.insert(path.clone());
        }
    }
    let uncovered = territory::uncovered(changed, &present, &scopes)?;
    let absent = interpreters
        .iter()
        .filter(|row| row["reason"] == "INTERPRETER_ABSENT")
        .count();
    let output = json!({"campaigns":catalog.contracts.len(),"receipt_files":grouped.values().map(Vec::len).sum::<usize>(),
        "host_pinned_interpreters":interpreters.len(),"absent_interpreters":absent,"uncovered_campaigns":evidence.len(),
        "campaigns_without_value_analysis":unmeasured.len(),"tests_that_kill_nothing":inert.len(),
        "uncovered_changed_files":uncovered.len(),"interpreters":interpreters,"evidence":evidence,
        "unmeasured":unmeasured,"inert_tests":inert,"uncovered_files":uncovered});
    Ok((output, interpreter_bindings))
}
pub(crate) struct AuditOptions<'a> {
    pub changed: &'a [String],
    pub baseline: Option<&'a Value>,
    pub root: &'a Path,
    pub summary: bool,
    pub record: bool,
}

pub(crate) struct PublicationEvidence<'a, 'snapshot> {
    pub bundle: Option<&'a RunnerBundle>,
    pub native_pin: Option<&'a crate::candidate_native_runner_pin::NativeRunnerPin>,
    pub checker: &'a Checker<'snapshot>,
    pub lease: &'a crate::candidate_review_lock::ReviewLock,
    pub list: Option<&'a crate::candidate_inputs::BoundFile>,
    pub interpreters: &'a [Interpreter],
}
impl PublicationEvidence<'_, '_> {
    pub(crate) fn verify(&self) -> Result<()> {
        if let Some(bundle) = self.bundle {
            bundle.verify()?;
        }
        if let Some(pin) = self.native_pin {
            pin.verify()?;
        }
        self.checker.verify()?;
        for interpreter in self.interpreters {
            interpreter.verify()?;
        }
        if let Some(list) = self.list {
            list.verify()?;
        }
        self.lease.verify()?;
        self.checker.poll()
    }
}

/// Run the corpus observation over held candidate inputs and operator pins.
pub(crate) fn audit_installed(
    catalog: &Catalog,
    capture: &mut dyn CorpusInputs,
    bundle: Option<&RunnerBundle>,
    native_pin: Option<&crate::candidate_native_runner_pin::NativeRunnerPin>,
    checker: &Checker<'_>,
    options: AuditOptions<'_>,
) -> Result<(Value, u8, Value, Vec<Interpreter>)> {
    let installed = if let Some(pin) = native_pin {
        if capture.native_inventory_absent()? {
            Some(pin.identity())
        } else {
            anyhow::ensure!(
                capture
                    .hash(conductor_native::mutation_native_runner::FINGERPRINT_KEY)?
                    .is_some(),
                "partial native source inventory"
            );
            None
        }
    } else {
        None
    };
    let AuditOptions {
        changed,
        baseline,
        root,
        summary,
        record,
    } = options;
    checker.poll()?;
    if let Some(bundle) = bundle {
        bundle.verify()?;
    }
    if let Some(pin) = native_pin {
        pin.verify()?;
    }
    capture.verify_inputs()?;
    let mut patches = patch_rows(catalog, checker, root)?;
    let (mut repro, interpreters) =
        reproduction(catalog, capture, bundle, installed, changed, root, checker)?;
    let found = delta::findings(&patches, &repro)?;
    let recorded = json!({"schema_version":1,"note":"Registered mutant reproducibility debt; this baseline is a ratchet.",
        "stale_mutations":found["stale_mutations"],"unloadable_manifests":found["unloadable_manifests"],
        "host_pinned_interpreters":found["host_pinned_interpreters"],"uncovered_campaigns":found["uncovered_campaigns"],
        "campaigns_without_value_analysis":found["campaigns_without_value_analysis"],"tests_that_kill_nothing":found["tests_that_kill_nothing"],
        "uncovered_changed_files":found["uncovered_changed_files"]});
    let mut change = if record {
        json!({"status":"RECORDED", "exit_code":0})
    } else {
        delta::compare(&found, baseline)?
    };
    let code = change["exit_code"]
        .as_u64()
        .context("missing corpus exit")? as u8;
    change
        .as_object_mut()
        .context("corpus delta must be object")?
        .remove("exit_code");
    let status = change["status"].clone();
    repro["baseline"] = change;
    if summary {
        patches
            .as_object_mut()
            .context("patch result must be object")?
            .remove("stale");
        let obj = repro
            .as_object_mut()
            .context("repro result must be object")?;
        for field in [
            "interpreters",
            "evidence",
            "unmeasured",
            "inert_tests",
            "uncovered_files",
        ] {
            obj.remove(field);
        }
    }
    capture.verify_inputs()?;
    if let Some(bundle) = bundle {
        bundle.verify()?;
    }
    if let Some(pin) = native_pin {
        pin.verify()?;
    }
    checker.verify()?;
    for interpreter in &interpreters {
        interpreter.verify()?;
    }
    checker.poll()?;
    Ok((
        json!({"status":status,"repo_root":root,"campaigns":catalog.contracts.len(),"patches":patches,"reproducibility":repro,"git_build_profile":checker.provenance()}),
        code,
        recorded,
        interpreters,
    ))
}
