//! Bounded status entries, serialized publication, and explicit cache refresh.
use crate::{active_state, file_lock::Exclusive, instant, state_publish};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::{Duration, Instant};

const MAX_LOG_BYTES: usize = 1024 * 1024;
const LOCK_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Serialize)]
pub struct AppendOutcome {
    pub entry: String,
    pub active_state_refreshed: bool,
    pub refresh_error: Option<String>,
}

impl AppendOutcome {
    pub fn exit_code(&self) -> u8 {
        if self.active_state_refreshed {
            0
        } else {
            3
        }
    }
}

fn body_lines(body: &str) -> usize {
    let mut lines = 1;
    let mut carriage_return = false;
    for character in body.chars() {
        if character == '\n' {
            lines += usize::from(!carriage_return);
        } else if matches!(
            character,
            '\r' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        ) {
            lines += 1;
        }
        carriage_return = character == '\r';
    }
    lines
}

fn entry(owner: &str, title: &str, body: &str) -> Result<String> {
    ensure!(
        owner.len() <= 256 && title.len() <= 4096 && body.len() <= 8192,
        "status arguments exceed byte limits"
    );
    let owner = owner.trim();
    conductor_native::a2a_store::validate_identity(owner)?;
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    ensure!(!title.is_empty(), "title is required");
    ensure!(title.chars().count() <= 120, "title exceeds 120 chars");
    ensure!(
        !title.chars().any(char::is_control),
        "title contains control characters"
    );
    let body = body.trim();
    ensure!(!body.is_empty(), "body is required");
    ensure!(body.chars().count() <= 1200, "body exceeds 1200 chars");
    ensure!(body_lines(body) <= 12, "body exceeds 12 lines");
    ensure!(
        !body
            .split(['\n', '\r', '\u{b}', '\u{c}', '\u{85}', '\u{2028}', '\u{2029}'])
            .any(|line| line
                .trim_start()
                .strip_prefix("##")
                .is_some_and(|rest| rest.starts_with(char::is_whitespace))),
        "body cannot inject a status heading"
    );
    let stamp = instant::format_ymd_hm(instant::now());
    let (date, time) = stamp.split_once(' ').context("UTC status timestamp")?;
    Ok(format!(
        "## {title} — {date} ~{time} UTC, {owner}\n\n{body}\n\n"
    ))
}

fn safe_file(metadata: &fs::Metadata) -> Result<()> {
    ensure!(
        metadata.is_file(),
        "status destination must be a regular file"
    );
    ensure!(
        metadata.nlink() == 1,
        "status destination must not have hard links"
    );
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "status destination has a different owner"
    );
    Ok(())
}

fn lock(host: &Path) -> Result<Exclusive> {
    let path = host.join(".current_work.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .context("open status writer lock")?;
    safe_file(&file.metadata()?)?;
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match Exclusive::try_new(file.try_clone()?, "status append") {
            Ok(lock) => {
                let named = fs::symlink_metadata(&path)?;
                let opened = file.metadata()?;
                safe_file(&named)?;
                ensure!(
                    named.dev() == opened.dev()
                        && named.ino() == opened.ino()
                        && !named.file_type().is_symlink(),
                    "status lock changed while waiting"
                );
                return Ok(lock);
            }
            Err(error) if Instant::now() >= deadline => {
                return Err(error.context("status writer lock deadline exceeded"))
            }
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

fn read_log(path: &Path) -> Result<String> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok("# Active Coordination\n\n".into())
        }
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    safe_file(&metadata)?;
    ensure!(
        metadata.len() <= MAX_LOG_BYTES as u64,
        "status log exceeds 1 MiB"
    );
    let mut bytes = Vec::new();
    file.take(MAX_LOG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_LOG_BYTES, "status log exceeds 1 MiB");
    String::from_utf8(bytes).context("status log must be UTF-8")
}

fn insert(existing: &str, entry: &str) -> String {
    if let Some(position) = existing
        .strip_prefix("## ")
        .map(|_| 0)
        .or_else(|| existing.find("\n## ").map(|position| position + 1))
    {
        return format!("{}{entry}{}", &existing[..position], &existing[position..]);
    }
    let separator = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    format!("{existing}{separator}{entry}")
}

fn refresh(host: &Path) -> Result<()> {
    let directory = host.join("conductor");
    match fs::symlink_metadata(&directory) {
        Ok(metadata) => ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "active-state directory must not be a symlink or non-directory"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    match fs::symlink_metadata(directory.join("active_state.json")) {
        Ok(metadata) => {
            ensure!(
                !metadata.file_type().is_symlink(),
                "active-state destination must not be a symlink"
            );
            safe_file(&metadata)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    active_state::save_active_state(host)?;
    Ok(())
}

/// Append one bounded heading and refresh derived state while holding the same
/// cooperative writer lock. A refresh error leaves the published entry intact
/// and is returned as exit-code-3 outcome, never as a rejected append.
pub fn append_status(host: &Path, owner: &str, title: &str, body: &str) -> Result<AppendOutcome> {
    let entry = entry(owner, title, body)?;
    let host = host.canonicalize().context("resolve status host")?;
    ensure!(host.is_dir(), "status host must be a directory");
    let _lock = lock(&host)?;
    let path = host.join(".current_work.md");
    let existing = read_log(&path)?;
    let updated = insert(&existing, &entry);
    ensure!(
        updated.len() <= MAX_LOG_BYTES,
        "updated status log exceeds 1 MiB"
    );
    state_publish::publish(&path, updated.as_bytes())
        .context("status publication failed; destination may already be visible")?;
    let refresh_error = refresh(&host).err().map(|error| format!("{error:#}"));
    Ok(AppendOutcome {
        entry,
        active_state_refreshed: refresh_error.is_none(),
        refresh_error,
    })
}
