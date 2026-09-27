//! Candidate-local build and import paths for selected Rust contracts.

use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const NATIVE_MANIFEST: &str = "native/conductor-native/Cargo.toml";
const FORGE_MANIFEST: &str = "native/forge/Cargo.toml";
const SLOP_MANIFEST: &str = "native/slop-core/Cargo.toml";

// Keep this list aligned with contracts that import slop_core. Candidate
// verification must build their extension from the selected source snapshot.
const SLOP_CONSUMERS: &[&str] = &[
    "python_contracts_candidate_style",
    "python_contracts_native_ablations",
    "python_contracts_reuse_inventory",
    "python_contracts_reuse_roi",
    "python_contracts_reuse_consolidation",
    "python_contracts_reuse_file_families",
];

fn required_string<'a>(request: &'a Value, key: &str) -> Result<&'a str, String> {
    request[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("contract runtime requires {key}"))
}

fn required_absolute(request: &Value, key: &str) -> Result<PathBuf, String> {
    let raw = required_string(request, key)?;
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(format!("contract runtime {key} must be absolute: {raw}"));
    }
    Ok(path)
}

fn selected_targets(request: &Value) -> Result<Vec<String>, String> {
    let values = request["targets"]
        .as_array()
        .ok_or("contract runtime targets must be a list")?;
    let mut targets = BTreeSet::new();
    for value in values {
        let target = value
            .as_str()
            .ok_or("contract runtime target must be a string")?;
        if !target.starts_with("python_contracts_")
            || !target
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(format!("invalid contract runtime target: {target}"));
        }
        targets.insert(target.to_owned());
    }
    Ok(targets.into_iter().collect())
}

fn build_step(root: &Path, manifest: &str, args: &[&str], from: &Path, to: &Path) -> Value {
    let mut argv = vec![
        "cargo".to_owned(),
        "build".to_owned(),
        "--offline".to_owned(),
        "--jobs".to_owned(),
        "2".to_owned(),
        "--locked".to_owned(),
        "--manifest-path".to_owned(),
        manifest.to_owned(),
    ];
    argv.extend(args.iter().map(|arg| (*arg).to_owned()));
    json!({
        "cwd": root,
        "argv": argv,
        "artifact": from,
        "destination": to,
    })
}

pub fn plan(request: &Value) -> Result<Value, String> {
    let root = required_absolute(request, "snapshot")?
        .canonicalize()
        .map_err(|error| format!("cannot resolve candidate snapshot: {error}"))?;
    let runtime = required_absolute(request, "runtime_dir")?;
    let python = required_absolute(request, "python_executable")?;
    crate::test_contracts::require_regular_file(&root, NATIVE_MANIFEST)
        .map_err(|error| format!("candidate native crate invalid: {error:#}"))?;
    let targets = selected_targets(request)?;
    if targets.is_empty() {
        return Err("contract runtime requires selected targets".into());
    }
    crate::test_contracts::require_regular_file(&root, FORGE_MANIFEST)
        .map_err(|error| format!("selected contracts require candidate Forge binary: {error:#}"))?;
    let target_dir = runtime.join("contract-cargo-target");
    let extension_dir = runtime.join("contract-extension");
    let extension = extension_dir.join("conductor_native.so");
    let mut build = vec![build_step(
        &root,
        NATIVE_MANIFEST,
        &["--features", "python", "--lib"],
        &target_dir.join("debug/libconductor_native.so"),
        &extension,
    )];
    if targets
        .iter()
        .any(|target| SLOP_CONSUMERS.contains(&target.as_str()))
    {
        crate::test_contracts::require_regular_file(&root, SLOP_MANIFEST).map_err(|error| {
            format!("selected contracts require candidate slop_core: {error:#}")
        })?;
        build.push(build_step(
            &root,
            SLOP_MANIFEST,
            &["--features", "extension-module", "--lib"],
            &target_dir.join("debug/libslop_core.so"),
            &extension_dir.join("slop_core.so"),
        ));
    }
    let mut env = json!({
        "CARGO_TARGET_DIR": target_dir,
        "CARGO_BUILD_JOBS": "2",
        "PYO3_PYTHON": python,
        "CUDA_VISIBLE_DEVICES": "",
        "ROCR_VISIBLE_DEVICES": "",
        "OMP_NUM_THREADS": "1",
        "OPENBLAS_NUM_THREADS": "1",
        "RAYON_NUM_THREADS": "1",
    });
    // Python wrappers and shared test helpers can launch Forge indirectly.
    let binary = runtime.join("contract-bin/forge");
    build.push(build_step(
        &root,
        FORGE_MANIFEST,
        &["--bin", "forge"],
        &target_dir.join("debug/forge"),
        &binary,
    ));
    env["FORGE_BIN"] = json!(binary);
    let mut python_paths = vec![extension_dir, root.join("src"), root.clone()];
    if let Some(sites) = request["python_sites"].as_array() {
        for site in sites {
            let raw = site.as_str().ok_or("python site must be a string")?;
            let path = PathBuf::from(raw);
            if !path.is_absolute() {
                return Err(format!("python site must be absolute: {raw}"));
            }
            python_paths.push(path);
        }
    }
    let joined = std::env::join_paths(&python_paths)
        .map_err(|error| format!("invalid contract Python import path: {error}"))?;
    env["PYTHONPATH"] = json!(joined.to_string_lossy());
    if let Some(libdir) = request["python_libdir"].as_str() {
        let existing = request["ld_library_path"].as_str().unwrap_or("");
        env["LD_LIBRARY_PATH"] = json!(if existing.is_empty() {
            libdir.to_owned()
        } else {
            format!("{libdir}:{existing}")
        });
    }
    // Probe the selected Python executable; an ambient PyO3 config can describe
    // a different interpreter and silently produce the wrong extension ABI.
    for key in ["SQLITE3_LIB_DIR", "RUSTUP_TOOLCHAIN"] {
        if let Some(value) = request[key].as_str() {
            env[key] = json!(value);
        }
    }
    Ok(json!({"build_commands": build, "test_env": env, "extension": extension}))
}
