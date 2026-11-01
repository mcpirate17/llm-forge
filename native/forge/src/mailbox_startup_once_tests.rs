use super::*;
use std::os::unix::fs::PermissionsExt;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(forge::resource_id("forge-startup-once-test").unwrap());
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn state_file(&self, text: &str) {
        let dir = self.0.join(".session-start");
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dir.join("native.json"))
            .unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn success_is_durable_scoped_and_nonblocking_failures_remain_retryable() {
    let fixture = Fixture::new();
    let first = State::open(&fixture.0, "grok", "session/private-text").unwrap();
    assert!(!first.already_done());
    let error = State::open(&fixture.0, "grok", "other").err().unwrap();
    assert!(error.to_string().contains("busy"));
    drop(first); // failure before commit does not consume the session
    let mut retry = State::open(&fixture.0, "grok", "session/private-text").unwrap();
    assert!(!retry.already_done());
    retry.commit().unwrap();
    drop(retry);
    assert!(State::open(&fixture.0, "grok", "session/private-text")
        .unwrap()
        .already_done());
    assert!(!State::open(&fixture.0, "codex", "session/private-text")
        .unwrap()
        .already_done());
    assert!(!State::open(&fixture.0, "grok", "other")
        .unwrap()
        .already_done());
    let bytes = fs::read_to_string(fixture.0.join(".session-start/native.json")).unwrap();
    assert!(!bytes.contains("private-text"));
    assert_eq!(
        fs::read_dir(fixture.0.join(".session-start"))
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn expired_entries_are_pruned_and_capacity_never_evicts_live_sessions() {
    let fixture = Fixture::new();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let entries = Done {
        sessions: (0..MAX_SESSIONS)
            .map(|n| (format!("{n:064x}"), now))
            .collect(),
    };
    fixture.state_file(&serde_json::to_string(&entries).unwrap());
    assert!(State::open(&fixture.0, "grok", "new")
        .err()
        .unwrap()
        .to_string()
        .contains("4096"));
    let path = fixture.0.join(".session-start/native.json");
    let mut old = entries;
    *old.sessions.values_mut().next().unwrap() = now - TTL;
    fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    let mut state = State::open(&fixture.0, "grok", "new").unwrap();
    state.commit().unwrap();
    let after: Done = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(after.sessions.len(), MAX_SESSIONS);
    assert!(!after.sessions.contains_key(&format!("{:064x}", 0)));
}

#[test]
fn malformed_future_oversized_and_duplicate_state_fails_without_replacement() {
    let future = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    for text in [
        "{bad".to_owned(),
        "{\"sessions\":{},\"sessions\":{}}".to_owned(),
        format!("{{\"sessions\":{{\"{:064x}\":{future}}}}}", 7),
        " ".repeat(MAX_BYTES + 1),
        "{\"sessions\":{},\"unknown\":1}".to_owned(),
    ] {
        let fixture = Fixture::new();
        fixture.state_file(&text);
        assert!(State::open(&fixture.0, "grok", "test").is_err());
        assert_eq!(
            fs::read_to_string(fixture.0.join(".session-start/native.json")).unwrap(),
            text
        );
    }
}

#[test]
fn private_state_refuses_symlinks_hardlinks_and_shared_permissions() {
    for name in ["native.json", "native.lock"] {
        let fixture = Fixture::new();
        fixture.state_file("{\"sessions\":{}}");
        let dir = fixture.0.join(".session-start");
        if name == "native.json" {
            fs::remove_file(dir.join(name)).unwrap();
        }
        let target = fixture.0.join("sentinel");
        fs::write(&target, "preserve").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&target, dir.join(name)).unwrap();
        assert!(State::open(&fixture.0, "grok", "test").is_err());
        fs::remove_file(dir.join(name)).unwrap();
        fs::hard_link(&target, dir.join(name)).unwrap();
        assert!(State::open(&fixture.0, "grok", "test").is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "preserve");
    }
    let fixture = Fixture::new();
    fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(State::open(&fixture.0, "grok", "test").is_err());
}

#[test]
fn invalid_session_ids_fail_before_creating_state() {
    let fixture = Fixture::new();
    for session in [
        "".to_owned(),
        "  ".to_owned(),
        "bad\n".to_owned(),
        "x".repeat(513),
    ] {
        assert!(State::open(&fixture.0, "grok", &session).is_err());
    }
    assert!(!fixture.0.join(".session-start").exists());
}
