//! Explicit operator-owned native formatter. Ancestors must remain immutable;
//! identity checks detect ordinary replacement, not a hostile same-UID race.
use crate::candidate_source::lines::FileRanges;
use anyhow::{ensure, Context, Result};
use std::{
    fs::{self, Metadata, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    time::Duration,
};

pub struct Tool {
    path: PathBuf,
    stamp: Stamp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mode: u32,
    uid: u32,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
}
fn stamp(meta: &Metadata) -> Stamp {
    Stamp {
        dev: meta.dev(),
        ino: meta.ino(),
        len: meta.len(),
        mode: meta.mode(),
        uid: meta.uid(),
        mtime: meta.mtime(),
        mtime_ns: meta.mtime_nsec(),
        ctime: meta.ctime(),
        ctime_ns: meta.ctime_nsec(),
    }
}
fn inspect(path: &Path) -> Result<Stamp> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.mode() & 0o111 != 0
            && meta.mode() & 0o022 == 0
            && (meta.uid() == 0 || meta.uid() == unsafe { libc::geteuid() }),
        "formatter must be an owned or root-owned executable without group/other writes"
    );
    let mut magic = [0; 4];
    file.read_exact(&mut magic)?;
    ensure!(
        magic == *b"\x7fELF",
        "formatter must be a native ELF executable"
    );
    let expected = stamp(&meta);
    ensure!(
        stamp(&file.metadata()?) == expected && stamp(&fs::symlink_metadata(path)?) == expected,
        "formatter changed during admission"
    );
    Ok(expected)
}

impl Tool {
    pub fn from_env(root: &Path) -> Result<Self> {
        let path = PathBuf::from(
            std::env::var_os("FORGE_CLANG_FORMAT")
                .context("FORGE_CLANG_FORMAT must name an explicitly provisioned native binary")?,
        );
        Self::open(path, root)
    }
    fn open(path: PathBuf, root: &Path) -> Result<Self> {
        let bytes = path.as_os_str().as_encoded_bytes();
        ensure!(
            bytes.len() <= 4096 && !bytes.iter().any(u8::is_ascii_control),
            "formatter path has unsafe bytes or exceeds 4096 bytes"
        );
        ensure!(
            path.is_absolute()
                && path
                    .components()
                    .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
                && path.canonicalize()? == path,
            "formatter path must be canonical and absolute"
        );
        ensure!(
            !path.starts_with(root),
            "formatter must be outside candidate snapshot"
        );
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            let tmp = PathBuf::from(tmp)
                .canonicalize()
                .context("resolve formatter scratch parent")?;
            ensure!(
                !path.starts_with(tmp),
                "formatter must be outside stage scratch"
            );
        }
        let stamp = inspect(&path)?;
        Ok(Self { path, stamp })
    }
    fn command(&self, root: &Path) -> Result<Command> {
        self.verify()?;
        let mut command = Command::new(&self.path);
        command
            .env_clear()
            .env("PATH", "")
            .env("LANG", "C")
            .current_dir(root)
            .stdin(Stdio::null());
        Ok(command)
    }
    fn verify(&self) -> Result<()> {
        ensure!(
            self.path.canonicalize()? == self.path && inspect(&self.path)? == self.stamp,
            "formatter executable changed after admission"
        );
        Ok(())
    }
    pub fn version(&self) -> Result<u8> {
        let mut command = self.command(Path::new("/"))?;
        command.arg("--version");
        let result = crate::process_probe::run(&mut command, Duration::from_secs(30), 16 * 1024)?;
        self.verify()?;
        std::io::stdout().write_all(&result.stdout)?;
        std::io::stderr().write_all(&result.stderr)?;
        status_code(result.status)
    }
    pub fn format(&self, root: &Path, file: &FileRanges, style: &str) -> Result<u8> {
        ensure!(
            !file.ranges.is_empty(),
            "formatter cannot widen an empty line selection"
        );
        let mut command = self.command(root)?;
        command.args([style, "--dry-run", "--Werror"]);
        for range in &file.ranges {
            command.arg(format!("--lines={}:{}", range.start, range.end));
        }
        command.arg(root.join(&file.path));
        let bytes: usize = command
            .get_args()
            .map(|arg| arg.as_encoded_bytes().len() + 1)
            .sum();
        ensure!(bytes <= 128 * 1024, "formatter arguments exceed 128 KiB");
        let status = command.status().context("execute native clang-format")?;
        self.verify()?;
        status_code(status)
    }
}

fn status_code(status: ExitStatus) -> Result<u8> {
    u8::try_from(status.code().context("formatter terminated by signal")?)
        .context("invalid formatter exit status")
}

#[cfg(test)]
#[path = "candidate_clang_tool_tests.rs"]
mod tests;
