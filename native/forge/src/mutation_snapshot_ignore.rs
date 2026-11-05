//! Git ignore rules as held data. No pathname Git status or filter operations.
use crate::candidate_inputs::{self, BoundFile};
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::{
    collections::BTreeSet,
    env,
    ffi::OsString,
    path::{Path, PathBuf},
};
const MAX_RULE_BYTES: usize = 1024 * 1024;
const MAX_RULE_FILES: usize = 256;
const MAX_RULE_CHECKS: usize = 16384;
const MAX_TOTAL_BYTES: usize = 32 * 1024 * 1024;

pub(super) struct Inputs {
    files: Vec<BoundFile>,
    absent: BTreeSet<PathBuf>,
    home: Option<OsString>,
    xdg: Option<OsString>,
    total: usize,
    git_env: Vec<(OsString, OsString)>,
}
impl Inputs {
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            absent: BTreeSet::new(),
            home: env::var_os("HOME"),
            xdg: env::var_os("XDG_CONFIG_HOME"),
            total: 0,
            git_env: Self::git_environment(),
        }
    }
    fn git_environment() -> Vec<(OsString, OsString)> {
        let mut values: Vec<_> = env::vars_os()
            .filter(|(key, _)| {
                let key = key.to_string_lossy();
                key.starts_with("GIT_CONFIG")
                    || matches!(
                        key.as_ref(),
                        "GIT_DIR"
                            | "GIT_WORK_TREE"
                            | "GIT_COMMON_DIR"
                            | "GIT_INDEX_FILE"
                            | "GIT_OBJECT_DIRECTORY"
                            | "GIT_ALTERNATE_OBJECT_DIRECTORIES"
                            | "GIT_NAMESPACE"
                            | "GIT_ATTR_SOURCE"
                            | "GIT_EXEC_PATH"
                            | "GIT_CEILING_DIRECTORIES"
                            | "GIT_DISCOVERY_ACROSS_FILESYSTEM"
                            | "GIT_TEMPLATE_DIR"
                            | "GIT_DEFAULT_HASH"
                    )
            })
            .collect();
        values.sort();
        values
    }
    pub fn admit_git_environment(&self) -> Result<()> {
        for (key, value) in &self.git_env {
            let key = key.to_string_lossy();
            let safe = (key == "GIT_CONFIG_GLOBAL" || key == "GIT_CONFIG_SYSTEM")
                && value == "/dev/null"
                || key == "GIT_CONFIG_NOSYSTEM" && value == "1";
            ensure!(safe, "unsupported Git environment override {key}");
        }
        Ok(())
    }
    pub fn config_disabled(&self, name: &str) -> bool {
        self.git_env
            .iter()
            .any(|(key, value)| key == name && (value == "/dev/null" || value == "1"))
    }
    pub fn home(&self) -> Result<PathBuf> {
        let path = PathBuf::from(self.home.as_ref().context("HOME required for Git config")?);
        ensure!(path.is_absolute(), "HOME must be absolute");
        Ok(path)
    }
    fn metadata(path: &Path) -> Result<Option<cap_std::fs::Metadata>> {
        use std::path::Component;
        ensure!(path.is_absolute(), "ignore input must be absolute");
        let mut dir = Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
        let mut parts = path.components().peekable();
        ensure!(
            matches!(parts.next(), Some(Component::RootDir)),
            "invalid absolute input"
        );
        while let Some(part) = parts.next() {
            let Component::Normal(name) = part else {
                anyhow::bail!("ignore input path is not canonical");
            };
            if parts.peek().is_none() {
                return match dir.symlink_metadata(name) {
                    Ok(meta) => Ok(Some(meta)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(error) => Err(error.into()),
                };
            }
            let file = match dir.open_with(
                name,
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW),
            ) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            dir = Dir::from_std_file(file.into_std());
        }
        anyhow::bail!("ignore input has no filename")
    }
    pub fn read(&mut self, path: &Path) -> Result<Option<Vec<u8>>> {
        ensure!(path.is_absolute(), "ignore input must be absolute");
        ensure!(
            self.absent.len() < MAX_RULE_CHECKS,
            "ignore absence count exceeds 16384"
        );
        if self.absent.contains(&path.to_path_buf()) {
            ensure!(
                Self::metadata(path)?.is_none(),
                "previously absent ignore input appeared"
            );
            return Ok(None);
        }
        match Self::metadata(path)? {
            Some(meta) => {
                ensure!(
                    meta.is_file() && !meta.file_type().is_symlink(),
                    "ignore input is unsafe"
                );
                ensure!(
                    self.files.len() < MAX_RULE_FILES,
                    "ignore file count exceeds 256"
                );
                let (held, bytes) = candidate_inputs::repository_live(path, MAX_RULE_BYTES)?;
                self.total = self
                    .total
                    .checked_add(bytes.len())
                    .context("ignore byte overflow")?;
                ensure!(self.total <= MAX_TOTAL_BYTES, "ignore inputs exceed 32 MiB");
                self.files.push(held);
                Ok(Some(bytes))
            }
            None => {
                self.absent.insert(path.to_owned());
                Ok(None)
            }
        }
    }

    pub fn require_absent(&mut self, path: &Path) -> Result<()> {
        ensure!(
            self.read(path)?.is_none(),
            "Git attributes input is unsupported: {}",
            path.display()
        );
        Ok(())
    }
    pub fn default_git_dir(&self) -> Result<PathBuf> {
        if let Some(value) = self.xdg.as_ref().filter(|s| !s.is_empty()) {
            let path = PathBuf::from(value);
            ensure!(path.is_absolute(), "XDG_CONFIG_HOME must be absolute");
            return Ok(path.join("git"));
        }
        let path = PathBuf::from(
            self.home
                .as_ref()
                .context("HOME required for default Git ignore")?,
        );
        ensure!(path.is_absolute(), "HOME must be absolute");
        Ok(path.join(".config/git"))
    }
    pub fn verify(&self) -> Result<()> {
        ensure!(
            Self::git_environment() == self.git_env,
            "Git environment changed during snapshot"
        );
        ensure!(
            env::var_os("HOME") == self.home && env::var_os("XDG_CONFIG_HOME") == self.xdg,
            "Git default ignore environment changed"
        );
        for file in &self.files {
            file.verify()?;
        }
        for path in &self.absent {
            ensure!(
                Self::metadata(path)?.is_none(),
                "previously absent ignore or attributes input appeared: {}",
                path.display()
            );
        }
        Ok(())
    }
}
pub(super) fn matcher(root: &Path, bytes: Option<Vec<u8>>) -> Result<Gitignore> {
    let mut builder = GitignoreBuilder::new(root);
    if let Some(bytes) = bytes {
        let text = std::str::from_utf8(&bytes).context("Git ignore rules must be UTF-8")?;
        for (index, line) in text.lines().enumerate() {
            let line = if index == 0 {
                line.trim_start_matches('\u{feff}')
            } else {
                line
            };
            builder
                .add_line(None, line)
                .map_err(|error| anyhow::anyhow!("invalid Git ignore rule: {error}"))?;
        }
    }
    builder
        .build()
        .map_err(|error| anyhow::anyhow!("invalid Git ignore rules: {error}"))
}
pub(super) fn ignored(rules: &[Gitignore], path: &Path, directory: bool) -> bool {
    for rule in rules.iter().rev() {
        match rule.matched(path, directory) {
            ignore::Match::Ignore(_) => return true,
            ignore::Match::Whitelist(_) => return false,
            ignore::Match::None => (),
        }
    }
    false
}
