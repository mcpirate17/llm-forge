//! Opt-in ordering of existing scoped manifests, never campaign execution.
//! Priority is lexicographic: source novelty, changed paired tests, current
//! dependency impact, then source path. These are ordinal signals, not calibrated
//! failure probabilities. Missing evidence and telemetry remain explicit unknowns.

use super::{sha256_file, PlanRequest};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Read, path::Path, process::Command};

#[derive(Debug, Deserialize)]
pub struct PriorityOptions {
    pub policy_path: String,
    pub base_ref: String,
    #[serde(default)]
    pub telemetry_path: Option<String>,
    #[serde(default)]
    pub budget_seconds: Option<f64>,
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|error| format!("cannot inspect priority provenance: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "priority git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn safe_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || Path::new(path).is_absolute()
        || path
            .split('/')
            .any(|part| part == ".." || part == "." || part.is_empty())
    {
        return Err(format!(
            "priority requires a repository-relative exact path: {path}"
        ));
    }
    Ok(())
}

fn policy(root: &Path, path: &str) -> Result<Vec<String>, String> {
    safe_path(path)?;
    let resolved = crate::project_paths::resolve(root)?;
    if resolved[0].0 != path {
        return Err(format!(
            "priority policy must match host configured candidate policy {}; supplied {path}",
            resolved[0].0
        ));
    }
    let text = fs::read_to_string(root.join(path))
        .map_err(|error| format!("priority policy unavailable at {path}: {error}"))?;
    let policy: toml::Value = toml::from_str(&text)
        .map_err(|error| format!("invalid priority policy at {path}: {error}"))?;
    let patterns = policy
        .get("risk")
        .and_then(|risk| risk.get("high"))
        .and_then(toml::Value::as_array)
        .ok_or("priority policy requires explicit [risk].high")?;
    patterns
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "[risk].high must contain strings".into())
        })
        .collect()
}

fn old_paths(root: &Path, base: &str) -> Result<BTreeMap<String, String>, String> {
    let output = git(root, &["diff", "--name-status", "-z", "-M", base, "--"])?;
    let fields: Vec<_> = output
        .split(|byte| *byte == 0)
        .filter(|v| !v.is_empty())
        .collect();
    let mut paths = BTreeMap::new();
    let mut index = 0;
    while index < fields.len() {
        let status = String::from_utf8_lossy(fields[index]);
        let count = if status.starts_with('R') || status.starts_with('C') {
            3
        } else {
            2
        };
        if index + count > fields.len() {
            return Err("malformed git priority change list".into());
        }
        if count == 3 {
            paths.insert(
                String::from_utf8_lossy(fields[index + 2]).into_owned(),
                String::from_utf8_lossy(fields[index + 1]).into_owned(),
            );
        }
        index += count;
    }
    Ok(paths)
}

fn novelty(
    root: &Path,
    base: &str,
    path: &str,
    renames: &BTreeMap<String, String>,
) -> Result<&'static str, String> {
    safe_path(path)?;
    let current = fs::read(root.join(path))
        .map_err(|error| format!("cannot read priority subject {path}: {error}"))?;
    let old_path = renames.get(path).map(String::as_str).unwrap_or(path);
    let object = format!("{base}:{old_path}");
    let exists = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "-e", &object])
        .output()
        .map_err(|error| format!("cannot inspect base source {path}: {error}"))?;
    if !exists.status.success() {
        return Ok("new");
    }
    let old = git(root, &["show", &object])?;
    Ok(if old == current {
        "unchanged"
    } else {
        "changed"
    })
}

fn telemetry(root: &Path, options: &PriorityOptions) -> Result<Value, String> {
    match &options.telemetry_path {
        None => Ok(Value::Null),
        Some(path) => {
            safe_path(path)?;
            let text = fs::read_to_string(root.join(path))
                .map_err(|error| format!("cannot read priority telemetry {path}: {error}"))?;
            serde_json::from_str(&text)
                .map_err(|error| format!("invalid priority telemetry {path}: {error}"))
        }
    }
}

fn worktree_hash(root: &Path, telemetry_path: Option<&str>) -> Result<String, String> {
    let mut hasher = Sha256::new();
    let mut args = vec![
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "HEAD",
        "--",
        ".",
    ];
    let excluded = telemetry_path.map(|path| format!(":(exclude,literal){path}"));
    if let Some(path) = &excluded {
        args.push(path);
    }
    hasher.update(git(root, &args)?);
    let untracked = git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    for path in untracked.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = std::str::from_utf8(path).map_err(|error| error.to_string())?;
        if Some(path) == telemetry_path {
            continue;
        }
        safe_path(path)?;
        hasher.update(path.as_bytes());
        let mut file = fs::File::open(root.join(path)).map_err(|error| {
            format!("cannot bind untracked priority provenance {path}: {error}")
        })?;
        let mut digest = Sha256::new();
        let mut buffer = [0; 65536];
        loop {
            let count = file
                .read(&mut buffer)
                .map_err(|error| format!("cannot hash priority provenance {path}: {error}"))?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        hasher.update(digest.finalize());
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn metric(
    row: &Value,
    tree: &str,
    worktree: &str,
    policy: &str,
    manifest: &Value,
    field: &str,
) -> Option<f64> {
    if row["tree_oid"] != tree
        || row["source_sha256"] != manifest["source_sha256"]
        || row["test_sha256"] != manifest["test_sha256"]
        || row["worktree_sha256"] != worktree
        || row["policy_sha256"] != policy
    {
        return None;
    }
    row[field]
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn ordinal(novelty: &str) -> u64 {
    match novelty {
        "new" => 2,
        "changed" => 1,
        _ => 0,
    }
}

#[cfg(feature = "source-analysis")]
fn changed_unsupported(current: &Value, base: &Value) -> bool {
    current.as_object().is_some_and(|definitions| {
        definitions.iter().any(|(name, definition)| {
            name.starts_with("__unsupported_") && base.get(name) != Some(definition)
        })
    })
}

#[cfg(feature = "source-analysis")]
fn test_change(
    root: &Path,
    base: &str,
    path: &str,
    renames: &BTreeMap<String, String>,
) -> Result<&'static str, String> {
    if ![".py", ".rs", ".c", ".cc", ".cpp", ".cxx"]
        .iter()
        .any(|suffix| path.ends_with(suffix))
    {
        return Ok("unchanged");
    }
    let state = novelty(root, base, path, renames)?;
    if state == "unchanged" {
        return Ok(state);
    }
    let parse = |source: &str| {
        if path.ends_with(".py") {
            crate::candidate_verification::python_test_definitions(source, path)
        } else {
            crate::candidate_verification::decide(
                "native_test_definitions",
                &json!({"path":path,"source":source,"repo_root":root}),
            )
        }
    };
    let current = fs::read_to_string(root.join(path)).map_err(|error| error.to_string())?;
    let definitions = parse(&current)?;
    if definitions.as_object().is_some_and(|d| d.is_empty()) {
        return Ok("unchanged");
    }
    if state == "new" {
        if changed_unsupported(&definitions, &Value::Null) {
            return Err(
                "new native test surface has unsupported expanded/module identities".into(),
            );
        }
        return Ok("new");
    }
    let object = format!(
        "{base}:{}",
        renames.get(path).map(String::as_str).unwrap_or(path)
    );
    let old =
        String::from_utf8(git(root, &["show", &object])?).map_err(|error| error.to_string())?;
    let old_definitions = parse(&old)?;
    if changed_unsupported(&definitions, &old_definitions) {
        return Err(
            "changed native test surface has unsupported expanded/module identities".into(),
        );
    }
    Ok(if old_definitions == definitions {
        "unchanged"
    } else {
        "changed"
    })
}

#[cfg(not(feature = "source-analysis"))]
fn test_change(
    _: &Path,
    _: &str,
    _: &str,
    _: &BTreeMap<String, String>,
) -> Result<&'static str, String> {
    Err("priority test novelty requires the source-analysis feature".into())
}

struct RankContext<'a> {
    language: &'a str,
    root: &'a Path,
    scope: &'a Vec<String>,
    patterns: Vec<String>,
    base: String,
    tree: String,
    renames: BTreeMap<String, String>,
    worktree: String,
    telemetry: Value,
    policy_hash: String,
    source_roles: Value,
}

fn source_roles(root: &Path, paths: &[&str]) -> Value {
    #[cfg(feature = "source-analysis")]
    {
        crate::candidate_verification::decide(
            "native_source_roles",
            &json!({"repo_root":root,"paths":paths}),
        )
        .unwrap_or_else(|error| {
            json!(paths
                .iter()
                .map(|path| (*path, json!({"role":"unknown","reason":error})))
                .collect::<BTreeMap<_, _>>())
        })
    }
    #[cfg(not(feature = "source-analysis"))]
    {
        let _ = root;
        json!(paths
            .iter()
            .map(|path| (
                *path,
                json!({"role":"unknown","reason":"source-analysis feature unavailable"})
            ))
            .collect::<BTreeMap<_, _>>())
    }
}

fn unknown_role_row(manifest: &Value, path: &str, role: Value, state: &str) -> Value {
    json!({"campaign_id":manifest["campaign_id"],"sources":[{"path":path,"change":state,"sha256":manifest["source_sha256"][path]}],
        "source_role":role,"decision":"deferred-unresolved-source-role","novelty_rank":ordinal(state),"changed_tests":[],
        "dependency_impact_hint":null,"cost_hint_seconds":null,"telemetry_status":"unknown",
        "mutation_evidence":"unknown: unresolved source role","coverage_gap":"unknown"})
}

fn scoped_manifests(request: &PlanRequest, manifests: &[Value]) -> Result<Vec<Value>, String> {
    let scope = request
        .only_sources
        .as_ref()
        .ok_or("priority requires exact scope")?;
    let mut scoped = Vec::new();
    for manifest in manifests {
        let sources = manifest["source_sha256"]
            .as_object()
            .ok_or("manifest source hashes unavailable")?;
        for (source, hash) in sources.iter().filter(|(source, _)| scope.contains(source)) {
            let mut single = manifest.clone();
            single["source_sha256"] = json!({source:hash});
            let engine_source = if request.language == "rust" {
                let root = manifest["generator"]["options"]["package_root"]
                    .as_str()
                    .ok_or("cargo package root unavailable")?;
                Path::new(source)
                    .strip_prefix(root)
                    .map_err(|_| "cargo source is outside package root")?
                    .to_string_lossy()
                    .into_owned()
            } else {
                source.clone()
            };
            single["generator"]["source"] = json!([engine_source]);
            if sources.len() > 1 {
                let id = manifest["campaign_id"]
                    .as_str()
                    .ok_or("campaign id unavailable")?;
                let digest = format!("{:x}", Sha256::digest(source.as_bytes()));
                single["campaign_id"] = json!(format!("{id}-{}", &digest[..12]));
            }
            scoped.push(single);
        }
    }
    Ok(scoped)
}
fn prepare(request: &PlanRequest) -> Result<RankContext<'_>, String> {
    let options = request
        .priority
        .as_ref()
        .ok_or("missing priority options")?;
    if options
        .budget_seconds
        .is_some_and(|n| !n.is_finite() || n < 0.0)
    {
        return Err("priority budget must be finite and nonnegative".into());
    }
    let scope = request
        .only_sources
        .as_ref()
        .ok_or("priority requires an exact changed-file scope; all-files is not authorized")?;
    if scope.is_empty() {
        return Err("priority requires a nonempty exact scope".into());
    }
    for path in scope {
        safe_path(path)?;
    }
    let root = Path::new(&request.repo_root);
    let patterns = policy(root, &options.policy_path)?;
    let policy_hash = sha256_file(&root.join(&options.policy_path));
    let base = String::from_utf8(git(
        root,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", options.base_ref),
        ],
    )?)
    .map_err(|error| error.to_string())?;
    let tree = String::from_utf8(git(root, &["rev-parse", "HEAD^{tree}"])?)
        .map_err(|error| error.to_string())?;
    let renames = old_paths(root, base.trim())?;
    let role_paths: Vec<_> = scope
        .iter()
        .filter(|path| {
            request.language == "rust"
                && path.ends_with(".rs")
                && patterns
                    .iter()
                    .any(|pattern| crate::candidate_policy::glob_match(path, pattern))
        })
        .map(String::as_str)
        .collect();
    let source_roles = source_roles(root, &role_paths);
    let worktree = worktree_hash(root, options.telemetry_path.as_deref())?;
    let telemetry = telemetry(root, options)?;
    Ok(RankContext {
        language: &request.language,
        root,
        scope,
        patterns,
        base,
        tree,
        renames,
        worktree,
        telemetry,
        policy_hash,
        source_roles,
    })
}
impl RankContext<'_> {
    fn rank(&self, manifest: &Value, excluded: &mut Vec<Value>) -> Result<Option<Value>, String> {
        let sources = manifest["source_sha256"]
            .as_object()
            .ok_or("manifest source hashes unavailable")?
            .clone();
        let mut eligible = Vec::new();
        let mut source_rows = Vec::new();
        for path in sources.keys() {
            if !self.scope.contains(path) {
                return Err(format!("priority manifest escapes exact scope: {path}"));
            }
            let state = novelty(self.root, self.base.trim(), path, &self.renames)?;
            if state == "unchanged" && !self.renames.contains_key(path) {
                excluded.push(
                    json!({"path":path,"reason":"source is unchanged against the verified base"}),
                );
                continue;
            }
            if self
                .patterns
                .iter()
                .any(|pattern| crate::candidate_policy::glob_match(path, pattern))
            {
                eligible.push(path.clone());
                source_rows.push(json!({"path":path,"change":state,"sha256":sources[path]}));
            } else {
                excluded.push(json!({"path":path,"reason":"outside explicit production/validation risk policy"}));
            }
        }
        if eligible.is_empty() {
            return Ok(None);
        }
        let role = if self.language == "rust" {
            self.source_roles.get(&eligible[0]).cloned().unwrap_or_else(
                || json!({"role":"unknown","reason":"batch role inspection did not supply a role"}),
            )
        } else {
            json!({"role":"production","reason":"existing Python source/test pairing"})
        };
        match role["role"].as_str() {
            Some("production") => {}
            Some("test-only") => {
                excluded.push(json!({"path":eligible[0],"reason":"verified test-only module/target; not a production mutation target","source_role":role}));
                return Ok(None);
            }
            _ => {
                return Ok(Some(unknown_role_row(
                    manifest,
                    &eligible[0],
                    role,
                    source_rows[0]["change"]
                        .as_str()
                        .ok_or("source change metadata unavailable")?,
                )))
            }
        }
        self.rank_eligible(manifest, source_rows, role)
    }

    fn rank_eligible(
        &self,
        manifest: &Value,
        source_rows: Vec<Value>,
        role: Value,
    ) -> Result<Option<Value>, String> {
        let mut changed_tests = Vec::new();
        let mut identity_gaps = Vec::new();
        if let Some(tests) = manifest["test_sha256"].as_object() {
            for path in tests.keys().filter(|p| self.scope.contains(p)) {
                match test_change(self.root, self.base.trim(), path, &self.renames) {
                    Ok("unchanged") => {}
                    Ok(state) => changed_tests.push(json!({"path":path,"change":state})),
                    Err(error) => identity_gaps.push(json!({"path":path,"reason":error})),
                }
            }
        }
        let id = manifest["campaign_id"]
            .as_str()
            .ok_or("manifest has no campaign ID")?;
        let measurements = &self.telemetry["campaigns"][id];
        let cost = metric(
            measurements,
            self.tree.trim(),
            &self.worktree,
            &self.policy_hash,
            manifest,
            "cost_hint_seconds",
        );
        let impact = metric(
            measurements,
            self.tree.trim(),
            &self.worktree,
            &self.policy_hash,
            manifest,
            "dependency_impact_hint",
        );
        let score = source_rows
            .iter()
            .filter_map(|r| r["change"].as_str())
            .map(ordinal)
            .max()
            .unwrap_or(0);
        Ok(Some(
            json!({"campaign_id":id,"sources":source_rows,"source_role":role,"changed_tests":changed_tests,"test_identity_gaps":identity_gaps,
            "novelty_rank":score,"dependency_impact_hint":impact,"cost_hint_seconds":cost,
            "telemetry_status":if cost.is_some() || impact.is_some() {"provenance-matched-unverified-hints"} else if measurements.is_null() {"missing"} else {"stale-or-incomplete"},
            "mutation_evidence":"unknown: registered manifests do not prove a current validated PASS",
            "coverage_gap":"unknown: no current validated coverage report supplied"}),
        ))
    }
}
fn select(
    rows: &mut [Value],
    manifests: &[Value],
    budget: Option<f64>,
) -> (Vec<Value>, Option<f64>) {
    rows.sort_by(|a, b| {
        b["novelty_rank"]
            .as_u64()
            .cmp(&a["novelty_rank"].as_u64())
            .then_with(|| {
                b["changed_tests"]
                    .as_array()
                    .map(Vec::len)
                    .cmp(&a["changed_tests"].as_array().map(Vec::len))
            })
            .then_with(|| {
                b["dependency_impact_hint"]
                    .as_f64()
                    .unwrap_or(-1.0)
                    .total_cmp(&a["dependency_impact_hint"].as_f64().unwrap_or(-1.0))
            })
            .then_with(|| {
                a["sources"][0]["path"]
                    .as_str()
                    .cmp(&b["sources"][0]["path"].as_str())
            })
    });
    let mut remaining = budget;
    let mut selected = Vec::new();
    for row in rows.iter_mut() {
        if row["decision"] == "deferred-unresolved-source-role" {
            continue;
        }
        let decision = match (remaining, row["cost_hint_seconds"].as_f64()) {
            (Some(_), None) => "deferred-unknown-cost",
            (Some(left), Some(cost)) if cost > left => "deferred-budget",
            (Some(left), Some(cost)) => {
                remaining = Some(left - cost);
                "selected"
            }
            (None, _) => "selected",
        };
        row["decision"] = json!(decision);
        if decision == "selected" {
            if let Some(manifest) = manifests
                .iter()
                .find(|m| m["campaign_id"] == row["campaign_id"])
            {
                selected.push(manifest.clone());
            }
        }
    }
    (selected, remaining)
}
pub(super) fn apply(request: &PlanRequest, manifests: &mut Vec<Value>) -> Result<Value, String> {
    let context = prepare(request)?;
    *manifests = scoped_manifests(request, manifests)?;
    let options = request
        .priority
        .as_ref()
        .ok_or("missing priority options")?;
    let mut rows = Vec::new();
    let mut excluded = Vec::new();
    for manifest in manifests.iter() {
        if let Some(row) = context.rank(manifest, &mut excluded)? {
            rows.push(row);
        }
    }
    let mut unpaired_changed_tests = Vec::new();
    for path in context.scope {
        let paired = manifests
            .iter()
            .filter(|m| rows.iter().any(|r| r["campaign_id"] == m["campaign_id"]))
            .any(|m| m["test_sha256"].get(path).is_some());
        if !paired {
            let state = match test_change(context.root, context.base.trim(), path, &context.renames)
            {
                Ok(state) => state,
                Err(error) => {
                    unpaired_changed_tests.push(json!({"path":path,"change":"unknown","reason":error,
                        "disposition":"diagnostic only; policy eligibility and a mutation obligation are not inferred"}));
                    continue;
                }
            };
            if state != "unchanged" {
                unpaired_changed_tests.push(json!({"path":path,"change":state,
                    "reason":"changed test has no campaign inside the exact authorized source scope; scope was not expanded",
                    "disposition":"diagnostic only; policy eligibility and a mutation obligation are not inferred"}));
            }
        }
    }
    let (selected, remaining) = select(&mut rows, manifests, options.budget_seconds);
    *manifests = selected;
    Ok(json!({"policy":options.policy_path,
        "base_oid":context.base.trim(),"tree_oid":context.tree.trim(),"worktree_sha256":context.worktree,"policy_sha256":context.policy_hash,
        "order":["source novelty","changed paired tests","provenance-matched supplied impact hint","source path"],
        "ranked":rows,"excluded":excluded,"changed_tests_without_campaign":unpaired_changed_tests,"remaining_budget_seconds":remaining,
        "budget_basis":"unverified caller-supplied cost hints; no observed runtime or execution cap is asserted",
        "execution":"not requested; planning only"}))
}
