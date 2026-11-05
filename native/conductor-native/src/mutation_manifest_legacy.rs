//! Shared reviewed contract validation stages over an explicit reader.
use super::capture::CampaignReader;
use super::*;
pub(super) struct Builder<'a, R> {
    reader: &'a R,
    root: &'a Path,
    resolved: PathBuf,
    resolved_root: PathBuf,
    candidate_paths: &'a BTreeSet<String>,
    python_test_nodeids: &'a HashMap<String, Vec<String>>,
    symbol_hashes: &'a HashMap<String, HashMap<String, String>>,
    inventory_from_source: bool,
    payload: Map<String, Value>,
    manifest_bytes: Vec<u8>,
    relative_manifest: &'a str,
    expected_mutations: usize,
    source_sha256: Map<String, Value>,
    ranked_nodeids: Vec<String>,
    ranked_tests: Vec<RankedTestContract>,
    ranked_paths: Vec<String>,
    planned_ids: Vec<String>,
    planned_targets: HashMap<String, String>,
    planned_killers: HashMap<String, Vec<String>>,
    planned_mutations: Vec<PlannedMutationContract>,
    mutations: Vec<MutationContract>,
    test_argv: Vec<String>,
    timeout_seconds: u64,
    poll_seconds: u64,
    blocked_process_substrings: Vec<String>,
    environment: Map<String, Value>,
    host_read_dependencies: Vec<String>,
    test_scopes: Map<String, Value>,
    source_symbols: Map<String, Value>,
    source_drifted: bool,
}
impl<R: CampaignReader> Builder<'_, R> {
    fn sources(&mut self) -> Result<(), String> {
        let payload = &self.payload;
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

        self.source_sha256 = source_sha256;
        Ok(())
    }
    fn ranked(&mut self) -> Result<(), String> {
        let payload = &self.payload;
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

        self.ranked_paths = ranked_nodeids
            .iter()
            .map(|nodeid| nodeid.split("::").next().unwrap_or_default().to_owned())
            .collect();
        self.ranked_nodeids = ranked_nodeids;
        self.ranked_tests = ranked_tests;
        Ok(())
    }
    fn planned(&mut self) -> Result<(), String> {
        let payload = &self.payload;
        let planned_rows = payload
            .get("planned_mutations")
            .and_then(Value::as_array)
            .ok_or_else(|| "planned_mutations must be a list".to_owned())?;
        if planned_rows.len() != self.expected_mutations {
            return Err(format!(
                "expected {} planned mutation slots, got {}",
                self.expected_mutations,
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

        self.planned_ids = planned_ids;
        self.planned_targets = planned_targets;
        self.planned_killers = planned_killers;
        self.planned_mutations = planned_mutations;
        Ok(())
    }
    fn baseline(&mut self) -> Result<(), String> {
        let payload = &self.payload;
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
        let missing_tests: Vec<String> = self
            .ranked_nodeids
            .iter()
            .filter(|nodeid| {
                !test_argv.contains(*nodeid)
                    && !test_argv
                        .contains(&nodeid.split("::").next().unwrap_or_default().to_owned())
            })
            .cloned()
            .collect();
        if !missing_tests.is_empty() {
            return Err(format!(
                "baseline.argv omits ranked tests: {}",
                python_list(missing_tests)
            ));
        }

        self.test_argv = test_argv;
        self.timeout_seconds = timeout_seconds;
        Ok(())
    }
    fn resources(&mut self) -> Result<(), String> {
        let payload = &self.payload;
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

        self.poll_seconds = poll_seconds;
        self.blocked_process_substrings = blocked_process_substrings;
        self.environment = environment;
        self.host_read_dependencies = host_read_dependencies;
        Ok(())
    }
    fn symbols(&mut self) -> Result<(), String> {
        let payload = &self.payload;
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
            if !self.source_sha256.contains_key(&path) {
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

        let relevant = self.ranked_paths.iter().any(|path| {
            self.candidate_paths.contains(path) && self.source_sha256.contains_key(path)
        });
        let mut source_drifted = false;
        if relevant {
            for (relative, expected) in &self.source_sha256 {
                let expected = expected.as_str().unwrap_or_default();
                if let Some(symbols) = source_symbols.get(relative).and_then(Value::as_object) {
                    let current = self.symbol_hashes.get(relative);
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
                } else if self.reader.hash(&self.root.join(relative)).as_deref() != Some(expected) {
                    source_drifted = true;
                    break;
                }
            }
        }

        self.source_symbols = source_symbols;
        self.source_drifted = source_drifted;
        Ok(())
    }
    fn materialized(&mut self) -> Result<(), String> {
        let rows = self
            .payload
            .get("mutations")
            .and_then(Value::as_array)
            .ok_or("mutations must be a list")?;
        if rows.len() != self.expected_mutations {
            return Err(format!(
                "expected {} materialized mutations, got {}",
                self.expected_mutations,
                rows.len()
            ));
        }
        let mut mutations = Vec::new();
        let mut ids = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            let mutation = self.materialized_row(index, row)?;
            ids.push(mutation.id.clone());
            mutations.push(mutation);
        }
        if ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
            return Err("mutations contains duplicate ids".into());
        }
        self.mutations = mutations;
        Ok(())
    }
    fn materialized_row(&self, index: usize, raw: &Value) -> Result<MutationContract, String> {
        let row = raw
            .as_object()
            .ok_or_else(|| format!("mutations[{index}] must be a JSON object"))?;
        let id = required_string(row, "id", &format!("mutations[{index}].id"))?;
        let patch_relative = safe_relative(
            &required_string(row, "patch_file", &format!("mutations[{index}].patch_file"))?,
            &format!("mutations[{index}].patch_file"),
        )?;
        let patch_path = self
            .resolved
            .parent()
            .unwrap_or(self.root)
            .join(&patch_relative);
        let patch_resolved = self.reader.resolve(&patch_path)?;
        if patch_resolved.strip_prefix(&self.resolved_root).is_err() {
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
        let actual_sha256 = self.reader.hash(&patch_resolved);
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
        let actual_paths = self.reader.patch_paths(&patch_resolved)?;
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
        if self.planned_killers.get(&id) != Some(&killers) {
            return Err(format!(
                "mutation {} expected_killers drifted from its slot",
                python_repr(Some(&Value::String(id.clone())))
            ));
        }
        let target = self.planned_targets.get(&id).ok_or_else(|| {
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
        Ok(MutationContract {
            id: id.clone(),
            patch_file: patch_resolved.to_string_lossy().into_owned(),
            patch_sha256,
            allowed_paths,
            expected_killers: killers,
        })
    }
    fn finish(self) -> Result<CampaignContract, String> {
        let value_analysis = validate_value_analysis(
            self.payload.get("value_analysis"),
            &self.ranked_nodeids,
            &self.planned_ids,
        )?;
        Ok(CampaignContract {
            campaign_id: required_string(&self.payload, "campaign_id", "campaign_id")?,
            title: required_string(&self.payload, "title", "title")?,
            language: required_string(&self.payload, "language", "language")?,
            mutation_engine: required_string(&self.payload, "mutation_engine", "mutation_engine")?,
            expected_mutations: self.expected_mutations,
            manifest: self.relative_manifest.to_owned(),
            manifest_sha256: format!("{:x}", Sha256::digest(self.manifest_bytes)),
            source_sha256: Value::Object(self.source_sha256),
            source_symbols: Value::Object(self.source_symbols),
            test_scopes: self.test_scopes,
            ranked_tests: self.ranked_tests,
            ranked_test_paths: self.ranked_paths,
            planned_mutations: self.planned_mutations,
            mutations: self.mutations,
            test_argv: self.test_argv,
            timeout_seconds: self.timeout_seconds,
            blocked_process_substrings: self.blocked_process_substrings,
            poll_seconds: self.poll_seconds,
            environment: self.environment,
            host_read_dependencies: self.host_read_dependencies,
            generated_value: None,
            value_analysis_payload: self.payload.get("value_analysis").cloned(),
            value_analysis,
            source_drifted: self.source_drifted,
            generated: false,
            survivor_baseline: Vec::new(),
            test_sha256: Value::Object(Map::new()),
        })
    }
}

pub(super) fn load<'a, R: CampaignReader>(
    root: &'a Path,
    relative_manifest: &'a str,
    python_test_nodeids: &'a HashMap<String, Vec<String>>,
    symbol_hashes: &'a HashMap<String, HashMap<String, String>>,
    candidate_paths: &'a BTreeSet<String>,
    inventory_from_source: bool,
    reader: &'a R,
) -> Result<CampaignContract, String> {
    let resolved = reader.resolve(&root.join(relative_manifest))?;
    let resolved_root = reader.resolve(root)?;
    if resolved.strip_prefix(&resolved_root).is_err() {
        return Err("campaign manifest must be inside the repository".into());
    }
    let manifest_bytes = reader.read(&resolved)?;
    let value: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("cannot load campaign {}: {error}", resolved.display()))?;
    let payload = value
        .as_object()
        .ok_or("campaign must be a JSON object")?
        .clone();
    if payload.get("schema_version").and_then(Value::as_i64) != Some(1) {
        return Err(format!(
            "unsupported schema_version={}; expected 1",
            python_repr(payload.get("schema_version"))
        ));
    }
    let engine = payload
        .get("mutation_engine")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if GENERATED_ENGINES.contains(&engine) {
        return generated(
            root,
            relative_manifest,
            &payload,
            manifest_bytes,
            engine,
            reader,
        );
    }
    if payload.contains_key("generated_value") {
        return Err("generated_value requires an automatic Cargo campaign".into());
    }
    let expected_mutations = payload
        .get("expected_mutations")
        .and_then(Value::as_u64)
        .filter(|value| *value >= 1)
        .ok_or("expected_mutations must be a positive integer")?
        as usize;
    let mut builder = Builder {
        reader,
        root,
        resolved,
        resolved_root,
        candidate_paths,
        python_test_nodeids,
        symbol_hashes,
        inventory_from_source,
        payload,
        manifest_bytes,
        relative_manifest,
        expected_mutations,
        source_sha256: Map::new(),
        ranked_nodeids: Vec::new(),
        ranked_tests: Vec::new(),
        ranked_paths: Vec::new(),
        planned_ids: Vec::new(),
        planned_targets: HashMap::new(),
        planned_killers: HashMap::new(),
        planned_mutations: Vec::new(),
        mutations: Vec::new(),
        test_argv: Vec::new(),
        timeout_seconds: 0,
        poll_seconds: 0,
        blocked_process_substrings: Vec::new(),
        environment: Map::new(),
        host_read_dependencies: Vec::new(),
        test_scopes: Map::new(),
        source_symbols: Map::new(),
        source_drifted: false,
    };
    builder.sources()?;
    builder.ranked()?;
    builder.planned()?;
    builder.baseline()?;
    builder.materialized()?;
    builder.resources()?;
    builder.scopes()?;
    builder.symbols()?;
    builder.finish()
}
fn generated<R: CampaignReader>(
    root: &Path,
    relative_manifest: &str,
    payload: &Map<String, Value>,
    manifest_bytes: Vec<u8>,
    engine: &str,
    reader: &R,
) -> Result<CampaignContract, String> {
    let mut ordinary = payload.clone();
    let generated_value = ordinary.remove("generated_value");
    let mut contract = generated_campaign_contract_with(
        &ordinary,
        manifest_bytes,
        relative_manifest,
        engine,
        |path| reader.hash(&root.join(path)),
    )?;
    if generated_value.is_some() {
        let inventory = reader
            .cargo_inventory()
            .ok_or("generated_value requires independently captured Cargo inventory")?;
        let original = Value::Object(payload.clone());
        let path = crate::mutation_evidence::cargo_value::declaration_path(&original)?
            .ok_or("generated value declaration missing")?;
        let declaration = reader.read(&root.join(path))?;
        crate::mutation_evidence::cargo_value::apply(
            &mut contract,
            &original,
            &declaration,
            inventory,
        )?;
    }
    Ok(contract)
}
struct Scope {
    path: String,
    mode: String,
    inventory: String,
    nodeids: Vec<String>,
}
impl<R: CampaignReader> Builder<'_, R> {
    fn scopes(&mut self) -> Result<(), String> {
        let empty = Value::Object(Map::new());
        let scopes = object(
            self.payload.get("test_scopes").or(Some(&empty)),
            "test_scopes",
        )?;
        let mut result = Map::new();
        for (path, value) in scopes {
            let scope = self.scope_header(path, value)?;
            self.scope_discovery(&scope)?;
            if scope.mode == "complete" && !self.ranked_paths.contains(&scope.path) {
                return Err(format!(
                    "complete test_scopes[{}] must contain at least one ranked test",
                    scope.path
                ));
            }
            result.insert(
                scope.path,
                json!({"mode":scope.mode,"inventory":scope.inventory,"nodeids":scope.nodeids}),
            );
        }
        let scoped: BTreeSet<_> = result
            .values()
            .filter_map(Value::as_object)
            .filter_map(|scope| scope.get("nodeids").and_then(Value::as_array))
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let mut missing: Vec<_> = self
            .ranked_nodeids
            .iter()
            .filter(|node| !scoped.contains(node.as_str()))
            .cloned()
            .collect();
        missing.sort();
        if !missing.is_empty() {
            return Err(format!(
                "ranked_tests nodeids are missing from declared test_scopes: {}",
                python_list(missing)
            ));
        }
        self.test_scopes = result;
        Ok(())
    }
    fn scope_header(&self, raw_path: &str, raw_scope: &Value) -> Result<Scope, String> {
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
        let wrong: Vec<_> = nodeids
            .iter()
            .filter(|node| node.split("::").next() != Some(path.as_str()))
            .cloned()
            .collect();
        if !wrong.is_empty() {
            return Err(format!(
                "test_scopes[{path}] contains nodeids from another file: {}",
                python_list(wrong)
            ));
        }
        if !self.source_sha256.contains_key(&path) {
            return Err(format!("test_scopes[{path}] is not bound in source_sha256"));
        }
        if !matches!(inventory.as_str(), "python_ast" | "cargo_test" | "c_test")
            && mode == "complete"
        {
            return Err(format!(
                "complete test scope inventory is unsupported: {inventory:?}"
            ));
        }
        let extension_ok = match inventory.as_str() {
            "python_ast" => path.ends_with(".py"),
            "cargo_test" => path.ends_with(".rs"),
            "c_test" => [".c", ".cc", ".cpp", ".cxx"]
                .iter()
                .any(|ext| path.ends_with(ext)),
            _ => true,
        };
        if !extension_ok {
            let expected = match inventory.as_str() {
                "python_ast" => "a .py file",
                "cargo_test" => "a .rs file",
                _ => "a .c/.cc/.cpp/.cxx file",
            };
            return Err(format!(
                "test_scopes[{path}].inventory='{inventory}' requires {expected}"
            ));
        }
        Ok(Scope {
            path,
            mode,
            inventory,
            nodeids,
        })
    }
    fn scope_discovery(&self, scope: &Scope) -> Result<(), String> {
        if scope.mode != "complete" {
            return Ok(());
        }
        let discovered = if scope.inventory == "python_ast" {
            if let Some(nodes) = self.python_test_nodeids.get(&scope.path) {
                nodes.clone()
            } else if self.inventory_from_source {
                self.reader
                    .inventory(self.root, &scope.path, "python_ast")?
            } else {
                return Err(format!(
                    "cannot inventory Python tests in {}: native AST map is missing",
                    scope.path
                ));
            }
        } else {
            self.reader
                .inventory(self.root, &scope.path, &scope.inventory)?
        };
        if scope.nodeids != discovered {
            let declared: BTreeSet<_> = scope.nodeids.iter().cloned().collect();
            let actual: BTreeSet<_> = discovered.iter().cloned().collect();
            let missing = actual.difference(&declared).cloned();
            let extra = declared.difference(&actual).cloned();
            let kind = match scope.inventory.as_str() {
                "python_ast" => "Python",
                "cargo_test" => "Rust",
                _ => "C",
            };
            return Err(format!("complete test scope does not match current {kind} inventory for {}: missing={}, extra={}, expected_order={}",
                scope.path,python_list(missing),python_list(extra),python_list(discovered)));
        }
        Ok(())
    }
}
