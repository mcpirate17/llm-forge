use crate::{candidate_source::Source, cgroup};
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Grants {
    pub version: u32,
    pub source_root: PathBuf,
    pub read_only: Vec<PathBuf>,
    pub programs: BTreeMap<String, PathBuf>,
    pub environment: BTreeMap<String, String>,
    pub scratch_parent: PathBuf,
    pub cgroup_parent: PathBuf,
    pub resources: cgroup::Limits,
    pub max_wall_seconds: u64,
    pub max_cpu_seconds: u64,
    pub max_address_space_mib: u64,
    pub python: Option<String>,
    #[serde(skip)]
    digest: String,
}
impl Grants {
    pub(crate) fn load(path: &Path, source: &Source) -> Result<Self> {
        ensure!(
            path.is_absolute() && !path.starts_with(source.root()),
            "grants file must be outside source repository"
        );
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?;
        let before = file.metadata()?;
        ensure!(
            before.is_file()
                && before.nlink() == 1
                && before.uid() == unsafe { libc::geteuid() }
                && before.mode() & 0o022 == 0
                && before.len() <= 1024 * 1024,
            "unsafe grants file"
        );
        let mut bytes = Vec::new();
        (&mut file).take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
        let after = file.metadata()?;
        ensure!(
            bytes.len() <= 1024 * 1024 && stable(&before, &after),
            "grants file changed while reading"
        );
        let actual = path.canonicalize()?;
        ensure!(
            !actual.starts_with(source.root()),
            "grants file must be outside source repository"
        );
        let current = std::fs::symlink_metadata(path)?;
        ensure!(
            stable(&before, &current),
            "grants path changed while reading"
        );
        let value = std::str::from_utf8(&bytes)?;
        let parsed = crate::tool_schema::parse_document_with_limit(value, 1024 * 1024)?;
        let mut grants: Grants = serde_json::from_value(parsed)?;
        grants.digest = format!("{:x}", Sha256::digest(&bytes));
        grants.validate(source)?;
        Ok(grants)
    }
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
    fn validate(&mut self, source: &Source) -> Result<()> {
        ensure!(self.version == 1, "unsupported grants version");
        self.source_root = absolute(&self.source_root)?;
        ensure!(
            self.source_root == source.root(),
            "grants source_root does not match source"
        );
        ensure!(
            self.read_only.len() <= 120
                && !self.programs.is_empty()
                && self.programs.len() <= 128
                && self.environment.len() <= 40,
            "grants collection limit exceeded"
        );
        for read in &mut self.read_only {
            *read = absolute(read)?;
        }
        ensure!(
            self.read_only.iter().any(|p| source.root().starts_with(p)),
            "source root lacks read grant"
        );
        ensure!(
            self.read_only
                .iter()
                .any(|p| source.common_dir().starts_with(p)),
            "common Git directory lacks explicit read grant"
        );
        for (alias, path) in &self.programs {
            ensure!(
                !alias.is_empty()
                    && alias.len() <= 256
                    && !alias.contains('/')
                    && !alias.chars().any(char::is_control),
                "invalid program alias"
            );
            ensure!(
                path.is_absolute() && path.file_name().is_some(),
                "program must use an absolute path"
            );
            let target = absolute(path)?;
            ensure!(
                target.is_file() && self.read_only.iter().any(|p| target.starts_with(p)),
                "program target lacks read grant"
            );
        }
        if let Some(python) = &self.python {
            ensure!(
                self.programs.contains_key(python),
                "Python must name an operator program alias"
            );
        }
        for key in self.environment.keys() {
            ensure!(
                !key.starts_with("GIT_")
                    && !key.starts_with("PYTHON")
                    && !key.starts_with("XDG_")
                    && !matches!(key.as_str(), "HOME" | "TMPDIR" | "PATH" | "LANG"),
                "reserved workspace environment variable"
            );
        }
        crate::sandbox::validate_environment(&self.environment)?;
        ensure!(
            (1..=86400).contains(&self.max_wall_seconds)
                && (1..=86400).contains(&self.max_cpu_seconds)
                && (1..=1048576).contains(&self.max_address_space_mib),
            "invalid execution grants resource ceilings"
        );
        ensure!(
            (10..=1_024_000).contains(&self.resources.cpu_millis)
                && (16..=1_048_576).contains(&self.resources.memory_mib)
                && (1..=4096).contains(&self.resources.processes),
            "invalid cgroup resource ceilings"
        );
        self.scratch_parent = absolute(&self.scratch_parent)?;
        self.cgroup_parent = absolute(&self.cgroup_parent)?;
        ensure!(
            !self.scratch_parent.starts_with(source.root())
                && !self.scratch_parent.starts_with(source.common_dir()),
            "scratch parent must be outside source repository"
        );
        Ok(())
    }
}
fn absolute(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "grant path must be absolute");
    path.canonicalize()
        .with_context(|| format!("resolve grant path {}", path.display()))
}
fn stable(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
        && b.nlink() == 1
}
