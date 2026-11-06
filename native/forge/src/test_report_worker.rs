//! Dedicated-process report admission. The caller's sandbox is the read boundary:
//! post-open identity/configuration checks do not prevent libgit2's initial reads.
use crate::{
    candidate_inputs::{self, BoundFile, DirectoryBinding},
    candidate_source::Source,
};
use anyhow::{ensure, Context, Result};
use clap::Args;
use git2::ConfigLevel;
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Args)]
pub(crate) struct WorkerArgs {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    config_root: PathBuf,
    #[arg(long)]
    max_bytes: usize,
}

fn environment() -> Result<()> {
    for (name, _) in std::env::vars_os() {
        ensure!(
            !name.as_encoded_bytes().starts_with(b"GIT_"),
            "test report worker refuses inherited Git environment overrides"
        );
    }
    Ok(())
}

fn configuration_root(path: &Path) -> Result<DirectoryBinding> {
    let held = DirectoryBinding::open(path, None)?;
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o7777 == 0o700,
        "worker config root must be private and owned by the current user"
    );
    ensure!(
        fs::read_dir(path)?.next().is_none(),
        "worker config root must be empty"
    );
    let text = path.to_str().context("worker config root must be UTF-8")?;
    ensure!(
        !text.contains(':') && !text.contains("$PATH"),
        "worker config root cannot contain libgit2 search-path syntax"
    );
    Ok(held)
}

fn isolate_configuration(path: &Path) -> Result<()> {
    for level in [
        ConfigLevel::System,
        ConfigLevel::Global,
        ConfigLevel::XDG,
        ConfigLevel::ProgramData,
    ] {
        // SAFETY: only the dedicated hidden worker calls this before any Repoopen
        // or Tokio runtime creation. No other thread may access libgit2 here.
        // These process-global options must never be set by the service process.
        unsafe {
            git2::opts::set_search_path(level, path.as_os_str())?;
        }
    }
    Ok(())
}

fn absent(path: &Path) -> Result<()> {
    ensure!(
        matches!(fs::symlink_metadata(path), Err(error)
        if error.kind() == std::io::ErrorKind::NotFound),
        "unsupported worker Git control present or inaccessible: {}",
        path.display()
    );
    Ok(())
}

pub(crate) struct Admission {
    root: PathBuf,
    git: PathBuf,
    directories: [DirectoryBinding; 2],
    config: BoundFile,
}
impl Admission {
    pub(crate) fn capture(root: &Path) -> Result<Self> {
        environment()?;
        let root_binding = DirectoryBinding::repository(root, None)?;
        let git = root.join(".git");
        // A real canonical directory is required; gitfiles and symlinked Git
        // metadata do not qualify. No parent repository discovery is admitted.
        let git_binding = DirectoryBinding::repository(&git, None)?;
        let (config, _) = candidate_inputs::repository_live(&git.join("config"), 1024 * 1024)?;
        let result = Self {
            root: root.to_owned(),
            git,
            directories: [root_binding, git_binding],
            config,
        };
        result.controls()?;
        Ok(result)
    }
    fn controls(&self) -> Result<()> {
        for relative in [
            "commondir",
            "gitdir",
            "config.worktree",
            "objects/info/alternates",
            "objects/info/http-alternates",
            "info/grafts",
            "refs/replace",
        ] {
            absent(&self.git.join(relative))?;
        }
        Ok(())
    }
    pub(crate) fn verify(&self, source: &Source) -> Result<()> {
        environment()?;
        for directory in &self.directories {
            directory.verify()?;
        }
        self.config.verify()?;
        self.controls()?;
        ensure!(
            source.root() == self.root
                && source.repository().path().canonicalize()? == self.git
                && source.common_dir() == self.git,
            "worker requires the exact source root and its in-root Git/common directory"
        );
        // libgit2 has already parsed this configuration under caller confinement.
        // Inspect parsed keys, not a second text parser or substring heuristic.
        let config = source.repository().config()?.snapshot()?;
        let mut entries = config.entries(None)?;
        let mut count = 0;
        while let Some(entry) = entries.next() {
            let entry = entry?;
            count += 1;
            ensure!(count <= 4096, "worker Git configuration exceeds bound");
            let name = std::str::from_utf8(entry.name_bytes())?.to_ascii_lowercase();
            ensure!(
                entry.include_depth() == 0
                    && name != "include.path"
                    && !(name.starts_with("includeif.") && name.ends_with(".path")),
                "worker Git configuration includes unsupported"
            );
            ensure!(
                !matches!(
                    name.as_str(),
                    "core.worktree"
                        | "core.excludesfile"
                        | "core.attributesfile"
                        | "extensions.worktreeconfig"
                ),
                "worker Git path/config override unsupported: {name}"
            );
        }
        Ok(())
    }
}

fn execute(args: WorkerArgs) -> Result<Vec<u8>> {
    ensure!(
        (512..=262144).contains(&args.max_bytes),
        "worker output budget must be 512..262144"
    );
    environment()?;
    let config = configuration_root(&args.config_root)?;
    ensure!(
        !args.config_root.starts_with(&args.root) && !args.root.starts_with(&args.config_root),
        "worker config and source roots must be disjoint"
    );
    isolate_configuration(&args.config_root)?;
    let output = crate::test_report::readonly_exact(&args.root, args.max_bytes)?;
    config.verify()?;
    ensure!(
        fs::read_dir(&args.config_root)?.next().is_none(),
        "worker config root changed"
    );
    Ok(output)
}

/// Called only by the hidden CLI branch before the harness creates a runtime.
pub(crate) fn run(args: WorkerArgs) -> Result<u8> {
    crate::analysis::write_output(&execute(args)?)?;
    Ok(0)
}

#[cfg(test)]
#[path = "test_report_worker_tests.rs"]
mod tests;
