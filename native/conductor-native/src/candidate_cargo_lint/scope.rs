#[path = "scope_manifest.rs"]
mod manifest;
use super::{inventory, path_parts, read, Budget, InputOwner, ScopeExpansion, ScopeReason};
use cap_std::fs::Dir;
use manifest::{within, Captured};
use std::collections::{BTreeMap, BTreeSet};
pub(super) struct Scope {
    pub manifests: BTreeSet<String>,
    pub expansions: Vec<ScopeExpansion>,
}
pub(super) fn expand(
    root: &Dir,
    inventory: &BTreeSet<String>,
    owners: &[InputOwner],
    budget: &mut Budget,
) -> Result<Scope, String> {
    let mut documents = BTreeMap::new();
    let mut remaining = 8 * 1024 * 1024usize;
    let mut targets = 0usize;
    for path in inventory {
        if remaining == 0 {
            return Err("captured manifests exceed 8 MiB".into());
        }
        let bytes = read::read_shared(root, path, remaining.min(1024 * 1024), budget)?
            .ok_or("inventory manifest disappeared")?;
        remaining = remaining
            .checked_sub(bytes.len())
            .ok_or("captured manifests exceed 8 MiB")?;
        let parsed = manifest::parse(root, path, &bytes, budget)?;
        targets = targets
            .checked_add(parsed.borrowed.len())
            .ok_or("target count overflow")?;
        if targets > 10_000 {
            return Err("borrowed target parents exceed 10000".into());
        }
        budget.retain(path.len())?;
        documents.insert(path.clone(), parsed);
    }
    validate_links(&documents)?;
    let mut selected = BTreeSet::new();
    for owner in owners {
        if !documents.contains_key(&owner.manifest) {
            return Err("selected owner missing from manifest inventory".into());
        }
        if !selected.contains(&owner.manifest) {
            budget.retain(owner.manifest.len() * 3)?;
            selected.insert(owner.manifest.clone());
        }
    }
    let mut expansions = Vec::new();
    for doc in documents.values().filter(|d| d.package) {
        for parent in &doc.borrowed {
            for owner in owners {
                budget.matching(parent, &owner.input)?;
                // A verified target parent may also lie below a selected directory.
                if within(&owner.input, parent) || within(parent, &owner.input) {
                    add(
                        &mut selected,
                        &mut expansions,
                        &doc.path,
                        ScopeReason::BorrowedTarget,
                        &owner.input,
                        budget,
                    )?;
                    break;
                }
            }
        }
    }
    let mut pending = selected.clone();
    let mut visited = BTreeSet::new();
    while let Some(path) = pending.pop_first() {
        if !visited.insert(path.clone()) {
            continue;
        }
        let doc = &documents[&path];
        if doc.workspace.is_some() {
            for member in workspace_members(root, doc, &documents, budget)? {
                if add(
                    &mut selected,
                    &mut expansions,
                    &member,
                    ScopeReason::Workspace,
                    &path,
                    budget,
                )? {
                    pending.insert(member);
                }
            }
        }
    }
    let manifests: BTreeSet<String> = selected
        .into_iter()
        .filter(|p| documents[p].package)
        .collect();
    if !owners.is_empty() && manifests.is_empty() {
        return Err("selected virtual workspace has no inventoried packages".into());
    }
    expansions.sort_by(|a, b| a.manifest.cmp(&b.manifest));
    Ok(Scope {
        manifests,
        expansions,
    })
}
fn validate_links(documents: &BTreeMap<String, Captured>) -> Result<(), String> {
    for doc in documents.values() {
        if let Some(path) = &doc.workspace_link {
            let workspace = documents
                .get(path)
                .ok_or("package.workspace missing from captured manifest inventory")?;
            if workspace.workspace.is_none() {
                return Err("package.workspace target must contain workspace table".into());
            }
        }
        for path in &doc.local_dependencies {
            let dependency = documents
                .get(path)
                .ok_or("local dependency missing from captured manifest inventory")?;
            if !dependency.package {
                return Err("local dependency manifest must contain package table".into());
            }
        }
    }
    Ok(())
}
fn add(
    selected: &mut BTreeSet<String>,
    expansions: &mut Vec<ScopeExpansion>,
    path: &str,
    reason: ScopeReason,
    trigger: &str,
    budget: &mut Budget,
) -> Result<bool, String> {
    if selected.contains(path) {
        return Ok(false);
    }
    if expansions.len() >= 10_000 {
        return Err("scope expansions exceed 10000".into());
    }
    budget.retain(path.len() * 4 + trigger.len())?;
    selected.insert(path.into());
    expansions.push(ScopeExpansion {
        manifest: path.into(),
        reason,
        trigger: trigger.into(),
    });
    Ok(true)
}
fn member_paths(
    root: &Dir,
    base: &str,
    pattern: &str,
    budget: &mut Budget,
) -> Result<BTreeSet<String>, String> {
    let path = manifest::normalize(root, base, pattern, true, budget)?;
    let path = if path == "." {
        "Cargo.toml".into()
    } else {
        format!("{path}/Cargo.toml")
    };
    path_parts(&path, false)?;
    let pattern = inventory::Pattern::new(&path)?;
    inventory::collect(root, &[pattern], budget)
}
fn workspace_members(
    root: &Dir,
    doc: &Captured,
    documents: &BTreeMap<String, Captured>,
    budget: &mut Budget,
) -> Result<BTreeSet<String>, String> {
    let workspace = doc.workspace.as_ref().expect("workspace admitted");
    let mut excluded = BTreeSet::new();
    for pattern in &workspace.excludes {
        excluded.extend(member_paths(root, &doc.crate_path, pattern, budget)?);
    }
    let mut selected = BTreeSet::new();
    for pattern in &workspace.members {
        let matched = member_paths(root, &doc.crate_path, pattern, budget)?;
        if matched.is_empty() {
            return Err("workspace member pattern has no represented manifest".into());
        }
        for path in matched {
            if excluded.contains(&path) {
                continue;
            }
            if !documents.contains_key(&path) {
                return Err("workspace member missing from roster manifest inventory".into());
            }
            selected.insert(path);
        }
    }
    for member in documents
        .values()
        .filter(|d| d.package && within(&d.crate_path, &doc.crate_path))
    {
        if !excluded.contains(&member.path) {
            selected.insert(member.path.clone());
        }
    }
    Ok(selected)
}
