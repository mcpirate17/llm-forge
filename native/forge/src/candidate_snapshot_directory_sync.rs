//! Record materialized directories and flush each one after its children.
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, Metadata, MetadataExt, OpenOptions, OpenOptionsExt};
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

const MAX_DIRECTORIES: usize = 100_000;
const MAX_PATH_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    mode: u32,
    uid: u32,
}

impl Identity {
    fn read(metadata: &Metadata) -> Result<Self> {
        ensure!(metadata.is_dir(), "candidate directory changed type");
        Ok(Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            mode: metadata.mode(),
            uid: metadata.uid(),
        })
    }
}

pub(super) struct Directories {
    paths: BTreeMap<PathBuf, Identity>,
    path_bytes: usize,
}

impl Directories {
    pub(super) fn new(root: &Dir) -> Result<Self> {
        let mut result = Self {
            paths: BTreeMap::new(),
            path_bytes: 0,
        };
        result.record(Path::new(""), root)?;
        Ok(result)
    }

    pub(super) fn record(&mut self, path: &Path, directory: &Dir) -> Result<()> {
        let identity = Identity::read(&directory.dir_metadata()?)?;
        if let Some(previous) = self.paths.get(path) {
            ensure!(
                *previous == identity,
                "candidate directory identity changed: {}",
                path.display()
            );
            return Ok(());
        }
        let bytes = self
            .path_bytes
            .checked_add(path.as_os_str().len())
            .context("candidate directory path budget overflow")?;
        ensure!(
            self.paths.len() < MAX_DIRECTORIES && bytes <= MAX_PATH_BYTES,
            "candidate directory path budget exceeded"
        );
        self.paths.insert(path.to_owned(), identity);
        self.path_bytes = bytes;
        Ok(())
    }

    pub(super) fn sync(&self, root: &Dir) -> Result<()> {
        // Reverse path order places every descendant before its ancestors.
        for path in self.paths.keys().rev() {
            #[cfg(test)]
            sync_hook(path)?;
            let directory = self.reopen(root, path)?;
            directory.open(".")?.sync_all()?;
        }
        Ok(())
    }

    fn reopen(&self, root: &Dir, path: &Path) -> Result<Dir> {
        let mut directory = root.try_clone()?;
        self.check(Path::new(""), &directory)?;
        let mut current = PathBuf::new();
        for component in path.components() {
            let Component::Normal(name) = component else {
                anyhow::bail!("unsafe candidate directory path");
            };
            let opened = directory.open_with(
                name,
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW),
            )?;
            directory = Dir::from_std_file(opened.into_std());
            current.push(name);
            self.check(&current, &directory)?;
        }
        Ok(directory)
    }

    fn check(&self, path: &Path, directory: &Dir) -> Result<()> {
        let expected = self
            .paths
            .get(path)
            .context("candidate directory identity missing")?;
        ensure!(
            *expected == Identity::read(&directory.dir_metadata()?)?,
            "candidate directory identity changed: {}",
            path.display()
        );
        Ok(())
    }
}

#[cfg(test)]
type SyncHook = Box<dyn FnMut(&Path) -> std::io::Result<()>>;

#[cfg(test)]
thread_local! {
    pub(super) static SYNC_HOOK: std::cell::RefCell<Option<SyncHook>> = std::cell::RefCell::new(None);
}

#[cfg(test)]
fn sync_hook(path: &Path) -> std::io::Result<()> {
    SYNC_HOOK.with(|hook| match hook.borrow_mut().as_mut() {
        Some(callback) => callback(path),
        None => Ok(()),
    })
}
