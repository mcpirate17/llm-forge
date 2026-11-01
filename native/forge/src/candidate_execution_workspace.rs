//! Private per-stage candidate workspace. Armed incomplete cleanup retains all
//! state for recovery; operator-controlled scratch ancestors remain trusted.
#[path = "candidate_execution_grants.rs"]
mod grants;
#[path = "candidate_execution_private_git.rs"]
mod private_git;
use crate::{
    candidate_command::PreparedArgv,
    candidate_snapshot::{Limits, Snapshot},
    candidate_source::Source,
    confined_execution::Resources,
    sandbox::{CommandPlan, Policy, Sandbox},
};
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, DirBuilder, DirBuilderExt, MetadataExt, OpenOptions, OpenOptionsExt};
pub(crate) use grants::Grants;
use serde_json::Value;
use std::{
    io::Write,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
};

struct Scratch {
    parent: Dir,
    name: String,
    path: PathBuf,
    identity: (u64, u64),
    keep: bool,
}
impl Scratch {
    fn new(parent: &Path) -> Result<Self> {
        let directory = Dir::open_ambient_dir(parent, cap_std::ambient_authority())?;
        let name = crate::native_id::new("forge-candidate-stage")?;
        directory.create_dir_with(&name, DirBuilder::new().mode(0o700))?;
        let metadata = directory.symlink_metadata(&name)?;
        ensure!(
            metadata.is_dir()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o777 == 0o700,
            "unsafe candidate scratch"
        );
        Ok(Self {
            parent: directory,
            path: parent.join(&name),
            name,
            identity: (metadata.dev(), metadata.ino()),
            keep: false,
        })
    }
    fn verify(&self) -> Result<()> {
        let meta = self.parent.symlink_metadata(&self.name)?;
        ensure!(
            meta.is_dir()
                && (meta.dev(), meta.ino()) == self.identity
                && meta.uid() == unsafe { libc::geteuid() },
            "candidate scratch path changed"
        );
        Ok(())
    }
    fn directory(&self) -> Result<Dir> {
        self.verify()?;
        Ok(Dir::from_std_file(
            self.parent
                .open_with(
                    &self.name,
                    OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW),
                )?
                .into_std(),
        ))
    }
    fn discard(&mut self) -> Result<()> {
        self.verify()?;
        self.parent.remove_dir_all(&self.name)?;
        self.keep = true;
        Ok(())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if !self.keep {
            if let Err(error) = self.discard() {
                eprintln!(
                    "forge: scratch cleanup failed at {}: {error:#}",
                    self.path.display()
                );
            }
        }
    }
}
pub(crate) struct Workspace {
    scratch: Scratch,
    snapshot: Option<Snapshot>,
    sandbox: Sandbox,
    runtime: PathBuf,
    git_dir: PathBuf,
    index: PathBuf,
    armed: bool,
    grants_digest: String,
}
impl Workspace {
    pub(crate) fn new(source: &Source, tree: &str, grants: &Grants) -> Result<Self> {
        let scratch = Scratch::new(&grants.scratch_parent)?;
        let sandbox = Sandbox::prepare(Policy {
            read_only: grants.read_only.clone(),
            scratch: scratch.path.clone(),
            working_directory: scratch.path.clone(),
            environment: grants.environment.clone(),
        })?;
        let snapshot = Snapshot::create(
            source,
            tree,
            &scratch.path.join("candidate"),
            Limits::default(),
        )?;
        let runtime = scratch.path.join("runtime");
        scratch
            .directory()?
            .create_dir_with("runtime", DirBuilder::new().mode(0o700))?;
        let runtime_dir = Dir::open_ambient_dir(&runtime, cap_std::ambient_authority())?;
        for name in ["home", "tmp", "cache"] {
            runtime_dir.create_dir_with(name, DirBuilder::new().mode(0o700))?;
        }
        let (git_dir, index) = private_git::prepare(source, tree, &runtime, &snapshot.path())?;
        Ok(Self {
            scratch,
            snapshot: Some(snapshot),
            sandbox,
            runtime,
            git_dir,
            index,
            armed: false,
            grants_digest: grants.digest().to_owned(),
        })
    }
    pub(crate) fn path(&self) -> PathBuf {
        self.snapshot.as_ref().expect("live workspace").path()
    }
    pub(crate) fn runtime(&self) -> PathBuf {
        self.runtime.clone()
    }
    pub(crate) fn sandbox_report(&self) -> Result<Value> {
        let mut report = serde_json::to_value(self.sandbox.report())?;
        // Admission starts with an empty scratch; the actual command enters the
        // subsequently populated, still privately owned snapshot under it.
        report["working_directory"] = serde_json::to_value(self.path())?;
        Ok(report)
    }
    pub(crate) fn discard(self) -> Result<()> {
        Box::new(self).finish(true)
    }
    pub(crate) fn plan(&self, prepared: &PreparedArgv, grants: &Grants) -> Result<CommandPlan> {
        self.scratch.verify()?;
        ensure!(
            grants.digest() == self.grants_digest,
            "workspace grants changed after preparation"
        );
        let alias = prepared.argv.first().context("empty prepared command")?;
        let program = grants.programs.get(alias).context("program not granted")?;
        if let Some(changed) = &prepared.changed_files {
            self.write_changed(&changed.path, &changed.bytes)?;
        }
        let mut plan = self.sandbox.command(program)?;
        let command = plan.command();
        command.arg0(program.file_name().context("program basename missing")?);
        command.args(&prepared.argv[1..]);
        command.current_dir(self.path());
        command
            .env("PATH", "")
            .env("HOME", self.runtime.join("home"))
            .env("TMPDIR", self.runtime.join("tmp"))
            .env("XDG_CACHE_HOME", self.runtime.join("cache"));
        command
            .env("GIT_DIR", &self.git_dir)
            .env("GIT_WORK_TREE", self.path())
            .env("GIT_INDEX_FILE", &self.index)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.runtime.join("gitconfig"))
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("PYTHONNOUSERSITE", "1");
        Ok(plan)
    }
    fn write_changed(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        ensure!(
            bytes.len() <= 8 * 1024 * 1024 && path.parent() == Some(self.runtime.as_path()),
            "changed-files artifact must be directly inside runtime"
        );
        let name = path.file_name().context("invalid changed-files name")?;
        let scratch = self.scratch.directory()?;
        let root = Dir::from_std_file(
            scratch
                .open_with(
                    "runtime",
                    OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW),
                )?
                .into_std(),
        );
        let mut file = root.open_with(
            name,
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK),
        )?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    }
}
impl Resources for Workspace {
    fn arm(&mut self) {
        self.armed = true;
    }
    fn finish(mut self: Box<Self>, cleanup_safe: bool) -> Result<()> {
        if !cleanup_safe {
            self.scratch.keep = true;
            if let Some(snapshot) = self.snapshot.take() {
                snapshot.retain_for_recovery()?;
            }
            anyhow::bail!(
                "candidate workspace retained for recovery at {}",
                self.scratch.path.display()
            );
        }
        self.scratch.keep = true;
        if let Some(snapshot) = self.snapshot.take() {
            snapshot.discard()?;
        }
        self.scratch.discard()?;
        self.armed = false;
        Ok(())
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        if self.armed {
            self.scratch.keep = true;
            if let Some(snapshot) = self.snapshot.take() {
                if let Err(error) = snapshot.retain_for_recovery() {
                    eprintln!("forge: workspace retention error: {error:#}");
                }
            }
            eprintln!(
                "forge: candidate workspace retained at {}",
                self.scratch.path.display()
            );
        } else if let Some(snapshot) = self.snapshot.take() {
            if let Err(error) = snapshot.discard() {
                self.scratch.keep = true;
                eprintln!(
                    "forge: snapshot cleanup failed at {}: {error:#}",
                    self.scratch.path.display()
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "candidate_execution_workspace_tests.rs"]
mod tests;
