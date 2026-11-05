use super::*;

const REGISTRY: &str = "campaigns/registry.json";
const CAMPAIGNS: &str = "campaigns";

fn manifest(id: &str) -> Value {
    json!({"title": "t", "campaign_id": id})
}

#[test]
fn writes_a_sorted_manifest_then_its_fragment() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    let written =
        write_manifests(&[manifest("a")], root, REGISTRY, CAMPAIGNS, false).expect("write");

    assert_eq!(written, ["campaigns/a.json", "campaigns/registry.d/a.json"]);
    assert_eq!(
        fs::read_to_string(root.join("campaigns/a.json")).expect("manifest"),
        "{\n  \"campaign_id\": \"a\",\n  \"title\": \"t\"\n}\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("campaigns/registry.d/a.json")).expect("fragment"),
        "{\n  \"manifest\": \"campaigns/a.json\"\n}\n"
    );
}

#[test]
fn an_existing_manifest_is_refused_without_force_and_left_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("campaigns")).expect("mkdir");
    fs::write(root.join("campaigns/a.json"), "recorded").expect("write");

    let error =
        write_manifests(&[manifest("a")], root, REGISTRY, CAMPAIGNS, false).expect_err("refused");

    assert!(
        error.contains("refusing to replace a recorded survivor"),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(root.join("campaigns/a.json")).expect("kept"),
        "recorded"
    );
}

#[test]
fn force_replaces_the_manifest_and_keeps_a_matching_fragment() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write_manifests(&[manifest("a")], root, REGISTRY, CAMPAIGNS, false).expect("first");

    let written =
        write_manifests(&[manifest("a")], root, REGISTRY, CAMPAIGNS, true).expect("forced");

    assert_eq!(written, ["campaigns/a.json"]);
}

#[test]
fn a_fragment_for_another_manifest_refuses_before_the_manifest_is_written() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("campaigns/registry.d")).expect("mkdir");
    fs::write(
        root.join("campaigns/registry.d/a.json"),
        r#"{"manifest": "campaigns/other.json"}"#,
    )
    .expect("write");

    let error =
        write_manifests(&[manifest("a")], root, REGISTRY, CAMPAIGNS, false).expect_err("refused");

    assert!(
        error.contains("already registers a different manifest"),
        "{error}"
    );
    assert!(!root.join("campaigns/a.json").exists());
}

#[test]
fn an_unreadable_fragment_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("campaigns/registry.d")).expect("mkdir");
    fs::write(root.join("campaigns/registry.d/a.json"), "{").expect("write");

    let error =
        write_manifests(&[manifest("a")], root, REGISTRY, CAMPAIGNS, false).expect_err("refused");

    assert!(error.starts_with("cannot read fragment"), "{error}");
}

#[test]
fn manifest_bytes_match_python_sorted_ascii_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let manifest = json!({"z": {"b": "caf\u{e9}", "a": [1, {"d": 2, "c": 3}]}, "campaign_id": "a"});

    write_manifests(&[manifest], root, REGISTRY, CAMPAIGNS, false).expect("write");

    let text = fs::read_to_string(root.join("campaigns/a.json")).expect("manifest");
    assert!(text.starts_with("{\n  \"campaign_id\": \"a\",\n  \"z\": {\n    \"a\": ["));
    assert!(text.contains("\"c\": 3,\n        \"d\": 2"), "{text}");
    assert!(text.contains(r#""b": "caf\u00e9""#), "{text}");
}

#[test]
fn unsafe_identifiers_and_output_roots_never_publish() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    fs::create_dir(&root).unwrap();
    for id in ["../../escaped", "/escaped", "a/b", "..", "a\\b"] {
        let error = write_manifests(&[manifest(id)], &root, REGISTRY, CAMPAIGNS, true).unwrap_err();
        assert!(error.contains("campaign_id"), "{error}");
    }
    for (registry, campaigns) in [("../registry.json", CAMPAIGNS), (REGISTRY, "../outside")] {
        let error =
            write_manifests(&[manifest("a")], &root, registry, campaigns, true).unwrap_err();
        assert!(error.contains("inside the repository"), "{error}");
    }
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn symlinked_campaign_and_registry_directories_cannot_escape() {
    for link in ["campaigns", "campaigns/registry.d"] {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = dir.path().join(link);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(outside.path(), &path).unwrap();
        let error =
            write_manifests(&[manifest("a")], dir.path(), REGISTRY, CAMPAIGNS, true).unwrap_err();
        assert!(error.contains("inside the repository"), "{error}");
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
        assert!(!dir.path().join("campaigns/a.json").exists());
    }
}

#[test]
fn publication_never_follows_a_preexisting_temporary_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let victim = dir.path().join("victim");
    fs::write(&victim, "preserve").unwrap();
    let old_temporary = dir
        .path()
        .join(format!(".receipt.json.{}.tmp", std::process::id()));
    std::os::unix::fs::symlink(&victim, &old_temporary).unwrap();
    atomic_write(&dir.path().join("receipt.json"), "receipt").unwrap();
    assert_eq!(fs::read_to_string(&victim).unwrap(), "preserve");
    assert!(fs::symlink_metadata(&old_temporary)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read_to_string(dir.path().join("receipt.json")).unwrap(),
        "receipt"
    );
}

#[test]
fn missing_prefix_cannot_hide_traversal_through_an_existing_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
    for (registry, campaigns) in [
        (REGISTRY, "missing/../link/nested"),
        ("missing/../link/registry.json", CAMPAIGNS),
    ] {
        let error =
            write_manifests(&[manifest("a")], dir.path(), registry, campaigns, true).unwrap_err();
        assert!(error.contains("inside the repository"), "{error}");
    }
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    assert!(!dir.path().join("missing").exists());
}

#[test]
fn noncanonical_host_paths_still_report_relative_publications() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("repo")).unwrap();
    let root = dir.path().join("repo/../repo");
    let written = write_manifests(&[manifest("a")], &root, REGISTRY, CAMPAIGNS, false).unwrap();
    assert_eq!(written, ["campaigns/a.json", "campaigns/registry.d/a.json"]);
}

#[test]
fn simultaneous_unforced_publications_cannot_overwrite_the_winner() {
    let dir = tempfile::tempdir().unwrap();
    let barrier = std::sync::Barrier::new(8);
    let successes = std::thread::scope(|scope| {
        let children: Vec<_> = (0..8)
            .map(|index| {
                let root = dir.path();
                let barrier = &barrier;
                scope.spawn(move || {
                    let body = json!({"campaign_id": "same", "title": index});
                    barrier.wait();
                    write_manifests(&[body], root, REGISTRY, CAMPAIGNS, false).map(|_| index)
                })
            })
            .collect();
        children
            .into_iter()
            .filter_map(|child| child.join().unwrap().ok())
            .collect::<Vec<_>>()
    });
    assert_eq!(successes.len(), 1);
    let manifest: Value =
        serde_json::from_slice(&fs::read(dir.path().join("campaigns/same.json")).unwrap()).unwrap();
    assert_eq!(manifest["title"], successes[0]);
    assert!(dir.path().join("campaigns/registry.d/same.json").is_file());
}
