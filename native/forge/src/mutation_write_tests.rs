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
