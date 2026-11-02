//! Opt-in isolated task execution.  The full bounded snapshot is writable;
//! artifact paths select evidence, not a write allowlist.  A retained tree OID
//! identifies the preimage only.  No output is promoted into the source tree.
use super::*;
use crate::{
    candidate_command::PreparedArgv,
    cgroup::Cgroup,
    confined_execution::{self, Cancellation, Execution, FinishState, Limits, Report},
};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    os::fd::AsRawFd,
    os::unix::ffi::OsStrExt,
    sync::{Arc, Mutex},
    time::Duration,
};

const MAX_ARTIFACTS: usize = 256;
const MAX_ARTIFACT_BYTES: usize = 32 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Copy)]
struct ArtifactBounds {
    per_file: usize,
    total: usize,
}
impl Default for ArtifactBounds {
    fn default() -> Self {
        Self {
            per_file: MAX_ARTIFACT_BYTES,
            total: MAX_TOTAL_BYTES,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct Artifact {
    path: String,
    bytes: usize,
    sha256: String,
}
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct Receipt {
    schema_version: u32,
    status: &'static str,
    preimage_tree: String,
    source_root: PathBuf,
    grants_sha256: String,
    outcome: confined_execution::Outcome,
    exit_code: Option<i32>,
    artifact_scope: &'static str,
    workspace: Option<PathBuf>,
    artifacts: Vec<Artifact>,
    cleanup_error: Option<String>,
}
impl Receipt {
    pub(crate) fn retained(&self) -> bool {
        self.status == "retained_for_review"
    }
}

/// Operator-owned literal argv and regular files selected for output evidence.
/// `prepare` does not launch a process or create a cgroup.
pub(crate) struct Prepared {
    workspace: Workspace,
    plan: CommandPlan,
    artifacts: Vec<String>,
    tree: String,
    receipt_path: PathBuf,
    grants_digest: String,
    source_root: PathBuf,
}

fn validate_input(argv: &[String], artifacts: &[String]) -> Result<()> {
    ensure!(
        !argv.is_empty() && argv.len() <= 4096,
        "task argv count exceeds bounds"
    );
    let bytes = argv.iter().try_fold(0usize, |sum, arg| {
        ensure!(
            arg.len() <= 131_071 && !arg.contains('\0'),
            "invalid task argument"
        );
        sum.checked_add(arg.len())
            .context("task argv size overflow")
    })?;
    ensure!(bytes <= 1024 * 1024, "task argv exceeds byte bound");
    ensure!(
        artifacts.len() <= MAX_ARTIFACTS,
        "too many selected artifacts"
    );
    let mut paths = std::collections::BTreeSet::new();
    for path in artifacts {
        ensure!(
            path.len() <= 4096 && !path.bytes().any(|c| c.is_ascii_control()),
            "invalid artifact path"
        );
        crate::source_file::validate_relative(Path::new(path))?;
        ensure!(
            path.split('/')
                .all(|s| !s.is_empty() && s != "." && s != ".."),
            "artifact must have canonical relative components"
        );
        ensure!(paths.insert(path), "duplicate artifact path");
    }
    Ok(())
}

impl Prepared {
    pub(crate) fn new(
        source: &Source,
        tree: &str,
        grants: &Grants,
        argv: Vec<String>,
        artifacts: Vec<String>,
    ) -> Result<Self> {
        validate_input(&argv, &artifacts)?;
        ensure!(
            tree.len() == 40 && tree.bytes().all(|b| b.is_ascii_hexdigit()),
            "task tree must be a full Git SHA-1 tree OID"
        );
        ensure!(
            source
                .list_tree(tree)?
                .iter()
                .all(|entry| entry.mode != "160000"),
            "task snapshot cannot omit gitlinks"
        );
        let workspace = Workspace::new(source, tree, grants)?;
        let plan = workspace.plan(
            &PreparedArgv {
                argv,
                changed_files: None,
            },
            grants,
        )?;
        let receipt_path = workspace
            .scratch
            .path
            .with_file_name(format!("{}.result.json", workspace.scratch.name));
        Ok(Self {
            workspace,
            plan,
            artifacts,
            tree: tree.to_ascii_lowercase(),
            receipt_path,
            grants_digest: grants.digest().into(),
            source_root: source.root().to_owned(),
        })
    }
    #[cfg(test)]
    pub(crate) fn receipt_path(&self) -> &Path {
        &self.receipt_path
    }
    #[cfg(test)]
    pub(crate) fn workspace_path(&self) -> PathBuf {
        self.workspace.path()
    }
    pub(crate) fn preview(&self) -> Result<serde_json::Value> {
        Ok(
            serde_json::json!({"executed":false,"preimage_tree":self.tree,"source_root":self.source_root,"grants_sha256":self.grants_digest,"artifact_scope":"selected_regular_files_only","artifacts":self.artifacts,"sandbox":self.workspace.sandbox_report()?}),
        )
    }
    pub(crate) fn dry_run(self) -> Result<()> {
        drop(self.plan);
        self.workspace.discard()
    }
    pub(crate) fn start(
        mut self,
        grants: &Grants,
        mask: Option<crate::process_signals::ChildSignalMask>,
    ) -> Result<TaskExecution> {
        ensure!(
            self.grants_digest == grants.digest(),
            "task grants changed after preparation"
        );
        if let Some(mask) = mask {
            mask.configure(self.plan.command());
        }
        let group = Cgroup::create(&grants.cgroup_parent, grants.resources)?;
        let limits = Limits {
            timeout: Duration::from_secs(grants.max_wall_seconds),
            max_output_chars: 262_144,
            cpu_seconds: Some(grants.max_cpu_seconds),
            address_space_bytes: Some(
                grants
                    .max_address_space_mib
                    .checked_mul(1024 * 1024)
                    .context("address space overflow")?,
            ),
        };
        let shared = Arc::new(Mutex::new(None));
        let receipt_parent = self.workspace.scratch.parent.try_clone()?;
        let resource = TaskResource {
            workspace: Some(self.workspace),
            artifacts: self.artifacts,
            tree: self.tree,
            receipt_path: self.receipt_path.clone(),
            receipt_parent,
            grants_digest: self.grants_digest,
            source_root: self.source_root,
            bounds: ArtifactBounds::default(),
            shared: shared.clone(),
        };
        let execution =
            confined_execution::start_with_resources(self.plan, group, limits, Box::new(resource))?;
        Ok(TaskExecution {
            execution,
            shared,
            receipt_path: self.receipt_path,
        })
    }
}

pub(crate) struct TaskExecution {
    execution: Execution,
    shared: Arc<Mutex<Option<Receipt>>>,
    receipt_path: PathBuf,
}
impl TaskExecution {
    pub(crate) fn cancellation(&self) -> Arc<Cancellation> {
        self.execution.cancellation()
    }
    pub(crate) fn receipt_path(&self) -> &Path {
        &self.receipt_path
    }
    pub(crate) async fn wait(self) -> Result<(Report, Receipt)> {
        let report = self.execution.wait().await?;
        let receipt = self
            .shared
            .lock()
            .map_err(|_| anyhow::anyhow!("task receipt lock poisoned"))?
            .take()
            .context("task owner returned without a disposition")?;
        Ok((report, receipt))
    }
}

struct TaskResource {
    workspace: Option<Workspace>,
    artifacts: Vec<String>,
    tree: String,
    receipt_path: PathBuf,
    receipt_parent: Dir,
    grants_digest: String,
    source_root: PathBuf,
    bounds: ArtifactBounds,
    shared: Arc<Mutex<Option<Receipt>>>,
}
impl Resources for TaskResource {
    fn arm(&mut self) {
        if let Some(workspace) = &mut self.workspace {
            workspace.arm();
        }
    }
    fn finish(self: Box<Self>, safe: bool) -> Result<()> {
        self.finish_with_state(FinishState {
            cleanup_safe: safe,
            outcome: confined_execution::Outcome::Failed,
            exit_code: None,
        })
    }
    fn finish_with_state(mut self: Box<Self>, state: FinishState) -> Result<()> {
        let mut receipt = Receipt {
            schema_version: 1,
            status: "incomplete",
            preimage_tree: self.tree.clone(),
            source_root: self.source_root.clone(),
            grants_sha256: self.grants_digest.clone(),
            outcome: state.outcome,
            exit_code: state.exit_code,
            artifact_scope: "selected_regular_files_only",
            workspace: None,
            artifacts: Vec::new(),
            cleanup_error: None,
        };
        let result = self.finalize(state, &mut receipt);
        if let Err(error) = &result {
            receipt.cleanup_error = Some(format!("{error:#}"));
        }
        let written = self.write_receipt(&receipt);
        if let Err(error) = &written {
            receipt.status = "incomplete";
            let prior = receipt.cleanup_error.take().unwrap_or_default();
            receipt.cleanup_error = Some(format!("{prior}; receipt publication: {error:#}"));
        }
        *self
            .shared
            .lock()
            .map_err(|_| anyhow::anyhow!("task receipt lock poisoned"))? = Some(receipt);
        match (result, written) {
            (Err(a), Err(b)) => {
                anyhow::bail!("task finalization: {a:#}; receipt publication: {b:#}")
            }
            (Err(error), _) | (_, Err(error)) => Err(error),
            _ => Ok(()),
        }
    }
}
impl TaskResource {
    fn finalize(&mut self, state: FinishState, receipt: &mut Receipt) -> Result<()> {
        let mut workspace = self.workspace.take().context("task workspace missing")?;
        let recovery = workspace.scratch.path.clone();
        receipt.workspace = Some(workspace.path());
        if !state.cleanup_safe {
            return Box::new(workspace)
                .finish(false)
                .context("task cleanup incomplete; owned workspace retained");
        }
        if !state.succeeded() {
            receipt.workspace = Some(recovery.clone());
            Box::new(workspace).finish(true).with_context(|| {
                format!(
                    "task discard incomplete; recovery directory {}",
                    recovery.display()
                )
            })?;
            receipt.workspace = None;
            receipt.status = "rolled_back_by_discard";
            return Ok(());
        }
        // From this point any failure keeps the owned snapshot for inspection.
        workspace.scratch.keep = true;
        receipt.artifacts = capture(&workspace, &self.artifacts, self.bounds)?;
        let snapshot = workspace.snapshot.take().context("task snapshot missing")?;
        let retained = snapshot.retain_task_output()?;
        receipt.workspace = Some(retained.output);
        receipt.status = "retained_for_review";
        workspace.armed = false;
        Ok(())
    }
    fn write_receipt(&self, receipt: &Receipt) -> Result<()> {
        let parent = self
            .receipt_path
            .parent()
            .context("task receipt parent missing")?;
        let directory = &self.receipt_parent;
        let held = directory.metadata(".")?;
        let current = std::fs::symlink_metadata(parent)?;
        use std::os::unix::fs::MetadataExt as _;
        ensure!(
            current.is_dir() && held.dev() == current.dev() && held.ino() == current.ino(),
            "task receipt parent changed"
        );
        let final_name = self
            .receipt_path
            .file_name()
            .context("task receipt name missing")?;
        let temp = crate::native_id::new("forge-task-result")?;
        let mut file = directory.open_with(
            &temp,
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC),
        )?;
        let result = (|| -> Result<()> {
            serde_json::to_writer(&mut file, receipt)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            let from = CString::new(temp.as_bytes())?;
            let to = CString::new(final_name.as_bytes())?;
            ensure!(
                unsafe {
                    libc::renameat2(
                        directory.as_raw_fd(),
                        from.as_ptr(),
                        directory.as_raw_fd(),
                        to.as_ptr(),
                        libc::RENAME_NOREPLACE,
                    )
                } == 0,
                "publish task receipt: {}",
                std::io::Error::last_os_error()
            );
            directory
                .open(".")?
                .sync_all()
                .context("task receipt published but parent sync failed")?;
            Ok(())
        })();
        if let Err(error) = &result {
            if let Err(cleanup) = directory.remove_file(&temp) {
                if cleanup.kind() != std::io::ErrorKind::NotFound {
                    anyhow::bail!("{error:#}; owned receipt temp cleanup: {cleanup}");
                }
            }
        }
        result
    }
}

fn capture(
    workspace: &Workspace,
    paths: &[String],
    bounds: ArtifactBounds,
) -> Result<Vec<Artifact>> {
    ensure!(
        (1..=MAX_ARTIFACT_BYTES).contains(&bounds.per_file) && bounds.total <= MAX_TOTAL_BYTES,
        "invalid artifact bounds"
    );
    let root = workspace.directory()?;
    let mut output = Vec::with_capacity(paths.len());
    let mut total = 0usize;
    for path in paths {
        let bytes =
            conductor_native::candidate_cargo_lint::read_optional(&root, path, bounds.per_file)
                .map_err(anyhow::Error::msg)?
                .with_context(|| format!("selected task artifact missing: {path}"))?;
        total = total
            .checked_add(bytes.len())
            .context("artifact total overflow")?;
        ensure!(
            total <= bounds.total,
            "task artifacts exceed aggregate limit"
        );
        output.push(Artifact {
            path: path.clone(),
            bytes: bytes.len(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        });
    }
    Ok(output)
}

#[cfg(test)]
#[path = "isolated_task_tests.rs"]
mod tests;
