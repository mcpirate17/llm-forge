use super::super::{read_sources, validate_document, APPLICATION, MODEL};
use super::*;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::sync::{Arc, Barrier};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(crate::resource_id("clerk-output-test").unwrap());
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn settings(&self) -> Settings {
        Settings::new(&self.0, &[]).unwrap()
    }
    fn document(&self, settings: &Settings) -> Value {
        let path = self.0.join("source.md");
        fs::write(&path, "# Facts\nBounded source.").unwrap();
        make_document(settings, &path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn make_document(settings: &Settings, source: &Path) -> Value {
    let sources = read_sources(settings, &[source.to_owned()]).unwrap();
    json!({"schema_version":1,"authority":"local-clerk-only","task_class":"compaction",
        "created_at":"2026-11-01T12:00:00+00:00","model":MODEL,"eval_count":1,
        "sources":sources.iter().map(|s|s.metadata(Some(s.text.chars().count()))).collect::<Vec<_>>(),
        "exact_duplicate_groups":[],"draft":{"summary":"facts","source_decisions":[],
        "open_tasks":[],"todo_items":[],"duplicate_candidates":[]},"application":APPLICATION})
}
fn entries(path: &Path) -> Vec<OsString> {
    fs::read_dir(path)
        .unwrap()
        .map(|item| item.unwrap().file_name())
        .collect()
}

#[test]
fn roundtrip_is_private_and_leaves_source_unchanged() {
    let f = Fixture::new();
    let settings = f.settings();
    let document = f.document(&settings);
    let result = write_document(&settings, Path::new("nested/draft.json"), &document).unwrap();
    let output = f.0.join(BASE).join("nested/draft.json");
    assert_eq!(result["written"], output.to_str().unwrap());
    assert_eq!(result["sources"], 1);
    assert_eq!(
        fs::metadata(&output).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::read_to_string(f.0.join("source.md")).unwrap(),
        "# Facts\nBounded source."
    );
    assert_eq!(
        validate_document(&settings, &output).unwrap()["valid"],
        true
    );
    let value: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    assert_eq!(value, document);
    assert_eq!(
        entries(output.parent().unwrap()),
        vec![OsString::from("draft.json")]
    );
}
#[test]
fn invalid_or_stale_provenance_does_not_create_output_tree() {
    let f = Fixture::new();
    let settings = f.settings();
    let mut document = f.document(&settings);
    document["draft"]["summary"] = json!("x".repeat(401));
    assert!(write_document(&settings, Path::new("draft.json"), &document).is_err());
    assert!(!f.0.join(BASE).exists());
    document = f.document(&settings);
    fs::write(f.0.join("source.md"), "changed").unwrap();
    assert!(write_document(&settings, Path::new("draft.json"), &document).is_err());
    assert!(!f.0.join(BASE).exists());
}
#[test]
fn invalid_and_outside_paths_fail_before_directory_creation() {
    let f = Fixture::new();
    let settings = f.settings();
    let document = f.document(&settings);
    for path in [
        "",
        ".",
        "../outside.json",
        "a/../outside.json",
        "note.txt",
        ".current_work.md",
        "/tmp/outside.json",
    ] {
        assert!(
            write_document(&settings, Path::new(path), &document).is_err(),
            "{path:?}"
        );
    }
    use std::os::unix::ffi::OsStringExt;
    let non_utf8 = PathBuf::from(OsString::from_vec(vec![
        b'x', 255, b'.', b'j', b's', b'o', b'n',
    ]));
    assert!(write_document(&settings, &non_utf8, &document).is_err());
    assert!(write_document(
        &settings,
        Path::new(&format!("{}.json", "x".repeat(4096))),
        &document
    )
    .is_err());
    assert!(!f.0.join(BASE).exists());
}
#[test]
fn existing_files_links_and_source_collision_never_change_contents() {
    let f = Fixture::new();
    let settings = f.settings();
    let document = f.document(&settings);
    fs::create_dir_all(f.0.join(BASE)).unwrap();
    let existing = f.0.join(BASE).join("existing.json");
    fs::write(&existing, b"sentinel").unwrap();
    assert!(write_document(&settings, &existing, &document).is_err());
    let link = f.0.join(BASE).join("link.json");
    symlink(&existing, &link).unwrap();
    assert!(write_document(&settings, &link, &document).is_err());
    let hard = f.0.join(BASE).join("hard.json");
    fs::hard_link(&existing, &hard).unwrap();
    assert!(write_document(&settings, &hard, &document).is_err());
    assert_eq!(fs::read(&existing).unwrap(), b"sentinel");
    let source = f.0.join(BASE).join("source.json");
    fs::write(&source, b"source").unwrap();
    let sourced = make_document(&settings, &source);
    assert!(write_document(&settings, &source, &sourced)
        .unwrap_err()
        .to_string()
        .contains("collides"));
    assert_eq!(fs::read(source).unwrap(), b"source");
}
#[test]
fn parent_symlink_cannot_redirect_publication() {
    let f = Fixture::new();
    let settings = f.settings();
    let document = f.document(&settings);
    let outside = f.0.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"safe").unwrap();
    fs::create_dir_all(f.0.join(BASE)).unwrap();
    symlink(&outside, f.0.join(BASE).join("redirect")).unwrap();
    assert!(write_document(&settings, Path::new("redirect/draft.json"), &document).is_err());
    assert!(!outside.join("draft.json").exists());
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"safe");
}
#[test]
fn parent_swap_before_link_refuses_and_cleans_only_owned_stage() {
    let f = Fixture::new();
    let settings = f.settings();
    let document = f.document(&settings);
    let outside = f.0.join("outside");
    fs::create_dir(&outside).unwrap();
    let moved = f.0.join(BASE).join("moved");
    let result = write_checked(
        &settings,
        Path::new("sub/draft.json"),
        &document,
        |output| {
            let parent = output.parent().unwrap();
            fs::rename(parent, &moved)?;
            symlink(&outside, parent)?;
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(entries(&moved).is_empty());
    assert!(!outside.join("draft.json").exists());
}
#[test]
fn competing_destination_created_before_link_is_preserved() {
    let f = Fixture::new();
    let settings = f.settings();
    let document = f.document(&settings);
    let result = write_checked(&settings, Path::new("draft.json"), &document, |output| {
        fs::write(output, b"winner")?;
        Ok(())
    });
    assert!(result.is_err());
    let output = f.0.join(BASE).join("draft.json");
    assert_eq!(fs::read(&output).unwrap(), b"winner");
    assert_eq!(
        entries(output.parent().unwrap()),
        vec![OsString::from("draft.json")]
    );
}
#[test]
fn concurrent_writers_publish_once_without_staging_leaks() {
    let f = Fixture::new();
    let settings = f.settings();
    let document = f.document(&settings);
    let barrier = Arc::new(Barrier::new(8));
    let results = std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..8 {
            let barrier = barrier.clone();
            let settings = &settings;
            let document = &document;
            workers.push(scope.spawn(move || {
                barrier.wait();
                write_document(settings, Path::new("shared/draft.json"), document)
            }));
        }
        workers
            .into_iter()
            .filter_map(|worker| worker.join().unwrap().ok())
            .count()
    });
    assert_eq!(results, 1);
    let output = f.0.join(BASE).join("shared/draft.json");
    assert_eq!(
        validate_document(&settings, &output).unwrap()["valid"],
        true
    );
    assert_eq!(
        entries(output.parent().unwrap()),
        vec![OsString::from("draft.json")]
    );
}
#[test]
fn publication_and_cleanup_errors_are_both_reported_without_removing_replacement() {
    let f = Fixture::new();
    let settings = f.settings();
    let document = f.document(&settings);
    let result = write_checked(&settings, Path::new("draft.json"), &document, |output| {
        let parent = output.parent().unwrap();
        let stage = fs::read_dir(parent)?.next().unwrap()?.path();
        fs::rename(&stage, parent.join("moved-stage"))?;
        fs::write(stage, b"foreign replacement")?;
        anyhow::bail!("injected publication failure")
    });
    let message = format!("{:#}", result.unwrap_err());
    assert!(
        message.contains("injected publication failure") && message.contains("cleanup failed"),
        "{message}"
    );
    let parent = f.0.join(BASE);
    assert!(fs::read_dir(&parent)
        .unwrap()
        .map(|e| fs::read(e.unwrap().path()).unwrap())
        .any(|v| v == b"foreign replacement"));
    assert!(!parent.join("draft.json").exists());
}

#[test]
fn preflight_is_read_only_and_validates_existing_components_and_target() {
    let f = Fixture::new();
    let settings = f.settings();
    assert!(preflight_output(&settings, Path::new("nested/draft.json")).is_ok());
    assert!(!f.0.join(BASE).exists());
    for path in [
        "../escape.json",
        "wrong.txt",
        "/tmp/outside.json",
        ".current_work.md",
    ] {
        assert!(
            preflight_output(&settings, Path::new(path)).is_err(),
            "{path}"
        );
    }
    assert!(!f.0.join(BASE).exists());
    fs::create_dir_all(f.0.join(BASE)).unwrap();
    let existing = f.0.join(BASE).join("draft.json");
    fs::write(&existing, b"unchanged").unwrap();
    assert!(preflight_output(&settings, Path::new("draft.json")).is_err());
    assert_eq!(fs::read(&existing).unwrap(), b"unchanged");
    symlink("missing", f.0.join(BASE).join("dangling.json")).unwrap();
    assert!(preflight_output(&settings, Path::new("dangling.json")).is_err());
    assert!(preflight_output(&settings, Path::new("fresh.json")).is_ok());
    assert!(!f.0.join(BASE).join("fresh.json").exists());
    let outside = f.0.join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, f.0.join(BASE).join("link")).unwrap();
    assert!(preflight_output(&settings, Path::new("link/draft.json")).is_err());
    assert!(entries(&outside).is_empty());
}

#[test]
fn preflight_refuses_a_replaced_host_root_without_creating_output() {
    let fixture = Fixture::new();
    let host = fixture.0.join("host");
    let moved = fixture.0.join("original");
    let outside = fixture.0.join("outside");
    fs::create_dir(&host).unwrap();
    fs::create_dir(&outside).unwrap();
    let settings = Settings::new(&host, &[]).unwrap();
    fs::rename(&host, &moved).unwrap();
    symlink(&outside, &host).unwrap();
    let result = preflight_output(&settings, Path::new("draft.json"));
    fs::remove_file(&host).unwrap();
    fs::rename(&moved, &host).unwrap();
    assert!(result.is_err());
    assert!(entries(&outside).is_empty());
    assert!(!host.join(BASE).exists());
}

#[test]
fn preflight_rejects_invalid_component_bytes_before_missing_parent() {
    let fixture = Fixture::new();
    let settings = fixture.settings();
    let long_name = format!("{}.json", "x".repeat(251));
    let long_parent = format!("{}/draft.json", "x".repeat(256));
    let unicode_parent = format!("{}/draft.json", "é".repeat(128));
    for path in [&long_name, &long_parent, &unicode_parent] {
        let error = preflight_output(&settings, Path::new(path)).unwrap_err();
        assert!(format!("{error:#}").contains("component exceeds 255 bytes"));
    }
    let error = preflight_output(&settings, Path::new("nested/bad\0.json")).unwrap_err();
    assert!(format!("{error:#}").contains("NUL"));
    let max_name = format!("{}.json", "x".repeat(250));
    assert!(preflight_output(&settings, Path::new(&max_name)).is_ok());
    assert!(!fixture.0.join(BASE).exists());
}
