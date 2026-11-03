//! Authored controls; not executed as part of the external-only draft.
use super::*;
use std::fs::{File, FileTimes};
use std::time::Duration;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "forge-hygiene-inputs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, path: &str, text: &str) {
        let target = self.0.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, text).unwrap();
    }
    fn store(&self, claims: &[Value]) {
        self.write(
            ".git/governance/ownership-claims.json",
            &json!({"schema_version":1,"claims":claims}).to_string(),
        );
    }
    fn touched(&self, path: &str, seconds: f64) {
        self.write(path, "x");
        File::options()
            .write(true)
            .open(self.0.join(path))
            .unwrap()
            .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs_f64(seconds)))
            .unwrap();
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn claim(paths: &[&str], created: &str, expires: &str) -> Value {
    let mut fields = json!({"owner":"test","paths":paths,"justification":"control",
        "created_at":created,"expires_at":expires});
    let hash = crate::ownership::sha256_json(&fields);
    fields["claim_id"] = json!(format!("claim-{}", &hash[..20]));
    fields
}
const CREATED: &str = "2026-11-01T00:00:00+00:00";
const EXPIRY: &str = "2026-11-01T12:00:00+00:00";

#[test]
fn real_store_declared_expiry_equality_mtime_and_creation_floor() {
    let scratch = Scratch::new();
    let start = timestamp(CREATED).unwrap();
    let expires = timestamp(EXPIRY).unwrap();
    scratch.touched("old.rs", start - 86_400.0);
    scratch.touched("recent.rs", start + 90.0 * 60.0);
    scratch.store(&[
        claim(&["old.rs"], CREATED, EXPIRY),
        claim(&["recent.rs"], CREATED, EXPIRY),
        claim(&["missing.rs"], CREATED, EXPIRY),
    ]);
    let (expired, idle) = claims(&scratch.0, start + 90.0 * 60.0, 90).unwrap();
    assert!(expired.is_empty());
    assert_eq!(idle.len(), 1);
    assert_eq!(idle[0]["idle_minutes"], "90");
    assert_eq!(idle[0]["paths"], "old.rs");
    assert!(claims(&scratch.0, expires, 90).unwrap().0.is_empty());
    let (expired, idle) = claims(&scratch.0, expires + 0.001, 90).unwrap();
    assert_eq!(expired.len(), 3);
    assert!(idle.is_empty());
}

#[test]
fn report_idle_orders_rounded_strings_stably_and_ignores_activity_deadline() {
    let scratch = Scratch::new();
    let now = timestamp(CREATED).unwrap() + 12_000.0;
    scratch.touched("a.rs", now - 90.5 * 60.0);
    scratch.touched("b.rs", now - 91.0 * 60.0);
    scratch.touched("c.rs", now - 92.0 * 60.0);
    let a = claim(&["a.rs"], CREATED, EXPIRY);
    scratch.store(&[
        a.clone(),
        claim(&["b.rs"], CREATED, EXPIRY),
        claim(&["c.rs"], CREATED, EXPIRY),
    ]);
    scratch.write(
        ".git/governance/claim-activity.json",
        &json!({"schema_version":1,
        "seen":{a["claim_id"].as_str().unwrap():"2026-11-01T03:20:00+00:00"}})
        .to_string(),
    );
    let idle = claims(&scratch.0, now, 90).unwrap().1;
    assert_eq!(
        idle.iter()
            .map(|r| r["paths"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["c.rs", "b.rs", "a.rs"]
    );
    assert_eq!(idle[2]["idle_minutes"], "90");
}

#[test]
fn malformed_and_content_tampered_real_claim_store_fail_loud() {
    let scratch = Scratch::new();
    let mut tampered = claim(&["a.rs"], CREATED, EXPIRY);
    tampered["owner"] = json!("other");
    scratch.store(&[tampered]);
    assert!(claims(&scratch.0, timestamp(CREATED).unwrap(), 90).is_err());
    scratch.write(".git/governance/ownership-claims.json", "{");
    assert!(claims(&scratch.0, 0.0, 90).is_err());
}

#[test]
fn configured_registry_merges_fragments_hidden_orphans_and_dangling_rows() {
    let scratch = Scratch::new();
    scratch.write(
        "pyproject.toml",
        "[tool.conductor]\nmutation_registry = 'custom/registry.json'\n",
    );
    scratch.write(
        "custom/registry.json",
        &json!({"campaigns":[{},4,
        {"manifest":"custom/live.json"},{"manifest":"custom/missing.json"}]})
        .to_string(),
    );
    scratch.write(
        "custom/registry.d/.fragment.json",
        r#"{"manifest":"custom/fragment.json"}"#,
    );
    scratch.write("custom/live.json", "{}");
    scratch.write("custom/fragment.json", "{}");
    scratch.write("custom/.orphan.json", "{}");
    scratch.write("custom/ignored.JSON", "{}");
    assert_eq!(
        manifests(&scratch.0).unwrap(),
        (
            vec!["custom/.orphan.json".to_string()],
            vec!["custom/missing.json".to_string()]
        )
    );
    scratch.write("custom/registry.d/bad.json", "{}");
    assert!(manifests(&scratch.0).is_err());
}

fn lease(scratch: &Scratch, name: &str, expiry: &str) -> PathBuf {
    scratch.write(&format!("{name}/.git"), "gitdir: elsewhere");
    scratch.write(
        &format!("{name}/.worktree-lease.json"),
        &json!({"schema":"worktree-lease.v1",
        "owner":"test","purpose":"control","expires_at":expiry})
        .to_string(),
    );
    scratch.0.join(name)
}
#[test]
fn leases_filter_main_and_missing_classify_equality_and_round_down_overdue() {
    let scratch = Scratch::new();
    let now = timestamp(EXPIRY).unwrap();
    let equal = lease(&scratch, "equal", EXPIRY);
    let old = lease(&scratch, "old", "2026-11-01T11:58:59+00:00");
    scratch.write("unleased/.git", "gitdir: elsewhere");
    fs::create_dir_all(scratch.0.join("main/.git")).unwrap();
    let rows = leases_at(
        &[
            equal,
            old,
            scratch.0.join("unleased"),
            scratch.0.join("main"),
            scratch.0.join("missing"),
        ],
        now,
    )
    .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["status"], "leased");
    assert_eq!(rows[0]["overdue_minutes"], 0);
    assert_eq!(rows[0]["branch"], "");
    assert_eq!(rows[1]["status"], "expired");
    assert_eq!(rows[1]["overdue_minutes"], 1);
    assert_eq!(rows[2]["status"], "unleased");
}
#[test]
fn leases_reject_bad_calendar_naive_empty_and_malformed_records() {
    let scratch = Scratch::new();
    for expiry in [
        "2026-02-29T00:00:00+00:00",
        "2026-04-31T00:00:00Z",
        "2026-11-01T00:00:00",
        "garbage",
        "",
    ] {
        let path = lease(&scratch, "bad", expiry);
        assert!(leases_at(&[path], 0.0).is_err(), "{expiry}");
    }
    let path = lease(&scratch, "bad", EXPIRY);
    scratch.write("bad/.worktree-lease.json", "{");
    assert!(leases_at(&[path], 0.0).is_err());
}
#[test]
fn native_index_import_closure_retains_transitive_untracked_edges() {
    let scratch = Scratch::new();
    let repo = git2::Repository::init(&scratch.0).unwrap();
    scratch.write("pkg/__init__.py", "");
    scratch.write("pkg/tracked.py", "from pkg import hidden\n");
    scratch.write("pkg/hidden.py", "from pkg import transitive\n");
    scratch.write("pkg/transitive.py", "VALUE = 1\n");
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("pkg/__init__.py")).unwrap();
    index.add_path(Path::new("pkg/tracked.py")).unwrap();
    index.write().unwrap();
    let rows = untracked_imports(&scratch.0).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["module"], "pkg/hidden.py");
    assert_eq!(rows[0]["tracked_importers"], json!(["pkg/tracked.py"]));
    assert_eq!(rows[1]["module"], "pkg/transitive.py");
    assert_eq!(rows[1]["tracked_importers"], json!([]));
}

#[test]
fn json_bounds_reject_sparse_oversize_nonregular_symlink_and_aggregate_overflow() {
    let scratch = Scratch::new();
    scratch.write("large.json", "{}");
    File::options()
        .write(true)
        .open(scratch.0.join("large.json"))
        .unwrap()
        .set_len(JSON_LIMIT as u64 + 1)
        .unwrap();
    assert!(read_json(&scratch.0.join("large.json"), &mut 0).is_err());
    assert!(read_json(&scratch.0, &mut 0).is_err());
    scratch.write("good.json", "{}");
    std::os::unix::fs::symlink(scratch.0.join("good.json"), scratch.0.join("link.json")).unwrap();
    assert!(read_json(&scratch.0.join("link.json"), &mut 0).is_err());
    assert!(read_json(&scratch.0.join("good.json"), &mut { TOTAL_LIMIT }).is_err());
    let mut total = 0;
    assert_eq!(
        read_json(&scratch.0.join("good.json"), &mut total).unwrap(),
        json!({})
    );
    assert_eq!(total, 2);
    assert!(charge(&mut total, TOTAL_LIMIT).is_err());
}

#[test]
fn bounded_import_scan_refuses_missing_tracked_source_and_python_parse_failure() {
    let scratch = Scratch::new();
    let repo = git2::Repository::init(&scratch.0).unwrap();
    scratch.write("pkg/tracked.py", "VALUE = 1\n");
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("pkg/tracked.py")).unwrap();
    index.write().unwrap();
    fs::remove_file(scratch.0.join("pkg/tracked.py")).unwrap();
    assert!(untracked_imports(&scratch.0).is_err());
    scratch.write("pkg/tracked.py", "def broken(:\n");
    assert!(untracked_imports(&scratch.0).is_err());
    scratch.write("pkg/tracked.py", "VALUE = 1\n");
    assert!(untracked_imports(&scratch.0).unwrap().is_empty());
}
