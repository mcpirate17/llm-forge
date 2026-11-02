//! Capability-relative binding updates. A permanent lock serializes cooperative
//! writers; parent directories and same-UID writers remain operator boundaries.
use crate::branch_policy::{self as policy, Binding};
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, DirBuilder, DirBuilderExt, MetadataExt, OpenOptions, OpenOptionsExt};
use std::{
    io::{Read, Write},
    os::fd::AsRawFd,
    path::Path,
};
const NAME: &str = "branch-bindings.json";
const MAX_BYTES: u64 = 4 * 1024 * 1024;
fn parent(path: &Path, create: bool) -> Result<Option<Dir>> {
    let root = path
        .parent()
        .and_then(Path::parent)
        .context("binding path has no common Git directory")?;
    let common = Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
    match common.symlink_metadata("governance") {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match common.create_dir_with("governance", DirBuilder::new().mode(0o700)) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
        Ok(metadata) => ensure!(
            metadata.is_dir(),
            "binding governance directory cannot be a symlink or special file"
        ),
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let directory = Dir::from_std_file(common.open_with("governance", &options)?.into_std());
    let metadata = directory.dir_metadata()?;
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o022 == 0,
        "binding directory must be owned and not writable by other users"
    );
    Ok(Some(directory))
}
fn text(directory: &Dir) -> Result<Option<String>> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    let mut file = match directory.open_with(NAME, &options) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("branch binding store is unreadable"),
    };
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.nlink() == 1
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o022 == 0
            && metadata.len() <= MAX_BYTES,
        "branch binding store must be a bounded, owned regular file without hardlinks"
    );
    let mut text = String::new();
    (&mut file).take(MAX_BYTES + 1).read_to_string(&mut text)?;
    ensure!(
        text.len() as u64 <= MAX_BYTES,
        "branch binding store exceeds4MiB"
    );
    Ok(Some(text))
}
fn parse(text: Option<&str>) -> Result<Vec<Binding>> {
    let rows =
        policy::validate_store(text.unwrap_or(policy::EMPTY_STORE)).map_err(anyhow::Error::msg)?;
    ensure!(
        rows.len() <= 100_000,
        "branch binding inventory exceeds100000 entries"
    );
    Ok(rows)
}
pub(super) fn load(path: &Path) -> Result<Vec<Binding>> {
    match parent(path, false)? {
        None => Ok(Vec::new()),
        Some(directory) => parse(text(&directory)?.as_deref()),
    }
}
pub(super) fn edit<T>(
    path: &Path,
    operation: impl FnOnce(&mut Vec<Binding>) -> Result<T>,
) -> Result<T> {
    let directory = parent(path, true)?.context("binding directory missing")?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    let lock = directory.open_with(".branch-bindings.lock", &options)?;
    let metadata = lock.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.nlink() == 1
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "binding writer lock is not private"
    );
    ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "another binding writer owns the store: {}",
        std::io::Error::last_os_error()
    );
    let before = text(&directory)?;
    let mut rows = parse(before.as_deref())?;
    let result = operation(&mut rows)?;
    let payload = serde_json::json!({"schema_version":1,"bindings":rows.iter().map(Binding::to_row).collect::<Vec<_>>()});
    let serialized = serde_json::to_string_pretty(&payload)? + "\n";
    ensure!(
        serialized.len() as u64 <= MAX_BYTES,
        "branch binding store exceeds4MiB"
    );
    parse(Some(&serialized))?;
    ensure!(
        text(&directory)? == before,
        "branch binding store changed during update; retry"
    );
    let temporary = crate::native_id::new("branch-pending")?;
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = directory.open_with(&temporary, &options)?;
    let published = (|| -> Result<()> {
        file.write_all(serialized.as_bytes())?;
        file.sync_all()?;
        directory.rename(&temporary, &directory, NAME)?;
        directory.try_clone()?.into_std_file().sync_all()?;
        Ok(())
    })();
    if published.is_err() {
        match directory.remove_file(&temporary) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error).context("remove owned binding temporary"),
        }
    }
    published?;
    Ok(result)
}
