//! Advisory dirty-source previews, isolated from committed landing evidence.

use super::{common_dir, evidence, git_bytes, root};
use crate::land_exec::{run_measured, Outcome, ShellRun};
use crate::performance::identity;
use anyhow::{ensure, Context, Result};
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Args)]
pub struct PreviewArgs {
    /// Exact paths; defaults to tracked dirty plus nonignored untracked source paths.
    #[arg(long)]
    pub path: Vec<String>,
    /// Execute in a disposable file snapshot. Default is a read-only plan.
    #[arg(long)]
    pub execute: bool,
    /// Include the graph's full selected-path inventory; default reports counts.
    #[arg(long)]
    pub details: bool,
    /// Optional Rust test name filter, applied only to preview tests.
    #[arg(long)]
    pub filter: Option<String>,
    #[arg(long, default_value_t = 600)]
    pub timeout_seconds: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Target {
    crate_name: String,
    manifest: String,
    features: Vec<String>,
    selector: Vec<String>,
}

#[derive(Serialize)]
struct Plan {
    schema: &'static str,
    advisory: bool,
    changed_paths: Vec<String>,
    targets: Vec<Target>,
    excluded_compatibility_tests: Vec<String>,
    selection: serde_json::Value,
    filter: Option<String>,
}

fn dirty_paths(root: &Path) -> Result<Vec<String>> {
    let mut paths = BTreeSet::new();
    for args in [
        vec!["diff", "--name-only", "-z", "HEAD"],
        vec!["ls-files", "--others", "--exclude-standard", "-z"],
    ] {
        for path in git_bytes(root, &args)?
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
        {
            let path = String::from_utf8(path.to_vec())?;
            if !path.starts_with("research/") {
                paths.insert(path);
            }
        }
    }
    Ok(paths.into_iter().collect())
}

fn crate_for(path: &str) -> Vec<&'static str> {
    if path.starts_with("native/forge/") || path.starts_with("native/forge-cli/") {
        vec!["forge"]
    } else if path.starts_with("native/conductor-native/") {
        vec!["conductor-native", "forge"]
    } else if path.starts_with("native/slop-core/") {
        vec!["slop-core", "forge"]
    } else if path.starts_with("src/conductor/") {
        vec!["conductor-native"]
    } else if path.starts_with("src/tooling/") {
        vec!["conductor-native", "forge"]
    } else if matches!(
        path,
        "Makefile" | "forge.toml" | "pyproject.toml" | "uv.lock" | "candidate_policy.toml"
    ) || path.starts_with(".forge/")
    {
        vec!["forge", "conductor-native", "slop-core"]
    } else {
        vec![]
    }
}

fn target(crate_name: &str, test: Option<&str>) -> Target {
    let selector = match test {
        Some(test) => vec!["--test".into(), test.into()],
        None if crate_name == "forge" => vec!["--lib".into(), "--bin".into(), "forge".into()],
        None => vec!["--lib".into()],
    };
    Target {
        crate_name: crate_name.into(),
        manifest: format!("native/{crate_name}/Cargo.toml"),
        features: if crate_name == "conductor-native" {
            let mut features = vec!["bundled-sqlite".into()];
            if test == Some("python_contracts_discovery") {
                features.push("contract-registry".into());
            }
            features
        } else {
            vec![]
        },
        selector,
    }
}

fn plan(root: &Path, paths: Vec<String>, filter: Option<String>, details: bool) -> Result<Plan> {
    for path in &paths {
        ensure!(
            !Path::new(path).is_absolute() && !path.split('/').any(|p| p == ".." || p.is_empty()),
            "preview path must be exact and relative: {path}"
        );
    }
    let crates: BTreeSet<_> = paths.iter().flat_map(|path| crate_for(path)).collect();
    let selection = conductor_native::graph_context::dispatch("test_selection", &json!({"repo":root,"paths":paths}))
        .unwrap_or_else(|error| json!({"complete":false,"scope":"affected-crates","reasons":[format!("graph unavailable: {error}")],"paths":[]}));
    let mut targets: Vec<_> = crates.iter().map(|name| target(name, None)).collect();
    let mut excluded_compatibility_tests = BTreeSet::new();
    let selected = selection["paths"].as_array().cloned().unwrap_or_default();
    for path in selected
        .iter()
        .filter_map(|p| p.as_str())
        .chain(paths.iter().map(String::as_str))
    {
        for name in &crates {
            let prefix = format!("native/{name}/tests/");
            if let Some(relative) = path.strip_prefix(&prefix) {
                if !relative.contains('/') && relative.ends_with(".rs") {
                    let test = relative.trim_end_matches(".rs");
                    // The discovery planner tests are native despite their
                    // historical prefix; only adapter contracts require Python.
                    if test.starts_with("python_contracts_") && test != "python_contracts_discovery"
                    {
                        excluded_compatibility_tests.insert(path.to_owned());
                        continue;
                    }
                    let row = target(name, Some(test));
                    if let Some(existing) = targets
                        .iter_mut()
                        .find(|t| t.crate_name == row.crate_name && t.features == row.features)
                    {
                        let already = existing
                            .selector
                            .windows(2)
                            .any(|pair| pair == row.selector.as_slice());
                        if !already {
                            existing.selector.extend(row.selector);
                        }
                    } else {
                        targets.push(row);
                    }
                }
            }
        }
    }
    Ok(Plan {
        schema: "forge.preview-plan.v1",
        advisory: true,
        changed_paths: paths,
        targets,
        excluded_compatibility_tests: excluded_compatibility_tests.into_iter().collect(),
        selection: if details {
            selection
        } else {
            json!({"complete":selection["complete"],"scope":selection["scope"],
            "reasons":selection["reasons"],"generation":selection["generation"],"selected_test_count":selected.len(),
            "details_available":true})
        },
        filter,
    })
}

/// Snapshot all versioned inputs plus nonignored source additions, never a Git tree.
fn inputs(root: &Path) -> Result<BTreeMap<String, String>> {
    let paths = git_bytes(
        root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?;
    let mut result = BTreeMap::new();
    for bytes in paths.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = String::from_utf8(bytes.to_vec())?;
        if path.starts_with("research/") {
            continue;
        }
        if !root.join(&path).exists() {
            continue;
        }
        let hashes = identity::files(root, std::slice::from_ref(&path))?;
        result.extend(hashes);
    }
    Ok(result)
}

fn snapshot(root: &Path, directory: &Path, inputs: &BTreeMap<String, String>) -> Result<()> {
    std::fs::create_dir_all(directory)?;
    for (path, expected) in inputs {
        let destination = directory.join(path);
        std::fs::create_dir_all(destination.parent().context("snapshot input parent")?)?;
        std::fs::copy(root.join(path), &destination)?;
        ensure!(
            evidence::sha256_file(&destination)? == *expected,
            "input changed during preview snapshot: {path}"
        );
    }
    Ok(())
}

fn commands(row: &Target, cache: &Path, no_run: bool, filter: &Option<String>) -> Vec<String> {
    let mut argv = vec![
        "cargo".into(),
        "test".into(),
        "--offline".into(),
        "--locked".into(),
        "--jobs".into(),
        "2".into(),
        "--manifest-path".into(),
        row.manifest.clone(),
        "--target-dir".into(),
        cache.display().to_string(),
    ];
    if !row.features.is_empty() {
        argv.extend(["--features".into(), row.features.join(",")]);
    }
    argv.extend(row.selector.clone());
    if no_run {
        argv.push("--no-run".into());
    } else {
        argv.extend(["--".into(), "--test-threads=2".into()]);
        if let Some(filter) = filter {
            argv.push(filter.clone());
        }
    }
    argv
}

fn cli_build(cache: &Path) -> Vec<String> {
    vec![
        "cargo".into(),
        "install".into(),
        "--debug".into(),
        "--force".into(),
        "--offline".into(),
        "--locked".into(),
        "--jobs".into(),
        "2".into(),
        "--path".into(),
        "native/forge-cli".into(),
        "--root".into(),
        cache.with_file_name("test-install").display().to_string(),
        "--target-dir".into(),
        cache.display().to_string(),
        "--bin".into(),
        "forge".into(),
    ]
}

fn clean_commands(row: &Target, cache: &Path) -> Vec<Vec<String>> {
    let packages = if row.crate_name == "forge" {
        vec!["forge", "forge-cli"]
    } else {
        vec![row.crate_name.as_str()]
    };
    packages
        .into_iter()
        .map(|package| {
            vec![
                "cargo".into(),
                "clean".into(),
                "--manifest-path".into(),
                row.manifest.clone(),
                "--target-dir".into(),
                cache.display().to_string(),
                "--package".into(),
                package.into(),
            ]
        })
        .collect()
}

fn invoke(
    root: &Path,
    argv: &[String],
    env: &[(String, String)],
    log: &Path,
    timeout: u64,
) -> Result<crate::land_exec::Usage> {
    let command = argv
        .iter()
        .map(|s| identity::quote(s))
        .collect::<Vec<_>>()
        .join(" ");
    let command = super::clean_command(&command, env);
    let measured = run_measured(
        &ShellRun {
            command: &command,
            cwd: root,
            env,
            log,
            timeout: Duration::from_secs(timeout),
        },
        16 * 1024 * 1024,
    )?;
    ensure!(
        measured.outcome == Outcome::Passed,
        "preview command failed {:?}: {}\n{}",
        measured.outcome,
        log.display(),
        crate::land_exec::tail(log, 20)
    );
    Ok(measured.usage)
}

fn cache_workspace(
    cache: &Path,
    snapshot: &Path,
    hashes: &BTreeMap<String, String>,
) -> Result<std::path::PathBuf> {
    let directory = cache.join("workspace");
    std::fs::create_dir_all(&directory)?;
    let manifest = cache.join("inputs.json");
    let old: BTreeMap<String, String> = match std::fs::read(&manifest) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(error) => return Err(error.into()),
    };
    for path in old.keys().filter(|path| !hashes.contains_key(*path)) {
        ensure!(
            !Path::new(path).is_absolute() && !path.split('/').any(|p| p == ".."),
            "unsafe cached preview input {path}"
        );
        match std::fs::remove_file(directory.join(path)) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    for (path, hash) in hashes {
        let destination = directory.join(path);
        let unchanged = destination.is_file() && evidence::sha256_file(&destination)? == *hash;
        if !unchanged {
            std::fs::create_dir_all(destination.parent().context("cached input parent")?)?;
            std::fs::copy(snapshot.join(path), &destination)?;
            ensure!(
                evidence::sha256_file(&destination)? == *hash,
                "cached preview source changed during copy: {path}"
            );
        }
    }
    evidence::write_atomic(&manifest, &serde_json::to_vec(hashes)?)?;
    Ok(directory)
}

fn cache_lock(cache: &Path) -> Result<forge::file_lock::Exclusive> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(cache.join("lock"))?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.nlink() == 1
            && meta.mode() & 0o077 == 0,
        "preview lock must be private, owned and singly linked"
    );
    forge::file_lock::Exclusive::try_new(file, "preview cache")
}

fn run_target(
    root: &Path,
    dir: &Path,
    row: &Target,
    inputs: &BTreeMap<String, String>,
    args: &PreviewArgs,
) -> Result<serde_json::Value> {
    let config = serde_json::to_vec(&(
        "native-preview-v3",
        (&row.crate_name, &row.manifest, &row.features),
        identity::toolchain()?,
        identity::environment(&[])?,
        root,
    ))?;
    let cache = common_dir(root)?
        .join("forge-previews/cache")
        .join(evidence::sha256(&config));
    std::fs::create_dir_all(&cache)?;
    let _lease = cache_lock(&cache)?;
    // Keep Cargo's managed directory separate from snapshot/cache metadata;
    // Cargo clean requires its own CACHEDIR.TAG to guard against data deletion.
    let artifacts = cache.join("artifacts");
    let state = cache.join("source.json");
    let target_state = cache.join(format!(
        "target-{}.sha256",
        evidence::sha256(&serde_json::to_vec(&row.selector)?)
    ));
    let digest = evidence::sha256(&serde_json::to_vec(inputs)?);
    let previous = match std::fs::read_to_string(&state) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let source_same = previous.as_deref() == Some(digest.as_str());
    let target_digest = match std::fs::read_to_string(&target_state) {
        Ok(value) => Some(value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let reused = source_same && target_digest.as_deref() == Some(digest.as_str());
    let snap = cache_workspace(&cache, &dir.join("snapshot"), inputs)?;
    let env = preview_env(&artifacts)?;
    // Source hashes invalidate package artifacts even if a same-size edit kept mtime.
    if !source_same && previous.is_some() {
        for (index, command) in clean_commands(row, &artifacts).iter().enumerate() {
            invoke(
                &snap,
                command,
                &env,
                &dir.join(format!("clean-{index}.log")),
                args.timeout_seconds,
            )?;
        }
    }
    let cli = if row.crate_name == "forge" {
        Some(invoke(
            &snap,
            &cli_build(&artifacts),
            &env,
            &dir.join("cli-build.log"),
            args.timeout_seconds,
        )?)
    } else {
        None
    };
    let build = invoke(
        &snap,
        &commands(row, &artifacts, true, &args.filter),
        &env,
        &dir.join("build.log"),
        args.timeout_seconds,
    )?;
    let tests = invoke(
        &snap,
        &commands(row, &artifacts, false, &args.filter),
        &env,
        &dir.join("tests.log"),
        args.timeout_seconds,
    )?;
    for (path, expected) in inputs {
        ensure!(
            evidence::sha256_file(&snap.join(path))? == *expected,
            "test altered cached preview source {path}"
        );
    }
    evidence::write_atomic(&state, digest.as_bytes())?;
    evidence::write_atomic(&target_state, digest.as_bytes())?;
    Ok(
        json!({"target":row,"cache":cache,"source_sha256":digest,"build_reused":reused,"cli_build":cli,"cli_build_log": if row.crate_name == "forge" { Some(dir.join("cli-build.log")) } else { None },"build":build,"tests":tests}),
    )
}

fn preview_env(artifacts: &Path) -> Result<Vec<(String, String)>> {
    Ok(vec![
        ("CUDA_VISIBLE_DEVICES".into(), "".into()),
        ("CARGO_BUILD_JOBS".into(), "2".into()),
        ("RUST_TEST_THREADS".into(), "2".into()),
        ("OMP_NUM_THREADS".into(), "1".into()),
        ("OPENBLAS_NUM_THREADS".into(), "1".into()),
        ("PYO3_PYTHON".into(), "/nonexistent/forbidden-python".into()),
        (
            "FORGE_TEST_BIN".into(),
            artifacts
                .with_file_name("test-install")
                .join("bin/forge")
                .to_str()
                .context("preview artifact path must be UTF-8")?
                .into(),
        ),
        (
            "FORGE_BIN".into(),
            std::env::current_exe()?.display().to_string(),
        ),
    ])
}

pub fn preview(args: PreviewArgs) -> Result<u8> {
    ensure!(
        (1..=86400).contains(&args.timeout_seconds),
        "preview timeout must be 1..86400"
    );
    let root = root()?;
    let paths = if args.path.is_empty() {
        dirty_paths(&root)?
    } else {
        args.path.clone()
    };
    let plan = plan(&root, paths, args.filter.clone(), args.details)?;
    if !args.execute {
        println!("{}", serde_json::to_string(&plan)?);
        return Ok(0);
    }
    ensure!(
        !plan.targets.is_empty(),
        "preview selected no executable test targets"
    );
    crate::performance::executable("cargo")?;
    crate::performance::executable("rustc")?;
    let before = inputs(&root)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let dir = common_dir(&root)?
        .join("forge-previews")
        .join(format!("run-{nonce}-{}", std::process::id()));
    snapshot(&root, &dir.join("snapshot"), &before)?;
    let mut records = Vec::new();
    for (index, row) in plan.targets.iter().enumerate() {
        let step_dir = dir.join(format!("target-{index}"));
        std::fs::create_dir_all(&step_dir)?;
        // All targets share this immutable source snapshot, but separate cache/configuration keys.
        std::os::unix::fs::symlink(dir.join("snapshot"), step_dir.join("snapshot"))?;
        records.push(run_target(&root, &step_dir, row, &before, &args)?);
    }
    ensure!(before == inputs(&root)?, "source changed during preview; results belong to the snapshot, no current-source receipt issued");
    let receipt = dir.join("preview.json");
    evidence::write_atomic(
        &receipt,
        &serde_json::to_vec_pretty(
            &json!({"schema":"forge.preview.v1","advisory":true,"plan":plan,"inputs":before,"records":records}),
        )?,
    )?;
    // Intentionally never writes .git/forge-checks/latest or a landing receipt.
    println!(
        "{}",
        json!({"advisory":true,"passed":true,"native_only":true,
            "excluded_compatibility_test_count":plan.excluded_compatibility_tests.len(),"receipt":receipt})
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_dependencies_and_library_targets_are_covered_without_bindings() {
        assert_eq!(
            crate_for("native/conductor-native/src/a.rs"),
            ["conductor-native", "forge"]
        );
        assert_eq!(
            crate_for("native/slop-core/src/engine.rs"),
            ["slop-core", "forge"]
        );
        assert_eq!(
            target("conductor-native", None).features,
            ["bundled-sqlite"]
        );
        assert_eq!(
            target("conductor-native", Some("python_contracts_discovery")).features,
            ["bundled-sqlite", "contract-registry"]
        );
        assert_eq!(target("forge", None).selector, ["--lib", "--bin", "forge"]);
        assert_eq!(
            target("forge", Some("local_check")).selector,
            ["--test", "local_check"]
        );
        assert!(
            !commands(&target("forge", None), Path::new("cache"), true, &None)
                .contains(&"--all-targets".into())
        );
    }
}

#[cfg(test)]
mod split_workspace_tests {
    use super::*;

    #[test]
    fn selects_cli_and_prepares_binary_and_both_cleans() {
        assert_eq!(crate_for("native/forge-cli/build.rs"), vec!["forge"]);
        let row = target("forge", Some("native_notes"));
        let artifacts = Path::new("/cache/artifacts");
        let build = cli_build(artifacts);
        assert!(build.windows(2).any(|v| v == ["--bin", "forge"]));
        assert!(!build.iter().any(|s| s == "--no-run"));
        let clean = clean_commands(&row, artifacts);
        assert_eq!(clean.len(), 2);
        assert!(clean[0].windows(2).any(|v| v == ["--package", "forge"]));
        assert!(clean[1].windows(2).any(|v| v == ["--package", "forge-cli"]));
        let env = preview_env(artifacts).unwrap();
        assert!(env.contains(&(
            "FORGE_TEST_BIN".into(),
            "/cache/test-install/bin/forge".into()
        )));
        assert!(env.iter().any(|(key, _)| key == "FORGE_BIN"));
    }
}
