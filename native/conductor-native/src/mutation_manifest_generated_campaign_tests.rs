mod generated_campaign_tests {
    use std::fs;
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde_json::{json, Map, Value};

    use super::{
        generated_campaign_contract, inventory_python_test_nodeids, lexical_absolute,
        load_campaign_contract, patch_paths, python_repr, python_str_or_empty, safe_relative,
        valid_sha256, validate_value_analysis, CampaignContract,
    };

    /// A manifest with two declared tests and a command that names both.
    fn payload(overrides: Value) -> Map<String, Value> {
        let mut base = json!({
            "schema_version": 1,
            "campaign_id": "subject_fest_20260906",
            "title": "Subject under fest",
            "language": "python",
            "mutation_engine": "fest",
            "generator": {
                "engine": "fest",
                "source": ["conductor/subject.py"],
                "run_timeout_seconds": 900,
                "jobs": 1
            },
            "source_sha256": {"conductor/subject.py": "a".repeat(64)},
            "test_sha256": {
                "conductor/test_subject.py": "b".repeat(64),
                "conductor/test_subject_extra.py": "c".repeat(64)
            },
            "test_argv": [
                "python", "-m", "pytest", "-q",
                "conductor/test_subject.py",
                "conductor/test_subject_extra.py"
            ],
            "survivor_baseline": ["constant_replace-abc123456789-0"]
        });
        let object = base.as_object_mut().expect("object");
        for (key, value) in overrides.as_object().expect("overrides object") {
            if value.is_null() {
                object.remove(key);
            } else {
                object.insert(key.clone(), value.clone());
            }
        }
        object.clone()
    }

    fn contract(overrides: Value) -> Result<CampaignContract, String> {
        generated_campaign_contract(
            Path::new("/nonexistent"),
            &payload(overrides),
            b"manifest bytes".to_vec(),
            "conductor/mutation_campaigns/subject_fest_20260906.json",
            "fest",
        )
    }

    fn live_fixture() -> (std::path::PathBuf, Map<String, Value>) {
        let root = std::env::temp_dir().join(format!(
            "conductor-native-generated-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(root.join("conductor")).expect("fixture directory");
        fs::write(root.join("conductor/subject.py"), "subject\n").expect("source");
        fs::write(root.join("conductor/test_subject.py"), "test\n").expect("test");
        let mut value = payload(json!({}));
        let source = value
            .get_mut("source_sha256")
            .and_then(Value::as_object_mut)
            .expect("source map");
        source.insert(
            "conductor/subject.py".to_owned(),
            Value::String(super::sha256_file(&root.join("conductor/subject.py")).expect("hash")),
        );
        let tests = value
            .get_mut("test_sha256")
            .and_then(Value::as_object_mut)
            .expect("test map");
        tests.remove("conductor/test_subject_extra.py");
        tests.insert(
            "conductor/test_subject.py".to_owned(),
            Value::String(
                super::sha256_file(&root.join("conductor/test_subject.py")).expect("hash"),
            ),
        );
        (root, value)
    }

    /// A parser-only fixture for the retired, hand-authored campaign schema.  It is never
    /// registered or executed: the hashes bind only files created under its temporary root.
    fn legacy_fixture(overrides: Value) -> (std::path::PathBuf, String) {
        let root = std::env::temp_dir().join(format!(
            "conductor-native-legacy-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let campaign_dir = root.join("conductor/mutation_campaigns");
        fs::create_dir_all(campaign_dir.join("patches")).expect("fixture directory");
        fs::write(
            root.join("conductor/subject.py"),
            "def subject():\n    return 1\n",
        )
        .expect("source");
        fs::write(
            root.join("conductor/test_subject.py"),
            "def test_subject():\n    assert True\n",
        )
        .expect("test");
        let patch = campaign_dir.join("patches/subject.patch");
        fs::write(
            &patch,
            "diff --git a/conductor/subject.py b/conductor/subject.py\n--- a/conductor/subject.py\n+++ b/conductor/subject.py\n@@ -1,2 +1,2 @@\n def subject():\n-    return 1\n+    return 2\n",
        )
        .expect("patch");
        let mut manifest = json!({
            "schema_version": 1,
            "campaign_id": "legacy_fixture",
            "title": "Parser-only legacy fixture",
            "language": "python",
            "mutation_engine": "reviewed_unified_diff",
            "expected_ranked_tests": 1,
            "expected_mutations": 1,
            "source_sha256": {
                "conductor/subject.py": "",
                "conductor/test_subject.py": ""
            },
            "ranked_tests": [{
                "rank": 1,
                "nodeid": "conductor/test_subject.py::test_subject",
                "contract": "subject remains callable",
                "rationale": "minimal parser fixture"
            }],
            "planned_mutations": [{
                "id": "subject_return",
                "target_path": "conductor/subject.py",
                "contract": "subject returns its value",
                "description": "replace the return value",
                "expected_killers": ["conductor/test_subject.py::test_subject"]
            }],
            "mutations": [{
                "id": "subject_return",
                "patch_file": "patches/subject.patch",
                "patch_sha256": "",
                "allowed_paths": ["conductor/subject.py"],
                "expected_killers": ["conductor/test_subject.py::test_subject"]
            }],
            "baseline": {
                "timeout_seconds": 10,
                "argv": ["python", "-m", "pytest", "conductor/test_subject.py::test_subject"]
            },
            "test_scopes": {
                "conductor/test_subject.py": {
                    "mode": "partial",
                    "inventory": "python_ast",
                    "nodeids": ["conductor/test_subject.py::test_subject"]
                }
            }
        });
        let source_hashes = manifest
            .get_mut("source_sha256")
            .and_then(Value::as_object_mut)
            .expect("source hashes");
        for path in ["conductor/subject.py", "conductor/test_subject.py"] {
            source_hashes.insert(
                path.to_owned(),
                Value::String(super::sha256_file(&root.join(path)).expect("hash")),
            );
        }
        manifest["mutations"][0]["patch_sha256"] =
            Value::String(super::sha256_file(&patch).expect("patch hash"));
        for (key, value) in overrides.as_object().expect("overrides object") {
            manifest[key] = value.clone();
        }
        let relative = "conductor/mutation_campaigns/legacy_fixture.json".to_owned();
        fs::write(
            root.join(&relative),
            serde_json::to_vec_pretty(&manifest).expect("manifest"),
        )
        .expect("manifest write");
        (root, relative)
    }

    /// Extend the parser-only fixture with real C and Rust source files.  The
    /// files are deliberately temporary: this exercises the loader's live
    /// inventory readers without registering an executable campaign.
    fn native_scope_fixture() -> (std::path::PathBuf, String) {
        let (root, relative) = legacy_fixture(json!({}));
        let c_path = root.join("conductor/native_fixture.c");
        let rust_path = root.join("conductor/native_fixture.rs");
        fs::write(
            &c_path,
            "static void test_c_alpha(void) { }\nstatic void test_c_beta(void);\n",
        )
        .expect("C source");
        fs::write(
            &rust_path,
            "#[test]\nfn rust_alpha() {}\n\n#[test]\npub fn rust_beta() {}\n",
        )
        .expect("Rust source");

        let manifest_path = root.join(&relative);
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).expect("fixture manifest"))
                .expect("fixture JSON");
        manifest["expected_ranked_tests"] = json!(4);
        let source_hashes = manifest["source_sha256"]
            .as_object_mut()
            .expect("source hashes");
        for path in ["conductor/native_fixture.c", "conductor/native_fixture.rs"] {
            source_hashes.insert(
                path.to_owned(),
                Value::String(super::sha256_file(&root.join(path)).expect("source hash")),
            );
        }
        let ranked = manifest["ranked_tests"]
            .as_array_mut()
            .expect("ranked tests");
        ranked.extend([
            json!({
                "rank": 2,
                "nodeid": "conductor/native_fixture.c::test_c_alpha",
                "contract": "the C test is registered by its exact signature",
                "rationale": "bind the complete C inventory to a ranked test"
            }),
            json!({
                "rank": 3,
                "nodeid": "conductor/native_fixture.rs::rust_alpha",
                "contract": "the first Rust test remains discoverable",
                "rationale": "bind Rust inventory and source order"
            }),
            json!({
                "rank": 4,
                "nodeid": "conductor/native_fixture.rs::rust_beta",
                "contract": "the second Rust test remains discoverable",
                "rationale": "bind every complete Rust inventory entry"
            }),
        ]);
        manifest["baseline"]["argv"]
            .as_array_mut()
            .expect("baseline argv")
            .extend([
                json!("conductor/native_fixture.c::test_c_alpha"),
                json!("conductor/native_fixture.rs::rust_alpha"),
                json!("conductor/native_fixture.rs::rust_beta"),
            ]);
        manifest["test_scopes"] = json!({
            "conductor/test_subject.py": {
                "mode": "partial",
                "inventory": "python_ast",
                "nodeids": ["conductor/test_subject.py::test_subject"]
            },
            "conductor/native_fixture.c": {
                "mode": "complete",
                "inventory": "c_test",
                "nodeids": ["conductor/native_fixture.c::test_c_alpha"]
            },
            "conductor/native_fixture.rs": {
                "mode": "complete",
                "inventory": "cargo_test",
                "nodeids": [
                    "conductor/native_fixture.rs::rust_alpha",
                    "conductor/native_fixture.rs::rust_beta"
                ]
            }
        });
        fs::write(
            manifest_path,
            serde_json::to_vec_pretty(&manifest).expect("fixture manifest JSON"),
        )
        .expect("fixture manifest write");
        (root, relative)
    }

    fn load_fixture_with_timeout(
        root: &std::path::Path,
        relative: &str,
        inventory_from_source: bool,
    ) -> Result<CampaignContract, String> {
        let (done, receive) = mpsc::channel();
        let root = root.to_owned();
        let relative = relative.to_owned();
        std::thread::spawn(move || {
            done.send(load_campaign_contract(
                &root,
                &relative,
                &std::collections::HashMap::new(),
                &std::collections::HashMap::new(),
                &std::collections::BTreeSet::new(),
                inventory_from_source,
            ))
            .expect("send loader result");
        });
        receive
            .recv_timeout(Duration::from_secs(5))
            .expect("manifest loader must terminate")
    }

    fn legacy_contract_error(overrides: Value) -> String {
        let (positive_root, positive_relative) = legacy_fixture(json!({}));
        load_campaign_contract(
            &positive_root,
            &positive_relative,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            &std::collections::BTreeSet::new(),
            false,
        )
        .expect("the unmodified legacy fixture must parse before testing an override");
        fs::remove_dir_all(positive_root).expect("positive fixture cleanup");

        let (root, relative) = legacy_fixture(overrides);
        let error = load_campaign_contract(
            &root,
            &relative,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            &std::collections::BTreeSet::new(),
            false,
        )
        .expect_err("malformed legacy table must fail");
        fs::remove_dir_all(root).expect("fixture cleanup");
        error
    }

    #[test]
    fn legacy_campaign_tables_require_complete_materialized_bindings() {
        let (root, relative) = legacy_fixture(json!({}));
        let contract = load_campaign_contract(
            &root,
            &relative,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            &std::collections::BTreeSet::new(),
            false,
        )
        .expect("valid parser-only legacy fixture");
        assert!(!contract.generated);
        assert_eq!(contract.expected_mutations, 1);
        assert_eq!(contract.mutations.len(), 1);
        fs::remove_dir_all(root).expect("fixture cleanup");

        for (label, overrides, expected) in [
            (
                "ranked",
                json!({"ranked_tests": []}),
                "ranked_tests must be a non-empty list",
            ),
            (
                "planned",
                json!({"planned_mutations": []}),
                "expected 1 planned mutation slots, got 0",
            ),
            (
                "materialized",
                json!({"mutations": []}),
                "expected 1 materialized mutations, got 0",
            ),
            (
                "baseline-precedes-materialized",
                json!({
                    "mutations": [],
                    "baseline": {"timeout_seconds": 10, "argv": ["python", "-m", "pytest"]}
                }),
                "baseline.argv omits ranked tests",
            ),
            (
                "scope",
                json!({"test_scopes": {}}),
                "ranked_tests nodeids are missing from declared test_scopes",
            ),
        ] {
            let error = legacy_contract_error(overrides);
            assert!(error.contains(expected), "{label}: {error}");
        }

        for (label, overrides, expected) in [
            (
                "schema",
                json!({"schema_version": 2}),
                "unsupported schema_version",
            ),
            (
                "expected",
                json!({"expected_mutations": 0}),
                "expected_mutations must be a positive integer",
            ),
            (
                "source-type",
                json!({"source_sha256": []}),
                "source_sha256 must be a JSON object",
            ),
            (
                "rank",
                json!({"ranked_tests": [{"rank": 2}]}),
                "ranked test nodeid must be a non-empty string",
            ),
            (
                "expected-ranked",
                json!({"expected_ranked_tests": 2}),
                "expected_ranked_tests=2",
            ),
            (
                "planned-id",
                json!({"planned_mutations": [{"target_path": "conductor/subject.py"}]}),
                "planned_mutations[0].id",
            ),
            (
                "planned-target",
                json!({"planned_mutations": [{"id": "x"}]}),
                "planned_mutations[0].target_path",
            ),
            (
                "mutation-id",
                json!({"mutations": [{"patch_file": "patches/subject.patch"}]}),
                "mutations[0].id",
            ),
            (
                "mutation-patch",
                json!({"mutations": [{"id": "subject_return"}]}),
                "mutations[0].patch_file",
            ),
            (
                "baseline",
                json!({"baseline": {"timeout_seconds": 0, "argv": ["python", "-m", "pytest", "conductor/test_subject.py::test_subject"]}}),
                "baseline.timeout_seconds must be a positive integer",
            ),
            (
                "scope-mode",
                json!({"test_scopes": {"conductor/test_subject.py": {"mode": "unknown"}}}),
                "mode must be 'complete' or 'partial'",
            ),
            (
                "scope-inventory",
                json!({"test_scopes": {"conductor/test_subject.py": {"mode": "partial", "nodeids": ["conductor/test_subject.py::test_subject"]}}}),
                "test_scopes[conductor/test_subject.py].inventory",
            ),
            (
                "scope-nodeids",
                json!({"test_scopes": {"conductor/test_subject.py": {"mode": "partial", "inventory": "python_ast", "nodeids": []}}}),
                "nodeids may not be empty",
            ),
            (
                "scope-inventory-unsupported",
                json!({"test_scopes": {"conductor/test_subject.py": {"mode": "complete", "inventory": "unknown", "nodeids": ["conductor/test_subject.py::test_subject"]}}}),
                "complete test scope inventory is unsupported",
            ),
            (
                "source-empty",
                json!({"source_sha256": {"conductor/subject.py": ""}}),
                "must be a non-empty string",
            ),
            (
                "source-symbols-unbound",
                json!({"source_symbols": {"conductor/other.py": {"subject": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}}),
                "is not bound in source_sha256",
            ),
            (
                "source-symbols-empty",
                json!({"source_symbols": {"conductor/subject.py": {}}}),
                "is empty; omit the path",
            ),
            (
                "source-symbols-digest",
                json!({"source_symbols": {"conductor/subject.py": {"subject": "not-a-sha256"}}}),
                "must be a lowercase SHA-256 digest",
            ),
            (
                "rank-order",
                json!({"ranked_tests": [{"rank": 2, "nodeid": "conductor/test_subject.py::test_subject", "contract": "x", "rationale": "x"}]}),
                "contiguous ranks",
            ),
            (
                "rank-duplicate",
                json!({"expected_ranked_tests": 2, "ranked_tests": [
                    {"rank": 1, "nodeid": "conductor/test_subject.py::test_subject", "contract": "x", "rationale": "x"},
                    {"rank": 2, "nodeid": "conductor/test_subject.py::test_subject", "contract": "x", "rationale": "x"}
                ]}),
                "duplicate nodeids",
            ),
            (
                "argv-missing",
                json!({"baseline": {"timeout_seconds": 10, "argv": ["python", "-m", "pytest"]}}),
                "baseline.argv omits ranked tests",
            ),
            (
                "resource-poll",
                json!({"resource_gate": {"poll_seconds": 0}}),
                "resource_gate.poll_seconds must be in [1, 300]",
            ),
            (
                "environment-type",
                json!({"environment": {"MODE": 1}}),
                "environment[\"MODE\"] must be a string",
            ),
            (
                "host-parent",
                json!({"host_read_dependencies": ["../escape"]}),
                "normalized repository-relative path",
            ),
            (
                "scope-duplicate",
                json!({"test_scopes": {"conductor/test_subject.py": {"mode": "partial", "inventory": "python_ast", "nodeids": ["conductor/test_subject.py::test_subject", "conductor/test_subject.py::test_subject"]}}}),
                "nodeids contains duplicates",
            ),
            (
                "scope-wrong-node",
                json!({"test_scopes": {"conductor/test_subject.py": {"mode": "partial", "inventory": "python_ast", "nodeids": ["other.py::test_subject"]}}}),
                "contains nodeids from another file",
            ),
        ] {
            let error = legacy_contract_error(overrides);
            assert!(error.contains(expected), "{label}: {error}");
        }
    }

    #[test]
    fn complete_python_scope_uses_live_inventory_and_refuses_missing_or_extra_nodes() {
        let complete = json!({"test_scopes": {"conductor/test_subject.py": {
            "mode": "complete", "inventory": "python_ast",
            "nodeids": ["conductor/test_subject.py::test_subject"]
        }}});
        let (root, relative) = legacy_fixture(complete);
        let python_nodeids = std::collections::HashMap::from([(
            "conductor/test_subject.py".to_owned(),
            vec!["conductor/test_subject.py::test_subject".to_owned()],
        )]);
        let loaded = load_campaign_contract(
            &root,
            &relative,
            &python_nodeids,
            &std::collections::HashMap::new(),
            &std::collections::BTreeSet::new(),
            true,
        );
        assert!(loaded.is_ok(), "{loaded:?}");
        fs::remove_dir_all(root).expect("fixture cleanup");
        for declared in [
            vec!["conductor/test_subject.py::missing".to_owned()],
            vec![
                "conductor/test_subject.py::test_subject".to_owned(),
                "conductor/test_subject.py::extra".to_owned(),
            ],
        ] {
            let (root, relative) =
                legacy_fixture(json!({"test_scopes": {"conductor/test_subject.py": {
                    "mode": "complete", "inventory": "python_ast", "nodeids": declared
                }}}));
            let error = load_campaign_contract(
                &root,
                &relative,
                &python_nodeids,
                &std::collections::HashMap::new(),
                &std::collections::BTreeSet::new(),
                false,
            )
            .expect_err("mismatched complete inventory");
            assert!(
                error.contains("complete test scope does not match current Python inventory"),
                "{error}"
            );
            fs::remove_dir_all(root).expect("fixture cleanup");
        }
    }

    #[test]
    fn complete_native_scopes_use_live_inventory_and_bind_ranked_sources() {
        let (root, relative) = native_scope_fixture();
        let loaded = load_fixture_with_timeout(&root, &relative, true)
            .expect("valid C and Rust complete scopes");
        assert_eq!(loaded.ranked_test_paths.len(), 4);
        assert_eq!(loaded.test_scopes.len(), 3);
        assert_eq!(
            loaded.mutations[0].allowed_paths,
            vec!["conductor/subject.py"]
        );
        fs::remove_dir_all(&root).expect("fixture cleanup");

        let (root, relative) = native_scope_fixture();
        let path = root.join(&relative);
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&path).expect("manifest")).expect("fixture JSON");
        manifest["expected_ranked_tests"] = json!(3);
        manifest["ranked_tests"]
            .as_array_mut()
            .expect("ranked tests")
            .retain(|entry| entry["nodeid"] != "conductor/native_fixture.c::test_c_alpha");
        for (index, entry) in manifest["ranked_tests"]
            .as_array_mut()
            .expect("ranked tests")
            .iter_mut()
            .enumerate()
        {
            entry["rank"] = json!(index + 1);
        }
        manifest["baseline"]["argv"]
            .as_array_mut()
            .expect("baseline argv")
            .retain(|entry| entry != "conductor/native_fixture.c::test_c_alpha");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&manifest).expect("manifest JSON"),
        )
        .expect("manifest write");
        let error = load_fixture_with_timeout(&root, &relative, true)
            .expect_err("complete scope without ranked path");
        assert!(
            error.contains("must contain at least one ranked test"),
            "{error}"
        );
        fs::remove_dir_all(&root).expect("fixture cleanup");

        for (label, scope, expected) in [
            (
                "C extra",
                json!({
                    "mode": "complete",
                    "inventory": "c_test",
                    "nodeids": [
                        "conductor/native_fixture.c::test_c_alpha",
                        "conductor/native_fixture.c::test_c_missing"
                    ]
                }),
                "complete test scope does not match current C inventory",
            ),
            (
                "Rust missing",
                json!({
                    "mode": "complete",
                    "inventory": "cargo_test",
                    "nodeids": ["conductor/native_fixture.rs::rust_alpha"]
                }),
                "complete test scope does not match current Rust inventory",
            ),
            (
                "ranked unbound",
                json!({
                    "mode": "partial",
                    "inventory": "c_test",
                    "nodeids": ["conductor/native_fixture.c::test_c_alpha"]
                }),
                "ranked_tests nodeids are missing from declared test_scopes",
            ),
        ] {
            let (root, relative) = native_scope_fixture();
            let path = root.join(&relative);
            let mut manifest: Value =
                serde_json::from_slice(&fs::read(&path).expect("manifest")).expect("fixture JSON");
            if label == "ranked unbound" {
                manifest["test_scopes"]
                    .as_object_mut()
                    .expect("scopes")
                    .remove("conductor/native_fixture.rs");
            } else if label == "C extra" {
                manifest["test_scopes"]["conductor/native_fixture.c"] = scope;
            } else {
                manifest["test_scopes"]["conductor/native_fixture.rs"] = scope;
            }
            fs::write(
                &path,
                serde_json::to_vec_pretty(&manifest).expect("manifest JSON"),
            )
            .expect("manifest write");
            let error = load_fixture_with_timeout(&root, &relative, true)
                .expect_err("invalid native scope");
            assert!(error.contains(expected), "{label}: {error}");
            fs::remove_dir_all(root).expect("fixture cleanup");
        }
    }

    #[test]
    fn native_loader_accepts_all_c_extensions_and_checks_source_drift_inputs() {
        for extension in ["cc", "cpp", "cxx"] {
            let (root, relative) = native_scope_fixture();
            let old = root.join("conductor/native_fixture.c");
            let new_relative = format!("conductor/native_fixture.{extension}");
            let new = root.join(&new_relative);
            fs::rename(old, &new).expect("rename C fixture");
            let manifest_path = root.join(&relative);
            let mut raw = fs::read_to_string(&manifest_path).expect("manifest");
            raw = raw.replace("conductor/native_fixture.c", &new_relative);
            fs::write(&manifest_path, raw).expect("manifest rewrite");
            let loaded = load_fixture_with_timeout(&root, &relative, true)
                .expect("supported C extension should load");
            assert!(
                loaded.test_scopes.contains_key(&new_relative),
                "missing {new_relative}"
            );
            fs::remove_dir_all(root).expect("fixture cleanup");
        }

        let (root, relative) = native_scope_fixture();
        let manifest_path = root.join(&relative);
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).expect("manifest"))
                .expect("fixture JSON");
        manifest["source_symbols"] = json!({
            "conductor/native_fixture.c": {"test_c_alpha": "a".repeat(64)}
        });
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).expect("manifest JSON"),
        )
        .expect("manifest write");
        let candidates =
            std::collections::BTreeSet::from(["conductor/native_fixture.c".to_owned()]);
        let symbols = std::collections::HashMap::from([(
            "conductor/native_fixture.c".to_owned(),
            std::collections::HashMap::from([("test_c_alpha".to_owned(), "b".repeat(64))]),
        )]);
        let drifted = load_campaign_contract(
            &root,
            &relative,
            &std::collections::HashMap::new(),
            &symbols,
            &candidates,
            true,
        )
        .expect("symbol drift fixture");
        assert!(drifted.source_drifted);
        let not_relevant = load_campaign_contract(
            &root,
            &relative,
            &std::collections::HashMap::new(),
            &symbols,
            &std::collections::BTreeSet::new(),
            true,
        )
        .expect("non-relevant source fixture");
        assert!(!not_relevant.source_drifted);
        manifest["source_symbols"] = Value::Object(Map::new());
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).expect("manifest JSON"),
        )
        .expect("manifest rewrite");
        let clean = load_campaign_contract(
            &root,
            &relative,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            &candidates,
            true,
        )
        .expect("matching content fixture");
        assert!(!clean.source_drifted);
        fs::write(
            root.join("conductor/native_fixture.c"),
            "static void test_c_alpha(void) { int changed = 1; }\n",
        )
        .expect("content drift");
        let content_drift = load_campaign_contract(
            &root,
            &relative,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            &candidates,
            true,
        )
        .expect("content drift fixture");
        assert!(content_drift.source_drifted);
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[cfg(feature = "python")]
    #[test]
    fn inspect_native_campaign_reports_readiness_reasons() {
        let (root, relative) = legacy_fixture(json!({}));
        let campaign = load_campaign_contract(
            &root,
            &relative,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            &std::collections::BTreeSet::new(),
            false,
        )
        .expect("inspect fixture");
        let request = json!({
            "campaign": campaign,
            "source_drift": [],
            "manifest_hash_drift": false,
            "blocking_processes": [],
            "value_analysis": Value::Null
        });
        let ready: Value = serde_json::from_str(
            &super::inspect_mutation_campaign_native(&request.to_string())
                .expect("ready inspection"),
        )
        .expect("ready JSON");
        assert_eq!(ready["status"], "READY");
        let mut blocked = request;
        blocked["source_drift"] = json!([{"path": "conductor/subject.py"}]);
        let blocked: Value = serde_json::from_str(
            &super::inspect_mutation_campaign_native(&blocked.to_string())
                .expect("blocked inspection"),
        )
        .expect("blocked JSON");
        assert_eq!(blocked["status"], "NOT_READY");
        assert_eq!(blocked["readiness_reasons"], json!(["source_hash_drift=1"]));
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn a_generated_contract_detects_source_test_and_missing_file_drift() {
        let (root, value) = live_fixture();
        let contract = generated_campaign_contract(
            &root,
            &value,
            b"manifest bytes".to_vec(),
            "conductor/mutation_campaigns/subject_fest_20260906.json",
            "fest",
        )
        .expect("live contract");
        assert!(!contract.source_drifted);

        fs::write(root.join("conductor/subject.py"), "changed\n").expect("source change");
        assert!(
            generated_campaign_contract(
                &root,
                &value,
                b"manifest bytes".to_vec(),
                "conductor/mutation_campaigns/subject_fest_20260906.json",
                "fest",
            )
            .expect("source drift contract")
            .source_drifted
        );

        fs::write(root.join("conductor/subject.py"), "subject\n").expect("source restore");
        fs::write(root.join("conductor/test_subject.py"), "changed\n").expect("test change");
        assert!(
            generated_campaign_contract(
                &root,
                &value,
                b"manifest bytes".to_vec(),
                "conductor/mutation_campaigns/subject_fest_20260906.json",
                "fest",
            )
            .expect("test drift contract")
            .source_drifted
        );

        fs::remove_file(root.join("conductor/test_subject.py")).expect("test removal");
        assert!(
            generated_campaign_contract(
                &root,
                &value,
                b"manifest bytes".to_vec(),
                "conductor/mutation_campaigns/subject_fest_20260906.json",
                "fest",
            )
            .expect("missing test contract")
            .source_drifted
        );
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn a_generated_manifest_declares_no_mutants_and_a_complete_scope() {
        let contract = contract(json!({})).expect("contract");
        assert!(contract.generated);
        // The engine decides how many mutants exist, so the manifest cannot promise a count.
        assert_eq!(contract.expected_mutations, 0);
        assert!(contract.planned_mutations.is_empty());
        assert!(contract.mutations.is_empty());
        assert_eq!(
            contract.ranked_test_paths,
            vec![
                "conductor/test_subject.py".to_owned(),
                "conductor/test_subject_extra.py".to_owned()
            ]
        );
        for scope in contract.test_scopes.values() {
            assert_eq!(scope.get("mode").and_then(Value::as_str), Some("complete"));
        }
        assert_eq!(
            contract.survivor_baseline,
            vec!["constant_replace-abc123456789-0".to_owned()]
        );
        assert!(contract.covers_test("conductor/test_subject.py"));
        assert!(!contract.covers_test("conductor/test_elsewhere.py"));
    }

    #[test]
    fn a_patch_campaign_covers_only_a_ranked_declared_test() {
        let mut contract = contract(json!({})).expect("contract");
        contract.generated = false;
        contract.source_sha256 = json!({"conductor/test_subject.py": "a".repeat(64)});
        contract.ranked_test_paths = vec!["conductor/test_subject.py".to_owned()];
        assert!(contract.covers_test("conductor/test_subject.py"));
        contract.ranked_test_paths.clear();
        assert!(!contract.covers_test("conductor/test_subject.py"));
        contract.ranked_test_paths = vec!["conductor/test_other.py".to_owned()];
        assert!(!contract.covers_test("conductor/test_subject.py"));
    }

    #[test]
    fn safe_relative_rejects_absolute_dot_and_parent_paths_but_normalizes_separators() {
        assert!(safe_relative("/absolute", "path").is_err());
        assert!(safe_relative("./relative", "path").is_err());
        assert!(safe_relative("nested/../escape", "path").is_err());
        assert_eq!(
            safe_relative("nested//./file", "path").unwrap(),
            "nested/file"
        );
    }

    #[test]
    fn registry_path_resolution_keeps_the_normalized_target() {
        let path = lexical_absolute(Path::new("fixture/../registry.json")).expect("path");
        assert!(path.ends_with("registry.json"));
        assert!(!path.to_string_lossy().contains("/../"));
    }

    #[test]
    fn python_rendering_helpers_preserve_scalar_types_and_text() {
        assert_eq!(python_repr(None), "None");
        assert_eq!(python_repr(Some(&Value::Bool(true))), "True");
        assert_eq!(python_repr(Some(&Value::String("x".to_owned()))), "'x'");
        assert_eq!(python_str_or_empty(None), "");
        assert_eq!(python_str_or_empty(Some(&Value::Bool(true))), "True");
        assert_eq!(
            python_str_or_empty(Some(&Value::String("x".to_owned()))),
            "x"
        );
        assert_eq!(super::python_list(Vec::<String>::new()), "[]");
        assert_eq!(
            super::python_list(vec!["x".to_owned(), "y".to_owned()]),
            "['x', 'y']"
        );
    }

    #[test]
    fn digest_validation_requires_exact_lowercase_sha256() {
        assert!(valid_sha256(&"a".repeat(64)));
        assert!(!valid_sha256(&"a".repeat(63)));
        assert!(!valid_sha256(&"A".repeat(64)));
        assert!(!valid_sha256(&format!("{}g", "a".repeat(63))));
    }

    #[test]
    fn mutation_patch_paths_reject_unsafe_shapes_and_accept_a_matching_diff() {
        let root =
            std::env::temp_dir().join(format!("conductor-patch-paths-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("fixture directory");
        let write_patch = |name: &str, body: &str| {
            let path = root.join(name);
            fs::write(&path, body).expect("patch");
            path
        };
        assert!(patch_paths(&write_patch("empty.patch", "")).is_err());
        assert!(patch_paths(&write_patch(
            "rename.patch",
            "diff --git a/a.py b/b.py\n--- a/a.py\n+++ b/b.py\n",
        ))
        .is_err());
        for directive in ["rename to a.py", "copy from a.py"] {
            let body = format!("diff --git a/a.py b/a.py\n{directive}\n");
            let error = patch_paths(&write_patch("single-directive.patch", &body))
                .expect_err("each rename/copy directive must be rejected");
            assert!(
                error.contains("rename or copy files"),
                "{directive}: {error}"
            );
        }
        assert!(patch_paths(&write_patch(
            "create.patch",
            "diff --git a/a.py b/a.py\n--- /dev/null\n+++ b/a.py\n",
        ))
        .is_err());
        for (name, body, diagnostic) in [
            (
                "binary.patch",
                "diff --git a/a.py b/a.py\nGIT binary patch\n",
                "textual unified diffs",
            ),
            (
                "binary-files.patch",
                "diff --git a/a.py b/a.py\nBinary files a/a.py and b/a.py differ\n",
                "textual unified diffs",
            ),
            (
                "copy.patch",
                "diff --git a/a.py b/a.py\ncopy from a.py\ncopy to b.py\n",
                "rename or copy files",
            ),
        ] {
            let error = patch_paths(&write_patch(name, body)).expect_err("unsupported patch");
            assert!(error.contains(diagnostic), "{name}: {error}");
        }
        assert!(patch_paths(&write_patch(
            "malformed-header.patch",
            "diff --git a/a.py b/a.py b/extra.py\n",
        ))
        .is_err());
        for body in [
            "diff --git a/a.py\n",
            "diff --git c/a.py b/a.py\n",
            "diff --git a/a.py c/a.py\n",
        ] {
            let error = patch_paths(&write_patch("invalid-header.patch", body))
                .expect_err("invalid unified-diff header");
            assert!(
                error.contains("unsupported mutation diff header"),
                "{body:?}: {error}"
            );
        }
        for (old, new, expected) in [
            ("c/a.py", "b/a.py", "unsupported mutation patch path"),
            ("a/a.py", "c/a.py", "unsupported mutation patch path"),
        ] {
            let body = format!("diff --git a/a.py b/a.py\n--- {old}\n+++ {new}\n");
            let error = patch_paths(&write_patch("invalid-path.patch", &body))
                .expect_err("invalid unified-diff path");
            assert!(error.contains(expected), "{body:?}: {error}");
        }
        assert!(patch_paths(&write_patch(
            "mismatched-old.patch",
            "diff --git a/a.py b/a.py\n--- a/other.py\n+++ b/a.py\n",
        ))
        .is_err());
        let valid = write_patch(
            "valid.patch",
            "diff --git a/a.py b/a.py\n--- a/a.py\n+++ b/a.py\n@@ -1 +1 @@\n-old\n+new\n",
        );
        assert_eq!(patch_paths(&valid).expect("valid patch"), vec!["a.py"]);
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn value_analysis_accepts_exact_contract_and_rejects_boundary_variants() {
        let ranked = vec!["tests::subject".to_owned()];
        let mutations = vec!["BinaryOperator-abc-0".to_owned()];
        let valid = json!({
            "enabled": true,
            "adapter": "cargo-libtest",
            "baseline_repetitions": 2,
            "tests": [{"nodeid": "tests::subject"}],
            "mutation_contracts": {"BinaryOperator-abc-0": {"kind": "boundary"}}
        });
        assert!(validate_value_analysis(Some(&valid), &ranked, &mutations).is_ok());

        let mut cases = Vec::new();
        for (key, value) in [
            ("enabled", json!(false)),
            ("adapter", json!("unknown")),
            ("baseline_repetitions", json!(1)),
            ("baseline_repetitions", json!(6)),
            ("tests", json!([])),
            ("tests", json!([{"nodeid": "other"}])),
            ("mutation_contracts", json!({})),
        ] {
            let mut candidate = valid.clone();
            candidate[key] = value;
            cases.push(candidate);
        }
        for candidate in cases {
            assert!(
                validate_value_analysis(Some(&candidate), &ranked, &mutations).is_err(),
                "invalid value_analysis variant unexpectedly accepted: {candidate}"
            );
        }
        let mut wrong_keys = valid.clone();
        wrong_keys["mutation_contracts"] = json!({
            "BinaryOperator-abc-0": {"kind": "boundary"},
            "unexpected-mutant": {"kind": "boundary"}
        });
        assert!(
            validate_value_analysis(Some(&wrong_keys), &ranked, &mutations).is_err(),
            "value_analysis must reject an extra contract even when the map length matches"
        );
    }

    #[test]
    fn python_inventory_reports_top_level_and_class_tests_and_actionable_failures() {
        let root =
            std::env::temp_dir().join(format!("native-python-inventory-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("fixture directory");
        let relative = "tests_subject.py";
        fs::write(
            root.join(relative),
            "def test_top(): pass\n\nclass TestGroup:\n    def test_inner(self): pass\n    def helper(self): pass\n\nclass HelperGroup:\n    def test_not_registered(self): pass\n\ndef helper(): pass\n",
        )
        .expect("source");
        assert_eq!(
            inventory_python_test_nodeids(&root, relative).expect("tests"),
            vec![
                "tests_subject.py::test_top".to_owned(),
                "tests_subject.py::TestGroup::test_inner".to_owned()
            ]
        );
        fs::write(root.join(relative), "def helper(): pass\n").expect("empty source");
        let empty = inventory_python_test_nodeids(&root, relative).expect_err("empty scope");
        assert!(
            empty.contains("complete Python test scope is empty"),
            "{empty}"
        );
        fs::write(root.join(relative), "def broken(:\n").expect("invalid source");
        let invalid = inventory_python_test_nodeids(&root, relative).expect_err("syntax error");
        assert!(
            invalid.contains("cannot inventory Python tests"),
            "{invalid}"
        );
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn a_command_naming_some_declared_tests_but_not_all_is_refused() {
        let error = contract(json!({
            "test_argv": ["python", "-m", "pytest", "-q", "conductor/test_subject.py"]
        }))
        .expect_err("partial test_argv must be refused");
        assert!(
            error.contains("conductor/test_subject_extra.py"),
            "error should name the test left out: {error}"
        );
    }

    #[test]
    fn a_suite_runner_that_names_no_test_file_is_accepted() {
        // `cargo test` and `ctest` run everything and name nothing. A suite runner covers more
        // than it declares, never less, so declaring tests it does not spell out is honest.
        let contract = contract(json!({"test_argv": ["cargo", "test", "--release"]}))
            .expect("suite runner contract");
        assert_eq!(contract.test_argv, vec!["cargo", "test", "--release"]);
        assert_eq!(contract.ranked_test_paths.len(), 2);
    }

    #[test]
    fn a_duplicated_baseline_id_is_refused() {
        let error = contract(json!({
            "survivor_baseline": ["dup-000000000000-0", "dup-000000000000-0"]
        }))
        .expect_err("duplicate baseline ids must be refused");
        assert!(error.contains("duplicate"), "{error}");
    }

    #[test]
    fn a_manifest_without_test_digests_is_refused() {
        // Without these a receipt outlives the tests it describes.
        let error = contract(json!({"test_sha256": null})).expect_err("missing test_sha256");
        assert!(error.contains("test_sha256"), "{error}");
        let error = contract(json!({"test_sha256": {}})).expect_err("empty test_sha256");
        assert!(error.contains("test_sha256"), "{error}");
    }

    #[test]
    fn a_manifest_without_a_run_timeout_is_refused() {
        let error = contract(json!({
            "generator": {"engine": "fest", "source": ["conductor/subject.py"]}
        }))
        .expect_err("missing run_timeout_seconds");
        assert!(error.contains("run_timeout_seconds"), "{error}");
    }

    #[test]
    fn a_missing_survivor_baseline_is_refused_but_an_empty_one_is_not() {
        // An absent baseline is an unanswered question; an empty one is the answer "none yet".
        let error = contract(json!({"survivor_baseline": null})).expect_err("missing baseline");
        assert!(error.contains("survivor_baseline"), "{error}");
        let contract = contract(json!({"survivor_baseline": []})).expect("empty baseline");
        assert!(contract.survivor_baseline.is_empty());
    }

    #[test]
    fn complete_python_inventory_requires_the_exact_current_nodeid_order() {
        let root =
            std::env::temp_dir().join(format!("native-complete-python-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("fixture directory");
        fs::write(
            root.join("tests.py"),
            "def test_first(): pass\n\ndef test_second(): pass\n",
        )
        .expect("test source");
        assert_eq!(
            inventory_python_test_nodeids(&root, "tests.py").expect("inventory"),
            vec!["tests.py::test_first", "tests.py::test_second"]
        );
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[cfg(feature = "python")]
    #[test]
    fn native_source_drift_reports_content_absence_and_symbol_pin_changes() {
        let root = std::env::temp_dir().join(format!("native-drift-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("fixture directory");
        fs::write(root.join("subject.py"), "before\n").expect("source");
        let expected = super::sha256_file(&root.join("subject.py")).expect("hash");
        fs::write(root.join("subject.py"), "after\n").expect("source drift");
        fs::write(root.join("content.py"), "before\n").expect("content source");
        let content_expected = super::sha256_file(&root.join("content.py")).expect("content hash");
        fs::write(root.join("content.py"), "after\n").expect("content drift");
        fs::create_dir(root.join("directory")).expect("directory source");
        let request = json!({
            "repo_root": root,
            "source_sha256": {"subject.py": expected, "content.py": content_expected, "absent.py": "a".repeat(64)},
            "source_symbols": {
                "subject.py": {"removed": "b".repeat(64)},
                "directory": {"symbol": "d".repeat(64)}
            },
            "symbol_hashes": {
                "subject.py": {"changed": "c".repeat(64)},
                "directory": {"symbol": "d".repeat(64)}
            }
        });
        let rows: Vec<Value> = serde_json::from_str(
            &super::mutation_source_drift_native(&request.to_string()).expect("drift"),
        )
        .expect("drift rows");
        assert!(rows
            .iter()
            .any(|row| row["path"] == "content.py" && row["symbol"].is_null()));
        assert!(rows.iter().any(|row| row["path"] == "absent.py"));
        assert!(rows
            .iter()
            .any(|row| row["symbol"] == "removed" && row["reason"] == "symbol removed"));
        assert!(rows
            .iter()
            .any(|row| row["path"] == "directory" && row["reason"] == "absent or symlink"));
        fs::remove_dir_all(root).expect("fixture cleanup");
    }
}
