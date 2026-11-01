//! Private scratch for clang-tidy. Trusted same-UID parents must remain stable;
//! path identity checks do not prevent a concurrent replacement after a check.
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, DirBuilder, DirBuilderExt, MetadataExt, OpenOptions, OpenOptionsExt};
use std::{
    env, fs,
    io::Write,
    os::unix::fs::MetadataExt as _,
    path::{Component, Path, PathBuf},
};

const MAX_CASES: usize = 4096;
const MAX_DB: usize = 32 * 1024 * 1024;
const MAX_CONFIG: usize = 64 * 1024;
const MAX_TOTAL: usize = 64 * 1024 * 1024;

pub struct State {
    parent: Dir,
    parent_path: PathBuf,
    parent_id: (u64, u64),
    name: String,
    root: Dir,
    root_id: (u64, u64),
    count: usize,
    bytes: usize,
    cleaned: bool,
}

fn directory_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    options
}

fn canonical(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && path.as_os_str().as_encoded_bytes().len() <= 4096
            && !path
                .as_os_str()
                .as_encoded_bytes()
                .iter()
                .any(u8::is_ascii_control)
            && path
                .components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
            && path.canonicalize()? == path,
        "runtime path must be bounded, canonical and absolute"
    );
    Ok(())
}

fn open_created(parent: &Dir, parent_path: &Path, name: &str) -> Result<Dir> {
    let result = (|| -> Result<Dir> {
        let root = Dir::from_std_file(parent.open_with(name, &directory_options())?.into_std());
        let metadata = root.dir_metadata()?;
        ensure!(
            metadata.is_dir()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o777 == 0o700,
            "unsafe private clang-tidy state"
        );
        Ok(root)
    })();
    result.with_context(|| {
        format!(
            "created private state may remain at {}",
            parent_path.join(name).display()
        )
    })
}

impl State {
    pub fn new(snapshot: &Path) -> Result<Self> {
        let tmp = env::var_os("TMPDIR").context("TMPDIR must name private stage scratch")?;
        Self::new_in(snapshot, Path::new(&tmp))
    }

    fn new_in(snapshot: &Path, tmp: &Path) -> Result<Self> {
        canonical(snapshot)?;
        canonical(tmp)?;
        ensure!(
            !tmp.starts_with(snapshot),
            "TMPDIR must be outside candidate snapshot"
        );
        let meta = fs::symlink_metadata(tmp)?;
        ensure!(
            meta.is_dir()
                && (meta.uid() == 0 || meta.uid() == unsafe { libc::geteuid() })
                && meta.mode() & 0o777 == 0o700,
            "TMPDIR must be owned and private (0700)"
        );
        let parent = Dir::open_ambient_dir(tmp, cap_std::ambient_authority())?;
        let held = parent.dir_metadata()?;
        ensure!(
            (held.dev(), held.ino()) == (meta.dev(), meta.ino()),
            "TMPDIR changed during admission"
        );
        let name = crate::resource_id("clang-tidy")?;
        parent.create_dir_with(&name, DirBuilder::new().mode(0o700))?;
        let root = open_created(&parent, tmp, &name)?;
        let state_meta = root.dir_metadata().with_context(|| {
            format!(
                "created private state may remain at {}",
                tmp.join(&name).display()
            )
        })?;
        let state = Self {
            parent,
            parent_path: tmp.into(),
            parent_id: (meta.dev(), meta.ino()),
            name,
            root,
            root_id: (state_meta.dev(), state_meta.ino()),
            count: 0,
            bytes: 0,
            cleaned: false,
        };
        state.verify()?;
        Ok(state)
    }

    pub fn path(&self) -> PathBuf {
        self.parent_path.join(&self.name)
    }

    fn verify(&self) -> Result<()> {
        canonical(&self.parent_path)?;
        let fresh = fs::symlink_metadata(&self.parent_path)?;
        let held = self.parent.dir_metadata()?;
        ensure!(
            fresh.is_dir()
                && (fresh.uid() == 0 || fresh.uid() == unsafe { libc::geteuid() })
                && fresh.mode() & 0o777 == 0o700
                && (fresh.dev(), fresh.ino()) == self.parent_id
                && (held.dev(), held.ino()) == self.parent_id,
            "runtime parent identity changed"
        );
        let child = self.parent.symlink_metadata(&self.name)?;
        let root = self.root.dir_metadata()?;
        ensure!(
            child.is_dir()
                && child.uid() == unsafe { libc::geteuid() }
                && child.mode() & 0o777 == 0o700
                && (child.dev(), child.ino()) == self.root_id
                && (root.dev(), root.ino()) == self.root_id,
            "runtime state identity changed"
        );
        Ok(())
    }

    pub fn write_case(
        &mut self,
        index: usize,
        database: &[u8],
        config: &[u8],
    ) -> Result<(PathBuf, PathBuf)> {
        ensure!(
            index < MAX_CASES && self.count < MAX_CASES,
            "clang-tidy case count exceeds limit"
        );
        ensure!(
            database.len() <= MAX_DB && config.len() <= MAX_CONFIG,
            "clang-tidy case exceeds byte limit"
        );
        let total = self
            .bytes
            .checked_add(database.len())
            .and_then(|v| v.checked_add(config.len()))
            .context("clang-tidy byte overflow")?;
        ensure!(
            total <= MAX_TOTAL,
            "clang-tidy total case bytes exceed 64 MiB"
        );
        self.verify()?;
        let name = format!("{index:05}");
        self.root
            .create_dir_with(&name, DirBuilder::new().mode(0o700))
            .context("create unique clang-tidy case")?;
        self.count += 1;
        self.bytes = total;
        let case = Dir::from_std_file(self.root.open_with(&name, &directory_options())?.into_std());
        let case_meta = case.dir_metadata()?;
        ensure!(
            case_meta.is_dir()
                && case_meta.uid() == unsafe { libc::geteuid() }
                && case_meta.mode() & 0o777 == 0o700,
            "unsafe clang-tidy case directory"
        );
        write_file(&case, "compile_commands.json", database)?;
        write_file(&case, "clang-tidy.yaml", config)?;
        self.verify()?;
        let current = self.root.symlink_metadata(&name)?;
        ensure!(
            current.is_dir()
                && (current.dev(), current.ino()) == (case_meta.dev(), case_meta.ino()),
            "clang-tidy case directory changed"
        );
        let db_dir = self.path().join(name);
        Ok((db_dir.clone(), db_dir.join("clang-tidy.yaml")))
    }

    fn cleanup(&mut self) -> Result<()> {
        if self.cleaned {
            return Ok(());
        }
        self.verify()?;
        self.parent.remove_dir_all(&self.name)?;
        self.cleaned = true;
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        self.cleanup()
    }
}

fn write_file(dir: &Dir, name: &str, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = dir.open_with(name, &options)?;
    file.write_all(bytes)?;
    Ok(())
}

impl Drop for State {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!(
                "clang-tidy state cleanup failed at {}: {error:#}",
                self.path().display()
            );
        }
    }
}

#[cfg(test)]
#[path = "candidate_clang_tidy_state_tests.rs"]
mod tests;
