use forge::{
    active_state::{write_state_atomic, ActiveState},
    ownership, state_publish,
};
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::PathBuf;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(forge::resource_id("state-test").unwrap());
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn state_and_activity_ignore_old_predictable_symlinks() {
    let scratch = Scratch::new();
    let target = scratch.0.join("active_state.json");
    let sentinel = scratch.0.join("sentinel");
    fs::write(&sentinel, b"preserve").unwrap();
    symlink(
        &sentinel,
        scratch
            .0
            .join(format!(".active_state.json.{}.tmp", std::process::id())),
    )
    .unwrap();
    let state = ActiveState {
        schema_version: 1,
        last_updated: "now".into(),
        standing_mandates: vec![],
        active_headings: vec![],
        active_claims: vec![],
    };
    write_state_atomic(&target, &state).unwrap();
    assert_eq!(fs::read(&sentinel).unwrap(), b"preserve");
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        serde_json::from_slice::<ActiveState>(&fs::read(&target).unwrap()).unwrap(),
        state
    );

    let governance = scratch.0.join(".git/governance");
    fs::create_dir_all(&governance).unwrap();
    symlink(&sentinel, governance.join("claim-activity.json.tmp")).unwrap();
    assert!(ownership::touch_claim(&scratch.0, "claim-a", 1_700_000_000.0).unwrap());
    assert_eq!(fs::read(&sentinel).unwrap(), b"preserve");
    let activity = governance.join("claim-activity.json");
    assert_eq!(
        fs::metadata(&activity).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(ownership::load_activity(&scratch.0)
        .unwrap()
        .contains_key("claim-a"));
}

#[test]
fn concurrent_publications_are_complete_and_private() {
    let scratch = Scratch::new();
    let target = scratch.0.join("state.json");
    let workers: Vec<_> = (0..8)
        .map(|i| {
            let target = target.clone();
            std::thread::spawn(move || {
                let bytes =
                    serde_json::to_vec(&serde_json::json!({"writer": i, "data": "x".repeat(8192)}))
                        .unwrap();
                for _ in 0..10 {
                    state_publish::publish(&target, &bytes).unwrap();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&target).unwrap()).unwrap();
    assert!(value["writer"].as_u64().unwrap() < 8);
    assert_eq!(value["data"].as_str().unwrap().len(), 8192);
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 1);
}

#[test]
fn failed_rename_preserves_destination_and_removes_owned_staging() {
    let scratch = Scratch::new();
    let target = scratch.0.join("directory");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("sentinel"), b"preserve").unwrap();
    assert!(state_publish::publish(&target, b"new").is_err());
    assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"preserve");
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 1);
}
