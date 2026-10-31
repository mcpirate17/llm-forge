//! Pure report shaping for deterministic duplicate-consolidation clusters.
//! AST extraction and cluster scoring remain in the existing slop-core lane.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};

#[derive(Clone, Deserialize, Serialize)]
struct Site {
    file: String,
    line_start: i64,
    line_end: i64,
    name: String,
    node_hash: String,
    tokens: i64,
    source: String,
}

#[derive(Deserialize)]
struct Cluster {
    kind: String,
    tokens: i64,
    sites: Vec<Site>,
    id: String,
    batch: i64,
    confidence: f64,
    value_score: i64,
    risk: String,
    disposition: String,
    rationale: String,
}

impl Cluster {
    fn est_bytes(&self) -> i64 {
        self.tokens * (self.sites.len() as i64 - 1)
    }

    fn row(&self, home: Option<&str>) -> Value {
        json!({
            "id": self.id, "kind": self.kind, "tokens": self.tokens,
            "n_sites": self.sites.len(), "est_bytes": self.est_bytes(),
            "confidence": self.confidence, "value_score": self.value_score,
            "risk": self.risk, "disposition": self.disposition,
            "rationale": self.rationale, "batch": self.batch,
            "suggested_home": home,
            "sites": self.sites.iter().map(|site| json!({
                "file": site.file, "line_start": site.line_start,
                "line_end": site.line_end, "name": site.name
            })).collect::<Vec<_>>()
        })
    }
}

fn clusters(payload: &Value) -> Result<Vec<Cluster>, String> {
    serde_json::from_value(payload["clusters"].clone())
        .map_err(|error| format!("invalid consolidation clusters: {error}"))
}

fn homes(payload: &Value, count: usize) -> Result<Vec<Option<String>>, String> {
    let homes: Vec<Option<String>> = serde_json::from_value(payload["homes"].clone())
        .map_err(|error| format!("invalid consolidation homes: {error}"))?;
    if homes.len() != count {
        return Err("consolidation homes must match clusters".into());
    }
    Ok(homes)
}

fn cluster_rows(payload: &Value) -> Result<Value, String> {
    let clusters = clusters(payload)?;
    let homes = homes(payload, clusters.len())?;
    Ok(json!(clusters
        .iter()
        .zip(homes.iter())
        .map(|(cluster, home)| cluster.row(home.as_deref()))
        .collect::<Vec<_>>()))
}

fn report_summary(payload: &Value) -> Result<Value, String> {
    let clusters = clusters(payload)?;
    let actionable: Vec<_> = clusters
        .iter()
        .filter(|cluster| cluster.disposition == "auto")
        .collect();
    let batches: HashSet<_> = actionable.iter().map(|cluster| cluster.batch).collect();
    Ok(json!({
        "n_clusters": clusters.len(),
        "n_actionable": actionable.len(),
        "n_validate": clusters.iter().filter(|cluster| cluster.disposition == "validate").count(),
        "n_ignored": clusters.iter().filter(|cluster| cluster.disposition == "ignore").count(),
        "n_batches": batches.len(),
        "total_redundant_bytes": clusters.iter().map(Cluster::est_bytes).sum::<i64>(),
        "actionable_redundant_bytes": actionable.iter().map(|cluster| cluster.est_bytes()).sum::<i64>(),
        "actionable_value_score": actionable.iter().map(|cluster| cluster.value_score).sum::<i64>(),
        "files_scanned": payload["files_scanned"],
        "files_unparsable": payload["files_unparsable"],
        "functions_considered": payload["functions_considered"]
    }))
}

fn nonempty_object<'a>(
    value: &'a Value,
    field: &str,
) -> Option<&'a serde_json::Map<String, Value>> {
    value
        .get(field)
        .and_then(Value::as_object)
        .filter(|row| !row.is_empty())
}

fn token_count(row: &Value) -> i64 {
    row.get("tokens")
        .and_then(Value::as_i64)
        .filter(|count| *count != 0)
        .or_else(|| {
            row.get("lines")
                .and_then(Value::as_i64)
                .filter(|count| *count != 0)
        })
        .unwrap_or(0)
}

fn token_clones(payload: &Value) -> Result<Value, String> {
    let empty = Vec::new();
    let rows = match payload.get("duplicates") {
        None => &empty,
        Some(value) => value.as_array().ok_or("duplicates must be a list")?,
    };
    let mut clusters = Vec::new();
    for row in rows {
        let (Some(first), Some(second)) = (
            nonempty_object(row, "firstFile"),
            nonempty_object(row, "secondFile"),
        ) else {
            continue;
        };
        let tokens = token_count(row);
        let site = |fields: &serde_json::Map<String, Value>| -> Result<Site, String> {
            Ok(Site {
                file: fields
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("clone site lacks name")?
                    .to_owned(),
                line_start: fields
                    .get("start")
                    .and_then(Value::as_i64)
                    .ok_or("clone site lacks start")?,
                line_end: fields
                    .get("end")
                    .and_then(Value::as_i64)
                    .ok_or("clone site lacks end")?,
                name: "<clone>".into(),
                node_hash: String::new(),
                tokens,
                source: String::new(),
            })
        };
        clusters.push(json!({"kind":"token", "tokens":tokens,
            "sites":[site(first)?, site(second)?]}));
    }
    Ok(json!(clusters))
}

fn markdown_row(row: &Value) -> Result<String, String> {
    let sites = row["sites"]
        .as_array()
        .ok_or("cluster sites must be a list")?;
    let site_strings = sites
        .iter()
        .map(|site| {
            format!(
                "{}:{}",
                display(&site["file"]),
                display(&site["line_start"])
            )
        })
        .collect::<Vec<_>>();
    let mut site_field = site_strings
        .iter()
        .take(6)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if site_strings.len() > 6 {
        site_field.push_str(&format!(", +{} more", site_strings.len() - 6));
    }
    let confidence = row["confidence"]
        .as_f64()
        .ok_or("cluster confidence must be numeric")?;
    let home = row["suggested_home"]
        .as_str()
        .filter(|home| !home.is_empty())
        .unwrap_or("review required");
    Ok(format!(
        "| {} | {} | {} | {} | {:.2} | {} | {} | {} | {} | {} |",
        display(&row["id"]),
        display(&row["batch"]),
        display(&row["kind"]),
        display(&row["disposition"]),
        confidence,
        display(&row["value_score"]),
        display(&row["n_sites"]),
        display(&row["est_bytes"]),
        home,
        site_field
    ))
}

fn display(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        _ => value.to_string(),
    }
}

fn markdown(payload: &Value) -> Result<Value, String> {
    let summary = &payload["summary"];
    let rows = payload["clusters"]
        .as_array()
        .ok_or("report clusters must be a list")?;
    let mut lines = vec![
        format!("# Consolidation report — generated {}", display(&payload["generated_at"])),
        String::new(),
        format!(
            "{} clusters across {} batches; {} auto-actionable, {} require validation; ~{} redundant AST-node-units; {} files scanned ({} unparsable), {} functions considered.",
            display(&summary["n_clusters"]), display(&summary["n_batches"]),
            display(&summary["n_actionable"]), display(&summary["n_validate"]),
            display(&summary["total_redundant_bytes"]), display(&summary["files_scanned"]),
            display(&summary["files_unparsable"]), display(&summary["functions_considered"])
        ),
        String::new(),
        "| id | batch | kind | disposition | confidence | value | n_sites | est_bytes | suggested_home | sites (file:line, ...) |".into(),
        "|---|---|---|---|---|---|---|---|---|---|".into(),
    ];
    for row in rows {
        lines.push(markdown_row(row)?);
    }
    Ok(json!(lines.join("\n") + "\n"))
}

/// A review bundle groups identical file sets and a proposed kind of work. It
/// deliberately does not imply that all its functions are interchangeable.
#[derive(Default)]
struct Opportunity {
    strategy: String,
    files: BTreeSet<String>,
    evidence: BTreeMap<String, Value>,
    sites: BTreeMap<String, Value>,
    net_files: Option<i64>,
    before_files: Option<u64>,
    reviewed: bool,
    rank: u8,
}

fn rows<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], String> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(rows)) => Ok(rows),
        _ => Err(format!("{key} must be an array")),
    }
}

fn file_set(row: &Value) -> Result<BTreeSet<String>, String> {
    let mut files = BTreeSet::new();
    for file in rows(row, "files")? {
        files.insert(file.as_str().ok_or("file must be a string")?.to_owned());
    }
    for site in rows(row, "sites")? {
        files.insert(site["file"].as_str().ok_or("site lacks file")?.to_owned());
    }
    Ok(files)
}

fn evidence_id(row: &Value, prefix: &str) -> Result<String, String> {
    let id = row["id"].as_str().ok_or("evidence lacks string id")?;
    Ok(format!("{prefix}:{id}"))
}

fn add_opportunity(
    groups: &mut BTreeMap<String, Opportunity>,
    strategy: &str,
    id: String,
    row: Value,
    rank: u8,
) -> Result<(), String> {
    let files = file_set(&row)?;
    if files.is_empty() {
        return Err(format!("opportunity evidence {id} has no files"));
    }
    // Plans retain their own identities; other bundles only join exact file
    // sets. A-B and B-C never silently turn into an A-B-C equivalence class.
    let key = if strategy == "retire_planned_files" {
        id.clone()
    } else {
        serde_json::to_string(&(strategy, &files)).map_err(|error| error.to_string())?
    };
    let group = groups.entry(key).or_insert_with(|| Opportunity {
        strategy: strategy.into(),
        files,
        rank,
        ..Opportunity::default()
    });
    group.rank = group.rank.min(rank);
    for site in rows(&row, "sites")? {
        let key = serde_json::to_string(&(
            &site["file"],
            &site["line_start"],
            &site["line_end"],
            &site["name"],
        ))
        .map_err(|error| error.to_string())?;
        group.sites.insert(key, site.clone());
    }
    if strategy == "retire_planned_files" {
        group.net_files = row["net_files_removed"].as_i64();
        group.before_files = row["before_files"].as_u64();
        group.reviewed = row["status"] == "reviewed";
    }
    group.evidence.insert(id, row);
    Ok(())
}

fn clone_opportunities(
    groups: &mut BTreeMap<String, Opportunity>,
    functions: &Value,
) -> Result<(), String> {
    for row in rows(functions, "clusters")? {
        let kind = row["kind"].as_str().ok_or("cluster lacks kind")?;
        let (strategy, rank) = match kind {
            "exact" | "near" => ("review_existing_callable", 2),
            "token" => ("extract_shared_block", 4),
            _ => return Err(format!("unknown clone kind: {kind}")),
        };
        add_opportunity(
            groups,
            strategy,
            evidence_id(row, "functions")?,
            row.clone(),
            rank,
        )?;
    }
    let abstractions = &functions["abstractions"];
    let profiles = rows(abstractions, "profiles")?;
    for (index, edge) in rows(abstractions, "edges")?.iter().enumerate() {
        let get = |side: &str| -> Result<Value, String> {
            let index = edge[side]
                .as_u64()
                .ok_or("abstraction edge index must be unsigned")?;
            let profile = profiles
                .get(index as usize)
                .ok_or("abstraction edge index out of range")?;
            Ok(json!({"file": profile["file"], "name": profile["name"],
                "qualified_name": profile["qualified_name"], "line_start": profile["line_start"],
                "line_end": profile["line_end"], "node_hash": profile["node_hash"],
                "semantic": profile["semantic"], "binding_resolution": profile["binding_resolution"]}))
        };
        let mut row = edge.as_object().ok_or("edge must be an object")?.clone();
        row.insert("sites".into(), json!([get("left")?, get("right")?]));
        add_opportunity(
            groups,
            "review_parameterized_abstraction",
            format!("abstractions:A{:05}", index + 1),
            Value::Object(row),
            3,
        )?;
    }
    Ok(())
}

fn registration_opportunities(
    groups: &mut BTreeMap<String, Opportunity>,
    families: &Value,
) -> Result<(), String> {
    let mut cohorts: BTreeMap<(String, String, bool), BTreeMap<String, Value>> = BTreeMap::new();
    for row in rows(families, "registration_candidates")? {
        let text = |key: &str| {
            row[key]
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("registration candidate lacks {key}"))
        };
        let factory = text("factory")?.to_owned();
        let name = text("name")?.to_owned();
        let file = text("file")?.to_owned();
        text("assignment")?;
        let deferred = row["has_deferred_implementation"]
            .as_bool()
            .ok_or("registration candidate lacks implementation boundary")?;
        let start = row["line_start"].as_u64().filter(|line| *line > 0);
        let end = row["line_end"].as_u64();
        if start.is_none() || end.is_none() || end < start {
            return Err("registration candidate has invalid source span".into());
        }
        if !row["import_bindings"].is_object()
            || !row["arguments"].is_array()
            || !row["keywords"].is_array()
        {
            return Err("registration candidate lacks import/argument evidence".into());
        }
        let cohort = cohorts.entry((factory, name, deferred)).or_default();
        if let Some(previous) = cohort.insert(file, row.clone()) {
            if previous != *row {
                return Err("conflicting registration candidates for one file".into());
            }
        }
    }
    for ((factory, name, deferred), members) in cohorts {
        if members.len() < 2 {
            continue;
        }
        let boundary = if deferred {
            "deferred_inline_implementation"
        } else {
            "registration_arguments"
        };
        let strategy = if deferred {
            "review_inline_factory_implementation"
        } else {
            "review_factory_registration"
        };
        let encoded =
            serde_json::to_vec(&(&factory, &name, boundary)).map_err(|error| error.to_string())?;
        let digest = format!("{:x}", Sha256::digest(&encoded));
        let sites: Vec<Value> = members.values().cloned().collect();
        let row = json!({
            "factory": factory, "export": name, "boundary": boundary,
            "has_deferred_implementation": deferred, "files": members.keys().collect::<Vec<_>>(),
            "sites": sites, "disposition": "REVIEW", "net_files_removed": null,
            "callable_return": "unknown", "side_effects": "unknown",
            "rationale": "Same imported factory and export binding; arguments and inline implementations require independent review. No return-type, purity, equivalence, or deletion proof."
        });
        add_opportunity(
            groups,
            strategy,
            format!("registrations:{}", &digest[..16]),
            row,
            3,
        )?;
    }
    Ok(())
}

fn gather_opportunities(payload: &Value) -> Result<Vec<Opportunity>, String> {
    let mut groups = BTreeMap::new();
    clone_opportunities(&mut groups, &payload["functions"])?;
    registration_opportunities(&mut groups, &payload["families"])?;
    for row in rows(&payload["families"], "families")? {
        let rank = if row["disposition"] == "ignore" || row["disposition"] == "review-data" {
            6
        } else {
            1
        };
        add_opportunity(
            &mut groups,
            "review_file_family",
            evidence_id(row, "families")?,
            row.clone(),
            rank,
        )?;
    }
    for row in rows(&payload["native_reuse"], "candidates")? {
        // The graph's normalized-name ID can repeat at distinct source spans.
        // Preserve every distinct row instead of overwriting its evidence.
        let encoded = serde_json::to_vec(row).map_err(|error| error.to_string())?;
        let digest = format!("{:x}", Sha256::digest(&encoded));
        add_opportunity(
            &mut groups,
            "review_existing_native",
            format!("{}@{}", evidence_id(row, "native")?, &digest[..16]),
            row.clone(),
            5,
        )?;
    }
    let mut planned_removals = BTreeSet::new();
    let mut planned_additions = BTreeSet::new();
    for plan in rows(&payload["retirement"], "plans")? {
        let mut row = plan.as_object().ok_or("plan must be an object")?.clone();
        let mut files = BTreeSet::new();
        let mut remove = BTreeSet::new();
        let mut additions = BTreeSet::new();
        for field in ["remove", "retain", "add", "add_adapters", "add_configs"] {
            for path in rows(plan, field)? {
                let path = path
                    .as_str()
                    .ok_or("plan path must be a string")?
                    .to_owned();
                files.insert(path.clone());
                if field == "remove" {
                    remove.insert(path);
                } else if field != "retain" {
                    additions.insert(path);
                }
            }
        }
        if !remove.is_disjoint(&additions)
            || !remove.is_disjoint(&planned_removals)
            || !additions.is_disjoint(&planned_additions)
            || !remove.is_disjoint(&planned_additions)
            || !additions.is_disjoint(&planned_removals)
        {
            return Err("retirement plans have overlapping removal/addition paths".into());
        }
        planned_removals.extend(remove.iter().cloned());
        planned_additions.extend(additions.iter().cloned());
        row.insert("before_files".into(), json!(files.len() - additions.len()));
        row.insert("files".into(), json!(files));
        row.insert(
            "net_files_removed".into(),
            json!(remove.len() as i64 - additions.len() as i64),
        );
        let rank = if plan["status"] == "reviewed" && remove.len() > additions.len() {
            0
        } else {
            7
        };
        add_opportunity(
            &mut groups,
            "retire_planned_files",
            evidence_id(plan, "plans")?,
            Value::Object(row),
            rank,
        )?;
    }
    let mut groups: Vec<_> = groups.into_values().collect();
    groups.sort_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.net_files.cmp(&left.net_files))
            .then_with(|| right.files.len().cmp(&left.files.len()))
            .then_with(|| right.sites.len().cmp(&left.sites.len()))
            .then_with(|| left.evidence.keys().cmp(right.evidence.keys()))
    });
    Ok(groups)
}

fn overlap_index(groups: &[Opportunity]) -> Vec<BTreeSet<usize>> {
    let mut by_file: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, group) in groups.iter().enumerate() {
        for file in &group.files {
            by_file.entry(file).or_default().push(index);
        }
    }
    let mut overlaps = vec![BTreeSet::new(); groups.len()];
    for members in by_file.values() {
        for &left in members {
            for &right in members {
                if left != right {
                    overlaps[left].insert(right);
                }
            }
        }
    }
    overlaps
}

fn site_identity(site: &Value) -> Result<String, String> {
    let file = site["file"].as_str().ok_or("caller site lacks file")?;
    let name = site["name"].as_str().ok_or("caller site lacks name")?;
    let line = site["line_start"]
        .as_u64()
        .ok_or("caller site lacks line")?;
    serde_json::to_string(&(file, name, line)).map_err(|error| error.to_string())
}

fn caller_hint_index(payload: &Value) -> Result<BTreeMap<String, Option<Value>>, String> {
    let hints = &payload["caller_hints"];
    let mut index = BTreeMap::new();
    if hints["complete"] != true {
        return Ok(index);
    }
    for hint in rows(hints, "sites")? {
        if !hint["caller_count"].is_null() && hint["caller_count"].as_u64().is_none() {
            return Err("caller_count must be a nonnegative integer or null".into());
        }
        let key = site_identity(hint)?;
        index
            .entry(key)
            .and_modify(|value| *value = None)
            .or_insert(Some(hint.clone()));
    }
    Ok(index)
}

fn has_callable_definitions(group: &Opportunity) -> bool {
    !matches!(
        group.strategy.as_str(),
        "extract_shared_block"
            | "review_factory_registration"
            | "review_inline_factory_implementation"
    )
}

fn existing_owner_candidates(
    group: &Opportunity,
    hints: &BTreeMap<String, Option<Value>>,
) -> Result<Vec<Value>, String> {
    if !has_callable_definitions(group) {
        return Ok(Vec::new());
    }
    let mut candidates = Vec::new();
    for site in group.sites.values() {
        let hint = hints.get(&site_identity(site)?).and_then(Option::as_ref);
        let mut candidate = serde_json::Map::new();
        for key in ["file", "name", "qualified_name", "line_start", "line_end"] {
            candidate.insert(key.into(), site[key].clone());
        }
        candidate.insert(
            "observed_graph_callers".into(),
            hint.map(|hint| hint["caller_count"].clone())
                .unwrap_or(Value::Null),
        );
        candidate.insert(
            "graph_qualified_name".into(),
            hint.map(|hint| hint["graph_qualified_name"].clone())
                .unwrap_or(Value::Null),
        );
        candidates.push(Value::Object(candidate));
    }
    candidates.sort_by(|left, right| {
        right["observed_graph_callers"]
            .as_u64()
            .cmp(&left["observed_graph_callers"].as_u64())
            .then_with(|| left["file"].as_str().cmp(&right["file"].as_str()))
            .then_with(|| {
                left["line_start"]
                    .as_u64()
                    .cmp(&right["line_start"].as_u64())
            })
    });
    Ok(candidates)
}

fn opportunity_row(
    index: usize,
    group: &Opportunity,
    overlaps: &BTreeSet<usize>,
    hints: &BTreeMap<String, Option<Value>>,
) -> Result<Value, String> {
    let explicit_plan = group.strategy == "retire_planned_files";
    let callable_sites = if !has_callable_definitions(group) {
        0
    } else {
        group.sites.len()
    };
    Ok(json!({
        "id": format!("O{:05}", index + 1), "strategy": group.strategy,
        "files": group.files, "existing_file_count": group.before_files.unwrap_or(group.files.len() as u64),
        "referenced_file_count": group.files.len(),
        "observed_callable_sites": callable_sites, "observed_sites": group.sites.len(),
        "sites": group.sites.values().collect::<Vec<_>>(),
        "existing_owner_candidates": existing_owner_candidates(group, hints)?,
        "evidence_ids": group.evidence.keys().collect::<Vec<_>>(),
        "evidence": group.evidence.values().collect::<Vec<_>>(),
        "shares_files_with": overlaps.iter().map(|index| format!("O{:05}", index + 1)).collect::<Vec<_>>(),
        "net_files_removed": group.net_files, "file_reduction_basis": if explicit_plan { "explicit_plan" } else { "not_established" },
        "measurement_status": "not_measured", "reviewed_plan": group.reviewed,
        "review_only": true, "ranking_tier": group.rank,
        "grouping_basis": "Same proposed work and identical file set; a review bundle, not an equivalence class",
        "review_requirements": [
            "Check imports, globals, defaults, decorators, side effects and caller contracts before choosing a canonical implementation.",
            "Preserve scientific controls, isolation boundaries, and distinct mechanisms; similarity does not establish substitutability.",
            "A file retires only after all remaining responsibilities and callers migrate; count added adapters, configs and tests."
        ]
    }))
}

fn opportunities(payload: &Value) -> Result<Value, String> {
    let groups = gather_opportunities(payload)?;
    let overlaps = overlap_index(&groups);
    let caller_hints = caller_hint_index(payload)?;
    let files: BTreeSet<_> = groups.iter().flat_map(|group| group.files.iter()).collect();
    let report: Vec<_> = groups
        .iter()
        .zip(&overlaps)
        .enumerate()
        .map(|(index, (group, overlaps))| opportunity_row(index, group, overlaps, &caller_hints))
        .collect::<Result<_, _>>()?;
    Ok(json!({
        "schema_version": 1, "review_only": true, "coverage": payload["coverage"],
        "native_graph_status": payload["native_reuse"]["graph_status"],
        "caller_hints": payload["caller_hints"],
        "abstraction_coverage": payload["functions"]["abstractions"]["coverage"],
        "summary": {
            "opportunities": groups.len(), "unique_files": files.len(),
            "evidence_items": groups.iter().map(|group| group.evidence.len()).sum::<usize>(),
            "reviewed_planned_net_files_removed": groups.iter().filter(|group| group.reviewed)
                .filter_map(|group| group.net_files).sum::<i64>(),
            "measured_net_files_removed": Value::Null,
            "savings_are_additive": false
        },
        "ranking": "Reviewed positive retirement plans, file families, existing callable reuse, parameterized abstractions and factory-registration review, token blocks, name-only native hints; then unique file/site footprint. File footprint is not savings.",
        "opportunities": report,
        "limits": [
            "Structural and name matches are review leads, not semantic equivalence or automatic deletion permission.",
            "Overlapping files are linked, not transitively merged; do not sum clone or family LOC estimates.",
            "Only explicit plans receive signed file-reduction estimates; actual reduction requires a measured Git tree diff.",
            "Existing callable sites are canonical candidates, not a selected canonical implementation; dependencies may prevent direct reuse."
        ]
    }))
}

fn markdown_cell(value: &str) -> String {
    value.replace('|', "\\|").replace(['\n', '\r'], " ")
}

fn opportunity_markdown(payload: &Value) -> Result<Value, String> {
    let mut lines = vec![
        "# Reuse opportunities".into(), String::new(),
        "Ranked review bundles. File footprint is not savings; only explicit retirement plans carry file-reduction estimates. Original detector evidence and behavioral differences remain in opportunities.json.".into(),
        String::new(),
        "| Opportunity | Proposed review | Files | Callable sites | Planned net files removed | Evidence |".into(),
        "|---|---|---:|---:|---:|---|".into(),
    ];
    for row in rows(payload, "opportunities")? {
        let evidence = rows(row, "evidence_ids")?
            .iter()
            .map(|id| {
                id.as_str()
                    .map(markdown_cell)
                    .ok_or("evidence id must be a string")
            })
            .collect::<Result<Vec<_>, _>>()?;
        lines.push(format!(
            "| {} | {} | {} | {} | {} | {} |",
            display(&row["id"]),
            markdown_cell(row["strategy"].as_str().ok_or("strategy missing")?),
            display(&row["existing_file_count"]),
            display(&row["observed_callable_sites"]),
            row["net_files_removed"]
                .as_i64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unknown".into()),
            evidence.join(", ")
        ));
    }
    Ok(json!(lines.join("\n") + "\n"))
}

pub fn decide(operation: &str, payload: &Value) -> Result<Value, String> {
    match operation {
        "token_clones" => token_clones(payload),
        "cluster_dicts" => cluster_rows(payload),
        "report_summary" => report_summary(payload),
        "markdown_row" => markdown_row(payload).map(|row| json!(row)),
        "markdown" => markdown(payload),
        "opportunities" => opportunities(payload),
        "opportunity_markdown" => opportunity_markdown(payload),
        other => Err(format!("unknown consolidation operation: {other}")),
    }
}

#[cfg(feature = "python")]
mod python {
    use super::*;
    use pyo3::exceptions::PyValueError;
    use pyo3::prelude::*;

    #[pyfunction]
    fn reuse_consolidation_native(operation: &str, payload_json: &str) -> PyResult<String> {
        let payload: Value = serde_json::from_str(payload_json)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let result = decide(operation, &payload).map_err(PyValueError::new_err)?;
        serde_json::to_string(&result).map_err(|error| PyValueError::new_err(error.to_string()))
    }

    pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
        module.add_function(wrap_pyfunction!(reuse_consolidation_native, module)?)?;
        Ok(())
    }
}

#[cfg(feature = "python")]
pub use python::register;

#[cfg(test)]
mod opportunity_tests {
    use super::*;

    fn registration_row(file: &str, deferred: bool) -> Value {
        json!({"file":file, "line_start":3, "line_end":3, "name":"ComponentHandler",
            "factory":"pkg.factories.make_handler", "import_bindings":{"f":"pkg.factories"},
            "arguments":[file], "keywords":[], "assignment":format!("handler for {file}"),
            "has_deferred_implementation":deferred})
    }

    #[test]
    fn registration_cohorts_keep_deferred_code_separate_without_savings_claims() {
        let input = vec![
            registration_row("b.py", false),
            registration_row("d.py", true),
            registration_row("a.py", false),
            registration_row("c.py", true),
        ];
        let report = opportunities(&json!({"families":{"registration_candidates":input}})).unwrap();
        let reversed: Vec<Value> = input.into_iter().rev().collect();
        assert_eq!(
            report,
            opportunities(&json!({"families":{"registration_candidates":reversed}})).unwrap()
        );
        let rows = report["opportunities"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let strategies: BTreeSet<&str> = rows
            .iter()
            .map(|row| row["strategy"].as_str().unwrap())
            .collect();
        assert_eq!(
            strategies,
            BTreeSet::from([
                "review_factory_registration",
                "review_inline_factory_implementation"
            ])
        );
        for row in rows {
            assert!(row["net_files_removed"].is_null());
            assert_eq!(row["review_only"], true);
            assert_eq!(row["observed_callable_sites"], 0);
            assert_eq!(row["existing_owner_candidates"], json!([]));
            assert_eq!(row["sites"].as_array().unwrap().len(), 2);
            assert_ne!(row["sites"][0]["arguments"], row["sites"][1]["arguments"]);
        }
    }

    #[test]
    fn registration_groups_deduplicate_identical_rows_and_reject_conflicts() {
        let first = registration_row("a.py", false);
        let report =
            opportunities(&json!({"families":{"registration_candidates":[first,first]}})).unwrap();
        assert!(report["opportunities"].as_array().unwrap().is_empty());
        let mut different = first.clone();
        different["arguments"] = json!(["changed"]);
        assert!(
            opportunities(&json!({"families":{"registration_candidates":[first,different]}}))
                .unwrap_err()
                .contains("conflicting registration")
        );
        let mut invalid = first;
        invalid["line_end"] = json!(1);
        assert!(
            opportunities(&json!({"families":{"registration_candidates":[invalid]}}))
                .unwrap_err()
                .contains("invalid source span")
        );
    }

    fn clone_row(id: &str, left: &str, right: &str, line: i64) -> Value {
        json!({"id": id, "kind": "exact", "sites": [
            {"file": left, "name": "fingerprint", "line_start": line, "line_end": line + 5},
            {"file": right, "name": "fingerprint", "line_start": line, "line_end": line + 5}
        ]})
    }

    #[test]
    fn groups_same_file_sets_without_transitive_equivalence_or_savings() {
        let payload = json!({"functions": {"clusters": [
            clone_row("C1", "a.py", "b.py", 1), clone_row("C2", "a.py", "b.py", 20),
            clone_row("C3", "b.py", "c.py", 1)
        ]}});
        let report = opportunities(&payload).unwrap();
        let rows = report["opportunities"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0]["evidence_ids"],
            json!(["functions:C1", "functions:C2"])
        );
        assert_eq!(rows[0]["observed_callable_sites"], 4);
        assert_eq!(rows[0]["files"], json!(["a.py", "b.py"]));
        assert_eq!(rows[0]["shares_files_with"], json!(["O00002"]));
        assert!(rows.iter().all(|row| row["net_files_removed"].is_null()));
        assert!(report["summary"]["measured_net_files_removed"].is_null());
        assert_eq!(report["summary"]["unique_files"], 3);
        assert_eq!(report["summary"]["savings_are_additive"], false);
    }

    #[test]
    fn reviewed_file_plans_count_all_additions_and_rank_before_clones() {
        let payload = json!({"functions": {"clusters": [clone_row("C1", "a.py", "b.py", 1)]},
            "retirement": {"plans": [{"id": "P1", "status": "reviewed",
                "remove": ["a.py", "b.py", "c.py", "d.py"], "retain": ["owner.py"],
                "add": ["merged.py"], "add_adapters": ["adapter.py"], "add_configs": ["config.toml"],
                "net_files_removed": 999}]}});
        let report = opportunities(&payload).unwrap();
        let first = &report["opportunities"][0];
        assert_eq!(first["strategy"], "retire_planned_files");
        assert_eq!(first["net_files_removed"], 1);
        assert_eq!(first["existing_file_count"], 5);
        assert_eq!(first["referenced_file_count"], 8);
        assert_eq!(first["measurement_status"], "not_measured");
        assert_eq!(report["summary"]["reviewed_planned_net_files_removed"], 1);
        let mut overlap = payload;
        overlap["retirement"]["plans"]
            .as_array_mut()
            .unwrap()
            .push(json!({
            "id": "P2", "status": "reviewed", "remove": ["a.py"], "add": []}));
        assert!(opportunities(&overlap).unwrap_err().contains("overlapping"));
    }

    #[test]
    fn structural_edges_retain_differences_and_reject_dangling_indices() {
        let mut payload = json!({"functions": {"abstractions": {
            "profiles": [{"file":"a.py", "name":"compute", "line_start":1, "line_end":10,
                "structure": ["large profile stays in detector report"], "semantic":{"literals":[1]},
                "binding_resolution":"syntactic; closures unresolved"},
                {"file":"b.py", "name":"compute", "line_start":2, "line_end":11}],
            "edges": [{"left":0, "right":1, "score":0.9, "differences":{"literals":[1,2]}}],
            "coverage": {"retrieval_recall":"unknown", "truncated":true}
        }}, "coverage":{"source_scans_complete":false}});
        let report = opportunities(&payload).unwrap();
        let first = &report["opportunities"][0];
        assert_eq!(first["strategy"], "review_parameterized_abstraction");
        assert_eq!(
            first["evidence"][0]["differences"]["literals"],
            json!([1, 2])
        );
        assert!(first["sites"][0].get("structure").is_none());
        assert_eq!(first["sites"][0]["semantic"]["literals"], json!([1]));
        assert_eq!(
            first["sites"][0]["binding_resolution"],
            "syntactic; closures unresolved"
        );
        assert_eq!(report["abstraction_coverage"]["truncated"], true);
        assert_eq!(report["coverage"]["source_scans_complete"], false);
        payload["functions"]["abstractions"]["edges"][0]["right"] = json!(2);
        assert!(opportunities(&payload)
            .unwrap_err()
            .contains("out of range"));
    }

    #[test]
    fn data_only_and_name_only_hints_do_not_outrank_callable_reuse() {
        let report = opportunities(&json!({"functions": {"clusters": [clone_row("C1", "a.py", "b.py", 1)]},
            "families":{"families":[{"id":"F1", "files":["js.py","css.py"], "disposition":"review-data"}]},
            "native_reuse":{"candidates":[{"id":"append", "files":["py.py","rust.rs"]}]}
        })).unwrap();
        let rows = report["opportunities"].as_array().unwrap();
        assert_eq!(rows[0]["strategy"], "review_existing_callable");
        assert_eq!(rows[1]["strategy"], "review_existing_native");
        assert_eq!(rows[2]["ranking_tier"], 6);
        let markdown = opportunity_markdown(&report).unwrap();
        assert!(markdown.as_str().unwrap().contains("unknown"));
        assert!(rows.iter().all(|row| row["review_only"] == true));
    }

    #[test]
    fn preserves_distinct_graph_rows_with_the_same_name_based_id() {
        let report = opportunities(&json!({"native_reuse":{"candidates":[
            {"id":"append", "files":["a.py","b.rs"], "location":"a.py:1", "confidence":0.5},
            {"id":"append", "files":["a.py","b.rs"], "location":"a.py:50", "confidence":0.8}
        ]}}))
        .unwrap();
        assert_eq!(report["summary"]["opportunities"], 1);
        assert_eq!(report["summary"]["evidence_items"], 2);
        assert_eq!(
            report["opportunities"][0]["evidence"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn owner_hints_require_complete_graph_and_unambiguous_site_counts() {
        let mut payload = json!({"functions":{"clusters":[clone_row("C1","a.py","b.py",1)]},
        "caller_hints":{"complete":true, "sites":[
            {"file":"a.py", "name":"fingerprint", "line_start":1,"caller_count":4},
            {"file":"b.py", "name":"fingerprint", "line_start":1,"caller_count":55}
        ]}});
        let report = opportunities(&payload).unwrap();
        let owners = &report["opportunities"][0]["existing_owner_candidates"];
        assert_eq!(owners[0]["file"], "b.py");
        assert_eq!(owners[0]["observed_graph_callers"], 55);
        payload["caller_hints"]["complete"] = json!(false);
        let report = opportunities(&payload).unwrap();
        assert!(report["opportunities"][0]["existing_owner_candidates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|site| site["observed_graph_callers"].is_null()));
        payload["caller_hints"]["complete"] = json!(true);
        payload["caller_hints"]["sites"]
            .as_array_mut()
            .unwrap()
            .push(json!({
            "file":"b.py", "name":"fingerprint", "line_start":1,"caller_count":70}));
        let report = opportunities(&payload).unwrap();
        let owners = &report["opportunities"][0]["existing_owner_candidates"];
        assert_eq!(owners[0]["file"], "a.py");
        assert!(owners[1]["observed_graph_callers"].is_null());
    }

    #[test]
    fn token_blocks_are_not_counted_as_callable_owners() {
        let mut row = clone_row("T1", "a.rs", "b.rs", 1);
        row["kind"] = json!("token");
        let report = opportunities(&json!({"functions":{"clusters":[row]}})).unwrap();
        let opportunity = &report["opportunities"][0];
        assert_eq!(opportunity["observed_sites"], 2);
        assert_eq!(opportunity["observed_callable_sites"], 0);
        assert_eq!(opportunity["existing_owner_candidates"], json!([]));
    }
}
