//! Offline generator for Unicode 15.0.0 full default case-fold mappings.
//! Usage: generate_candidate_casefold CaseFolding-15.0.0.txt > candidate_unicode_table.rs
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env,
    fs::OpenOptions,
    io::{self, Read, Write},
    path::Path,
};
fn read_source(input: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(input)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err("Unicode source must be a regular file".into());
    }
    if metadata.len() > 256 * 1024 {
        return Err("Unicode source exceeds 256 KiB".into());
    }
    let mut source = String::new();
    file.take(256 * 1024 + 1).read_to_string(&mut source)?;
    if source.len() > 256 * 1024 {
        return Err("Unicode source exceeds 256 KiB".into());
    }
    if format!("{:x}", Sha256::digest(source.as_bytes()))
        != "cdd49e55eae3bbf1f0a3f6580c974a0263cb86a6a08daa10fbf705b4808a56f7"
    {
        return Err("Unicode source SHA-256 mismatch".into());
    }
    if !source.starts_with("# CaseFolding-15.0.0.txt\n") {
        return Err("wrong Unicode source version".into());
    }
    Ok(source)
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = env::args_os()
        .nth(1)
        .ok_or("expected Unicode CaseFolding.txt")?;
    let source = read_source(Path::new(&input))?;
    let mut rows = BTreeMap::new();
    for line in source.lines() {
        let Some(data) = line.split('#').next() else {
            continue;
        };
        let fields: Vec<_> = data.split(';').map(str::trim).collect();
        if fields.len() < 3 || !matches!(fields[1], "C" | "F") {
            continue;
        }
        let point = u32::from_str_radix(fields[0], 16)?;
        if char::from_u32(point).is_none() {
            return Err("invalid source scalar".into());
        }
        let mut mapping = String::new();
        for code in fields[2].split_whitespace() {
            let mapped = u32::from_str_radix(code, 16)?;
            if char::from_u32(mapped).is_none() {
                return Err("invalid mapping scalar".into());
            }
            mapping.push_str(&format!("\\u{{{}}}", code.to_ascii_lowercase()));
        }
        if mapping.is_empty() || rows.insert(point, mapping).is_some() {
            return Err("empty or duplicate full mapping".into());
        }
    }
    if rows.len() != 1530 {
        return Err("unexpected Unicode 15 mapping count".into());
    }
    let mut output = io::BufWriter::new(io::stdout().lock());
    writeln!(output, "// Copyright © 2022 Unicode, Inc. Licensed under the Unicode License; see data/UNICODE-LICENSE.txt.")?;
    writeln!(
        output,
        "// Generated from Unicode 15.0.0 CaseFolding.txt; Unicode data license applies."
    )?;
    writeln!(
        output,
        "// SHA-256 cdd49e55eae3bbf1f0a3f6580c974a0263cb86a6a08daa10fbf705b4808a56f7"
    )?;
    writeln!(
        output,
        "#[rustfmt::skip]\npub(super) static CASE_FOLD: &[(u32, &str)] = &["
    )?;
    let mut count = 0;
    for (point, mapping) in rows {
        if count % 4 == 0 {
            write!(output, "    ")?;
        } else {
            write!(output, " ")?;
        }
        write!(output, "(0x{point:04x}, \"{mapping}\"),")?;
        count += 1;
        if count % 4 == 0 {
            writeln!(output)?;
        }
    }
    if count % 4 != 0 {
        writeln!(output)?;
    }
    writeln!(output, "];")?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{ffi::CString, fs, os::unix::ffi::OsStrExt, path::PathBuf};

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn rejects_untrusted_source_without_blocking_on_fifo() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("forge-unicode-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let _cleanup = Scratch(root.clone());
        assert!(read_source(&root).is_err());
        let input = root.join("input");
        fs::write(&input, "# CaseFolding-15.0.0.txt\ncorrupted").unwrap();
        assert!(read_source(&input)
            .unwrap_err()
            .to_string()
            .contains("SHA-256"));
        fs::write(&input, vec![b'a'; 256 * 1024 + 1]).unwrap();
        assert!(read_source(&input)
            .unwrap_err()
            .to_string()
            .contains("exceeds"));
        let link = root.join("link");
        std::os::unix::fs::symlink(&input, &link).unwrap();
        assert!(read_source(&link).is_err());
        let fifo = root.join("fifo");
        let cpath = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: cpath is a live NUL-terminated path; no pointer escapes mkfifo.
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        assert!(read_source(&fifo)
            .unwrap_err()
            .to_string()
            .contains("regular"));
    }
}
