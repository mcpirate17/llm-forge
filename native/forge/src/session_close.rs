//! Bounded native session close. Completion across status, claims and memory is not atomic.
use crate::{
    active_state, embedding,
    memory_catalog::Catalog,
    memory_ingest::{self, Settings},
    memory_store::Store,
    ownership_lifecycle,
};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub struct Request {
    pub host: PathBuf,
    pub owner: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub claim_ids: Vec<String>,
    pub all_owner_claims: bool,
    pub no_memory_index: bool,
    pub memory_embedding: Option<PathBuf>,
    pub memory_store: Option<PathBuf>,
    pub memory_catalog: Option<PathBuf>,
}
#[derive(Serialize)]
pub struct Outcome {
    pub owner: String,
    pub status: &'static str,
    pub handoff_entry: Option<String>,
    pub status_publication: &'static str,
    pub claims_released: Vec<String>,
    pub claim_release: &'static str,
    pub active_headings_count: Option<usize>,
    pub active_claims_count: Option<usize>,
    pub memory_status: String,
    pub memory_report: Option<serde_json::Value>,
    pub errors: Vec<String>,
}
impl Outcome {
    pub fn exit_code(&self) -> u8 {
        if self.status == "complete" {
            0
        } else {
            3
        }
    }
    fn partial(&mut self, stage: &str, error: impl std::fmt::Display) {
        self.status = "partial";
        self.errors.push(format!("{stage}: {error}"));
    }
    fn state(&mut self, state: &active_state::ActiveState) {
        self.active_headings_count = Some(state.active_headings.len());
        self.active_claims_count = Some(state.active_claims.len());
    }
}
fn config_path(host: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        host.join(path)
    }
}
fn prepare_memory(request: &Request, host: &Path) -> Result<Option<Settings>> {
    if request.no_memory_index {
        ensure!(
            request.memory_embedding.is_none()
                && request.memory_store.is_none()
                && request.memory_catalog.is_none(),
            "--no-memory-index cannot be combined with memory settings"
        );
        return Ok(None);
    }
    let config = request
        .memory_embedding
        .as_ref()
        .context("memory sync requires --memory-embedding or explicit --no-memory-index")?;
    let store = request
        .memory_store
        .as_ref()
        .context("memory sync requires --memory-store or explicit --no-memory-index")?;
    ensure!(store.is_absolute(), "memory store must be absolute");
    let bytes = crate::bounded_file::read(&config_path(host, config), 16384)?;
    let document = crate::tool_schema::parse_document(std::str::from_utf8(&bytes)?)?;
    let embedding: embedding::Config = serde_json::from_value(document)?;
    let fingerprint = memory_ingest::fingerprint(&embedding)?;
    let catalog = Catalog::load(host, request.memory_catalog.as_deref())?;
    let selected: BTreeSet<_> = catalog
        .entries
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    ensure!(!selected.is_empty(), "memory catalog has no sources");
    let inventory = catalog.scan(&selected)?;
    ensure!(
        !inventory.files.is_empty(),
        "memory catalog has no indexable input files"
    );
    match std::fs::symlink_metadata(store) {
        Ok(_) => {
            let existing = Store::open(store)?;
            match std::fs::symlink_metadata(existing.root().join("current")) {
                Ok(_) => {
                    let pin = existing.pin()?;
                    // A changed recipe re-embeds; malformed prior state fails before mutation.
                    let _recipe_changed = pin.manifest.fingerprint != fingerprint;
                    memory_ingest::validate_missing_roots(&pin, &inventory.missing_roots)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = store.parent().context("memory store parent")?;
            ensure!(parent.is_dir(), "memory store parent must already exist");
        }
        Err(error) => return Err(error.into()),
    }
    Ok(Some(Settings {
        host: host.to_owned(),
        store: store.clone(),
        catalog: request.memory_catalog.clone(),
        sources: Vec::new(),
        full: false,
        allow_missing_roots: false,
        timeout_seconds: 120,
        embedding,
    }))
}
fn preflight_claims(host: &Path, request: &Request) -> Result<()> {
    ensure!(
        !(request.all_owner_claims && !request.claim_ids.is_empty()),
        "claim IDs and --all-owner-claims are exclusive"
    );
    let ids: BTreeSet<_> = request.claim_ids.iter().collect();
    ensure!(ids.len() == request.claim_ids.len(), "duplicate claim IDs");
    if ids.is_empty() {
        return Ok(());
    }
    let (claims, _) = ownership_lifecycle::list(host)?;
    for id in ids {
        let claim = claims
            .iter()
            .find(|claim| &claim.claim_id == id)
            .with_context(|| format!("claim {id} does not exist"))?;
        ensure!(
            claim.owner.to_lowercase() == request.owner.trim().to_lowercase(),
            "claim {id} belongs to another owner"
        );
    }
    Ok(())
}
fn prepare(request: &Request) -> Result<(PathBuf, Option<Settings>)> {
    ensure!(
        !request.owner.trim().is_empty() && request.owner.len() <= 256,
        "owner is required and must be at most 256 bytes"
    );
    ensure!(
        request.title.is_some() == request.body.is_some(),
        "both --title and --body are required together"
    );
    let host = request
        .host
        .canonicalize()
        .context("resolve session host")?;
    let (host, _) = crate::repository::checkout_of_checked(&host)?
        .context("session host must be a Git checkout")?;
    let memory = prepare_memory(request, &host)?;
    preflight_claims(&host, request)?;
    Ok((host, memory))
}
fn base(request: &Request) -> Outcome {
    Outcome {
        owner: request.owner.trim().into(),
        status: "complete",
        handoff_entry: None,
        status_publication: "not_requested",
        claims_released: Vec::new(),
        claim_release: "not_requested",
        active_headings_count: None,
        active_claims_count: None,
        memory_status: if request.no_memory_index {
            "skipped".into()
        } else {
            "not_started".into()
        },
        memory_report: None,
        errors: Vec::new(),
    }
}
fn preserve_status(host: &Path, request: &Request, outcome: &mut Outcome) -> bool {
    if let (Some(title), Some(body)) = (&request.title, &request.body) {
        match crate::session_status::append_status(host, &outcome.owner, title, body) {
            Ok(result) => {
                outcome.status_publication = "published";
                outcome.handoff_entry = result.entry.lines().next().map(str::to_owned);
                if let Some(error) = result.refresh_error {
                    outcome.partial("status appended but active state refresh failed", error);
                    return false;
                }
            }
            Err(error) => {
                outcome.status_publication = "uncertain";
                outcome.partial(
                    "status append failed; publication may already be visible",
                    format!("{error:#}"),
                );
                return false;
            }
        }
    }
    match active_state::save_active_state(host) {
        Ok(state) => {
            outcome.state(&state);
            true
        }
        Err(error) => {
            outcome.partial(
                "active state refresh failed; claim release was not attempted",
                format!("{error:#}"),
            );
            false
        }
    }
}
fn release(host: &Path, request: &Request, outcome: &mut Outcome) -> bool {
    if request.claim_ids.is_empty() && !request.all_owner_claims {
        return true;
    }
    let mut followup_failed = false;
    match ownership_lifecycle::release_many(
        host,
        &request.claim_ids,
        &outcome.owner,
        request.all_owner_claims,
    ) {
        Ok(result) => {
            outcome.claim_release = if result.released.is_empty() {
                "no_matching_claims"
            } else {
                "published"
            };
            outcome.claims_released = result.released;
            if let Some(error) = result.followup_error {
                outcome.partial("claims released", error);
                followup_failed = true;
            }
        }
        Err(error) => {
            outcome.claim_release = "uncertain";
            outcome.partial(
                "claim release failed; publication may already be visible",
                format!("{error:#}"),
            );
            return false;
        }
    }
    match active_state::save_active_state(host) {
        Ok(state) => {
            outcome.state(&state);
            !followup_failed
        }
        Err(error) => {
            outcome.partial(
                "claims released but post-release active state refresh failed",
                format!("{error:#}"),
            );
            false
        }
    }
}
/// Validate all static inputs before status or claims change. A later failure is
/// returned in a partial outcome with every known completed phase preserved.
pub fn close(request: Request) -> Result<Outcome> {
    let (host, memory) = prepare(&request)?;
    let mut outcome = base(&request);
    if !preserve_status(&host, &request, &mut outcome) {
        return Ok(outcome);
    }
    if !release(&host, &request, &mut outcome) {
        return Ok(outcome);
    }
    if let Some(settings) = memory {
        match memory_ingest::ingest(settings) {
            Ok(report) => {
                outcome.memory_status = "ok".into();
                outcome.memory_report = Some(serde_json::to_value(report)?);
            }
            Err(error) => {
                outcome.memory_status = "failed".into();
                outcome.partial(
                    "memory sync failed after session work; a new generation may already be visible",
                    format!("{error:#}"),
                );
            }
        }
    }
    Ok(outcome)
}
