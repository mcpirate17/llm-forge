//! Bounded native session journal, retaining the existing TSV accumulator format.
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const ACCUM_LIMIT: usize = 1024 * 1024;
const DAILY_LIMIT: usize = 8 * 1024 * 1024;

fn root(host: &Path) -> Result<PathBuf> {
    let path = std::env::var_os("FORGE_JOURNAL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/claude-session-journal"));
    let path = if path.is_absolute() {
        path
    } else {
        host.join(path)
    };
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&path)?;
    let meta = fs::symlink_metadata(&path)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() },
        "journal state must be an owned directory"
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path)
}
fn session_id(sid: &str) -> Result<()> {
    ensure!(
        !sid.is_empty()
            && sid.len() <= 128
            && sid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid session journal ID"
    );
    Ok(())
}
fn open_owned(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.uid() == unsafe { libc::geteuid() } && meta.nlink() == 1,
        "journal file must be owned, regular and singly linked"
    );
    Ok(file)
}
fn lock(path: &Path) -> Result<File> {
    let file = open_owned(path)?;
    ensure!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "session journal is busy; retry"
    );
    Ok(file)
}

pub(super) fn append(host: &Path, sid: &str, line: &str) -> Result<()> {
    session_id(sid)?;
    let body = line
        .strip_suffix('\n')
        .context("journal record requires newline")?;
    ensure!(
        !body.contains(['\n', '\r']) && body.split('\t').count() == 3,
        "journal record contains unsupported separators"
    );
    records(body)?;
    let root = root(host)?;
    let _lock = lock(&root.join(format!("{sid}.lock")))?;
    let mut file = open_owned(&root.join(format!("{sid}.tsv")))?;
    ensure!(
        file.metadata()?.len() + line.len() as u64 <= ACCUM_LIMIT as u64,
        "session journal exceeds 1 MiB"
    );
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.seek(SeekFrom::End(0))?;
    file.write_all(line.as_bytes())?;
    file.sync_all()?;
    File::open(root)?.sync_all()?;
    Ok(())
}

fn records(text: &str) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let mut edits = BTreeSet::new();
    let mut memories = BTreeSet::new();
    for line in text.lines() {
        let parts: Vec<_> = line.split('\t').collect();
        ensure!(
            parts.len() == 3 && crate::instant::parse(parts[0]).is_some() && parts[2].len() <= 4096,
            "malformed session journal record"
        );
        match parts[1] {
            "edit" => {
                edits.insert(parts[2].to_owned());
            }
            "memory" => {
                memories.insert(parts[2].to_owned());
            }
            "memory-index" => (),
            _ => anyhow::bail!("unknown session journal record kind"),
        }
        ensure!(
            edits.len() + memories.len() <= 4096,
            "too many session journal paths"
        );
    }
    Ok((edits, memories))
}
fn inline(value: &str) -> String {
    let bounded: String = value.chars().take(4096).collect();
    let escaped = serde_json::to_string(&bounded).expect("string JSON");
    escaped[1..escaped.len() - 1].replace('`', "\\`")
}
fn git_context(host: &Path) -> Result<(String, String, String)> {
    let repo = match git2::Repository::open(host) {
        Ok(repo) => repo,
        Err(e) if e.code() == git2::ErrorCode::NotFound => {
            return Ok(("?".into(), "?".into(), String::new()))
        }
        Err(e) => return Err(e.into()),
    };
    let head = match repo.head() {
        Ok(head) => Some(head),
        Err(error)
            if matches!(
                error.code(),
                git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound
            ) =>
        {
            None
        }
        Err(error) => return Err(error).context("read journal repository HEAD"),
    };
    let branch = match &head {
        Some(head) => head.shorthand()?.to_owned(),
        None => "?".into(),
    };
    let commit = head
        .as_ref()
        .and_then(|h| h.target())
        .map(|id| id.to_string()[..7].to_owned())
        .unwrap_or_else(|| "?".into());
    let mut options = git2::StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    let statuses = repo.statuses(Some(&mut options))?;
    let mut status = String::new();
    for entry in statuses.iter().take(256) {
        let path = String::from_utf8_lossy(entry.path_bytes());
        let flags = entry.status();
        status.push_str(&format!("{:?} {}\n", flags, inline(&path)));
        if status.len() > 32768 {
            status.push_str("[remaining status omitted]\n");
            break;
        }
    }
    if statuses.len() > 256 {
        status.push_str("[remaining status omitted]\n");
    }
    Ok((branch, commit, status))
}

fn block(host: &Path, sid: &str, reason: &str, text: &str, now: &str) -> Result<String> {
    let (edits, memories) = records(text)?;
    let (branch, head, status) = git_context(host)?;
    let mut lines = vec![
        format!("## {now} — session `{}`", &sid[..sid.len().min(8)]),
        format!("- Branch: `{}`  HEAD: `{head}`", inline(&branch)),
    ];
    if !reason.is_empty() {
        lines.push(format!("- End reason: `{}`", inline(reason)));
    }
    if !memories.is_empty() {
        lines.push(format!("\n### Memory writes ({})", memories.len()));
        for path in memories {
            lines.push(format!(
                "- [[memory/{}]] — `{}`",
                super::slug(
                    Path::new(&path)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("memory")
                ),
                inline(&path)
            ));
        }
    }
    if !edits.is_empty() {
        lines.push(format!("\n### Files edited ({})", edits.len()));
        for path in edits {
            lines.push(format!("- `{}`", inline(&path)));
        }
    }
    if !status.is_empty() {
        lines.push(format!(
            "\n### git status at session end\n```text\n{status}```"
        ));
    }
    Ok(lines.join("\n") + "\n")
}

fn publish(vault: &Path, day: &str, body: &str, identity: &str) -> Result<()> {
    fs::create_dir_all(vault)?;
    let _lock = lock(&vault.join(format!(".{day}.lock")))?;
    let mut file = open_owned(&vault.join(format!("{day}.md")))?;
    ensure!(
        file.metadata()?.len() <= DAILY_LIMIT as u64,
        "daily journal exceeds 8 MiB"
    );
    let mut existing = String::new();
    (&mut file)
        .take(DAILY_LIMIT as u64 + 1)
        .read_to_string(&mut existing)?;
    ensure!(existing.len() <= DAILY_LIMIT, "daily journal exceeds 8 MiB");
    let marker = format!("<!-- forge-session:{identity} -->");
    if existing.lines().any(|line| line == marker) {
        // A preceding publication may have renamed successfully but failed its
        // directory sync. Do not consume the accumulator until retry seals it.
        file.sync_all()?;
        File::open(vault)?.sync_all()?;
        return Ok(());
    }
    let header = if existing.is_empty() {
        format!("---\ndate: {day}\nsource: claude-code\ntags: [source/claude-code, journal/daily]\n---\n\n# {day} — Claude Code Sessions\n\n")
    } else {
        String::new()
    };
    let addition = format!("{header}\n{marker}\n{body}\n");
    ensure!(
        existing.len() + addition.len() <= DAILY_LIMIT,
        "daily journal exceeds 8 MiB"
    );
    // Publish the whole bounded day atomically. An interrupted append could
    // otherwise leave a complete retry marker ahead of a partial session body.
    replace_day(vault, day, &existing, &addition)
}

fn replace_day(vault: &Path, day: &str, existing: &str, addition: &str) -> Result<()> {
    let temporary = vault.join(forge::resource_id("journal-pending")?);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let outcome = (|| -> Result<()> {
        file.write_all(existing.as_bytes())?;
        file.write_all(addition.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, vault.join(format!("{day}.md")))?;
        File::open(vault)?.sync_all()?;
        Ok(())
    })();
    if outcome.is_err() {
        if let Err(error) = fs::remove_file(&temporary) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("forge: journal staging cleanup: {error}");
            }
        }
    }
    outcome
}

pub(super) fn run(host: &Path, payload: &Value) -> Result<()> {
    ensure!(
        payload.is_object(),
        "session journal payload must be an object"
    );
    let sid = payload
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown");
    session_id(sid)?;
    let reason = payload.get("reason").and_then(Value::as_str).unwrap_or("");
    ensure!(
        reason.len() <= 4096,
        "session end reason exceeds 4096 bytes"
    );
    let root = root(host)?;
    let _lock = lock(&root.join(format!("{sid}.lock")))?;
    let path = root.join(format!("{sid}.tsv"));
    let text = match fs::symlink_metadata(&path) {
        Ok(_) => {
            let file = open_owned(&path)?;
            ensure!(
                file.metadata()?.len() <= ACCUM_LIMIT as u64,
                "session journal exceeds 1 MiB"
            );
            let mut text = String::new();
            file.take(ACCUM_LIMIT as u64 + 1)
                .read_to_string(&mut text)?;
            ensure!(text.len() <= ACCUM_LIMIT, "session journal exceeds 1 MiB");
            text
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    if text.is_empty() && reason.is_empty() {
        return Ok(());
    }
    let now = crate::instant::isoformat_utc(crate::instant::now());
    let body = block(host, sid, reason, &text, &now)?;
    let identity = format!("{:x}", Sha256::digest(format!("{sid}\0{reason}\0{text}")));
    publish(&super::vault_root(host), &now[..10], &body, &identity)?;
    match fs::remove_file(path) {
        Ok(()) => File::open(root)?.sync_all()?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
