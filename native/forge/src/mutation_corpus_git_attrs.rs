//! Bind Git's non-worktree attribute files without trusting ambient config paths.
use crate::{
    candidate_inputs::{self, BoundFile},
    process_probe,
};
use anyhow::{ensure, Context, Result};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    process::Command,
    time::Duration,
};
struct Input {
    path: PathBuf,
    parent: PathBuf,
    identity: (u64, u64, u32, u32),
    file: Option<BoundFile>,
    size: u64,
}
impl Input {
    fn open(root: &Path, path: &Path) -> Result<Self> {
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            root.join(path)
        };
        ensure!(
            path.is_absolute()
                && path
                    .components()
                    .all(|c| matches!(c, Component::RootDir | Component::Normal(_))),
            "Git attribute path must be normalized"
        );
        let mut parent = path.parent().context("Git attribute parent missing")?;
        loop {
            match fs::symlink_metadata(parent) {
                Ok(meta) => {
                    ensure!(
                        meta.is_dir() && !meta.file_type().is_symlink(),
                        "unsafe Git attribute parent"
                    );
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    parent = parent.parent().context("Git attribute ancestor missing")?
                }
                Err(error) => return Err(error.into()),
            }
        }
        ensure!(
            parent.canonicalize()? == parent,
            "Git attribute parent contains symlink"
        );
        let metadata = fs::metadata(parent)?;
        ensure!(
            (metadata.uid() == 0 || metadata.uid() == unsafe { libc::geteuid() })
                && metadata.mode() & 0o002 == 0,
            "Git attribute parent ownership or permissions are unsafe"
        );
        let file = match fs::symlink_metadata(&path) {
            Ok(_) => Some(candidate_inputs::repository_live(&path, 32 * 1024 * 1024)?.0),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let parent = parent.to_owned();
        let size = if file.is_some() {
            fs::metadata(&path)?.len()
        } else {
            0
        };
        let this = Self {
            path,
            parent,
            identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.uid(),
                metadata.mode(),
            ),
            size,
            file,
        };
        this.verify()?;
        Ok(this)
    }
    fn verify(&self) -> Result<()> {
        let meta = fs::symlink_metadata(&self.parent)?;
        ensure!(
            meta.is_dir()
                && (meta.dev(), meta.ino(), meta.uid(), meta.mode()) == self.identity
                && self.parent.canonicalize()? == self.parent,
            "Git attribute parent changed"
        );
        if let Some(file) = &self.file {
            file.verify()?
        } else {
            match fs::symlink_metadata(&self.path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
                Ok(_) => anyhow::bail!("previously absent Git attribute file appeared"),
            }
        }
        Ok(())
    }
}
pub(crate) struct Attributes {
    inputs: Vec<Input>,
    environment: BTreeMap<OsString, OsString>,
}
impl Attributes {
    pub(crate) fn capture(
        git: &Path,
        root: &Path,
        profile: &crate::mutation_corpus_git_profile::Profile,
        interrupts: Option<&crate::process_signals::Interrupts>,
    ) -> Result<Self> {
        admit_environment()?;
        let environment = environment();
        let mut inputs = Vec::new();
        for args in [
            ["rev-parse", "--git-path", "info/attributes"],
            ["rev-parse", "--git-path", "index"],
        ] {
            if let Some(path) = query(git, root, &args, true, interrupts)? {
                inputs.push(Input::open(root, Path::new(&path))?);
            }
        }
        let configured = query(
            git,
            root,
            &["config", "--path", "--get", "core.attributesfile"],
            false,
            interrupts,
        )?;
        let global = match configured {
            Some(path) => PathBuf::from(path),
            None => default_global()?,
        };
        inputs.push(Input::open(root, &global)?);
        if let Some(path) = profile.system_path() {
            inputs.push(Input::open(root, path)?);
        }
        profile.verify()?;
        let total = inputs
            .iter()
            .try_fold(0u64, |sum, input| sum.checked_add(input.size))
            .context("Git attribute byte count overflow")?;
        ensure!(
            total <= 64 * 1024 * 1024,
            "Git attribute and index bytes exceed 64 MiB"
        );
        let held = Self {
            inputs,
            environment,
        };
        held.verify()?;
        Ok(held)
    }
    pub(crate) fn verify(&self) -> Result<()> {
        ensure!(
            environment() == self.environment,
            "Git environment changed during audit"
        );
        for input in &self.inputs {
            input.verify()?;
        }
        ensure!(
            environment() == self.environment,
            "Git environment changed while verifying attributes"
        );
        Ok(())
    }
}
fn environment() -> BTreeMap<OsString, OsString> {
    std::env::vars_os()
        .filter(|(name, _)| {
            let name = name.to_string_lossy();
            name.starts_with("GIT_")
                || name.starts_with("LC_")
                || matches!(
                    name.as_ref(),
                    "HOME" | "XDG_CONFIG_HOME" | "XDG_CONFIG_DIRS" | "PATH" | "LANG"
                )
        })
        .collect()
}
fn admit_environment() -> Result<()> {
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_ATTR_SOURCE",
        "GIT_EXEC_PATH",
    ] {
        ensure!(
            std::env::var_os(name).is_none(),
            "unsupported Git environment override {name}"
        );
    }
    Ok(())
}
fn default_global() -> Result<PathBuf> {
    if let Some(base) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(base);
        ensure!(path.is_absolute(), "XDG_CONFIG_HOME must be absolute");
        return Ok(path.join("git/attributes"));
    }
    let home = std::env::var_os("HOME").context("HOME required to bind default Git attributes")?;
    let path = PathBuf::from(home);
    ensure!(path.is_absolute(), "HOME must be absolute");
    Ok(path.join(".config/git/attributes"))
}
fn query(
    git: &Path,
    root: &Path,
    args: &[&str],
    allow_not_repository: bool,
    interrupts: Option<&crate::process_signals::Interrupts>,
) -> Result<Option<String>> {
    let mut command = Command::new(git);
    command.current_dir(root).args(args);
    let result = match interrupts {
        Some(owner) => {
            process_probe::run_interruptible(&mut command, Duration::from_secs(30), 65536, owner)?
        }
        None => process_probe::run(&mut command, Duration::from_secs(30), 65536)?,
    };
    if result.status.success() {
        let text = std::str::from_utf8(&result.stdout)?.trim_end_matches('\n');
        ensure!(
            !text.is_empty() && text.len() <= 4096 && !text.contains('\n'),
            "invalid Git attribute path"
        );
        return Ok(Some(text.into()));
    }
    ensure!(
        result.status.code() == Some(1)
            || (allow_not_repository && result.status.code() == Some(128)),
        "Git attribute discovery failed"
    );
    Ok(None)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn auxiliary_attribute_inputs_detect_absence_and_byte_drift() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let path = root.join("global-attributes");
        let absent = Input::open(&root, &path).unwrap();
        fs::write(&path, b"*.py text\n").unwrap();
        assert!(absent.verify().is_err());
        let held = Input::open(&root, &path).unwrap();
        held.verify().unwrap();
        fs::write(&path, b"*.py -text\n").unwrap();
        assert!(held.verify().is_err());
    }
    #[test]
    fn profile_declared_system_attribute_path_is_bound_and_revalidated() {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("source");
        let operator = tmp.path().join("operator");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&operator).unwrap();
        fs::set_permissions(&operator, fs::Permissions::from_mode(0o700)).unwrap();
        let attributes = operator.join("system-attributes");
        fs::write(&attributes, b"*.py text\n").unwrap();
        let profile_path = operator.join("profile.json");
        let git_sha = "a".repeat(64);
        let document = serde_json::json!({"schema_version":1,"git_sha256":git_sha,
            "system_attributes":{"mode":"path","path":attributes}});
        let bytes = serde_json::to_vec(&document).unwrap();
        fs::write(&profile_path, &bytes).unwrap();
        fs::set_permissions(&profile_path, fs::Permissions::from_mode(0o600)).unwrap();
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let profile = crate::mutation_corpus_git_profile::Profile::open(
            &profile_path,
            &digest,
            &git_sha,
            &root,
        )
        .unwrap();
        let input = Input::open(&root, profile.system_path().unwrap()).unwrap();
        fs::write(&attributes, b"*.py -text\n").unwrap();
        assert!(input.verify().is_err());
    }
}
