//! Human output for the complete report. Findings are observations, not authority.
use super::*;
fn text<'a>(row: &'a Value, key: &str) -> Result<&'a str> {
    row[key]
        .as_str()
        .with_context(|| format!("report row missing string {key}"))
}
fn section(lines: &mut Vec<String>, title: String, rows: impl IntoIterator<Item = String>) {
    lines.push(title);
    lines.extend(rows);
}
fn drift_and_leases(report: &Report, lines: &mut Vec<String>) -> Result<()> {
    let drift = &report.checkout_drift;
    if drift["behind"].as_u64().unwrap_or(0) > 0 {
        lines.push(format!(
            "CHECKOUT BEHIND -- {} commit(s) behind {}",
            drift["behind"],
            text(drift, "remote")?
        ));
        if drift["fast_forwardable"] == true {
            lines.extend([
                "  tracked changes alone do not block a fast-forward".into(),
                "  checkout-sync must classify untracked/ignored collisions before applying".into(),
            ]);
        } else {
            for path in drift["blocked_by"]
                .as_array()
                .context("drift missing blocked_by")?
            {
                lines.push(format!(
                    "  blocked by local edit: {}",
                    path.as_str().context("blocked path must be string")?
                ));
            }
            if drift["ahead"].as_u64().unwrap_or(0) > 0 {
                lines.push(format!(
                    "  and {} commit(s) ahead: this checkout is carrying work nobody else can see",
                    drift["ahead"]
                ));
            }
        }
    }
    let overdue = report
        .worktree_leases
        .iter()
        .filter(|row| row["status"] != "leased")
        .collect::<Vec<_>>();
    lines.push(format!(
        "WORKTREES PAST THEIR LEASE -- ask the owner, then remove ({})",
        overdue.len()
    ));
    for row in overdue {
        let worktree = text(row, "worktree")?;
        if row["status"] == "unleased" {
            lines.push(format!("  {worktree} -- no lease: created outside `make worktree`, so nobody knows what it is for"));
        } else {
            lines.push(format!(
                "  {worktree} [{}] -- {}, {} min overdue: {}",
                text(row, "branch")?,
                text(row, "owner")?,
                row["overdue_minutes"],
                text(row, "purpose")?
            ));
        }
    }
    Ok(())
}
fn branches_and_trees(report: &Report, lines: &mut Vec<String>) -> Result<()> {
    let safe = report
        .redundant_branches
        .iter()
        .filter(|row| row.get("blocked_by").is_none())
        .collect::<Vec<_>>();
    let blocked = report
        .redundant_branches
        .iter()
        .filter(|row| row.get("blocked_by").is_some())
        .collect::<Vec<_>>();
    lines.push(format!(
        "SAFE TO DELETE -- branches with 0 unique commits ({})",
        safe.len()
    ));
    for row in safe {
        lines.push(format!(
            "  {}  {}",
            text(row, "sha")?.chars().take(8).collect::<String>(),
            text(row, "branch")?
        ));
    }
    if !blocked.is_empty() {
        lines.push(format!(
            "REDUNDANT REF, BLOCKED -- do not delete yet ({})",
            blocked.len()
        ));
        for row in blocked {
            lines.push(format!(
                "  {}  {}  <-- {}",
                text(row, "sha")?.chars().take(8).collect::<String>(),
                text(row, "branch")?,
                text(row, "blocked_by")?
            ));
        }
    }
    section(
        lines,
        format!(
            "SAFE TO PRUNE -- worktree registrations with no directory ({})",
            report.stale_worktrees.len()
        ),
        report
            .stale_worktrees
            .iter()
            .map(|path| format!("  {path}")),
    );
    lines.push(String::new());
    lines.push(format!(
        "CLEAN WORKTREES -- containment/remote-retirement observations against {} ({})",
        report.landed_ref,
        report.landed_worktrees.len()
    ));
    for row in &report.landed_worktrees {
        let path = text(row, "worktree")?;
        lines.push(format!(
            "  {}  {path} [{}] -- {}{}",
            text(row, "head")?,
            text(row, "branch")?,
            text(row, "reason")?,
            if row["requires_review"] == true {
                " (review only)"
            } else {
                ""
            }
        ));
    }
    lines.push(String::new());
    lines.push(format!(
        "DO NOT REMOVE -- worktrees holding uncommitted work ({})",
        report.dirty_worktrees.len()
    ));
    for row in &report.dirty_worktrees {
        lines.push(format!(
            "  {} [{}] -- {} entries",
            text(row, "worktree")?,
            text(row, "branch")?,
            row["entries"]
        ));
        for sample in row["sample"]
            .as_array()
            .context("dirty sample must be array")?
        {
            lines.push(format!(
                "      {}",
                sample.as_str().context("dirty sample must be string")?
            ));
        }
    }
    Ok(())
}
fn claims_and_unpushed(report: &Report, lines: &mut Vec<String>) -> Result<()> {
    lines.push(String::new());
    lines.push(format!(
        "UNPUSHED -- commits that exist on no remote ({})",
        report.unpushed_work.len()
    ));
    for row in &report.unpushed_work {
        lines.push(format!(
            "  {:>4} ahead  {}  vs {}",
            text(row, "ahead")?,
            text(row, "branch")?,
            text(row, "remote")?
        ));
    }
    lines.push(String::new());
    lines.push(format!(
        "IDLE CLAIMS -- live, but the holder stopped writing ({})",
        report.idle_claims.len()
    ));
    for row in &report.idle_claims {
        lines.push(format!(
            "  {}  {}  idle {}m  {}",
            text(row, "claim_id")?,
            text(row, "owner")?,
            text(row, "idle_minutes")?,
            text(row, "paths")?
        ));
    }
    lines.push(String::new());
    lines.push(format!("EXPIRED CLAIMS ({})", report.expired_claims.len()));
    for row in &report.expired_claims {
        lines.push(format!(
            "  {}  owner={}  expired={}",
            text(row, "claim_id")?,
            text(row, "owner")?,
            text(row, "expired_at")?
        ));
    }
    Ok(())
}
fn manifests_and_imports(report: &Report, lines: &mut Vec<String>) -> Result<()> {
    lines.push(String::new());
    section(
        lines,
        format!(
            "ORPHAN MANIFESTS -- on disk, unregistered ({})",
            report.orphan_manifests.len()
        ),
        report
            .orphan_manifests
            .iter()
            .map(|path| format!("  {path}")),
    );
    section(
        lines,
        format!(
            "DANGLING -- registered, file missing ({})",
            report.dangling_manifests.len()
        ),
        report
            .dangling_manifests
            .iter()
            .map(|path| format!("  {path}  <-- repo-wide REFUSE risk")),
    );
    lines.push(String::new());
    lines.push(format!(
        "BREAKS A CLEAN CLONE -- untracked, import-reachable from tracked code ({})",
        report.untracked_imports.len()
    ));
    for row in &report.untracked_imports {
        lines.push(format!(
            "  {}  <- {} importer(s), {} tracked",
            text(row, "module")?,
            row["importers"]
                .as_array()
                .context("importers must be array")?
                .len(),
            row["tracked_importers"]
                .as_array()
                .context("tracked_importers must be array")?
                .len()
        ));
    }
    Ok(())
}
pub(super) fn report(report: &Report) -> Result<String> {
    let mut lines = vec![
        format!("workspace hygiene | live line: {}", report.live_ref),
        String::new(),
    ];
    drift_and_leases(report, &mut lines)?;
    lines.push(String::new());
    branches_and_trees(report, &mut lines)?;
    claims_and_unpushed(report, &mut lines)?;
    manifests_and_imports(report, &mut lines)?;
    lines.push(String::new());
    lines.push(exposure::render(
        &json!({"stale_feature_branches":report.stale_feature_branches,
        "gh_available":report.gh_available,"gh_errors":report.gh_errors,
        "local_only_commit_exposure":report.local_only_commit_exposure,
        "stale_dirty_files":report.stale_dirty_files,"reap_preview":report.reap_preview}),
    )?);
    let total = lines.iter().try_fold(0usize, |sum, line| {
        sum.checked_add(line.len() + 1)
            .context("human report size overflow")
    })?;
    ensure!(total <= 64 * 1024 * 1024, "human report exceeds64MiB");
    Ok(lines.join("\n"))
}
