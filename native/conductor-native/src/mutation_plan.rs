//! Native port of `conductor.mutation_campaign_generate`'s `plan` step.
//!
//! Ports the changed-file walk, package-relative test pairing, exception filter
//! and manifest emission (`python_subjects`, `rust_subjects`, `fest_manifest`,
//! `cargo_manifest`, `existing_subjects`, `_admit_extra_tests`, `_plan_python`,
//! `_plan_rust`, `plan`) from `src/conductor/mutation_campaign_generate.py`.
//! Byte-identical output for the same inputs. `changed_sources()` (git diff plus
//! ownership-claim narrowing) and `[tool.conductor]` config resolution stay in
//! Python / the `forge` CLI wiring -- this module takes an already-resolved
//! `repo_root`, `campaigns_root` and `only_sources` scope.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

const SKIP_PARTS: &[&str] = &[
    ".git",
    ".mutation-native-crate",
    ".mull-build",
    ".tox",
    "__pycache__",
    "build",
    "node_modules",
    "site-packages",
    "target",
    "vendor",
];

fn is_skip_part(part: &str) -> bool {
    SKIP_PARTS.contains(&part)
        || part == "venv"
        || part.starts_with(".venv")
        || part.starts_with("venv-")
}

fn is_skipped_relative(relative: &str) -> bool {
    relative.split('/').any(is_skip_part)
}

fn is_test_name(relative: &str, name: &str) -> bool {
    name.starts_with("test_") || name == "conftest.py" || relative.contains("/tests/")
}

/// Recursively collect files under `root` matching `extension`, pruning any
/// directory whose name is a skip part so we never descend into it.
fn collect_files(root: &Path, extension: &str) -> Vec<PathBuf> {
    fn visit(dir: &Path, extension: &str, found: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if !is_skip_part(&name) {
                    visit(&path, extension, found);
                }
            } else if path.is_file() && path.extension().is_some_and(|ext| ext == extension) {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    visit(root, extension, &mut found);
    found
}

fn relative_posix(repo_root: &Path, path: &Path) -> String {
    path.strip_prefix(repo_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn sha256_file(path: &Path) -> String {
    match fs::read(path) {
        Ok(bytes) => format!("{:x}", Sha256::digest(bytes)),
        Err(_) => String::new(),
    }
}

fn line_count(path: &Path) -> u64 {
    match fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            if text.is_empty() {
                0
            } else {
                text.lines().count() as u64
            }
        }
        Err(_) => 0,
    }
}

fn stem_of(basename: &str) -> String {
    match basename.rfind('.') {
        Some(index) if index > 0 => basename[..index].to_string(),
        _ => basename.to_string(),
    }
}

fn parent_name(path_str: &str) -> String {
    let parts: Vec<&str> = path_str.split('/').collect();
    if parts.len() >= 2 {
        parts[parts.len() - 2].to_string()
    } else {
        String::new()
    }
}

fn slug(stem: &str) -> String {
    let replaced: String = stem
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    replaced.trim_matches('_').to_lowercase()
}

fn unique_slugs(keys: &[String]) -> BTreeMap<String, String> {
    let mut stems: BTreeMap<String, String> = BTreeMap::new();
    for key in keys {
        let basename = key.rsplit('/').next().unwrap_or(key.as_str());
        stems.insert(key.clone(), slug(&stem_of(basename)));
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for value in stems.values() {
        *counts.entry(value.clone()).or_insert(0) += 1;
    }
    let mut out = BTreeMap::new();
    for (key, stem) in &stems {
        if counts[stem] == 1 {
            out.insert(key.clone(), stem.clone());
        } else {
            let parent_slug = slug(&parent_name(key));
            let value = if parent_slug.is_empty() {
                stem.clone()
            } else {
                format!("{parent_slug}_{stem}")
            };
            out.insert(key.clone(), value);
        }
    }
    out
}

fn campaign_id(owner: &str, slug: &str, engine_slug: &str, day: &str) -> String {
    format!("{owner}_{slug}_{engine_slug}_{day}")
}

fn python_list_repr(items: &[String]) -> String {
    let escaped: Vec<String> = items
        .iter()
        .map(|item| {
            let escaped = item
                .replace('\\', "\\\\")
                .replace('\'', "\\'")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t");
            format!("'{escaped}'")
        })
        .collect();
    format!("[{}]", escaped.join(", "))
}

/// A module `a/b/c.py` pairs with `a/b/test_c.py` beside it, or with any
/// `<prefix>/tests/<tail>/test_c.py` cut from its own path.
fn mirrors(candidate_directories: &[&str], directories: &[&str]) -> bool {
    if candidate_directories == directories {
        return true;
    }
    let length = directories.len();
    for prefix in 0..=length {
        for tail in prefix..=length {
            let mut expected: Vec<&str> = directories[..prefix].to_vec();
            expected.push("tests");
            expected.extend_from_slice(&directories[tail..]);
            if candidate_directories == expected.as_slice() {
                return true;
            }
        }
    }
    false
}

fn mirrored_tests(source: &str, candidates: &[String]) -> Vec<String> {
    let mut parts: Vec<&str> = source.split('/').collect();
    parts.pop();
    let directories = parts;
    let mut out: Vec<String> = candidates
        .iter()
        .filter(|candidate| {
            let mut candidate_parts: Vec<&str> = candidate.split('/').collect();
            candidate_parts.pop();
            mirrors(&candidate_parts, &directories)
        })
        .cloned()
        .collect();
    out.sort();
    out
}

#[derive(Debug, Clone, Serialize)]
struct PySubject {
    source: String,
    lines: u64,
    tests: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct Unpaired {
    source: String,
    lines: u64,
}

fn python_subjects(repo_root: &Path) -> Result<(Vec<PySubject>, Vec<Unpaired>), String> {
    let mut files = collect_files(repo_root, "py");
    files.sort();

    let mut tests: HashMap<String, Vec<String>> = HashMap::new();
    let mut sources: Vec<(String, PathBuf)> = Vec::new();
    for path in &files {
        let relative = relative_posix(repo_root, path);
        if is_skipped_relative(&relative) {
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if is_test_name(&relative, &name) {
            tests.entry(name).or_default().push(relative);
        } else {
            sources.push((relative, path.clone()));
        }
    }
    sources.sort_by(|a, b| a.0.cmp(&b.0));

    let mut paired: Vec<PySubject> = Vec::new();
    let mut unpaired: Vec<Unpaired> = Vec::new();
    for (relative, path) in &sources {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let lines = line_count(path);
        let empty: Vec<String> = Vec::new();
        let matches = tests.get(&format!("test_{name}")).unwrap_or(&empty);
        let mirrored = mirrored_tests(relative, matches);
        if !mirrored.is_empty() {
            paired.push(PySubject {
                source: relative.clone(),
                lines,
                tests: mirrored,
            });
        } else if matches.len() == 1 {
            paired.push(PySubject {
                source: relative.clone(),
                lines,
                tests: matches.clone(),
            });
        } else if !matches.is_empty() {
            let mut sorted_matches = matches.clone();
            sorted_matches.sort();
            return Err(format!(
                "{relative} has no test in its own package, and several unrelated files share the name test_{name}: {}. Put the test beside the module (or under its tests/ mirror) so the pairing can say which one counts.",
                python_list_repr(&sorted_matches)
            ));
        } else {
            unpaired.push(Unpaired {
                source: relative.clone(),
                lines,
            });
        }
    }
    Ok((paired, unpaired))
}

#[derive(Debug, Clone, Serialize)]
struct RustCrate {
    package: String,
    root: String,
    manifest: String,
    files: Vec<String>,
    lines: u64,
}

fn package_name(manifest: &Path) -> Option<String> {
    let text = fs::read_to_string(manifest).ok()?;
    let mut in_package = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package && line.starts_with("name") {
            let (_, value) = line.split_once('=')?;
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if value.is_empty() {
                return None;
            }
            return Some(value.to_string());
        }
    }
    None
}

fn rust_subjects(repo_root: &Path) -> Result<Vec<RustCrate>, String> {
    let mut manifests = collect_files(repo_root, "toml")
        .into_iter()
        .filter(|path| path.file_name().is_some_and(|n| n == "Cargo.toml"))
        .collect::<Vec<_>>();
    manifests.sort();

    let mut crates = Vec::new();
    for manifest in &manifests {
        let relative_manifest = relative_posix(repo_root, manifest);
        if is_skipped_relative(&relative_manifest) {
            continue;
        }
        let Some(package) = package_name(manifest) else {
            continue;
        };
        let crate_dir = manifest.parent().unwrap_or(repo_root);
        let root = relative_posix(repo_root, crate_dir);
        let src_dir = crate_dir.join("src");
        let mut files: Vec<String> = collect_files(&src_dir, "rs")
            .into_iter()
            .map(|p| relative_posix(repo_root, &p))
            .filter(|relative| !is_skipped_relative(relative))
            .collect();
        files.sort();
        if files.is_empty() {
            return Err(format!(
                "crate {root} declares a package but has no src/*.rs"
            ));
        }
        let lines = files.iter().map(|f| line_count(&repo_root.join(f))).sum();
        crates.push(RustCrate {
            package,
            root,
            manifest: relative_manifest,
            files,
            lines,
        });
    }
    Ok(crates)
}

fn rust_test_files(subject: &RustCrate, repo_root: &Path) -> Result<Vec<String>, String> {
    let root = repo_root.join(&subject.root);
    let mut found: BTreeSet<String> = BTreeSet::new();
    let tests_dir = root.join("tests");
    if tests_dir.is_dir() {
        for path in collect_files(&tests_dir, "rs") {
            let relative = relative_posix(repo_root, &path);
            if !is_skipped_relative(&relative) {
                found.insert(relative);
            }
        }
    }
    for relative in &subject.files {
        let path = repo_root.join(relative);
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {relative}: {error}"))?;
        if text.contains("#[cfg(test)]") {
            found.insert(relative.clone());
        }
    }
    if found.is_empty() {
        return Err(format!(
            "crate {} has no tests: no tests/*.rs and no #[cfg(test)] module. A mutation campaign over untested code would report every mutant as survived and prove nothing that reading the crate does not already say.",
            subject.package
        ));
    }
    Ok(found.into_iter().collect())
}

const SURVIVOR_BASELINE_NOTE: &str = "Empty and unrecorded. The FIRST engine run writes this list itself and flips survivor_baseline_recorded to true; every later run is scored against it, so a survivor that appears afterwards is a test that stopped defending its code. No agent ever writes this field -- hand-authored baselines are forbidden (KB-MUT-02), and an unrecorded baseline is why a fresh campaign used to be red on every run.";

fn fest_manifest(
    subject: &PySubject,
    campaign_id: &str,
    repo_root: &Path,
    run_timeout_seconds: i64,
) -> Value {
    let source = subject.source.clone();
    let tests = subject.tests.clone();
    let mut source_sha256 = Map::new();
    source_sha256.insert(
        source.clone(),
        Value::String(sha256_file(&repo_root.join(&source))),
    );
    let mut test_sha256 = Map::new();
    for test in &tests {
        test_sha256.insert(
            test.clone(),
            Value::String(sha256_file(&repo_root.join(test))),
        );
    }
    let mut test_argv = vec![
        "python".to_string(),
        "-m".to_string(),
        "pytest".to_string(),
        "-q".to_string(),
        "--rootdir=.".to_string(),
    ];
    test_argv.extend(tests);
    json!({
        "schema_version": 1,
        "campaign_id": campaign_id,
        "title": format!("Generated mutants for {source}, scored on the survivor set"),
        "language": "python",
        "mutation_engine": "fest",
        "generator": {
            "source": [source],
            "exclude": ["**/test_*.py", "**/conftest.py"],
            "operators": [],
            "seed": 0,
            "run_timeout_seconds": run_timeout_seconds,
        },
        "test_argv": test_argv,
        "environment": {},
        "source_sha256": Value::Object(source_sha256),
        "test_sha256": Value::Object(test_sha256),
        "survivor_baseline": [],
        "survivor_baseline_recorded": false,
        "survivor_baseline_note": SURVIVOR_BASELINE_NOTE,
    })
}

fn cargo_manifest(
    subject: &RustCrate,
    campaign_id: &str,
    repo_root: &Path,
    jobs: i64,
    run_timeout_seconds: i64,
    sources: Option<&[String]>,
) -> Result<Value, String> {
    let selected: Vec<String> = match sources {
        None => subject.files.clone(),
        Some(explicit) => {
            let mut set: BTreeSet<String> = explicit.iter().cloned().collect();
            let unknown: Vec<String> = set
                .iter()
                .filter(|s| !subject.files.contains(s))
                .cloned()
                .collect();
            if !unknown.is_empty() {
                return Err(format!(
                    "scoped source is not in '{}': {}",
                    subject.package,
                    python_list_repr(&unknown)
                ));
            }
            let mut vec: Vec<String> = set.iter().cloned().collect();
            vec.sort();
            set.clear();
            vec
        }
    };
    if selected.is_empty() {
        return Err(format!(
            "cargo campaign for '{}' has no scoped Rust source",
            subject.package
        ));
    }
    let package_root = PathBuf::from(&subject.root);
    let source_relative: Vec<String> = selected
        .iter()
        .map(|relative| {
            Path::new(relative)
                .strip_prefix(&package_root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| relative.clone())
        })
        .collect();
    let mut source_sha256 = Map::new();
    for relative in &selected {
        source_sha256.insert(
            relative.clone(),
            Value::String(sha256_file(&repo_root.join(relative))),
        );
    }
    let mut test_sha256 = Map::new();
    for relative in rust_test_files(subject, repo_root)? {
        let hash = sha256_file(&repo_root.join(&relative));
        test_sha256.insert(relative, Value::String(hash));
    }
    Ok(json!({
        "schema_version": 1,
        "campaign_id": campaign_id,
        "title": format!("Generated mutants for the {} crate, scored on the survivor set", subject.package),
        "language": "rust",
        "mutation_engine": "cargo-mutants",
        "generator": {
            "source": source_relative,
            "exclude": [],
            "operators": [],
            "options": {
                "manifest_path": subject.manifest,
                "package": subject.package,
                "package_root": subject.root,
            },
            "seed": 0,
            "jobs": jobs,
            "run_timeout_seconds": run_timeout_seconds,
        },
        "test_argv": [
            "cargo", "test",
            "--manifest-path", subject.manifest,
            "--package", subject.package,
        ],
        "environment": {},
        "source_sha256": Value::Object(source_sha256),
        "test_sha256": Value::Object(test_sha256),
        "survivor_baseline": [],
        "survivor_baseline_recorded": false,
        "survivor_baseline_note": SURVIVOR_BASELINE_NOTE,
    }))
}

const ENGINE_KEYS: &[&str] = &["fest", "cargo-mutants", "mull"];

fn existing_subjects(repo_root: &Path, campaigns_root: &str) -> BTreeSet<String> {
    let mut covered = BTreeSet::new();
    let dir = repo_root.join(campaigns_root);
    let Ok(entries) = fs::read_dir(&dir) else {
        return covered;
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    for path in paths {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(Value::Object(payload)) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(engine) = payload.get("mutation_engine").and_then(Value::as_str) else {
            continue;
        };
        if !ENGINE_KEYS.contains(&engine) {
            continue;
        }
        let Some(Value::Object(generator)) = payload.get("generator") else {
            continue;
        };
        if let Some(Value::Object(options)) = generator.get("options") {
            if let Some(package) = options.get("package").and_then(Value::as_str) {
                covered.insert(package.to_string());
            }
        }
        if let Some(Value::Array(sources)) = generator.get("source") {
            for source in sources {
                if let Some(text) = source.as_str() {
                    covered.insert(text.to_string());
                }
            }
        }
    }
    covered
}

fn admit_extra_tests(
    paired: Vec<PySubject>,
    unpaired: Vec<Unpaired>,
    extra_tests: &BTreeMap<String, Vec<String>>,
) -> Result<(Vec<PySubject>, Vec<Unpaired>), String> {
    if extra_tests.is_empty() {
        return Ok((paired, unpaired));
    }
    let mut order: Vec<String> = Vec::new();
    let mut by_source: HashMap<String, PySubject> = HashMap::new();
    for subject in paired {
        order.push(subject.source.clone());
        by_source.insert(subject.source.clone(), subject);
    }
    let still_unpaired: Vec<Unpaired> = unpaired
        .iter()
        .filter(|s| !extra_tests.contains_key(&s.source))
        .cloned()
        .collect();
    for subject in &unpaired {
        if extra_tests.contains_key(&subject.source) {
            if !by_source.contains_key(&subject.source) {
                order.push(subject.source.clone());
            }
            by_source.insert(
                subject.source.clone(),
                PySubject {
                    source: subject.source.clone(),
                    lines: subject.lines,
                    tests: Vec::new(),
                },
            );
        }
    }
    for (source, tests) in extra_tests {
        let Some(entry) = by_source.get_mut(source) else {
            return Err(format!("--extra-test names no python subject: {source}"));
        };
        let mut merged: BTreeSet<String> = entry.tests.iter().cloned().collect();
        merged.extend(tests.iter().cloned());
        entry.tests = merged.into_iter().collect();
    }
    let result: Vec<PySubject> = order
        .into_iter()
        .filter_map(|key| by_source.remove(&key))
        .collect();
    Ok((result, still_unpaired))
}

type PythonPlanResult = Result<(Vec<Value>, Vec<Unpaired>, Vec<String>), String>;
type RustPlanResult = Result<(Vec<Value>, Vec<String>, Vec<Value>), String>;

#[allow(clippy::too_many_arguments)]
fn plan_python(
    repo_root: &Path,
    covered: &BTreeSet<String>,
    scope: Option<&BTreeSet<String>>,
    owner: &str,
    day: &str,
    run_timeout_seconds: i64,
    extra_tests: &BTreeMap<String, Vec<String>>,
) -> PythonPlanResult {
    let (paired, unpaired) = python_subjects(repo_root)?;
    let (mut paired, mut unpaired) = admit_extra_tests(paired, unpaired, extra_tests)?;
    if let Some(scope) = scope {
        paired.retain(|s| scope.contains(&s.source) || s.tests.iter().any(|t| scope.contains(t)));
        unpaired.retain(|s| scope.contains(&s.source));
    }
    let mut already: Vec<String> = paired
        .iter()
        .filter(|s| covered.contains(&s.source))
        .map(|s| s.source.clone())
        .collect();
    already.sort();
    paired.retain(|s| !covered.contains(&s.source));
    let sources: Vec<String> = paired.iter().map(|s| s.source.clone()).collect();
    let slugs = unique_slugs(&sources);
    let mut manifests = Vec::new();
    for subject in &paired {
        let id = campaign_id(owner, &slugs[&subject.source], "fest", day);
        manifests.push(fest_manifest(subject, &id, repo_root, run_timeout_seconds));
    }
    Ok((manifests, unpaired, already))
}

#[allow(clippy::too_many_arguments)]
fn plan_rust(
    repo_root: &Path,
    covered: &BTreeSet<String>,
    scope: Option<&BTreeSet<String>>,
    owner: &str,
    day: &str,
    jobs: i64,
    run_timeout_seconds: i64,
) -> RustPlanResult {
    let mut crates = rust_subjects(repo_root)?;
    let mut scoped_sources: HashMap<String, Vec<String>> = HashMap::new();
    if let Some(scope) = scope {
        for crate_ in &crates {
            let selected: Vec<String> = crate_
                .files
                .iter()
                .filter(|f| scope.contains(*f))
                .cloned()
                .collect();
            if !selected.is_empty() {
                scoped_sources.insert(crate_.package.clone(), selected);
            }
        }
        crates.retain(|c| scoped_sources.contains_key(&c.package));
    }
    let mut already: Vec<String> = crates
        .iter()
        .filter(|c| covered.contains(&c.package))
        .map(|c| c.package.clone())
        .collect();
    already.sort();
    crates.retain(|c| !covered.contains(&c.package));
    let roots: Vec<String> = crates.iter().map(|c| c.root.clone()).collect();
    let slugs = unique_slugs(&roots);
    let mut manifests = Vec::new();
    let mut untested = Vec::new();
    for subject in &crates {
        let id = campaign_id(owner, &slugs[&subject.root], "cargo", day);
        let sources = if scope.is_none() {
            None
        } else {
            scoped_sources.get(&subject.package).map(|v| v.as_slice())
        };
        match cargo_manifest(subject, &id, repo_root, jobs, run_timeout_seconds, sources) {
            Ok(manifest) => manifests.push(manifest),
            Err(reason) => untested.push(json!({
                "package": subject.package,
                "root": subject.root,
                "lines": subject.lines,
                "reason": reason,
            })),
        }
    }
    Ok((manifests, already, untested))
}

#[derive(Debug, Deserialize)]
pub struct PlanRequest {
    pub language: String,
    pub repo_root: String,
    pub owner: String,
    pub day: String,
    #[serde(default = "default_jobs")]
    pub jobs: i64,
    #[serde(default = "default_timeout")]
    pub run_timeout_seconds: i64,
    pub campaigns_root: String,
    #[serde(default)]
    pub only_sources: Option<Vec<String>>,
    #[serde(default)]
    pub include_covered: bool,
    #[serde(default)]
    pub extra_tests: BTreeMap<String, Vec<String>>,
}

fn default_jobs() -> i64 {
    4
}

fn default_timeout() -> i64 {
    1800
}

/// Compute one `plan()`-equivalent response. No Python involved: pure filesystem
/// reads under `request.repo_root`.
pub fn compute_plan(request: &PlanRequest) -> Result<Value, String> {
    if request.language != "python" && request.language != "rust" {
        return Err(format!(
            "unknown language '{}'; known: python, rust",
            request.language
        ));
    }
    if !request.extra_tests.is_empty() && request.language != "python" {
        return Err("--extra-test pairs python subjects only".to_string());
    }
    let repo_root = Path::new(&request.repo_root);
    let covered = if request.include_covered {
        BTreeSet::new()
    } else {
        existing_subjects(repo_root, &request.campaigns_root)
    };
    let scope: Option<BTreeSet<String>> = request
        .only_sources
        .as_ref()
        .map(|paths| paths.iter().cloned().collect());

    let (manifests, unpaired_json, untested_json, already): (
        Vec<Value>,
        Vec<Value>,
        Vec<Value>,
        Vec<String>,
    ) = if request.language == "python" {
        let (manifests, unpaired, already) = plan_python(
            repo_root,
            &covered,
            scope.as_ref(),
            &request.owner,
            &request.day,
            request.run_timeout_seconds,
            &request.extra_tests,
        )?;
        let unpaired_json = unpaired
            .into_iter()
            .map(|u| json!({"source": u.source, "lines": u.lines}))
            .collect();
        (manifests, unpaired_json, Vec::new(), already)
    } else {
        let (manifests, already, untested) = plan_rust(
            repo_root,
            &covered,
            scope.as_ref(),
            &request.owner,
            &request.day,
            request.jobs,
            request.run_timeout_seconds,
        )?;
        (manifests, Vec::new(), untested, already)
    };

    let unpaired_lines: u64 = unpaired_json
        .iter()
        .filter_map(|u| u.get("lines").and_then(Value::as_u64))
        .sum();

    Ok(json!({
        "language": request.language,
        "manifests": manifests,
        "unpaired": unpaired_json,
        "unpaired_lines": unpaired_lines,
        "already_covered": already,
        "untested": untested_json,
    }))
}

fn value_error(message: impl Into<String>) -> PyErr {
    PyValueError::new_err(message.into())
}

#[pyfunction]
pub fn mutation_plan_native(request_json: &str) -> PyResult<String> {
    let request: PlanRequest = serde_json::from_str(request_json)
        .map_err(|error| value_error(format!("invalid mutation plan request: {error}")))?;
    let result = compute_plan(&request).map_err(value_error)?;
    serde_json::to_string(&result).map_err(|error| value_error(error.to_string()))
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(mutation_plan_native, module)?)?;
    Ok(())
}

#[cfg(test)]
mod fixture_parity_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn fixtures_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mutation_plan")
    }

    fn scratch_dir() -> PathBuf {
        let id = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "conductor-native-mutation-plan-test-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn copy_tree(src: &Path, dst: &Path) {
        for entry in fs::read_dir(src).expect("read tree dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            let target = dst.join(entry.file_name());
            if path.is_dir() {
                fs::create_dir_all(&target).expect("mkdir");
                copy_tree(&path, &target);
            } else {
                fs::copy(&path, &target).expect("copy file");
            }
        }
    }

    fn run_case(name: &str) -> (Result<Value, String>, PathBuf) {
        let case_dir = fixtures_root().join(name);
        let tree_src = case_dir.join("tree");
        let repo_root = scratch_dir();
        if tree_src.is_dir() {
            copy_tree(&tree_src, &repo_root);
        }
        let request_text =
            fs::read_to_string(case_dir.join("request.json")).expect("read request.json");
        let mut request_value: Value =
            serde_json::from_str(&request_text).expect("parse request.json");
        request_value["repo_root"] = Value::String(repo_root.to_string_lossy().to_string());
        let request: PlanRequest =
            serde_json::from_value(request_value).expect("deserialize request");
        (compute_plan(&request), repo_root)
    }

    fn cases() -> Vec<&'static str> {
        vec![
            "case01_basic_pair",
            "case02_mirrored_tests_dir",
            "case03_legacy_single_match",
            "case04_ambiguous_same_basename",
            "case05_unpaired",
            "case06_extra_tests",
            "case07_already_covered",
            "case08_scope_filter",
            "case09_rust_inline_test",
            "case10_rust_tests_dir",
            "case11_rust_untested",
            "case12_rust_scoped",
            "case13_skip_dirs",
            "case14_slug_collision",
            "case15_include_covered",
        ]
    }

    #[test]
    fn frozen_fixtures_match_recorded_python_output() {
        for name in cases() {
            let case_dir = fixtures_root().join(name);
            let (result, repo_root) = run_case(name);
            let expected_error_path = case_dir.join("expected_error.txt");
            if expected_error_path.is_file() {
                let expected =
                    fs::read_to_string(&expected_error_path).expect("read expected_error.txt");
                let err = result.expect_err(&format!("{name}: expected an error"));
                assert_eq!(err, expected.trim_end(), "case {name} error mismatch");
            } else {
                let expected_text = fs::read_to_string(case_dir.join("expected.json"))
                    .unwrap_or_else(|_| panic!("{name}: missing expected.json"));
                let expected: Value =
                    serde_json::from_str(&expected_text).expect("parse expected.json");
                let actual = result.unwrap_or_else(|e| panic!("{name}: unexpected error: {e}"));
                assert_eq!(actual, expected, "case {name} output mismatch");
            }
            let _ = fs::remove_dir_all(&repo_root);
        }
    }

    #[test]
    fn unknown_language_is_refused() {
        let repo_root = scratch_dir();
        let request = PlanRequest {
            language: "cobol".to_string(),
            repo_root: repo_root.to_string_lossy().to_string(),
            owner: "llm-b0".to_string(),
            day: "20260913".to_string(),
            jobs: 4,
            run_timeout_seconds: 1800,
            campaigns_root: "conductor/mutation_campaigns".to_string(),
            only_sources: None,
            include_covered: false,
            extra_tests: BTreeMap::new(),
        };
        let err = compute_plan(&request).expect_err("unknown language must be refused");
        assert!(err.contains("unknown language"));
        let _ = fs::remove_dir_all(&repo_root);
    }
}
