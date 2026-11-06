//! Pure framing for complete, independently enumerated snapshot inputs.
//! This proves neither enumeration completeness nor compiler/environment identity.
use crate::mutation_cargo_supporting_binding::SupportingInputs;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const MAX_LINKS: usize = 4096;
const MAX_LINK_BYTES: usize = 16 * 1024 * 1024;
const MAX_LINK_WORK: usize = 2_000_000;

fn frame(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

/// Validate a canonical repository-relative path without resolving filesystem links.
pub fn validate_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > 4096
        || path.contains('\0')
        || path.split('/').any(|part| {
            part.is_empty() || part == "." || part == ".." || part.eq_ignore_ascii_case(".git")
        })
    {
        return Err("invalid snapshot input path".into());
    }
    Ok(())
}

/// Strict descendant of an independently selected repository-relative namespace.
pub fn within_namespace(path: &str, root: &str) -> bool {
    validate_path(path).is_ok()
        && validate_path(root).is_ok()
        && path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

/// One deterministic identity over a coordinator-derived exact exclusion set and sorted entries.
pub struct Builder {
    hash: Sha256,
    last: Option<String>,
    files: usize,
    excluded: BTreeSet<String>,
    links: BTreeMap<String, String>,
    link_bytes: usize,
}
impl Builder {
    pub fn new(excluded: &BTreeSet<String>) -> Result<Self, String> {
        if excluded.len() > 4096 {
            return Err("too many snapshot exclusions".into());
        }
        let mut hash = Sha256::new();
        frame(&mut hash, b"forge.snapshot.inputs.v2");
        for path in excluded {
            validate_path(path)?;
            frame(&mut hash, path.as_bytes());
        }
        frame(&mut hash, b"entries");
        Ok(Self {
            hash,
            last: None,
            files: 0,
            excluded: excluded.clone(),
            links: BTreeMap::new(),
            link_bytes: 0,
        })
    }
    pub fn add(&mut self, path: &str, mode: u32, bytes: &[u8]) -> Result<(), String> {
        validate_path(path)?;
        if !matches!(mode, 0o100644 | 0o100755 | 0o120000) {
            return Err("unsupported snapshot input mode".into());
        }
        if self.last.as_deref().is_some_and(|old| old >= path) {
            return Err("snapshot entries must be strictly sorted".into());
        }
        self.files = self
            .files
            .checked_add(1)
            .ok_or("snapshot input count overflow")?;
        if self.files > 100_000 {
            return Err("snapshot input count exceeds bound".into());
        }
        if mode == 0o120000 {
            let target =
                std::str::from_utf8(bytes).map_err(|_| "snapshot link target must be UTF-8")?;
            if target.is_empty()
                || target.len() > 4096
                || target.starts_with('/')
                || target.contains('\0')
            {
                return Err("unsupported snapshot link target".into());
            }
            if self.links.len() >= MAX_LINKS {
                return Err("snapshot link count exceeds bound".into());
            }
            self.link_bytes = self
                .link_bytes
                .checked_add(path.len())
                .and_then(|n| n.checked_add(target.len()))
                .ok_or("snapshot link bytes overflow")?;
            if self.link_bytes > MAX_LINK_BYTES {
                return Err("snapshot link bytes exceed bound".into());
            }
            self.links.insert(path.to_owned(), target.to_owned());
        }
        frame(&mut self.hash, path.as_bytes());
        frame(&mut self.hash, &mode.to_be_bytes());
        frame(&mut self.hash, &Sha256::digest(bytes));
        self.last = Some(path.to_owned());
        Ok(())
    }
    pub fn finish(self) -> Result<SupportingInputs, String> {
        let mut work = 0;
        for path in self.links.keys() {
            validate_link(path, &self.links, &self.excluded, &mut work)?;
        }
        Ok(SupportingInputs {
            schema_version: 2,
            algorithm: "sha256-framed-path-content-v1".into(),
            scope: "complete-snapshot-inputs-v2".into(),
            digest: format!("{:x}", self.hash.finalize()),
            files: self.files,
        })
    }
}

fn charge(work: &mut usize, amount: usize) -> Result<(), String> {
    *work = work
        .checked_add(amount)
        .ok_or("snapshot link work overflow")?;
    if *work > MAX_LINK_WORK {
        return Err("snapshot link resolution work exceeds bound".into());
    }
    Ok(())
}

fn excluded(path: &str, outputs: &BTreeSet<String>, work: &mut usize) -> Result<bool, String> {
    charge(work, 1)?;
    if outputs.contains(path) {
        return Ok(true);
    }
    for (index, _) in path.match_indices('/') {
        charge(work, 1)?;
        if outputs.contains(&path[..index]) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn ancestor(path: &str, outputs: &BTreeSet<String>, work: &mut usize) -> Result<bool, String> {
    charge(work, 1)?;
    if path.is_empty() {
        return Ok(!outputs.is_empty());
    }
    let prefix = format!("{path}/");
    Ok(outputs
        .range(prefix.clone()..)
        .next()
        .is_some_and(|output| output.starts_with(&prefix)))
}

/// Resolve included links and directory prefixes with one global work budget.
fn validate_link(
    path: &str,
    links: &BTreeMap<String, String>,
    outputs: &BTreeSet<String>,
    work: &mut usize,
) -> Result<(), String> {
    if ancestor(path, outputs, work)? {
        return Err("snapshot symlink is an ancestor of excluded output".into());
    }
    charge(work, path.len())?;
    let mut pending: Vec<String> = path.split('/').map(str::to_owned).rev().collect();
    let mut resolved: Vec<String> = Vec::new();
    let mut expansions = 0usize;
    while let Some(part) = pending.pop() {
        charge(work, 1)?;
        match part.as_str() {
            "" | "." => continue,
            ".." => {
                if resolved.pop().is_none() {
                    return Err("snapshot symlink escapes root".into());
                }
                continue;
            }
            _ => resolved.push(part),
        }
        charge(work, resolved.len())?;
        let size = resolved
            .iter()
            .try_fold(0usize, |n, part| n.checked_add(part.len() + 1))
            .ok_or("snapshot link path size overflow")?;
        if size > 4097 {
            return Err("snapshot symlink path exceeds bound".into());
        }
        charge(work, size)?;
        let name = resolved.join("/");
        if excluded(&name, outputs, work)? {
            return Err("snapshot symlink resolves into excluded output".into());
        }
        if let Some(target) = links.get(&name) {
            expansions += 1;
            if expansions > 256 {
                return Err("snapshot symlink chain exceeds bound or cycles".into());
            }
            charge(work, target.len())?;
            resolved.pop();
            pending.extend(target.split('/').rev().map(str::to_owned));
        }
        if resolved.len() + pending.len() > 4096 {
            return Err("snapshot symlink path exceeds bound".into());
        }
    }
    if ancestor(&resolved.join("/"), outputs, work)? {
        return Err("snapshot symlink can traverse into excluded output".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn digest_binds_exclusions_paths_modes_and_exact_bytes() {
        fn digest(excluded: &[&str], path: &str, mode: u32, text: &[u8]) -> SupportingInputs {
            let mut h = Builder::new(&excluded.iter().map(|s| s.to_string()).collect()).unwrap();
            h.add(path, mode, text).unwrap();
            h.finish().unwrap()
        }
        let base = digest(&[], "a", 0o100644, b"one");
        assert_ne!(base, digest(&["receipt"], "a", 0o100644, b"one"));
        assert_ne!(base, digest(&[], "b", 0o100644, b"one"));
        assert_ne!(base, digest(&[], "a", 0o100755, b"one"));
        assert_ne!(base, digest(&[], "a", 0o100644, b"two"));
        assert_ne!(base, digest(&[], "a", 0o120000, b"one"));
    }
    #[test]
    fn links_cannot_reach_excluded_outputs_through_prefixes_or_chains() {
        fn check(rows: &[(&str, &str)]) -> Result<SupportingInputs, String> {
            let mut builder = Builder::new(&["out/receipts".to_owned()].into_iter().collect())?;
            for (path, target) in rows {
                builder.add(path, 0o120000, target.as_bytes())?;
            }
            builder.finish()
        }
        assert!(check(&[("a", "out/receipts/missing")]).is_err());
        assert!(check(&[("a", "out")]).is_err());
        assert!(check(&[("a", "b/item"), ("b", "out/receipts")]).is_err());
        assert!(check(&[("a", "b"), ("b", "a")]).is_err());
        assert!(check(&[("src/link", "../candidate.toml")]).is_ok());
    }
    #[test]
    fn link_count_and_global_resolution_work_fail_closed() {
        let mut builder = Builder::new(&BTreeSet::new()).unwrap();
        for i in 0..MAX_LINKS {
            builder
                .add(&format!("link{i:04}"), 0o120000, b"target")
                .unwrap();
        }
        assert!(builder
            .add("overflow", 0o120000, b"target")
            .unwrap_err()
            .contains("count"));
        let mut builder = Builder::new(&BTreeSet::new()).unwrap();
        let target = "x/../".repeat(800);
        for i in 0..600 {
            builder
                .add(&format!("link{i:04}"), 0o120000, target.as_bytes())
                .unwrap();
        }
        assert!(builder.finish().unwrap_err().contains("work"));
    }
    #[test]
    fn indexed_large_output_set_keeps_safe_relative_links_valid() {
        let outputs = (0..4096)
            .map(|i| format!("output{i:04}/receipt.json"))
            .collect();
        let mut builder = Builder::new(&outputs).unwrap();
        builder
            .add("native/fixtures/outer/link", 0o120000, b"../nested")
            .unwrap();
        builder
            .add("native/fixtures/symlink", 0o120000, b"small/a.txt")
            .unwrap();
        builder
            .add(
                "src/conductor/policy",
                0o120000,
                b"../../candidate_policy.toml",
            )
            .unwrap();
        assert!(builder.finish().is_ok());
    }
    #[test]
    fn receipt_namespace_requires_strict_relative_descendant() {
        assert!(within_namespace(
            "campaigns/receipts/a.json",
            "campaigns/receipts"
        ));
        for path in [
            "campaigns/receipts",
            "campaigns/receipts2/a.json",
            "campaigns/receipts/../outside",
            "/campaigns/receipts/a.json",
        ] {
            assert!(!within_namespace(path, "campaigns/receipts"));
        }
        assert!(!within_namespace(
            "campaigns/receipts/a.json",
            "../campaigns"
        ));
    }
    #[test]
    fn malformed_or_duplicate_entries_refuse() {
        let mut h = Builder::new(&BTreeSet::new()).unwrap();
        assert!(h.add("../a", 0o100644, b"").is_err());
        assert!(h.add(".git/config", 0o100644, b"").is_err());
        h.add("a", 0o100644, b"").unwrap();
        assert!(h.add("a", 0o100644, b"").is_err());
    }
}
