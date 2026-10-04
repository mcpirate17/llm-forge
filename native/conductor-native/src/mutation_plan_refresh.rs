// Regenerate a recorded campaign using the same subjects and manifest builders as planning.
// The caller owns manifest file I/O; this module only computes replacement content.

#[derive(Debug, Deserialize)]
pub struct RefreshRequest {
    language: String,
    repo_root: String,
    manifest_path: String,
    campaign_id: String,
    existing: Value,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    extra_tests: BTreeMap<String, Vec<String>>,
    #[serde(default = "default_jobs")]
    jobs: i64,
    #[serde(default = "default_timeout")]
    run_timeout_seconds: i64,
}

pub fn compute_refresh(request: &RefreshRequest) -> Result<Value, String> {
    let root = Path::new(&request.repo_root);
    match request.language.as_str() {
        "python" => refresh_python(root, request),
        "rust" => refresh_rust(root, request),
        other => Err(format!("unknown refresh language {other:?}")),
    }
}

fn generator(request: &RefreshRequest) -> Option<&Map<String, Value>> {
    request.existing.get("generator").and_then(Value::as_object)
}

fn retain_ratchet(refreshed: &mut Value, existing: &Value) {
    for key in [
        "survivor_baseline",
        "survivor_baseline_recorded",
        "survivor_baseline_note",
        "survivor_baseline_recorded_at",
    ] {
        if let Some(value) = existing.get(key) {
            refreshed[key] = value.clone();
        }
    }
}

fn recorded_tests(request: &RefreshRequest) -> Result<Vec<String>, String> {
    let Some(tests) = request
        .existing
        .get("test_sha256")
        .and_then(Value::as_object)
    else {
        return Err(format!(
            "{} has no recorded test list to carry forward; regenerate it with plan/write and let the engine record a fresh baseline",
            request.manifest_path
        ));
    };
    if tests.is_empty() {
        return Err(format!(
            "{} has no recorded test list to carry forward; regenerate it with plan/write and let the engine record a fresh baseline",
            request.manifest_path
        ));
    }
    Ok(tests.keys().cloned().collect())
}

fn python_refresh_tests(
    root: &Path,
    request: &RefreshRequest,
    source: &str,
    paired_tests: &[String],
) -> Result<Vec<String>, String> {
    if request.extra_tests.keys().any(|path| path != source) {
        return Err(
            "--extra-test names a different Python source than the recorded campaign".into(),
        );
    }
    let mut tests: BTreeSet<String> = recorded_tests(request)?.into_iter().collect();
    tests.extend(paired_tests.iter().cloned());
    if let Some(extra) = request.extra_tests.get(source) {
        tests.extend(extra.iter().cloned());
    }
    let canonical_root = fs::canonicalize(root).map_err(|error| error.to_string())?;
    for test in &tests {
        let relative = Path::new(test);
        let name = relative
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
            || !test.ends_with(".py")
            || !is_test_name(test, name)
            || !fs::canonicalize(root.join(relative))
                .is_ok_and(|path| path.is_file() && path.starts_with(&canonical_root))
        {
            return Err(format!(
                "refresh test must be an existing repository-relative Python test file: {test}"
            ));
        }
    }
    Ok(tests.into_iter().collect())
}

fn refresh_python(root: &Path, request: &RefreshRequest) -> Result<Value, String> {
    if request
        .existing
        .get("mutation_engine")
        .and_then(Value::as_str)
        != Some("fest")
    {
        return Err(format!(
            "{} is not a generated fest campaign",
            request.manifest_path
        ));
    }
    let sources = generator(request)
        .and_then(|row| row.get("source"))
        .and_then(Value::as_array);
    let Some([source]) = sources.map(Vec::as_slice) else {
        return Err(format!(
            "{} must declare exactly one Python source to refresh",
            request.manifest_path
        ));
    };
    let Some(source) = source.as_str() else {
        return Err(format!(
            "{} must declare exactly one Python source to refresh",
            request.manifest_path
        ));
    };
    if request.run_timeout_seconds <= 0 {
        return Err("refresh run timeout must be positive".into());
    }
    let in_scope = |path: &str, _: &[String]| path == source;
    let (paired, unpaired) = python_subjects(root, Some(&in_scope))?;
    let mut subject = if let Some(found) = paired.into_iter().find(|row| row.source == source) {
        found
    } else if let Some(orphan) = unpaired.into_iter().find(|row| row.source == source) {
        PySubject {
            source: orphan.source,
            lines: orphan.lines,
            tests: Vec::new(),
        }
    } else {
        return Err(format!(
            "generated Python source {source:?} no longer exists in the tree"
        ));
    };
    subject.tests = python_refresh_tests(root, request, source, &subject.tests)?;
    let mut refreshed = fest_manifest(
        &subject,
        &request.campaign_id,
        root,
        request.run_timeout_seconds,
    );
    retain_ratchet(&mut refreshed, &request.existing);
    Ok(refreshed)
}

fn implicit_rust_sources(
    request: &RefreshRequest,
    subject: &RustCrate,
    root: &Path,
) -> Result<Vec<String>, String> {
    let Some(declared) = generator(request)
        .and_then(|row| row.get("source"))
        .and_then(Value::as_array)
        .filter(|rows| !rows.is_empty())
    else {
        return Err(format!(
            "{} has no non-empty generated Rust source scope",
            request.manifest_path
        ));
    };
    let mut paths = Vec::with_capacity(declared.len());
    for value in declared {
        let Some(text) = value.as_str().filter(|text| {
            !text.is_empty()
                && !text.chars().any(|ch| matches!(ch, '*' | '?' | '[' | ']'))
                && !Path::new(text).is_absolute()
                && !Path::new(text)
                    .components()
                    .any(|part| part == std::path::Component::ParentDir)
        }) else {
            return Err(format!(
                "{} has malformed generated Rust source scope; pass --source explicitly",
                request.manifest_path
            ));
        };
        paths.push(text);
    }
    let package_root = root.join(&subject.root);
    let resolved_root = fs::canonicalize(&package_root).map_err(|error| error.to_string())?;
    let resolved_repo = fs::canonicalize(root).map_err(|error| error.to_string())?;
    let allowed: BTreeSet<String> = subject
        .files
        .iter()
        .filter_map(|path| {
            Path::new(path)
                .strip_prefix(Path::new(&subject.root))
                .ok()
                .map(|part| part.to_string_lossy().replace('\\', "/"))
        })
        .collect();
    let mut selected = Vec::with_capacity(paths.len());
    for path in paths {
        let candidate = fs::canonicalize(package_root.join(path)).map_err(|_| {
            format!(
                "{} has Rust sources outside its current crate; pass --source explicitly",
                request.manifest_path
            )
        })?;
        let relative = candidate.strip_prefix(&resolved_root).map_err(|_| {
            format!(
                "{} has Rust sources outside its current crate; pass --source explicitly",
                request.manifest_path
            )
        })?;
        if !allowed.contains(&relative.to_string_lossy().replace('\\', "/")) {
            return Err(format!(
                "{} has Rust sources outside its current crate; pass --source explicitly",
                request.manifest_path
            ));
        }
        let repo_relative = candidate.strip_prefix(&resolved_repo).map_err(|_| {
            format!(
                "{} has Rust sources outside its current crate; pass --source explicitly",
                request.manifest_path
            )
        })?;
        selected.push(repo_relative.to_string_lossy().replace('\\', "/"));
    }
    Ok(selected)
}

fn refresh_rust(root: &Path, request: &RefreshRequest) -> Result<Value, String> {
    if request
        .existing
        .get("mutation_engine")
        .and_then(Value::as_str)
        != Some("cargo-mutants")
    {
        return Err(format!(
            "{} is not a cargo-mutants generated campaign",
            request.manifest_path
        ));
    }
    let options = generator(request)
        .and_then(|row| row.get("options"))
        .and_then(Value::as_object);
    let package = options
        .and_then(|row| row.get("package"))
        .and_then(Value::as_str);
    let manifest = options
        .and_then(|row| row.get("manifest_path"))
        .and_then(Value::as_str);
    let (Some(package), Some(manifest)) = (package, manifest) else {
        return Err(format!(
            "{} has no generated cargo package",
            request.manifest_path
        ));
    };
    let candidates: Vec<RustCrate> = rust_subjects(root)?
        .into_iter()
        .filter(|row| row.package == package)
        .collect();
    let subject = candidates
        .iter()
        .find(|row| row.manifest == manifest)
        .or_else(|| (candidates.len() == 1).then(|| &candidates[0]))
        .ok_or_else(|| format!("generated cargo package {package:?} no longer exists"))?;
    let sources = if request.sources.is_empty() {
        implicit_rust_sources(request, subject, root)?
    } else {
        request.sources.clone()
    };
    let mut refreshed = cargo_manifest(
        subject,
        &request.campaign_id,
        root,
        request.jobs,
        request.run_timeout_seconds,
        Some(&sources),
    )?;
    refreshed["test_sha256"] = refreshed["source_sha256"].clone();
    retain_ratchet(&mut refreshed, &request.existing);
    Ok(refreshed)
}
