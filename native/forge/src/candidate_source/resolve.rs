use super::{Candidate, Kind, Source, EMPTY_TREE, MAX_ENTRIES, MAX_OUTPUT};
use anyhow::{ensure, Context, Result};
use git2::{ErrorCode, Index, Oid};
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};

fn valid_ref(reference: &str) -> Result<()> {
    ensure!(
        !reference.is_empty()
            && reference.len() <= 4096
            && !reference.starts_with('-')
            && !reference.contains('\0'),
        "invalid or ambiguous Git ref"
    );
    Ok(())
}

// Check declared count and expanded v4 path sizes before libgit2 expands the
// index. A small compressed index must not allocate an enormous path inventory.
fn preflight_index(raw: &[u8]) -> Result<()> {
    ensure!(raw.len() <= MAX_OUTPUT, "Git index exceeds 64 MiB");
    ensure!(
        raw.len() >= 32 && &raw[..4] == b"DIRC",
        "invalid Git index header"
    );
    let version = u32::from_be_bytes(raw[4..8].try_into()?);
    let count = u32::from_be_bytes(raw[8..12].try_into()?) as usize;
    ensure!(
        (2..=4).contains(&version) && count <= MAX_ENTRIES,
        "unsupported Git index version or excessive entry count"
    );
    let end = raw.len() - 20;
    let mut cursor = 12usize;
    let mut previous = 0usize;
    let mut total = 0usize;
    for _ in 0..count {
        let start = cursor;
        ensure!(
            cursor.checked_add(62).is_some_and(|n| n <= end),
            "truncated Git index entry"
        );
        let extended = raw[cursor + 60] & 0x40 != 0;
        cursor += 62;
        if extended {
            ensure!(
                version >= 3 && cursor + 2 <= end,
                "invalid extended Git index entry"
            );
            cursor += 2;
        }
        let removed = if version == 4 {
            index_varint(&raw[..end], &mut cursor)?
        } else {
            previous
        };
        ensure!(
            removed <= previous,
            "invalid compressed Git index path prefix"
        );
        let suffix = raw[cursor..end]
            .iter()
            .position(|b| *b == 0)
            .context("unterminated Git index path")?;
        previous = (previous - removed)
            .checked_add(suffix)
            .context("Git index path size overflow")?;
        ensure!(
            previous <= super::MAX_PATH,
            "Git index path exceeds 4096 bytes"
        );
        super::budget(&mut total, previous + 160)?;
        cursor += suffix + 1;
        if version != 4 {
            cursor = start
                .checked_add((cursor - start + 7) & !7)
                .context("Git index entry alignment overflow")?;
        }
        ensure!(cursor <= end, "truncated padded Git index entry");
    }
    Ok(())
}

pub(super) fn read_index(path: &Path) -> Result<Index> {
    match crate::bounded_file::read(path, MAX_OUTPUT) {
        Ok(raw) => parse_index(&raw),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(Index::new()?)
        }
        Err(error) => Err(error).context("read candidate Git index"),
    }
}

/// libgit2 reopens filenames, so give it an immutable private copy rather than
/// the mutable admitted path. No index bytes or temporary files reach disk.
pub(crate) fn parse_index(raw: &[u8]) -> Result<Index> {
    preflight_index(raw)?;
    let descriptor = unsafe {
        libc::memfd_create(
            c"forge-candidate-index".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    ensure!(
        descriptor >= 0,
        "create private candidate index: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: memfd_create returned a new descriptor owned by this File.
    let mut file = unsafe { File::from_raw_fd(descriptor) };
    file.write_all(raw)
        .context("copy admitted candidate index")?;
    let seals = libc::F_SEAL_SEAL | libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK;
    ensure!(
        unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } == 0,
        "seal admitted candidate index: {}",
        std::io::Error::last_os_error()
    );
    let path = format!("/proc/self/fd/{}", file.as_raw_fd());
    let index = Index::open(Path::new(&path))?;
    ensure!(
        index.len() <= MAX_ENTRIES && !index.has_conflicts(),
        "Git index exceeds entry limit or has unresolved conflicts"
    );
    let mut bytes = 0usize;
    for entry in index.iter() {
        super::safe_path(&entry.path)?;
        super::budget(&mut bytes, entry.path.len() + 160)?;
    }
    Ok(index)
}

fn index_varint(raw: &[u8], cursor: &mut usize) -> Result<usize> {
    let mut byte = *raw
        .get(*cursor)
        .context("truncated compressed Git index path")?;
    *cursor += 1;
    let mut value = usize::from(byte & 0x7f);
    while byte & 0x80 != 0 {
        byte = *raw
            .get(*cursor)
            .context("truncated compressed Git index path")?;
        *cursor += 1;
        value = value
            .checked_add(1)
            .and_then(|n| n.checked_mul(128))
            .and_then(|n| n.checked_add(usize::from(byte & 0x7f)))
            .context("compressed Git index prefix overflow")?;
    }
    Ok(value)
}

impl Source {
    pub fn resolve_commit(&self, reference: &str) -> Result<String> {
        valid_ref(reference)?;
        let simple = reference
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b));
        let full_oid =
            matches!(reference.len(), 40 | 64) && reference.bytes().all(|b| b.is_ascii_hexdigit());
        let mut matches = Vec::new();
        if !reference.starts_with("refs/") && reference != "HEAD" && simple && !full_oid {
            for namespace in ["refs/heads/", "refs/tags/", "refs/remotes/", "refs/"] {
                let exact = format!("{namespace}{reference}");
                match self.repository.find_reference(&exact) {
                    Ok(_) => matches.push(exact),
                    Err(error)
                        if error.code() == ErrorCode::NotFound
                            || error.code() == ErrorCode::InvalidSpec => {}
                    Err(error) => return Err(error).context("inspect exact Git ref namespace"),
                }
            }
            ensure!(
                matches.len() <= 1,
                "ambiguous Git ref; use one exact refs/ namespace"
            );
        }
        let selected = matches.first().map(String::as_str).unwrap_or(reference);
        Ok(self
            .repository
            .revparse_single(selected)
            .context("resolve candidate Git ref")?
            .peel_to_commit()
            .context("Git ref must name a commit")?
            .id()
            .to_string())
    }

    pub fn resolve(
        &self,
        kind: Kind,
        target_ref: &str,
        base_ref: Option<&str>,
    ) -> Result<Candidate> {
        if kind == Kind::Index {
            return self.resolve_index(base_ref);
        }
        let target = self.resolve_commit(target_ref)?;
        let commit = self.repository.find_commit(Oid::from_str(&target)?)?;
        let base = match kind {
            Kind::Commit => {
                if let Some(base) = base_ref.filter(|s| !s.is_empty()) {
                    Some(self.resolve_commit(base)?)
                } else {
                    ensure!(
                        commit.parent_count() <= 1,
                        "merge commits require an explicit --base-ref"
                    );
                    if commit.parent_count() == 0 {
                        None
                    } else {
                        Some(commit.parent_id(0)?.to_string())
                    }
                }
            }
            Kind::Range => {
                let base = base_ref
                    .filter(|s| !s.is_empty())
                    .context("range candidates require an explicit --base-ref")?;
                let base = Oid::from_str(&self.resolve_commit(base)?)?;
                Some(
                    self.repository
                        .merge_base(base, commit.id())
                        .context("candidate range has no merge base")?
                        .to_string(),
                )
            }
            Kind::Index => unreachable!(),
        };
        let base_tree = base
            .as_ref()
            .map(|oid| self.commit_tree(oid))
            .transpose()?
            .unwrap_or_else(|| EMPTY_TREE.into());
        let (integration_base_oid, integration_base_detail) = match &base {
            Some(base) => self.ancestor(
                base,
                &target,
                if kind == Kind::Commit {
                    "commit candidate base"
                } else {
                    "range candidate base"
                },
            )?,
            None => (None, "commit candidate has no base commit".into()),
        };
        let tree_oid = commit.tree_id().to_string();
        let changes = self.changes(&base_tree, &tree_oid)?;
        Ok(Candidate {
            kind,
            tree_oid,
            base_tree_oid: base_tree,
            base_commit_oid: base,
            commit_oid: Some(target),
            target_ref: Some(target_ref.into()),
            changes,
            integration_base_oid,
            integration_base_detail,
        })
    }

    fn index(&self) -> Result<Index> {
        let path = match std::env::var_os("GIT_INDEX_FILE") {
            Some(path) => {
                let path = PathBuf::from(path);
                if path.is_absolute() {
                    path
                } else {
                    self.root.join(path)
                }
            }
            None => self.repository.path().join("index"),
        };
        ensure!(
            path.as_os_str().len() <= 4096,
            "Git index path exceeds 4096 bytes"
        );
        read_index(&path)
    }

    fn resolve_index(&self, base_ref: Option<&str>) -> Result<Candidate> {
        let head = match self.repository.head() {
            Ok(head) => Some(head.peel_to_commit()?.id().to_string()),
            Err(error)
                if error.code() == ErrorCode::UnbornBranch
                    || error.code() == ErrorCode::NotFound =>
            {
                None
            }
            Err(error) => return Err(error).context("resolve candidate HEAD"),
        };
        let base = match base_ref {
            Some(base) => Some(self.resolve_commit(base)?),
            None => head.clone(),
        };
        let base_tree = base
            .as_ref()
            .map(|oid| self.commit_tree(oid))
            .transpose()?
            .unwrap_or_else(|| EMPTY_TREE.into());
        let tree = self.index()?.write_tree_to(&self.repository)?.to_string();
        let (integration_base_oid, integration_base_detail) = match &head {
            None => (None, "HEAD names no commit".into()),
            Some(head) => match base_ref {
                Some(reference) => self.ancestor(
                    base.as_deref().context("explicit base is missing")?,
                    head,
                    &format!("explicit base ref {reference}"),
                )?,
                None => self.integration_base(head)?,
            },
        };
        let changes = self.changes(&base_tree, &tree)?;
        Ok(Candidate {
            kind: Kind::Index,
            tree_oid: tree,
            base_tree_oid: base_tree,
            base_commit_oid: base,
            commit_oid: None,
            target_ref: None,
            changes,
            integration_base_oid,
            integration_base_detail,
        })
    }

    fn ancestor(&self, base: &str, target: &str, detail: &str) -> Result<(Option<String>, String)> {
        let same = base == target;
        let ancestry = self
            .repository
            .graph_descendant_of(Oid::from_str(target)?, Oid::from_str(base)?);
        match ancestry {
            Ok(true) => Ok((Some(base.into()), detail.into())),
            Ok(false) if same => Ok((Some(base.into()), detail.into())),
            Ok(false) | Err(_) => Ok((
                None,
                format!(
                    "{} ({detail}) is not an ancestor of {}",
                    &base[..12],
                    &target[..12]
                ),
            )),
        }
    }

    fn integration_base(&self, target: &str) -> Result<(Option<String>, String)> {
        let configured = std::env::var("CONDUCTOR_INTEGRATION_REF").unwrap_or_default();
        let references = if configured.trim().is_empty() {
            let branch = conductor_native::project_paths::integration_branch(&self.root)
                .map_err(anyhow::Error::msg)?;
            valid_ref(&branch)?;
            vec![format!("origin/{branch}"), branch]
        } else {
            valid_ref(configured.trim())?;
            vec![configured.trim().to_owned()]
        };
        for reference in &references {
            let Ok(commit) = self.resolve_commit(reference) else {
                continue;
            };
            let base = match self
                .repository
                .merge_base(Oid::from_str(&commit)?, Oid::from_str(target)?)
            {
                Ok(base) => base.to_string(),
                Err(_) => {
                    return Ok((
                        None,
                        format!("no merge base between {reference} and {}", &target[..12]),
                    ))
                }
            };
            return self.ancestor(&base, target, &format!("merge base with {reference}"));
        }
        Ok((
            None,
            format!(
                "no integration ref resolved (tried {})",
                references.join(", ")
            ),
        ))
    }
}
