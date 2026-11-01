//! Explicit operator-owned native Clang tool. Ancestors must remain immutable;
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
        "native Clang tool must be an owned or root-owned executable without group/other writes"
    );
    let mut magic = [0; 4];
    file.read_exact(&mut magic)?;
    ensure!(
        magic == *b"\x7fELF",
        "native Clang tool must be a native ELF executable"
    );
    let expected = stamp(&meta);
    ensure!(
        stamp(&file.metadata()?) == expected && stamp(&fs::symlink_metadata(path)?) == expected,
        "native Clang tool changed during admission"
    );
    Ok(expected)
}

impl Tool {
    pub fn from_env(root: &Path) -> Result<Self> {
        Self::from_env_key(root, "FORGE_CLANG_FORMAT")
    }
    pub(crate) fn from_env_key(root: &Path, key: &str) -> Result<Self> {
        let path =
            PathBuf::from(std::env::var_os(key).with_context(|| {
                format!("{key} must name an explicitly provisioned native binary")
            })?);
        Self::open(path, root)
    }
    fn open(path: PathBuf, root: &Path) -> Result<Self> {
        let bytes = path.as_os_str().as_encoded_bytes();
        ensure!(
            bytes.len() <= 4096 && !bytes.iter().any(u8::is_ascii_control),
            "native Clang tool path has unsafe bytes or exceeds 4096 bytes"
        );
        ensure!(
            path.is_absolute()
                && path
                    .components()
                    .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
                && path.canonicalize()? == path,
            "native Clang tool path must be canonical and absolute"
        );
        ensure!(
            !path.starts_with(root),
            "native Clang tool must be outside candidate snapshot"
        );
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            let tmp = PathBuf::from(tmp)
                .canonicalize()
                .context("resolve Clang tool scratch parent")?;
            ensure!(
                !path.starts_with(tmp),
                "native Clang tool must be outside stage scratch"
            );
        }
        let stamp = inspect(&path)?;
        Ok(Self { path, stamp })
    }
    pub(crate) fn command(&self, root: &Path) -> Result<Command> {
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
    pub(crate) fn verify(&self) -> Result<()> {
        ensure!(
            self.path.canonicalize()? == self.path && inspect(&self.path)? == self.stamp,
            "Clang tool executable changed after admission"
        );
        Ok(())
    }
    pub fn version(&self) -> Result<u8> {
        self.print_version(false)
    }
    pub(crate) fn tidy_version(&self) -> Result<u8> {
        self.print_version(true)
    }
    fn print_version(&self, tidy: bool) -> Result<u8> {
        let mut command = self.command(Path::new("/"))?;
        command.arg("--version");
        let result = crate::process_probe::run(&mut command, Duration::from_secs(30), 16 * 1024)?;
        self.verify()?;
        if tidy && result.status.success() {
            let output =
                std::str::from_utf8(&result.stdout).context("clang-tidy version must be UTF-8")?;
            let version = output
                .lines()
                .map(str::trim)
                .find(|line| line.starts_with("LLVM version "))
                .context("clang-tidy did not report an LLVM version")?;
            ensure!(
                version.len() <= 256 && !version.bytes().any(|b| b.is_ascii_control()),
                "invalid clang-tidy version"
            );
            println!("{version}");
        } else {
            std::io::stdout().write_all(&result.stdout)?;
        }
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

pub(crate) fn status_code(status: ExitStatus) -> Result<u8> {
    u8::try_from(
        status
            .code()
            .context("native Clang tool terminated by signal")?,
    )
    .context("invalid Clang tool exit status")
}

#[cfg(test)]
#[path = "candidate_clang_tool_tests.rs"]
mod tests;
