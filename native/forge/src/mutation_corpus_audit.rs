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
}
impl CorpusInputs for Capture<'_> {
    fn hash(&mut self, path: &str) -> Result<Option<String>> {
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
}
impl CorpusInputs for crate::mutation_live_inputs::LiveInputs {
    fn hash(&mut self, path: &str) -> Result<Option<String>> {
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
fn receipt_rows(catalog: &Catalog) -> Result<BTreeMap<String, Vec<Value>>> {
    let mut grouped = BTreeMap::<String, Vec<Value>>::new();
    for receipt in &catalog.receipts {
        let payload = crate::tool_schema::parse_document(&receipt.text)?;
        if let Some(id) = payload["campaign_id"].as_str() {
            grouped.entry(id.into()).or_default().push(payload);
        }
    }
    Ok(grouped)
}
fn campaign_receipts(
    campaign: &Value,
    rows: &[Value],
    capture: &mut dyn CorpusInputs,
    bundle: &RunnerBundle,
) -> Result<(Option<Value>, Vec<Value>, Vec<Value>)> {
    let id = string(campaign, "campaign_id")?;
    let symbols: BTreeSet<_> = keys(&campaign["source_symbols"])?.into_iter().collect();
    let view = receipts::Campaign {
        campaign_id: id,
        generated: campaign["generated"] == true,
        declares_value_analysis: !campaign["value_analysis_payload"].is_null(),
        source_symbols: &symbols,
    };
    let reject = |row: &Value, capture: &mut dyn CorpusInputs| {
        receipts::rejection(
            &view,
            row,
            &bundle.runner["components"],
            |path| capture.hash(path),
            |recorded| {
                Ok(bundle.lineage.as_ref().is_some_and(|lineage| {
                    conductor_native::mutation_evidence::captured_lineage_accepts(recorded, lineage)
                }))
            },
        )
    };
    let mut reasons = BTreeSet::new();
    let mut accepted = Vec::new();
    for row in rows {
        match reject(row, capture)? {
            Some(reason) => {
                reasons.insert(reason);
            }
            None => accepted.push(row.clone()),
        }
    }
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
fn reproduction(
    catalog: &Catalog,
    capture: &mut dyn CorpusInputs,
    bundle: &RunnerBundle,
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
    for campaign in &catalog.contracts {
        checker.poll()?;
        let id = string(campaign, "campaign_id")?;
        if let Some((row, binding)) = interpreter(campaign, root)? {
            interpreters.push(row);
            interpreter_bindings.push(binding);
        }
        let rows = grouped.get(id).map_or(&[][..], Vec::as_slice);
        let (failure, missing, tests) = campaign_receipts(campaign, rows, capture, bundle)?;
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
    pub bundle: &'a RunnerBundle,
    pub checker: &'a Checker<'snapshot>,
    pub lease: &'a crate::candidate_review_lock::ReviewLock,
    pub list: Option<&'a crate::candidate_inputs::BoundFile>,
    pub interpreters: &'a [Interpreter],
}
impl PublicationEvidence<'_, '_> {
    pub(crate) fn verify(&self) -> Result<()> {
        self.bundle.verify()?;
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

/// Run the complete corpus observation using held snapshot, runner and baseline bytes.
pub(crate) fn audit(
    catalog: &Catalog,
    capture: &mut dyn CorpusInputs,
    bundle: &RunnerBundle,
    checker: &Checker<'_>,
    options: AuditOptions<'_>,
) -> Result<(Value, u8, Value, Vec<Interpreter>)> {
    let AuditOptions {
        changed,
        baseline,
        root,
        summary,
        record,
    } = options;
    checker.poll()?;
    bundle.verify()?;
    capture.verify_inputs()?;
    let mut patches = patch_rows(catalog, checker, root)?;
    let (mut repro, interpreters) = reproduction(catalog, capture, bundle, changed, root, checker)?;
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
    bundle.verify()?;
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
