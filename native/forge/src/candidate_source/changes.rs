use super::{budget, safe_path, Change, Source, EMPTY_TREE, MAX_ENTRIES};
use anyhow::{ensure, Context, Result};
use git2::{Delta, DiffDelta, DiffFindOptions, DiffFormat, DiffOptions, Oid};

impl Source {
    pub(crate) fn changes(&self, base: &str, target: &str) -> Result<Vec<Change>> {
        // Validate/count tree metadata before libgit2 allocates the combined diff.
        self.list_tree(base)?;
        self.list_tree(target)?;
        let old = if base == EMPTY_TREE {
            None
        } else {
            Some(self.repository.find_tree(Oid::from_str(base)?)?)
        };
        let new = if target == EMPTY_TREE {
            None
        } else {
            Some(self.repository.find_tree(Oid::from_str(target)?)?)
        };
        let mut options = DiffOptions::new();
        options.include_typechange(true).id_abbrev(40);
        let mut diff =
            self.repository
                .diff_tree_to_tree(old.as_ref(), new.as_ref(), Some(&mut options))?;
        ensure!(
            diff.deltas().len() <= MAX_ENTRIES,
            "candidate diff exceeds 100000 entries"
        );
        let odb = self.repository.odb()?;
        let mut examined_bytes = 0u64;
        for delta in diff.deltas() {
            for file in [delta.old_file(), delta.new_file()] {
                if file.id().is_zero() || file.mode() == git2::FileMode::Commit {
                    continue;
                }
                let (size, _) = odb.read_header(file.id())?;
                ensure!(
                    size <= 64 * 1024 * 1024,
                    "candidate similarity blob exceeds 64 MiB"
                );
                examined_bytes = examined_bytes
                    .checked_add(size as u64)
                    .context("candidate similarity size overflow")?;
                ensure!(
                    examined_bytes <= 2 * 1024 * 1024 * 1024,
                    "candidate similarity inputs exceed 2 GiB"
                );
            }
        }
        let mut find = DiffFindOptions::new();
        find.renames(true)
            .copies(true)
            .rename_threshold(50)
            .copy_threshold(50)
            .rename_limit(1000);
        diff.find_similar(Some(&mut find))?;
        let mut changes = Vec::new();
        let mut bytes = 0usize;
        let mut failure = None;
        diff.print(DiffFormat::Raw, |delta, _, line| {
            let result = change(delta, line.content()).and_then(|change| {
                budget(
                    &mut bytes,
                    change.path.len() + change.old_path.as_ref().map_or(0, String::len) + 240,
                )?;
                ensure!(
                    changes.len() < MAX_ENTRIES,
                    "candidate diff exceeds 100000 entries"
                );
                changes.push(change);
                Ok(())
            });
            if let Err(error) = result {
                failure = Some(error);
                false
            } else {
                true
            }
        })
        .or_else(|error| {
            if failure.is_some() {
                Ok(())
            } else {
                Err(error)
            }
        })?;
        if let Some(error) = failure {
            return Err(error);
        }
        changes.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(changes)
    }
}

fn change(delta: DiffDelta<'_>, raw: &[u8]) -> Result<Change> {
    // git2 does not expose DiffDelta.similarity. Its RAW header preserves the
    // exact libgit2 score (R050/C100); paths come from typed byte fields instead.
    let header = raw
        .split(|byte| *byte == b'\t')
        .next()
        .context("missing raw Git diff header")?;
    let header = std::str::from_utf8(header).context("Git diff metadata must be UTF-8")?;
    let fields: Vec<_> = header.split_ascii_whitespace().collect();
    ensure!(
        fields.len() == 5 && fields[0].starts_with(':'),
        "malformed libgit2 raw diff header"
    );
    let status = fields[4];
    ensure!(
        !status.is_empty() && status.bytes().all(|b| b.is_ascii_alphanumeric()),
        "invalid raw Git diff status"
    );
    let old = delta.old_file();
    let new = delta.new_file();
    let path = if delta.status() == Delta::Deleted {
        old.path_bytes()
    } else {
        new.path_bytes()
    };
    let path = safe_path(path.context("Git change has no path")?)?.to_owned();
    let old_path = if matches!(delta.status(), Delta::Renamed | Delta::Copied) {
        Some(safe_path(old.path_bytes().context("Git rename/copy has no source")?)?.to_owned())
    } else {
        None
    };
    Ok(Change {
        status: status.into(),
        path,
        old_path,
        old_mode: fields[0][1..].into(),
        new_mode: fields[1].into(),
        old_oid: old.id().to_string(),
        new_oid: new.id().to_string(),
        classes: Vec::new(),
        risk: "normal".into(),
    })
}
