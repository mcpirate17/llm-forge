//! Private, complete, create-only publication. The operator owns the stable
//! parent; same-UID hostile replacement is outside the directory binding model.
use crate::candidate_inputs::DirectoryBinding;
use anyhow::{ensure, Context, Result};
use cap_std::fs::{
    Dir, File, MetadataExt, OpenOptions, OpenOptionsExt, Permissions, PermissionsExt,
};
use std::{
    ffi::CString,
    io::{self, Write},
    os::{fd::AsRawFd, unix::ffi::OsStrExt},
    path::{Component, Path, PathBuf},
};

pub(super) struct Destination {
    binding: DirectoryBinding,
    parent: Dir,
    path: PathBuf,
    name: CString,
}

impl Destination {
    pub(super) fn open(path: &Path) -> Result<Self> {
        ensure!(
            path.is_absolute()
                && path.as_os_str().as_bytes().len() <= 4096
                && !path.as_os_str().as_bytes().iter().any(u8::is_ascii_control)
                && path
                    .components()
                    .all(|c| matches!(c, Component::RootDir | Component::Normal(_))),
            "runner output must be a bounded normalized absolute path"
        );
        let name = path.file_name().context("runner output filename missing")?;
        ensure!(name.len() <= 255, "runner output filename exceeds255 bytes");
        let directory = path.parent().context("runner output parent missing")?;
        ensure!(
            directory.canonicalize()?.join(name).as_os_str() == path.as_os_str(),
            "runner output parent must be canonical and filename must be normalized"
        );
        let binding = DirectoryBinding::open(directory, None)
            .context("runner output requires an existing canonical mode-0700 parent")?;
        let parent = Dir::open_ambient_dir(directory, cap_std::ambient_authority())?;
        let result = Self {
            binding,
            parent,
            path: directory.to_path_buf(),
            name: CString::new(name.as_bytes())?,
        };
        result.verify()?;
        result.absent()?;
        Ok(result)
    }

    fn verify(&self) -> Result<()> {
        self.binding.verify()?;
        let meta = self.parent.dir_metadata()?;
        let actual = std::fs::symlink_metadata(&self.path)?;
        use std::os::unix::fs::MetadataExt as _;
        ensure!(
            meta.is_dir()
                && meta.uid() == unsafe { libc::geteuid() }
                && meta.mode() & 0o7777 == 0o700
                && actual.is_dir()
                && (actual.dev(), actual.ino()) == (meta.dev(), meta.ino()),
            "runner output parent must be owned by current UID with mode0700"
        );
        Ok(())
    }

    fn absent(&self) -> Result<()> {
        match self.parent.symlink_metadata(self.name.to_str()?) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
            Ok(_) => anyhow::bail!("runner output already exists; refusing overwrite"),
        }
    }

    fn same_stage(&self, name: &str, held: &File) -> Result<()> {
        let actual = self.parent.symlink_metadata(name)?;
        let expected = held.metadata()?;
        ensure!(
            actual.is_file()
                && expected.is_file()
                && actual.nlink() == 1
                && expected.nlink() == 1
                && actual.uid() == unsafe { libc::geteuid() }
                && (actual.dev(), actual.ino()) == (expected.dev(), expected.ino()),
            "runner output staging inode changed"
        );
        Ok(())
    }

    fn cleanup(&self, name: &str, held: &File) -> Result<()> {
        self.same_stage(name, held)?;
        self.parent
            .remove_file(name)
            .context("remove owned runner staging file")
    }

    fn failure(&self, error: anyhow::Error, name: &str, held: &File) -> anyhow::Error {
        match self.cleanup(name, held) {
            Ok(()) => error,
            Err(cleanup) => anyhow::anyhow!(
                "runner output failed: {error:#}; staging cleanup failed: {cleanup:#}"
            ),
        }
    }

    pub(super) fn publish(
        &self,
        bytes: &[u8],
        verify_inputs: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        ensure!(
            bytes.len() <= crate::candidate_mutation_runner::MAX_MANIFEST_BYTES,
            "runner manifest exceeds1MiB"
        );
        self.verify()?;
        self.absent()?;
        let stage = format!(".{}", crate::resource_id("runner-bundle-pending")?);
        let mut held = self.parent.open_with(
            &stage,
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC),
        )?;
        let result = (|| -> Result<()> {
            held.set_permissions(Permissions::from_mode(0o600))?;
            held.write_all(bytes)?;
            held.sync_all()?;
            self.same_stage(&stage, &held)?;
            ensure!(
                held.metadata()?.mode() & 0o7777 == 0o600,
                "runner output must be mode0600"
            );
            verify_inputs()?;
            self.verify()?;
            let from = CString::new(stage.as_bytes())?;
            let fd = self.parent.as_raw_fd();
            ensure!(
                unsafe {
                    libc::renameat2(
                        fd,
                        from.as_ptr(),
                        fd,
                        self.name.as_ptr(),
                        libc::RENAME_NOREPLACE,
                    )
                } == 0,
                "publish runner output without overwrite: {}",
                io::Error::last_os_error()
            );
            Ok(())
        })();
        if let Err(error) = result {
            return Err(self.failure(error, &stage, &held));
        }
        self.parent
            .open(".")?
            .sync_all()
            .context("runner output published but parent sync failed")?;
        Ok(())
    }
}
