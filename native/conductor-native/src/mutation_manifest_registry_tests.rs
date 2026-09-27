mod registry_resilience_tests {
    use std::collections::{BTreeSet, HashMap};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{load_campaigns, load_registry_fragments, load_registry_manifest_paths};

    static NEXT_TREE: AtomicU64 = AtomicU64::new(0);

    /// A throwaway repository root containing a registry and whatever manifests a test needs.
    fn tree(label: &str) -> PathBuf {
        let serial = NEXT_TREE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "llm-registry-resilience-{label}-{}-{serial}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("conductor/mutation_campaigns")).expect("create tree");
        root
    }

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, body).expect("write file");
    }

    fn write_registry(root: &Path, campaigns: &str) {
        write(
            &root.join("conductor/mutation_campaigns/registry.json"),
            &format!(
                r#"{{"schema_version":1,"enforcement":"changed_tests",
                    "test_patterns":["**/test_*.py"],
                    "receipt_directories":["conductor/mutation_campaigns/receipts"],
                    "campaigns":{campaigns}}}"#
            ),
        );
    }

    fn registry_of(root: &Path) -> PathBuf {
        root.join("conductor/mutation_campaigns/registry.json")
    }

    #[test]
    fn absent_fragment_directory_is_not_an_error() {
        let root = tree("no-fragments");
        write_registry(
            &root,
            r#"[{"manifest":"conductor/mutation_campaigns/a.json"}]"#,
        );
        let fragments = load_registry_fragments(&registry_of(&root)).expect("fragments");
        assert!(fragments.is_empty());
    }

    #[test]
    fn unreadable_fragment_directory_is_reported_instead_of_treated_as_absent() {
        let root = tree("fragment-file");
        write_registry(&root, "[]");
        write(
            &root.join("conductor/mutation_campaigns/registry.d"),
            "not a directory",
        );
        let error = load_registry_fragments(&registry_of(&root))
            .expect_err("a registry.d file is not an absent directory");
        assert!(
            error.contains("cannot read campaign fragment directory"),
            "{error}"
        );
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn fragments_register_campaigns_without_touching_the_shared_array() {
        let root = tree("fragments");
        write_registry(&root, "[]");
        // Two lanes each drop their own file; neither edits a file the other owns.
        write(
            &root.join("conductor/mutation_campaigns/registry.d/zeta.json"),
            r#"{"manifest":"conductor/mutation_campaigns/zeta.json"}"#,
        );
        write(
            &root.join("conductor/mutation_campaigns/registry.d/alpha.json"),
            r#"{"manifest":"conductor/mutation_campaigns/alpha.json"}"#,
        );
        let (_, manifests) =
            load_registry_manifest_paths(&root, &registry_of(&root), None).expect("registry");
        // Sorted by fragment filename so the order a tree yields is stable.
        assert_eq!(
            manifests,
            vec![
                "conductor/mutation_campaigns/alpha.json".to_owned(),
                "conductor/mutation_campaigns/zeta.json".to_owned(),
            ]
        );
    }

    #[test]
    fn malformed_fragment_manifest_paths_fail_with_path_specific_diagnostics() {
        for (label, manifest, expected) in [
            (
                "absolute",
                "/tmp/escape.json",
                "normalized repository-relative path",
            ),
            (
                "parent",
                "conductor/../escape.json",
                "normalized repository-relative path",
            ),
        ] {
            let root = tree(label);
            write_registry(&root, "[]");
            write(
                &root.join("conductor/mutation_campaigns/registry.d/bad.json"),
                &format!(r#"{{"manifest":"{manifest}"}}"#),
            );
            let error = load_registry_manifest_paths(&root, &registry_of(&root), None)
                .expect_err("unsafe fragment path");
            assert!(error.contains(expected), "{label}: {error}");
        }
    }

    #[test]
    fn a_campaign_in_both_the_array_and_a_fragment_is_loaded_once() {
        let root = tree("dedup");
        write_registry(
            &root,
            r#"[{"manifest":"conductor/mutation_campaigns/a.json"}]"#,
        );
        write(
            &root.join("conductor/mutation_campaigns/registry.d/a.json"),
            r#"{"manifest":"conductor/mutation_campaigns/a.json"}"#,
        );
        let (_, manifests) =
            load_registry_manifest_paths(&root, &registry_of(&root), None).expect("registry");
        assert_eq!(manifests, vec!["conductor/mutation_campaigns/a.json"]);
    }

    #[test]
    fn a_registry_declaring_no_campaigns_at_all_is_still_refused() {
        let root = tree("empty");
        write_registry(&root, "[]");
        let error = load_registry_manifest_paths(&root, &registry_of(&root), None)
            .expect_err("empty registry must be refused");
        assert!(error.contains("at least one campaign"), "{error}");
    }

    #[test]
    fn one_unloadable_manifest_does_not_sink_the_whole_registry() {
        // The regression: this returned Err, which the gate reported as a repository-wide
        // REFUSED, blocking every agent over one lane's half-written manifest.
        let root = tree("broken");
        write_registry(
            &root,
            r#"[{"manifest":"conductor/mutation_campaigns/broken.json"},
                {"manifest":"conductor/mutation_campaigns/absent.json"}]"#,
        );
        write(
            &root.join("conductor/mutation_campaigns/broken.json"),
            "{ not json",
        );
        let (loaded, broken) = load_campaigns(
            &root,
            &registry_of(&root),
            &["**/test_*.py".to_owned()],
            &HashMap::new(),
            &BTreeSet::new(),
        )
        .expect("a broken manifest must be reported, not propagated");
        assert!(loaded.is_empty());
        assert_eq!(broken.len(), 2);
        let manifests: Vec<&str> = broken.iter().map(|entry| entry.manifest.as_str()).collect();
        assert!(manifests.contains(&"conductor/mutation_campaigns/broken.json"));
        assert!(manifests.contains(&"conductor/mutation_campaigns/absent.json"));
        assert!(broken.iter().all(|entry| !entry.error.is_empty()));
    }
}
