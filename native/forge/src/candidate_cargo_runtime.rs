//! Operator-provisioned Cargo runtime. Isolation is provided by the caller's Stage.
//! Operator-owned toolchain/cache ancestors are trusted to remain immutable.
//! Identity checks reject stable runtime replacement but cannot prevent a
//! concurrent same-UID mutator after a check; they are not compiler attestation.
#[path = "candidate_cargo_runtime_cache.rs"]
mod cache;
#[cfg(test)]
#[path = "candidate_cargo_runtime_tests.rs"]
mod tests;
use anyhow::{bail, ensure, Context, Result};
use cap_std::fs::{
    Dir, DirBuilder, DirBuilderExt, MetadataExt, OpenOptions, OpenOptionsExt as CapOpenOptionsExt,
};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs,
    io::Read,
    os::unix::fs::{MetadataExt as StdMetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    process::Command,
};

pub struct Runtime {
    candidate: PathBuf,
    bin: PathBuf,
    paths: Vec<PathBuf>,
    state: State,
    mode: Mode,
}
#[derive(Clone, Copy)]
enum Mode {
    Fmt,
    Clippy,
}
struct State {
    parent: Dir,
    parent_path: PathBuf,
    name: String,
    dev: u64,
    ino: u64,
    cleaned: bool,
    children: Option<[(u64, u64); 3]>,
}
fn directory_options() -> OpenOptions {
    let mut opts = OpenOptions::new();
    opts.read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    opts
}
impl State {
    fn new(tmp: &Path) -> Result<Self> {
        let parent = Dir::open_ambient_dir(tmp, cap_std::ambient_authority())?;
        let name = crate::resource_id("cargo-lint")?;
        parent.create_dir_with(&name, DirBuilder::new().mode(0o700))?;
        let meta = parent.symlink_metadata(&name)?;
        ensure!(
            meta.is_dir()
                && meta.uid() == unsafe { libc::geteuid() }
                && meta.mode() & 0o777 == 0o700,
            "unsafe cargo runtime directory"
        );
        Ok(Self {
            parent,
            parent_path: tmp.to_owned(),
            name,
            dev: meta.dev(),
            ino: meta.ino(),
            cleaned: false,
            children: None,
        })
    }
    fn path(&self) -> PathBuf {
        self.parent_path.join(&self.name)
    }
    fn cleanup(&mut self) -> Result<()> {
        if self.cleaned {
            return Ok(());
        }
        let meta = self.parent.symlink_metadata(&self.name)?;
        ensure!(
            meta.is_dir() && (meta.dev(), meta.ino()) == (self.dev, self.ino),
            "cargo runtime identity changed"
        );
        self.parent.remove_dir_all(&self.name)?;
        self.cleaned = true;
        Ok(())
    }
    fn root(&self) -> Result<Dir> {
        let dir = Dir::from_std_file(
            self.parent
                .open_with(&self.name, &directory_options())?
                .into_std(),
        );
        let meta = dir.dir_metadata()?;
        ensure!(
            (meta.dev(), meta.ino()) == (self.dev, self.ino),
            "cargo runtime identity changed"
        );
        Ok(dir)
    }
    fn initialize(&mut self) -> Result<()> {
        let root = self.root()?;
        let mut identities = [(0, 0); 3];
        for (index, name) in ["cwd", "cargo-home", "targets"].iter().enumerate() {
            root.create_dir_with(name, DirBuilder::new().mode(0o700))?;
            let child = root.open_with(name, &directory_options())?;
            let meta = child.metadata()?;
            ensure!(
                meta.is_dir()
                    && meta.uid() == unsafe { libc::geteuid() }
                    && meta.mode() & 0o777 == 0o700,
                "unsafe cargo runtime component"
            );
            identities[index] = (meta.dev(), meta.ino());
        }
        self.children = Some(identities);
        Ok(())
    }
    fn verify_layout(&self) -> Result<Dir> {
        let root = self.root()?;
        let identities = self.children.context("cargo runtime is not initialized")?;
        for (index, name) in ["cwd", "cargo-home", "targets"].iter().enumerate() {
            let child = root.open_with(name, &directory_options())?;
            let meta = child.metadata()?;
            ensure!(
                (meta.dev(), meta.ino()) == identities[index] && meta.is_dir(),
                "cargo runtime component changed"
            );
            if *name == "cargo-home" {
                let home = Dir::from_std_file(child.into_std());
                for file in ["config", "config.toml"] {
                    match home.symlink_metadata(file) {
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                        Ok(_) => bail!("private Cargo home config is forbidden"),
                        Err(error) => return Err(error.into()),
                    }
                }
            }
        }
        Ok(root)
    }
    fn target(&self, name: &str) -> Result<PathBuf> {
        let state = self.verify_layout()?;
        let targets =
            Dir::from_std_file(state.open_with("targets", &directory_options())?.into_std());
        match targets.create_dir_with(name, DirBuilder::new().mode(0o700)) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        let opened = targets.open_with(name, &directory_options())?;
        ensure!(
            opened.metadata()?.is_dir(),
            "invalid cargo target directory"
        );
        Ok(self.path().join("targets").join(name))
    }
}
impl Drop for State {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!(
                "forge: cargo runtime cleanup failed at {}: {error:#}",
                self.path().display()
            );
        }
    }
}
fn absolute_dir(path: &Path, label: &str) -> Result<PathBuf> {
    let bytes = path.as_os_str().as_encoded_bytes();
    ensure!(
        bytes.len() <= 4096 && !bytes.iter().any(|b| b.is_ascii_control() || *b == b':'),
        "{label} contains unsafe bytes or exceeds 4096 bytes"
    );
    ensure!(
        path.is_absolute()
            && path
                .components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_))),
        "{label} must be absolute and normalized"
    );
    let actual = path
        .canonicalize()
        .with_context(|| format!("resolve {label}"))?;
    ensure!(
        actual == path && actual.is_dir(),
        "{label} must be a canonical directory without symlinks"
    );
    Ok(actual)
}
fn exclusive(path: &Path, candidate: &Path, tmp: &Path) -> Result<()> {
    ensure!(
        !path.starts_with(candidate) && !path.starts_with(tmp),
        "operator input must be outside candidate and scratch"
    );
    if candidate
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "candidate" || name.starts_with(".forge-candidate-"))
    {
        if let Some(parent) = candidate
            .parent()
            .filter(|parent| tmp == parent.join("runtime/tmp"))
        {
            ensure!(
                !path.starts_with(parent),
                "operator input must be outside candidate scratch"
            );
        }
    }
    Ok(())
}
fn tool(bin: &Path, name: &str) -> Result<PathBuf> {
    let path = bin.join(name);
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
        .with_context(|| format!("open tool {name}"))?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.permissions().mode() & 0o111 != 0
            && (meta.uid() == unsafe { libc::geteuid() } || meta.uid() == 0)
            && meta.mode() & 0o022 == 0,
        "tool must be an owned or root-owned executable with no group/other writes"
    );
    let mut magic = [0; 4];
    file.read_exact(&mut magic)?;
    ensure!(&magic == b"\x7fELF", "tool must be a native ELF executable");
    Ok(path)
}
fn extra_paths(raw: Option<OsString>, candidate: &Path, tmp: &Path) -> Result<Vec<PathBuf>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let mut paths = Vec::new();
    for path in std::env::split_paths(&raw) {
        ensure!(
            !path.as_os_str().is_empty() && paths.len() < 16,
            "invalid extra tool path list"
        );
        let dir = absolute_dir(&path, "extra tool path")?;
        exclusive(&dir, candidate, tmp)?;
        paths.push(dir);
    }
    Ok(paths)
}
fn clean_cwd(path: &Path) -> Result<()> {
    for parent in path.ancestors() {
        let cargo = parent.join(".cargo");
        match fs::symlink_metadata(&cargo) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
            Ok(meta) => ensure!(
                meta.is_dir(),
                "Cargo config directory must not be a symlink or special file"
            ),
        }
        for name in ["config", "config.toml"] {
            match fs::symlink_metadata(cargo.join(name)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Ok(_) => bail!("Cargo config in runtime cwd ancestors is forbidden"),
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}
impl Runtime {
    /// Explicitly remove owned runtime state and propagate cleanup failure.
    pub fn finish(mut self) -> Result<()> {
        self.state.cleanup()
    }
    pub fn new(candidate: &Path, mode: &str, seed: bool) -> Result<Self> {
        let mode = match mode {
            "fmt" => Mode::Fmt,
            "clippy" => Mode::Clippy,
            _ => bail!("unknown cargo lint mode"),
        };
        let candidate = absolute_dir(candidate, "candidate root")?;
        let tmp = absolute_dir(
            Path::new(&std::env::var_os("TMPDIR").context("TMPDIR is required")?),
            "TMPDIR",
        )?;
        let bin = absolute_dir(
            Path::new(&std::env::var_os("FORGE_CARGO_BIN").context("FORGE_CARGO_BIN is required")?),
            "Cargo bin",
        )?;
        exclusive(&bin, &candidate, &tmp)?;
        for name in ["cargo", "rustc", "rustdoc"] {
            tool(&bin, name)?;
        }
        match mode {
            Mode::Fmt => {
                tool(&bin, "cargo-fmt")?;
                tool(&bin, "rustfmt")?;
            }
            Mode::Clippy => {
                tool(&bin, "cargo-clippy")?;
                tool(&bin, "clippy-driver")?;
            }
        }
        let paths = extra_paths(std::env::var_os("FORGE_CARGO_EXTRA_PATH"), &candidate, &tmp)?;
        let mut state = State::new(&tmp)?;
        state.initialize()?;
        let path = state.path();
        clean_cwd(&path.join("cwd"))?;
        if seed {
            if let Some(raw) = std::env::var_os("FORGE_CARGO_HOME_SEED") {
                let seed = absolute_dir(Path::new(&raw), "Cargo cache seed")?;
                exclusive(&seed, &candidate, &tmp)?;
                cache::copy_seed(&seed, &path.join("cargo-home"))?;
            }
        }
        Ok(Self {
            candidate,
            bin,
            paths,
            state,
            mode,
        })
    }
    /// Returns a command with inherited stdio; caller owns its process lifecycle.
    pub fn command(&self, manifest: Option<&Path>) -> Result<Command> {
        let state = self.state.path();
        let target = match manifest {
            Some(path) => {
                ensure!(path.is_absolute(), "Cargo manifest must be absolute");
                let actual = path.canonicalize().context("resolve Cargo manifest")?;
                ensure!(
                    actual == path
                        && actual.starts_with(&self.candidate)
                        && actual.is_file()
                        && actual.file_name().is_some_and(|s| s == "Cargo.toml"),
                    "unsafe Cargo manifest"
                );
                format!("{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes()))
            }
            None => "version".into(),
        };
        let target_dir = self.state.target(&target)?;
        clean_cwd(&state.join("cwd"))?;
        self.state.verify_layout()?;
        let mut paths = vec![self.bin.clone()];
        paths.extend(self.paths.iter().cloned());
        let mut command = Command::new(self.bin.join("cargo"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("CARGO_")
                || key.to_string_lossy().starts_with("RUST")
                || key.to_string_lossy().starts_with("LD_")
            {
                command.env_remove(key);
            }
        }
        command
            .current_dir(state.join("cwd"))
            .env("PATH", std::env::join_paths(paths)?)
            .env("CARGO_HOME", state.join("cargo-home"))
            .env("CARGO_TARGET_DIR", target_dir)
            .env("CARGO_NET_OFFLINE", "true")
            .env("CARGO_BUILD_JOBS", "2")
            .env("RUSTC", self.bin.join("rustc"))
            .env("RUSTDOC", self.bin.join("rustdoc"))
            .env("RUSTC_WRAPPER", "")
            .env("RUSTC_WORKSPACE_WRAPPER", "")
            .env("CARGO_ENCODED_RUSTFLAGS", "");
        if matches!(self.mode, Mode::Fmt) {
            command.env("RUSTFMT", self.bin.join("rustfmt"));
        }
        Ok(command)
    }
}
