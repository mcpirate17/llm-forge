use super::*;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

struct Fixture {
    root: tempfile::TempDir,
    campaign: Campaign,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        for path in ["src/a.py", "tests/test_a.py", "src/unselected.py"] {
            let path = root.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"pass\n").unwrap();
        }
        let campaign = crate::mutation_run_test_support::campaign(root.path(), json!({}));
        Self { root, campaign }
    }
    fn report(&self) -> Value {
        json!({"meta":{"show_contexts":true},"files":{
            "src/a.py":{"contexts":{"1":["", "tests/test_a.py::test_ok|run",
                "tests/test_a.py::test_ok|setup", "tests/test_a.py::test_ok|run"],
                "2":["tests/test_a.py::test_other|teardown"]}},
            "src/unselected.py":{"contexts":{"1":["tests/test_a.py::test_ok|run"]}}
        }})
    }
    fn curate(&self, value: &Value) -> Result<Value> {
        curate(value, self.root.path(), &self.campaign)
    }
    fn rejected(&self, value: &Value, reason: &str) {
        let error = self.curate(value).unwrap_err();
        assert!(format!("{error:#}").contains(reason), "{error:#}");
    }
}

#[test]
fn projection_keeps_only_selected_source_and_deduplicated_phase_stripped_nodes() {
    let f = Fixture::new();
    let expected = json!({"files":{(f.root.path().join("src/a.py").to_str().unwrap()):{
        "contexts":{"1":["tests/test_a.py::test_ok"],"2":["tests/test_a.py::test_other"]}
    }}});
    assert_eq!(f.curate(&f.report()).unwrap(), expected);
    let mut absolute = f.report();
    let files = absolute["files"].as_object_mut().unwrap();
    let source = files.remove("src/a.py").unwrap();
    files.insert(
        f.root.path().join("src/a.py").to_str().unwrap().into(),
        source,
    );
    assert_eq!(f.curate(&absolute).unwrap(), expected);
}

#[test]
fn projection_refuses_unpinned_or_malformed_contexts_and_missing_selected_coverage() {
    let f = Fixture::new();
    for (context, reason) in [
        ("tests/test_a.py::test_ok|unknown", "unsupported pytest"),
        ("tests/other.py::test_ok|run", "unselected test"),
        ("tests/test_a.py::|run", "unselected test"),
        ("tests/test_a.py::test_ok", "lacks pytest phase"),
        ("tests/test_a.py::test_ok|run|setup", "unsupported pytest"),
    ] {
        let mut value = f.report();
        value["files"]["src/a.py"]["contexts"]["1"] = json!([context]);
        f.rejected(&value, reason);
    }
    let mut value = f.report();
    value["files"]["src/a.py"]["contexts"] = json!({"1":[""]});
    f.rejected(&value, "lacks selected source/test contexts");
    value["files"].as_object_mut().unwrap().remove("src/a.py");
    f.rejected(&value, "lacks selected source/test contexts");
    for contexts in [
        json!([]),
        json!({"0":[]}),
        json!({"one":[]}),
        json!({"1":null}),
        json!({"1":[3]}),
    ] {
        let mut value = f.report();
        value["files"]["src/a.py"]["contexts"] = contexts;
        assert!(f.curate(&value).is_err());
    }
    let mut value = f.report();
    value["meta"]["show_contexts"] = json!(false);
    f.rejected(&value, "must include test contexts");
}

#[test]
fn projection_rejects_escaped_aliased_paths_and_context_size_overflow() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    for path in ["../outside.py".to_owned(), "/outside.py".to_owned()] {
        let mut value = f.report();
        let source = value["files"]
            .as_object_mut()
            .unwrap()
            .remove("src/a.py")
            .unwrap();
        value["files"][path] = source;
        assert!(f.curate(&value).is_err());
    }
    symlink("a.py", f.root.path().join("src/alias.py")).unwrap();
    let mut value = f.report();
    value["files"]["src/alias.py"] = json!({"contexts":{}});
    f.rejected(&value, "aliased or missing");
    let mut value = f.report();
    value["files"]["src/a.py"]["contexts"]["1"] = json!(["x".repeat(4097)]);
    f.rejected(&value, "context exceeds bound");
}

fn captured(f: &Fixture) -> (BoundFile, BoundFile) {
    let raw = f.root.path().join("coverage.json");
    fs::write(&raw, serde_json::to_vec(&f.report()).unwrap()).unwrap();
    capture(
        &raw,
        &f.root.path().join("curated.json"),
        f.root.path(),
        &f.campaign,
    )
    .unwrap()
}

#[test]
fn captured_raw_and_projection_are_independently_held_and_refuse_drift() {
    for path in ["coverage.json", "curated.json"] {
        let f = Fixture::new();
        let (raw, curated) = captured(&f);
        raw.verify().unwrap();
        curated.verify().unwrap();
        fs::write(f.root.path().join(path), b"{}").unwrap();
        assert!(if path == "coverage.json" {
            raw.verify()
        } else {
            curated.verify()
        }
        .is_err());
    }
}

#[test]
fn raw_capture_refuses_invalid_json_duplicate_keys_and_oversized_documents() {
    let f = Fixture::new();
    let raw = f.root.path().join("bad.json");
    let output = f.root.path().join("curated.json");
    for bytes in [
        b"not json".as_slice(),
        br#"{"files":{},"files":{}}"#.as_slice(),
    ] {
        fs::write(&raw, bytes).unwrap();
        assert!(capture(&raw, &output, f.root.path(), &f.campaign).is_err());
        assert!(!output.exists());
    }
    let file = fs::File::create(&raw).unwrap();
    file.set_len(MAX_BYTES as u64 + 1).unwrap();
    assert!(capture(&raw, &output, f.root.path(), &f.campaign).is_err());
    assert!(!output.exists());
}
