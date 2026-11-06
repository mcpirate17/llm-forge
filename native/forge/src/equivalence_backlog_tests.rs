use super::*;
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::Path,
};

fn summary(overrides: Value) -> Vec<u8> {
    let mut value = serde_json::to_value(Summary::default()).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .extend(overrides.as_object().unwrap().clone());
    serde_json::to_vec(&value).unwrap()
}

struct Fixture(tempfile::TempDir);
impl Fixture {
    fn new() -> Self {
        let fixture = Self(tempfile::tempdir().unwrap());
        fs::set_permissions(fixture.0.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(fixture.path("runs")).unwrap();
        fs::set_permissions(fixture.path("runs"), fs::Permissions::from_mode(0o700)).unwrap();
        fixture.write("pyproject.toml", b"[tool.forge.test_record]\nstore='record.db'\n[tool.forge.equivalence_probe]\nledger='ledger.json'\nreport='report.md'\nrun_artifacts='runs'\n");
        fixture.write("explicit.json", &summary(serde_json::from_str(r#"{"measured":[{"module":"a.py","units":null}],"advisory":[{"module":"a.py","qualname":"f","verdict":"NO_DIFFERENCE_OBSERVED","rule":"drop_clamp","lineno":3,"description":"x"}]}"#).unwrap()));
        fixture
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.path().join(name)
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        fs::write(self.path(name), bytes).unwrap();
        fs::set_permissions(self.path(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn args(&self) -> BacklogArgs {
        BacklogArgs {
            summaries: vec![self.path("explicit.json")],
            root: self.0.path().into(),
            ledger: None,
            report: None,
            apply: false,
            dry_run: false,
            consume_recorded: false,
            json: false,
        }
    }
    fn recorded(&self) {
        self.write(
            "runs/gate-first.json",
            &summary(json!({"measured":[{"module":"b.py","units":null}]})),
        );
    }
}
fn refused(args: &BacklogArgs) -> String {
    match prepare(args) {
        Err(error) => format!("{error:#}"),
        Ok(_) => panic!("unexpected admission"),
    }
}

#[test]
fn preview_and_apply_preserve_inputs_until_explicit_consumption() {
    let fixture = Fixture::new();
    fixture.recorded();
    let before = fs::read(fixture.path("explicit.json")).unwrap();
    let prepared = prepare(&fixture.args()).unwrap();
    let preview = prepared.finish(false, false).unwrap();
    assert_eq!(preview["status"], "PREVIEW");
    assert_eq!(preview["new"], 1);
    assert!(!fixture.path("ledger.json").exists());
    assert!(!fixture.path("report.md").exists());
    let applied = prepared.finish(true, false).unwrap();
    assert_eq!(applied["status"], "APPLIED");
    assert!(fixture.path("runs/gate-first.json").exists());
    assert_eq!(fs::read(fixture.path("explicit.json")).unwrap(), before);
    let second = prepare(&fixture.args()).unwrap();
    assert_eq!(second.folded.carried, 1);
    let applied = second.finish(true, true).unwrap();
    assert_eq!(
        applied["consumed"],
        json!([fixture.path("runs/gate-first.json")])
    );
    assert!(!fixture.path("runs/gate-first.json").exists());
    assert_eq!(fs::read(fixture.path("explicit.json")).unwrap(), before);
}

#[test]
fn consumption_uses_frozen_discovery_and_keeps_later_arrivals() {
    let fixture = Fixture::new();
    fixture.recorded();
    let mut args = fixture.args();
    // A file named explicitly AND discovered is consumed because it was discovered.
    args.summaries.push(fixture.path("runs/gate-first.json"));
    let prepared = prepare(&args).unwrap();
    assert_eq!(prepared.summaries.len(), 3);
    fixture.write("runs/gate-later.json", b"{}");
    fixture.write("runs/notes.json", b"{}");
    prepared.finish(true, true).unwrap();
    assert!(!fixture.path("runs/gate-first.json").exists());
    assert!(fixture.path("runs/gate-later.json").exists());
    assert!(fixture.path("runs/notes.json").exists());
    assert!(fixture.path("explicit.json").exists());
}

#[test]
fn stale_ledger_and_replaced_summary_refuse_before_publication() {
    let fixture = Fixture::new();
    fixture.recorded();
    fixture.write("report.md", b"previous report");
    let prepared = prepare(&fixture.args()).unwrap();
    fixture.write("ledger.json", b"{}");
    assert!(format!("{:#}", prepared.finish(true, true).unwrap_err()).contains("previously absent"));
    assert_eq!(
        fs::read(fixture.path("report.md")).unwrap(),
        b"previous report"
    );
    let prepared = prepare(&fixture.args()).unwrap();
    fixture.write(
        "replacement",
        &fs::read(fixture.path("runs/gate-first.json")).unwrap(),
    );
    fs::rename(
        fixture.path("replacement"),
        fixture.path("runs/gate-first.json"),
    )
    .unwrap();
    assert!(prepared.finish(true, true).is_err());
    assert!(fixture.path("runs/gate-first.json").exists());
    assert_eq!(fs::read(fixture.path("ledger.json")).unwrap(), b"{}");
}

#[test]
fn post_publication_recheck_keeps_artifacts_if_inputs_drifted() {
    let fixture = Fixture::new();
    fixture.recorded();
    let prepared = prepare(&fixture.args()).unwrap();
    prepared.finish(true, false).unwrap();
    fixture.write("runs/gate-first.json", b"{\"measured\":[]}");
    let error = format!("{:#}", prepared.consume().unwrap_err());
    assert!(
        error.contains("outputs published") && error.contains("no artifacts consumed"),
        "{error}"
    );
    assert!(fixture.path("runs/gate-first.json").exists());
}

#[test]
fn unsafe_paths_and_aliases_never_overwrite_or_consume() {
    let fixture = Fixture::new();
    let mut args = fixture.args();
    args.report = Some(fixture.path("explicit.json"));
    assert!(refused(&args).contains("aliases input"));
    args.report = Some(fixture.path("ledger.json"));
    assert!(refused(&args).contains("distinct"));
    args.report = Some(fixture.path("missing/report.md"));
    assert!(refused(&args).contains("parent must exist"));
    args = fixture.args();
    symlink(fixture.path("explicit.json"), fixture.path("ledger.json")).unwrap();
    assert!(refused(&args).contains("symlink"));
    fs::remove_file(fixture.path("ledger.json")).unwrap();
    fs::hard_link(fixture.path("explicit.json"), fixture.path("ledger.json")).unwrap();
    assert!(refused(&args).contains("singly linked"));
    assert!(!fixture.path("report.md").exists());
}

#[test]
fn configured_external_output_needs_explicit_operator_path() {
    let fixture = Fixture::new();
    let external = tempfile::tempdir().unwrap();
    fs::set_permissions(external.path(), fs::Permissions::from_mode(0o700)).unwrap();
    fixture.write("pyproject.toml", format!("[tool.forge.test_record]\nstore='r'\n[tool.forge.equivalence_probe]\nledger={:?}\nreport='report.md'\n", external.path().join("ledger.json")).as_bytes());
    let mut args = fixture.args();
    assert!(refused(&args).contains("outside the host"));
    args.ledger = Some(external.path().join("ledger.json"));
    prepare(&args).unwrap().finish(true, false).unwrap();
    assert!(external.path().join("ledger.json").exists());
}

#[test]
fn configured_external_artifact_consumption_is_refused() {
    let fixture = Fixture::new();
    let external = tempfile::tempdir().unwrap();
    fixture.write("pyproject.toml", format!("[tool.forge.test_record]\nstore='r'\n[tool.forge.equivalence_probe]\nledger='ledger.json'\nreport='report.md'\nrun_artifacts={:?}\n", external.path()).as_bytes());
    let mut args = fixture.args();
    args.consume_recorded = true;
    assert!(refused(&args).contains("outside the host"));
    args.consume_recorded = false;
    prepare(&args).unwrap().finish(false, false).unwrap();
}

#[test]
fn host_uses_nearest_declaring_ancestor_and_validates_unused_keys() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.path("nested")).unwrap();
    fs::set_permissions(fixture.path("nested"), fs::Permissions::from_mode(0o700)).unwrap();
    fixture.write("nested/pyproject.toml", b"[project]\nname='nested'\n");
    let mut args = fixture.args();
    args.root = fixture.path("nested");
    let prepared = prepare(&args).unwrap();
    assert_eq!(prepared.host.root, fixture.0.path());
    assert_eq!(prepared.previous_ledger.path, fixture.path("ledger.json"));
    for extra in ["jobs=true", "typo=1", "per_module_timeout='bad'"] {
        fixture.write(
            "pyproject.toml",
            format!(
                "[tool.forge.test_record]\nstore='r'\n[tool.forge.equivalence_probe]\n{extra}\n"
            )
            .as_bytes(),
        );
        assert!(refused(&args).contains("known key"));
    }
    fixture.write("pyproject.toml", b"[tool.forge.test_record]\nstore='r'\nlines=false\n[tool.forge.equivalence_probe]\nledger='ledger.json'\nreport='report.md'\njobs=-2\nper_module_timeout=0.5\n");
    prepare(&args).unwrap();
}

#[test]
fn absent_record_table_uses_start_and_empty_ledger_remains_valid() {
    let fixture = Fixture::new();
    fixture.write(
        "pyproject.toml",
        b"[tool.forge.equivalence_probe]\nledger='ledger.json'\nreport='report.md'\n",
    );
    fixture.write("ledger.json", b" \n\t");
    let prepared = prepare(&fixture.args()).unwrap();
    assert_eq!(prepared.host.root, fixture.0.path());
    assert_eq!(prepared.folded.new, 1);
    fixture.write("ledger.json", b"{invalid");
    assert!(!refused(&fixture.args()).is_empty());
}

#[test]
fn malformed_duplicate_and_oversize_summaries_fail_without_outputs() {
    let fixture = Fixture::new();
    fixture.write("explicit.json", b"{invalid");
    assert!(!refused(&fixture.args()).is_empty());
    let valid = String::from_utf8(summary(json!({}))).unwrap();
    let duplicate = valid.replacen(
        "\"modules_probed\":0",
        "\"modules_probed\":0,\"modules_probed\":0",
        1,
    );
    assert_ne!(valid, duplicate);
    assert_eq!(
        serde_json::from_str::<Value>(&valid).unwrap(),
        serde_json::from_str::<Value>(&duplicate).unwrap()
    );
    fixture.write("explicit.json", duplicate.as_bytes());
    assert!(refused(&fixture.args()).contains("duplicate"));
    fs::OpenOptions::new()
        .write(true)
        .open(fixture.path("explicit.json"))
        .unwrap()
        .set_len(FILE_LIMIT as u64 + 1)
        .unwrap();
    assert!(refused(&fixture.args()).contains("bounded"));
    let mut args = fixture.args();
    args.summaries = vec![fixture.path("explicit.json"); MAX_SUMMARIES + 1];
    assert!(refused(&args).contains("256 summaries"));
    assert!(!fixture.path("ledger.json").exists());
    assert!(!fixture.path("report.md").exists());
}

#[test]
fn changed_host_configuration_refuses_publication() {
    let fixture = Fixture::new();
    let prepared = prepare(&fixture.args()).unwrap();
    fixture.write("pyproject.toml", b"[project]\nname='changed'\n");
    assert!(prepared.finish(true, false).is_err());
    assert!(!fixture.path("ledger.json").exists());
}

#[test]
fn absent_outputs_cannot_become_discovered_summaries() {
    let fixture = Fixture::new();
    let mut args = fixture.args();
    args.ledger = Some(fixture.path("runs/gate-future.json"));
    assert!(refused(&args).contains("discovery namespace"));
    args.ledger = None;
    args.report = Some(fixture.path("runs/gate-future.json"));
    assert!(refused(&args).contains("discovery namespace"));
    assert!(!fixture.path("runs/gate-future.json").exists());
}

#[test]
fn traversal_and_named_user_paths_fail_loudly() {
    assert!(absolute(Path::new("../ledger.json")).is_err());
    let fixture = Fixture::new();
    fixture.write(
        "pyproject.toml",
        b"[tool.forge.equivalence_probe]\nledger='~other/ledger.json'\nreport='report.md'\n",
    );
    assert!(refused(&fixture.args()).contains("named-user"));
}

#[test]
fn shared_output_parent_is_readable_but_apply_refuses_before_staging() {
    let fixture = Fixture::new();
    fs::set_permissions(fixture.0.path(), fs::Permissions::from_mode(0o775)).unwrap();
    let prepared = prepare(&fixture.args()).unwrap();
    prepared.finish(false, false).unwrap();
    let error = format!("{:#}", prepared.finish(true, false).unwrap_err());
    assert!(error.contains("mutation directory"), "{error}");
    assert!(!fixture.path("report.md").exists());
    assert!(!fixture.path("ledger.json").exists());
}

#[test]
fn shared_existing_output_leaves_are_observable_but_not_replaceable() {
    for name in ["ledger.json", "report.md"] {
        let fixture = Fixture::new();
        fixture.write(name, b"{}");
        fs::set_permissions(fixture.path(name), fs::Permissions::from_mode(0o664)).unwrap();
        let prepared = prepare(&fixture.args()).unwrap();
        prepared.finish(false, false).unwrap();
        assert!(
            format!("{:#}", prepared.finish(true, false).unwrap_err()).contains("mutation input")
        );
        assert_eq!(fs::read(fixture.path(name)).unwrap(), b"{}");
        assert!(!fixture
            .path(if name == "ledger.json" {
                "report.md"
            } else {
                "ledger.json"
            })
            .exists());
    }
}

#[test]
fn shared_recorded_directory_or_leaf_refuses_consumption_before_publication() {
    for name in ["runs", "runs/gate-first.json"] {
        let fixture = Fixture::new();
        fixture.recorded();
        let mode = if name == "runs" { 0o775 } else { 0o664 };
        fs::set_permissions(fixture.path(name), fs::Permissions::from_mode(mode)).unwrap();
        let prepared = prepare(&fixture.args()).unwrap();
        prepared.finish(false, false).unwrap();
        assert!(format!("{:#}", prepared.finish(true, true).unwrap_err()).contains("mutation"));
        assert!(!fixture.path("report.md").exists());
        assert!(!fixture.path("ledger.json").exists());
        assert!(fixture.path("runs/gate-first.json").exists());
        prepared.finish(true, false).unwrap();
        assert!(fixture.path("runs/gate-first.json").exists());
    }
}

#[test]
fn output_permission_change_after_capture_is_stale() {
    let fixture = Fixture::new();
    let prepared = prepare(&fixture.args()).unwrap();
    fs::set_permissions(fixture.0.path(), fs::Permissions::from_mode(0o775)).unwrap();
    assert!(prepared.finish(true, false).is_err());
    assert!(!fixture.path("report.md").exists());
}
