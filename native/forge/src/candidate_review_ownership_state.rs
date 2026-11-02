//! Pins live operator claim inputs; parsing remains in shipped ownership APIs.
use super::*;
use crate::candidate_inputs::{self, BoundFile, DirectoryBinding};
use std::path::PathBuf;

pub(super) struct HostState {
    root: PathBuf,
    common: PathBuf,
    common_binding: DirectoryBinding,
    governance: Option<DirectoryBinding>,
    held: Vec<BoundFile>,
    absent: Vec<PathBuf>,
    pub files: Vec<Value>,
}
impl HostState {
    pub fn capture(source: &Source, snapshot: &Snapshot) -> Result<Self> {
        let common = source.common_dir().to_owned();
        let governance_path = common.join("governance");
        let governance = match std::fs::symlink_metadata(&governance_path) {
            Ok(_) => Some(DirectoryBinding::repository(
                &governance_path,
                Some(&snapshot.path()),
            )?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let mut state = Self {
            root: source.root().into(),
            common: common.clone(),
            common_binding: DirectoryBinding::repository(&common, Some(&snapshot.path()))?,
            governance,
            held: Vec::new(),
            absent: Vec::new(),
            files: Vec::new(),
        };
        if state.governance.is_none() {
            state.absent.push(governance_path.clone());
        }
        for name in ["ownership-claims.json", "claim-activity.json"] {
            let path = governance_path.join(name);
            if state.governance.is_none() {
                state.files.push(json!({"name":name,"present":false}));
                continue;
            }
            match std::fs::symlink_metadata(&path) {
                Ok(_) => {
                    let (held, bytes) = candidate_inputs::repository_observed(
                        &path,
                        1024 * 1024,
                        &snapshot.path(),
                    )?;
                    state
                        .files
                        .push(json!({"name":name,"present":true,"bytes":bytes.len(),
                        "sha256":format!("{:x}",Sha256::digest(&bytes))}));
                    state.held.push(held);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    state.absent.push(path);
                    state.files.push(json!({"name":name,"present":false}));
                }
                Err(error) => return Err(error.into()),
            }
        }
        state.verify()?;
        Ok(state)
    }
    pub fn verify(&self) -> Result<()> {
        self.common_binding.verify()?;
        ensure!(
            ownership::checked_claim_path(&self.root, "ownership-claims.json")?
                == Some(self.common.join("governance/ownership-claims.json")),
            "ownership host Git common directory changed"
        );
        if let Some(governance) = &self.governance {
            governance.verify()?;
        }
        for held in &self.held {
            held.verify()?;
        }
        for path in &self.absent {
            match std::fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
                Ok(_) => anyhow::bail!("absent ownership host state appeared: {}", path.display()),
            }
        }
        Ok(())
    }
    pub fn observe(&self, now: f64) -> Result<Observation> {
        self.verify()?;
        let result = ownership::load_claims(&self.root)
            .map_err(anyhow::Error::from)
            .and_then(|(claims, digest)| {
                let mut active = Vec::new();
                for claim in &claims {
                    if claim.active(now)? {
                        active.push(claim.claim_id.clone());
                    }
                }
                Ok(Observation::Valid {
                    claims,
                    digest,
                    active,
                })
            });
        self.verify()?;
        Ok(result.unwrap_or_else(|error| Observation::Invalid(format!("{error:#}"))))
    }
}
pub(super) enum Observation {
    Valid {
        claims: Vec<ownership::OwnershipClaim>,
        digest: String,
        active: Vec<String>,
    },
    Invalid(String),
}
impl Observation {
    pub fn binding(&self) -> Value {
        match self {
            Self::Valid { digest, active, .. } => {
                json!({"state_sha256":digest,"active_claim_ids":active})
            }
            Self::Invalid(error) => json!({"error":error}),
        }
    }
}
