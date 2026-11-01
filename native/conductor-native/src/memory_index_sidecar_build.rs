//! Explicit compilation: one source descriptor, bounded vectors, exclusive
//! publication lease, unique temporary inode, fsync and atomic replacement.
use super::format::{self, Header, Stamp, HEADER_LEN, MAX_FILE, MAX_ROWS, MAX_SOURCE};
use crate::memory_index::{read_line, row_object, validate_preview, validate_row};
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

struct Collected {
    matrix: Vec<f32>,
    offsets: Vec<u64>,
    dims: usize,
    fingerprint: String,
    source_sha: [u8; 32],
}
fn collect(file: &File) -> Result<Collected> {
    let mut reader = BufReader::new(file.take(MAX_SOURCE + 1));
    let mut output = Collected {
        matrix: Vec::new(),
        offsets: Vec::new(),
        dims: 0,
        fingerprint: String::new(),
        source_sha: [0; 32],
    };
    let mut line = Vec::new();
    let mut offset = 0_u64;
    let mut hash = Sha256::new();
    while read_line(&mut reader, &mut line).map_err(anyhow::Error::msg)? != 0 {
        let start = offset;
        offset += line.len() as u64;
        ensure!(offset <= MAX_SOURCE, "memory source exceeds 512 MiB");
        hash.update(&line);
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        ensure!(
            output.offsets.len() < MAX_ROWS,
            "memory source exceeds one million rows"
        );
        let value: Value = serde_json::from_slice(&line)?;
        let row = row_object(&value).map_err(anyhow::Error::msg)?;
        let metadata = validate_row(row).map_err(anyhow::Error::msg)?;
        validate_preview(row).map_err(anyhow::Error::msg)?;
        if output.dims == 0 {
            output.dims = metadata.dimension;
            output.fingerprint = metadata.fingerprint.into();
        }
        ensure!(
            metadata.dimension == output.dims && metadata.fingerprint == output.fingerprint,
            "mixed memory embedding space"
        );
        ensure!(
            (1..=8192).contains(&output.dims),
            "memory dimension exceeds 8192"
        );
        ensure!(
            (output.matrix.len() + output.dims) as u64 * 4
                + (output.offsets.len() + 1) as u64 * 8
                + HEADER_LEN as u64
                <= MAX_FILE,
            "memory snapshot exceeds 2 GiB"
        );
        output.matrix.try_reserve(output.dims)?;
        for cell in row["vector"]
            .as_array()
            .context("vector must be an array")?
        {
            let value = cell.as_f64().context("vector values must be numbers")? as f32;
            ensure!(
                value.is_finite(),
                "sidecar vector values must fit finite f32"
            );
            output.matrix.push(value);
        }
        output.offsets.push(start);
    }
    ensure!(!output.offsets.is_empty(), "memory index is empty");
    output.source_sha = hash.finalize().into();
    Ok(output)
}

fn parent(path: &Path) -> Result<&Path> {
    ensure!(
        path.file_name().is_some(),
        "snapshot output needs a filename"
    );
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let meta = fs::symlink_metadata(parent)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o022 == 0,
        "snapshot directory must be owned and not writable by others"
    );
    Ok(parent)
}
fn validate_target(path: &Path, source: &Stamp) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => ensure!(
            meta.is_file()
                && meta.uid() == unsafe { libc::geteuid() }
                && meta.nlink() == 1
                && meta.mode() & 0o022 == 0
                && (meta.dev(), meta.ino()) != (source.0[0], source.0[1]),
            "unsafe snapshot output or output aliases source"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
fn lock(path: &Path) -> Result<File> {
    let mut name = path.as_os_str().to_os_string();
    name.push(".lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(PathBuf::from(name))?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.nlink() == 1
            && meta.mode() & 0o077 == 0,
        "snapshot lock must be private, owned and singly linked"
    );
    ensure!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "memory snapshot writer is busy"
    );
    Ok(file)
}
fn write(file: &mut File, header: &Header, data: &Collected) -> Result<()> {
    let mut hash = Sha256::new();
    {
        let mut writer = BufWriter::new(&mut *file);
        writer.write_all(&[0; HEADER_LEN])?;
        for value in &data.matrix {
            let bytes = value.to_le_bytes();
            writer.write_all(&bytes)?;
            hash.update(bytes);
        }
        for offset in &data.offsets {
            let bytes = offset.to_le_bytes();
            writer.write_all(&bytes)?;
            hash.update(bytes);
        }
        writer.flush()?;
    }
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header.bytes(hash.finalize().into()))?;
    file.set_permissions(Permissions::from_mode(0o400))?;
    file.sync_all()?;
    Ok(())
}

pub(super) fn build(source: &Path, output: &Path) -> Result<usize> {
    let directory = parent(output)?;
    let input = format::source(source)?;
    let stamp = Stamp::read(&input)?;
    validate_target(output, &stamp)?;
    let _lease = lock(output)?;
    let data = collect(&input)?;
    ensure!(
        Stamp::read(&input)? == stamp,
        "memory source changed during compilation"
    );
    let header = Header::new(
        data.dims,
        data.offsets.len(),
        stamp.clone(),
        data.source_sha,
        data.fingerprint.clone(),
    )?;
    let temporary = directory.join(crate::native_id::new("memory-pending")?);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| -> Result<usize> {
        write(&mut file, &header, &data)?;
        ensure!(
            Stamp::read(&input)? == stamp && Stamp::read(&format::source(source)?)? == stamp,
            "memory source changed before publication"
        );
        validate_target(output, &stamp)?;
        fs::rename(&temporary, output)?;
        File::open(directory)?
            .sync_all()
            .context("snapshot published but directory sync failed")?;
        Ok(header.rows)
    })();
    if result.is_err() {
        if let Err(error) = fs::remove_file(&temporary) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("memory snapshot cleanup: {error}");
            }
        }
    }
    result
}
