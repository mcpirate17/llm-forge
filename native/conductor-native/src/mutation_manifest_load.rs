pub(crate) fn load_campaign_contract(
    root: &Path,
    relative_manifest: &str,
    python_test_nodeids: &HashMap<String, Vec<String>>,
    symbol_hashes: &HashMap<String, HashMap<String, String>>,
    candidate_paths: &BTreeSet<String>,
    inventory_from_source: bool,
) -> Result<CampaignContract, String> {
    let manifest_path = root.join(relative_manifest);
    let resolved = fs::canonicalize(&manifest_path).unwrap_or_else(|_| manifest_path.clone());
    let resolved_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if resolved.strip_prefix(&resolved_root).is_err() {
        return Err("campaign manifest must be inside the repository".to_owned());
    }
    let (manifest_bytes, payload) = read_json_object(&resolved, "campaign")?;
    if payload.get("schema_version").and_then(Value::as_i64) != Some(1) {
        return Err(format!(
            "unsupported schema_version={}; expected 1",
            python_repr(payload.get("schema_version"))
        ));
    }
    let declared_engine = payload
        .get("mutation_engine")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if GENERATED_ENGINES.contains(&declared_engine.as_str()) {
        return generated_campaign_contract(
            root,
            &payload,
            manifest_bytes,
            relative_manifest,
            &declared_engine,
        );
    }
    let expected_mutations = payload
        .get("expected_mutations")
        .and_then(Value::as_u64)
        .filter(|value| *value >= 1)
        .ok_or_else(|| "expected_mutations must be a positive integer".to_owned())?
        as usize;
    let source_raw = object(payload.get("source_sha256"), "source_sha256")?;
    let mut source_sha256 = Map::new();
    for (raw_path, raw_digest) in source_raw {
        let path = safe_relative(raw_path, "source_sha256 path")?;
        let digest = raw_digest
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("source_sha256[{path}] must be a non-empty string"))?;
        if !valid_sha256(digest) {
            return Err(format!(
                "source_sha256[{path}] must be a lowercase SHA-256 digest"
            ));
        }
        source_sha256.insert(path, Value::String(digest.to_owned()));
    }

    let ranked_rows = payload
        .get("ranked_tests")
        .and_then(Value::as_array)
        .filter(|rows| !rows.is_empty())
        .ok_or_else(|| "ranked_tests must be a non-empty list".to_owned())?;
    let mut ranked_nodeids = Vec::with_capacity(ranked_rows.len());
    let mut ranks = Vec::with_capacity(ranked_rows.len());
    let mut ranked_tests = Vec::with_capacity(ranked_rows.len());
    for (index, raw) in ranked_rows.iter().enumerate() {
        let row = raw
            .as_object()
            .ok_or_else(|| format!("ranked_tests[{index}] must be a JSON object"))?;
        let rank = row
            .get("rank")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("ranked_tests[{index}].rank must be an integer"))?;
        let nodeid = required_string(row, "nodeid", "ranked test nodeid")?;
        let contract = required_string(row, "contract", "ranked test contract")?;
        let rationale = required_string(row, "rationale", "ranked test rationale")?;
        ranks.push(rank as usize);
        ranked_nodeids.push(nodeid.clone());
        ranked_tests.push(RankedTestContract {
            rank: rank as i64,
            nodeid,
            contract,
            rationale,
        });
    }
    let expected_ranks: Vec<usize> = (1..=ranked_rows.len()).collect();
    if ranks != expected_ranks {
        return Err(format!(
            "ranked_tests must be ordered with contiguous ranks, got {ranks:?}"
        ));
    }
    if ranked_nodeids.iter().collect::<BTreeSet<_>>().len() != ranked_nodeids.len() {
        return Err("ranked_tests contains duplicate nodeids".to_owned());
    }
    let expected_ranked = payload
        .get("expected_ranked_tests")
        .and_then(Value::as_u64)
        .map_or(ranked_nodeids.len(), |value| value as usize);
    if expected_ranked != ranked_nodeids.len() {
        return Err(format!(
            "expected_ranked_tests={expected_ranked}, but {} tests are ranked",
            ranked_nodeids.len()
        ));
    }

    let planned_rows = payload
        .get("planned_mutations")
        .and_then(Value::as_array)
        .ok_or_else(|| "planned_mutations must be a list".to_owned())?;
    if planned_rows.len() != expected_mutations {
        return Err(format!(
            "expected {expected_mutations} planned mutation slots, got {}",
            planned_rows.len()
        ));
    }
    let mut planned_ids = Vec::with_capacity(planned_rows.len());
    let mut planned_targets = HashMap::new();
    let mut planned_killers = HashMap::new();
    let mut planned_mutations = Vec::with_capacity(planned_rows.len());
    for (index, raw) in planned_rows.iter().enumerate() {
        let row = raw
            .as_object()
            .ok_or_else(|| format!("planned_mutations[{index}] must be a JSON object"))?;
        let id = required_string(row, "id", &format!("planned_mutations[{index}].id"))?;
        let target = safe_relative(
            &required_string(
                row,
                "target_path",
                &format!("planned_mutations[{index}].target_path"),
            )?,
            &format!("planned_mutations[{index}].target_path"),
        )?;
        let contract = required_string(
            row,
            "contract",
            &format!("planned_mutations[{index}].contract"),
        )?;
        let description = required_string(
            row,
            "description",
            &format!("planned_mutations[{index}].description"),
        )?;
        let killers = string_list(
            row.get("expected_killers"),
            &format!("planned_mutations[{index}].expected_killers"),
        )?;
        planned_targets.insert(id.clone(), target.clone());
        planned_killers.insert(id.clone(), killers.clone());
        planned_mutations.push(PlannedMutationContract {
            id: id.clone(),
            target_path: target,
            contract,
            description,
            expected_killers: killers,
        });
        planned_ids.push(id);
    }
    if planned_ids.iter().collect::<BTreeSet<_>>().len() != planned_ids.len() {
        return Err("planned_mutations contains duplicate ids".to_owned());
    }

    // The baseline is the first executable contract.  Preserve the Python loader's
    // observable precedence when a malformed manifest also has incomplete
    // materialized mutations: an omitted ranked test is more actionable than a
    // later table-count mismatch.
    let baseline = object(payload.get("baseline"), "baseline")?;
    let test_argv = string_list(baseline.get("argv"), "baseline.argv")?;
    let timeout_seconds = baseline
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .filter(|value| *value >= 1)
        .ok_or_else(|| "baseline.timeout_seconds must be a positive integer".to_owned())?;
    let missing_tests: Vec<String> = ranked_nodeids
        .iter()
        .filter(|nodeid| {
            !test_argv.contains(*nodeid)
                && !test_argv.contains(&nodeid.split("::").next().unwrap_or_default().to_owned())
        })
        .cloned()
        .collect();
    if !missing_tests.is_empty() {
        return Err(format!(
            "baseline.argv omits ranked tests: {}",
            python_list(missing_tests)
        ));
    }

    let mutation_rows = payload
        .get("mutations")
        .and_then(Value::as_array)
        .ok_or_else(|| "mutations must be a list".to_owned())?;
    if mutation_rows.len() != expected_mutations {
        return Err(format!(
            "expected {expected_mutations} materialized mutations, got {}",
            mutation_rows.len()
        ));
    }
    let mut mutations = Vec::with_capacity(mutation_rows.len());
    let mut mutation_ids = Vec::with_capacity(mutation_rows.len());
    for (index, raw) in mutation_rows.iter().enumerate() {
        let row = raw
            .as_object()
            .ok_or_else(|| format!("mutations[{index}] must be a JSON object"))?;
        let id = required_string(row, "id", &format!("mutations[{index}].id"))?;
        let patch_relative = safe_relative(
            &required_string(row, "patch_file", &format!("mutations[{index}].patch_file"))?,
            &format!("mutations[{index}].patch_file"),
        )?;
        let patch_path = resolved.parent().unwrap_or(root).join(&patch_relative);
        let patch_resolved = fs::canonicalize(&patch_path).unwrap_or(patch_path);
        if patch_resolved.strip_prefix(&resolved_root).is_err() {
            return Err(format!(
                "mutations[{index}].patch_file escapes the repository"
            ));
        }
        let allowed = string_list(
            row.get("allowed_paths"),
            &format!("mutations[{index}].allowed_paths"),
        )?;
        if allowed.is_empty() {
            return Err(format!("mutations[{index}].allowed_paths may not be empty"));
        }
        let mut allowed_paths: Vec<String> = allowed
            .iter()
            .map(|path| safe_relative(path, &format!("mutations[{index}].allowed_paths")))
            .collect::<Result<_, _>>()?;
        let patch_sha256 = required_string(
            row,
            "patch_sha256",
            &format!("mutations[{index}].patch_sha256"),
        )?;
        if !valid_sha256(&patch_sha256) {
            return Err(format!(
                "mutations[{index}].patch_sha256 must be a lowercase SHA-256 digest"
            ));
        }
        let actual_sha256 = sha256_file(&patch_resolved);
        if actual_sha256.as_deref() != Some(patch_sha256.as_str()) {
            let actual_value = actual_sha256.clone().map(Value::String);
            return Err(format!(
                "mutation {} patch hash drifted: expected {patch_sha256}, got {}",
                python_repr(Some(&Value::String(id.clone()))),
                python_repr(actual_value.as_ref())
            ));
        }
        allowed_paths.sort();
        allowed_paths.dedup();
        let actual_paths = patch_paths(&patch_resolved)?;
        if actual_paths != allowed_paths {
            return Err(format!(
                "mutation {} patch paths {} do not match allowed_paths {}",
                python_repr(Some(&Value::String(id.clone()))),
                python_list(actual_paths),
                python_list(allowed_paths)
            ));
        }
        let killers = string_list(
            row.get("expected_killers"),
            &format!("mutations[{index}].expected_killers"),
        )?;
        if planned_killers.get(&id) != Some(&killers) {
            return Err(format!(
                "mutation {} expected_killers drifted from its slot",
                python_repr(Some(&Value::String(id.clone())))
            ));
        }
        let target = planned_targets.get(&id).ok_or_else(|| {
            format!(
                "materialized mutations lack planned slots: [{}]",
                python_repr(Some(&Value::String(id.clone())))
            )
        })?;
        if !allowed_paths.contains(target) {
            return Err(format!(
                "mutation {} does not patch its planned target",
                python_repr(Some(&Value::String(id.clone())))
            ));
        }
        mutations.push(MutationContract {
            id: id.clone(),
            patch_file: patch_resolved.to_string_lossy().into_owned(),
            patch_sha256,
            allowed_paths,
            expected_killers: killers,
        });
        mutation_ids.push(id);
    }
    if mutation_ids.iter().collect::<BTreeSet<_>>().len() != mutation_ids.len() {
        return Err("mutations contains duplicate ids".to_owned());
    }

    let empty_object = Value::Object(Map::new());
    let resource_gate = object(
        payload.get("resource_gate").or(Some(&empty_object)),
        "resource_gate",
    )?;
    let poll_seconds = resource_gate
        .get("poll_seconds")
        .map_or(Some(30), Value::as_u64)
        .filter(|value| (1..=300).contains(value))
        .ok_or_else(|| "resource_gate.poll_seconds must be in [1, 300]".to_owned())?;
    let blocked_process_substrings = string_list(
        resource_gate
            .get("blocked_process_substrings")
            .or(Some(&Value::Array(Vec::new()))),
        "resource_gate.blocked_process_substrings",
    )?;
    let environment_raw = object(
        payload.get("environment").or(Some(&empty_object)),
        "environment",
    )?;
    let mut environment = Map::new();
    for (key, value) in environment_raw {
        if key.trim().is_empty() {
            return Err("environment key must be a non-empty string".to_owned());
        }
        if !value.is_string() {
            return Err(format!("environment[{key:?}] must be a string"));
        }
        environment.insert(key.clone(), value.clone());
    }
    let host_read_dependencies = string_list(
        payload
            .get("host_read_dependencies")
            .or(Some(&Value::Array(Vec::new()))),
        "host_read_dependencies",
    )?
    .into_iter()
    .map(|path| safe_relative(&path, "host_read_dependencies"))
    .collect::<Result<Vec<_>, _>>()?;

    let ranked_paths: Vec<String> = ranked_nodeids
        .iter()
        .map(|nodeid| nodeid.split("::").next().unwrap_or_default().to_owned())
        .collect();
    let ranked_path_set: BTreeSet<&str> = ranked_paths.iter().map(String::as_str).collect();
    let empty_test_scopes = Value::Object(Map::new());
    let scopes = object(
        payload.get("test_scopes").or(Some(&empty_test_scopes)),
        "test_scopes",
    )?;
    let mut test_scopes = Map::new();
    for (raw_path, raw_scope) in scopes {
        let path = safe_relative(raw_path, "test_scopes path")?;
        let row = raw_scope
            .as_object()
            .ok_or_else(|| format!("test_scopes[{path}] must be a JSON object"))?;
        let mode = required_string(row, "mode", &format!("test_scopes[{path}].mode"))?;
        if mode != "complete" && mode != "partial" {
            return Err(format!(
                "test_scopes[{path}].mode must be 'complete' or 'partial'"
            ));
        }
        let inventory =
            required_string(row, "inventory", &format!("test_scopes[{path}].inventory"))?;
        let nodeids = string_list(row.get("nodeids"), &format!("test_scopes[{path}].nodeids"))?;
        if nodeids.is_empty() {
            return Err(format!("test_scopes[{path}].nodeids may not be empty"));
        }
        if nodeids.iter().collect::<BTreeSet<_>>().len() != nodeids.len() {
            return Err(format!("test_scopes[{path}].nodeids contains duplicates"));
        }
        let wrong: Vec<String> = nodeids
            .iter()
            .filter(|nodeid| nodeid.split("::").next() != Some(path.as_str()))
            .cloned()
            .collect();
        if !wrong.is_empty() {
            return Err(format!(
                "test_scopes[{path}] contains nodeids from another file: {}",
                python_list(wrong)
            ));
        }
        if !source_sha256.contains_key(&path) {
            return Err(format!("test_scopes[{path}] is not bound in source_sha256"));
        }
        if inventory != "python_ast"
            && inventory != "cargo_test"
            && inventory != "c_test"
            && mode == "complete"
        {
            return Err(format!(
                "complete test scope inventory is unsupported: {inventory:?}"
            ));
        }
        if inventory == "python_ast" && !path.ends_with(".py") {
            return Err(format!(
                "test_scopes[{path}].inventory='python_ast' requires a .py file"
            ));
        }
        if inventory == "cargo_test" && !path.ends_with(".rs") {
            return Err(format!(
                "test_scopes[{path}].inventory='cargo_test' requires a .rs file"
            ));
        }
        if inventory == "c_test"
            && !(path.ends_with(".c")
                || path.ends_with(".cc")
                || path.ends_with(".cpp")
                || path.ends_with(".cxx"))
        {
            return Err(format!(
                "test_scopes[{path}].inventory='c_test' requires a .c/.cc/.cpp/.cxx file"
            ));
        }
        if inventory == "c_test" && mode == "complete" {
            let discovered = inventory_c_test_nodeids(root, &path)?;
            if nodeids != discovered {
                let declared: BTreeSet<&str> = nodeids.iter().map(String::as_str).collect();
                let actual: BTreeSet<&str> = discovered.iter().map(String::as_str).collect();
                let missing = actual
                    .difference(&declared)
                    .map(|value| (*value).to_owned());
                let extra = declared
                    .difference(&actual)
                    .map(|value| (*value).to_owned());
                return Err(format!(
                    "complete test scope does not match current C inventory for {path}: missing={}, extra={}, expected_order={}",
                    python_list(missing),
                    python_list(extra),
                    python_list(discovered.clone())
                ));
            }
        }
        if inventory == "cargo_test" && mode == "complete" {
            let discovered = inventory_rust_test_nodeids(root, &path)?;
            if nodeids != discovered {
                let declared: BTreeSet<&str> = nodeids.iter().map(String::as_str).collect();
                let actual: BTreeSet<&str> = discovered.iter().map(String::as_str).collect();
                let missing = actual
                    .difference(&declared)
                    .map(|value| (*value).to_owned());
                let extra = declared
                    .difference(&actual)
                    .map(|value| (*value).to_owned());
                return Err(format!(
                    "complete test scope does not match current Rust inventory for {path}: missing={}, extra={}, expected_order={}",
                    python_list(missing),
                    python_list(extra),
                    python_list(discovered.clone())
                ));
            }
        }
        if inventory == "python_ast" && mode == "complete" {
            let source_inventory;
            let discovered = if let Some(discovered) = python_test_nodeids.get(&path) {
                discovered
            } else if inventory_from_source {
                source_inventory = inventory_python_test_nodeids(root, &path)?;
                &source_inventory
            } else {
                return Err(format!(
                    "cannot inventory Python tests in {path}: native AST map is missing"
                ));
            };
            if &nodeids != discovered {
                let declared: BTreeSet<&str> = nodeids.iter().map(String::as_str).collect();
                let actual: BTreeSet<&str> = discovered.iter().map(String::as_str).collect();
                let missing = actual
                    .difference(&declared)
                    .map(|value| (*value).to_owned());
                let extra = declared
                    .difference(&actual)
                    .map(|value| (*value).to_owned());
                return Err(format!(
                    "complete test scope does not match current Python inventory for {path}: missing={}, extra={}, expected_order={}",
                    python_list(missing),
                    python_list(extra),
                    python_list(discovered.clone())
                ));
            }
        }
        if mode == "complete" && !ranked_path_set.contains(path.as_str()) {
            return Err(format!(
                "complete test_scopes[{path}] must contain at least one ranked test"
            ));
        }
        test_scopes.insert(
            path,
            json!({
                "mode": mode,
                "inventory": inventory,
                "nodeids": nodeids,
            }),
        );
    }
    let scoped_nodeids: BTreeSet<&str> = test_scopes
        .values()
        .filter_map(Value::as_object)
        .filter_map(|scope| scope.get("nodeids").and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut missing_ranked: Vec<String> = ranked_nodeids
        .iter()
        .filter(|nodeid| !scoped_nodeids.contains(nodeid.as_str()))
        .cloned()
        .collect();
    missing_ranked.sort();
    if !missing_ranked.is_empty() {
        return Err(format!(
            "ranked_tests nodeids are missing from declared test_scopes: {}",
            python_list(missing_ranked)
        ));
    }

    let empty_source_symbols = Value::Object(Map::new());
    let source_symbols_raw = object(
        payload
            .get("source_symbols")
            .or(Some(&empty_source_symbols)),
        "source_symbols",
    )?;
    let mut source_symbols = Map::new();
    for (raw_path, raw_symbols) in source_symbols_raw {
        let path = safe_relative(raw_path, "source_symbols key")?;
        if !source_sha256.contains_key(&path) {
            return Err(format!(
                "source_symbols[{path:?}] is not bound in source_sha256; a symbolically pinned file must still declare the file it belongs to"
            ));
        }
        let symbols = raw_symbols
            .as_object()
            .filter(|symbols| !symbols.is_empty())
            .ok_or_else(|| {
                format!(
                    "source_symbols[{path:?}] is empty; omit the path instead of pinning nothing, which would silently disable drift detection for it"
                )
            })?;
        for (symbol, digest) in symbols {
            let digest = digest.as_str().unwrap_or_default();
            if symbol.trim().is_empty() || !valid_sha256(digest) {
                return Err(format!(
                    "source_symbols[{path:?}][{symbol:?}] must be a lowercase SHA-256 digest"
                ));
            }
        }
        source_symbols.insert(path, raw_symbols.clone());
    }

    let relevant = ranked_paths
        .iter()
        .any(|path| candidate_paths.contains(path) && source_sha256.contains_key(path));
    let mut source_drifted = false;
    if relevant {
        for (relative, expected) in &source_sha256 {
            let expected = expected.as_str().unwrap_or_default();
            if let Some(symbols) = source_symbols.get(relative).and_then(Value::as_object) {
                let current = symbol_hashes.get(relative);
                if current.is_none()
                    || symbols.iter().any(|(symbol, digest)| {
                        current
                            .and_then(|values| values.get(symbol))
                            .map(String::as_str)
                            != digest.as_str()
                    })
                {
                    source_drifted = true;
                    break;
                }
            } else if sha256_file(&root.join(relative)).as_deref() != Some(expected) {
                source_drifted = true;
                break;
            }
        }
    }

    let value_analysis =
        validate_value_analysis(payload.get("value_analysis"), &ranked_nodeids, &planned_ids)?;
    Ok(CampaignContract {
        campaign_id: required_string(&payload, "campaign_id", "campaign_id")?,
        title: required_string(&payload, "title", "title")?,
        language: required_string(&payload, "language", "language")?,
        mutation_engine: required_string(&payload, "mutation_engine", "mutation_engine")?,
        expected_mutations,
        manifest: relative_manifest.to_owned(),
        manifest_sha256: format!("{:x}", Sha256::digest(manifest_bytes)),
        source_sha256: Value::Object(source_sha256),
        source_symbols: Value::Object(source_symbols),
        test_scopes,
        ranked_tests,
        ranked_test_paths: ranked_paths,
        planned_mutations,
        mutations,
        test_argv,
        timeout_seconds,
        blocked_process_substrings,
        poll_seconds,
        environment,
        host_read_dependencies,
        value_analysis_payload: payload.get("value_analysis").cloned(),
        value_analysis,
        source_drifted,
        generated: false,
        survivor_baseline: Vec::new(),
        test_sha256: Value::Object(Map::new()),
    })
}
