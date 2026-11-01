use super::*;
use std::fs;
use std::os::unix::fs::symlink;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "forge-clerk-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }
    fn settings(&self) -> Settings {
        Settings::new(&self.0, &[]).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn document(sources: &[Source]) -> Value {
    json!({"schema_version":1,"authority":"local-clerk-only","task_class":"compaction",
        "created_at":"2026-11-01T12:34:56+00:00","model":MODEL,"eval_count":12,
        "sources":sources.iter().map(|s|s.metadata(Some(s.text.chars().count()))).collect::<Vec<_>>(),
        "exact_duplicate_groups":exact_duplicate_groups(sources),
        "draft":{"summary":"facts","source_decisions":[],"open_tasks":[],
            "todo_items":[],"duplicate_candidates":[]},"application":APPLICATION})
}
fn rejected(settings: &Settings, value: &Value, message: &str) {
    let error = validate_value(settings, value).unwrap_err().to_string();
    assert!(error.contains(message), "{error}");
}

#[test]
fn inventory_preserves_input_order_dedup_and_literal_full_sha() {
    let fixture = Fixture::new();
    let a = fixture.write("a.md", "abc");
    let b = fixture.write("b.md", "abc");
    let c = fixture.write("c.md", "# Different ###\nbody");
    let alias = fixture.0.join("alias.md");
    symlink(&a, &alias).unwrap();
    let result = inventory(
        &fixture.settings(),
        &[b.clone(), alias, c.clone(), a.clone()],
    )
    .unwrap();
    assert_eq!(result["authority"], "deterministic-clerk-inventory");
    assert_eq!(result["sources"].as_array().unwrap().len(), 3);
    assert_eq!(result["sources"][0]["path"], b.to_str().unwrap());
    assert_eq!(
        result["sources"][1]["sha256"],
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(result["sources"][2]["title"], "Different");
    assert_eq!(result["exact_duplicate_groups"], json!([[b, a]]));
    assert!(result["sources"][0].get("prompt_text_chars").is_none());
}

#[test]
fn prefix_retention_does_not_truncate_hash_or_duplicate_identity() {
    let fixture = Fixture::new();
    let mut bytes = vec![b'x'; MAX_RETAINED_BYTES + 1];
    let a = fixture.write("a.md", &bytes);
    bytes[MAX_RETAINED_BYTES] = b'y';
    let b = fixture.write("b.md", &bytes);
    let sources = read_sources(&fixture.settings(), &[a, b]).unwrap();
    assert_eq!(sources[0].text, sources[1].text);
    assert_eq!(sources[0].text.chars().count(), MAX_TEXT_CHARS);
    assert_ne!(sources[0].sha256, sources[1].sha256);
    assert!(sources[0].metadata(None).truncated_for_prompt);
    assert!(exact_duplicate_groups(&sources).is_empty());
}

#[test]
fn snippets_are_lossy_utf8_and_unicode_character_bounded() {
    let fixture = Fixture::new();
    let mut bytes = "界".repeat(3000).into_bytes();
    bytes[0] = 255;
    let source = fixture.write("unicode.md", bytes);
    let sources = read_sources(&fixture.settings(), &[source]).unwrap();
    assert!(sources[0].text.starts_with('\u{fffd}'));
    assert_eq!(sources[0].text.chars().count(), MAX_TEXT_CHARS);
    assert!(sources[0].metadata(Some(128)).truncated_for_prompt);
}

#[test]
fn source_count_and_finite_byte_admission_fail_loudly() {
    let fixture = Fixture::new();
    let paths: Vec<_> = (0..4)
        .map(|n| fixture.write(&format!("{n}.md"), "12345"))
        .collect();
    let settings = fixture.settings();
    assert!(read_sources(&settings, &[])
        .unwrap_err()
        .to_string()
        .contains("at least one"));
    assert!(read_sources(&settings, &paths)
        .unwrap_err()
        .to_string()
        .contains("at most 3"));
    let tiny = Settings::with_limits(
        &fixture.0,
        &[],
        Limits {
            file_bytes: 4,
            aggregate_bytes: 8,
        },
    )
    .unwrap();
    assert!(read_sources(&tiny, &paths[..1])
        .unwrap_err()
        .to_string()
        .contains("source byte limit"));
    let tiny = Settings::with_limits(
        &fixture.0,
        &[],
        Limits {
            file_bytes: 5,
            aggregate_bytes: 8,
        },
    )
    .unwrap();
    assert!(read_sources(&tiny, &paths[..2])
        .unwrap_err()
        .to_string()
        .contains("aggregate byte limit"));
}

#[test]
fn allowed_roots_are_explicit_and_status_log_aliases_are_forbidden() {
    let host = Fixture::new();
    let external = Fixture::new();
    let note = external.write("note.md", "facts");
    assert!(read_sources(&host.settings(), std::slice::from_ref(&note))
        .unwrap_err()
        .to_string()
        .contains("outside"));
    let settings = Settings::new(&host.0, std::slice::from_ref(&external.0)).unwrap();
    assert_eq!(read_sources(&settings, &[note]).unwrap().len(), 1);
    let protected = host.write(".current_work.md", "fixture only");
    let alias = host.0.join("ordinary.md");
    symlink(&protected, &alias).unwrap();
    for path in [protected, alias] {
        assert!(read_sources(&settings, &[path])
            .unwrap_err()
            .to_string()
            .contains("forbidden"));
    }
    let ordinary = host.write("ordinary2.md", "facts");
    let forbidden_alias = external.0.join(".current_work.md");
    symlink(ordinary, &forbidden_alias).unwrap();
    assert!(read_sources(&settings, &[forbidden_alias])
        .unwrap_err()
        .to_string()
        .contains("forbidden"));
}

#[test]
fn nonregular_sources_do_not_block_or_pass_as_notes() {
    let fixture = Fixture::new();
    let fifo = fixture.0.join("pipe");
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let error = read_sources(&fixture.settings(), &[fifo]).unwrap_err();
    assert!(error.to_string().contains("regular file"));
    assert!(read_sources(&fixture.settings(), std::slice::from_ref(&fixture.0)).is_err());
}

#[test]
fn provenance_roundtrip_detects_changed_tail_and_metadata_tampering() {
    let fixture = Fixture::new();
    let path = fixture.write("note.md", "# Facts\n".to_owned() + &"x".repeat(70_000));
    let settings = fixture.settings();
    let sources = read_sources(&settings, std::slice::from_ref(&path)).unwrap();
    let valid = document(&sources);
    validate_value(&settings, &valid).unwrap();
    for (field, value, message) in [
        ("sha256", json!("0".repeat(64)), "source changed"),
        ("size_bytes", json!(0), "metadata changed"),
        ("title", json!("bad"), "metadata changed"),
        ("prompt_text_chars", json!(127), "prompt coverage"),
        ("truncated_for_prompt", json!(false), "truncation metadata"),
    ] {
        let mut changed = valid.clone();
        changed["sources"][0][field] = value;
        rejected(&settings, &changed, message);
    }
    let mut bytes = fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() = b'y';
    fs::write(path, bytes).unwrap();
    rejected(&settings, &valid, "source changed");
}

#[test]
fn validation_rejects_spoofed_authority_schema_counts_and_duplicate_groups() {
    let fixture = Fixture::new();
    let path = fixture.write("note.md", "facts");
    let settings = fixture.settings();
    let valid = document(&read_sources(&settings, &[path]).unwrap());
    for (field, value) in [
        ("schema_version", json!(true)),
        ("authority", json!("approved")),
        ("task_class", json!("approval")),
        ("model", json!("other")),
        ("eval_count", json!(true)),
        ("created_at", json!("2025-02-29T00:00:00+00:00")),
        ("application", json!("complete")),
        ("exact_duplicate_groups", json!([["spoof"]])),
    ] {
        let mut changed = valid.clone();
        changed[field] = value;
        assert!(validate_value(&settings, &changed).is_err(), "{field}");
    }
    let mut changed = valid.clone();
    changed["sources"] = json!([valid["sources"][0], valid["sources"][0]]);
    rejected(&settings, &changed, "unique");
    changed["sources"] = json!([]);
    rejected(&settings, &changed, "1..3");
    changed = valid.clone();
    changed["extra"] = json!(null);
    rejected(&settings, &changed, "unexpected fields");
}

#[test]
fn document_reader_rejects_duplicate_keys_nonutf8_symlinks_and_large_inputs() {
    let fixture = Fixture::new();
    let settings = fixture.settings();
    let bad = fixture.write(
        "duplicate.json",
        "{\"schema_version\":1,\"schema_version\":1}",
    );
    assert!(validate_document(&settings, &bad).is_err());
    let bad = fixture.write("invalid.json", [255]);
    assert!(validate_document(&settings, &bad).is_err());
    let alias = fixture.0.join("alias.json");
    symlink(bad, &alias).unwrap();
    assert!(validate_document(&settings, &alias).is_err());
    let large = fixture.write("large.json", vec![b' '; MAX_DOCUMENT_BYTES + 1]);
    assert!(validate_document(&settings, &large).is_err());
}

#[test]
fn model_shape_normalization_preserves_legacy_unicode_bounds() {
    let value = json!({"summary":"界".repeat(401),"source_decisions":[false,"a","b","c","d"],
        "open_tasks":[],"todo_items":["界".repeat(161)],"duplicate_candidates":[]});
    let normalized = normalize_model_draft(&value).unwrap();
    assert_eq!(normalized["summary"].as_str().unwrap().chars().count(), 400);
    assert_eq!(normalized["source_decisions"], json!(["a", "b", "c"]));
    assert_eq!(
        normalized["todo_items"][0]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        160
    );
    let mut bad = value;
    bad["todo_items"] = json!("wrong");
    assert!(normalize_model_draft(&bad).is_err());
}

#[test]
fn saved_document_validation_returns_legacy_shape_and_reads_no_status_log() {
    let fixture = Fixture::new();
    let source = fixture.write("note.md", "preamble\u{2028}# Unicode heading ###\nbody");
    let settings = fixture.settings();
    let sources = read_sources(&settings, &[source]).unwrap();
    assert_eq!(sources[0].title, "Unicode heading");
    let valid = document(&sources);
    let saved = fixture.write("draft.json", serde_json::to_vec(&valid).unwrap());
    assert_eq!(
        validate_document(&settings, &saved).unwrap(),
        json!({"valid":true,"sources":1,"path":saved})
    );
    let protected = fixture.write(".current_work.md", serde_json::to_vec(&valid).unwrap());
    assert!(validate_document(&settings, &protected)
        .unwrap_err()
        .to_string()
        .contains("forbidden"));
    let mut too_many = valid.clone();
    too_many["sources"] = json!([
        valid["sources"][0],
        valid["sources"][0],
        valid["sources"][0],
        valid["sources"][0]
    ]);
    rejected(&settings, &too_many, "1..3");
    let mut nested = Value::Null;
    for _ in 0..34 {
        nested = json!([nested]);
    }
    rejected(&settings, &nested, "structural limit");
}

#[test]
fn pinned_root_refuses_rename_and_symlink_at_canonicalize_open_boundary() {
    let fixture = Fixture::new();
    let host = fixture.0.join("host");
    let external = fixture.0.join("external");
    fs::create_dir(&host).unwrap();
    fs::create_dir(&external).unwrap();
    fs::write(host.join("note.md"), "admitted facts").unwrap();
    fs::write(external.join("note.md"), "outside secret").unwrap();
    let settings = Settings::new(&host, &[]).unwrap();
    let moved = fixture.0.join("original");
    let result = read_sources_checked(&settings, &[host.join("note.md")], |at, _| {
        if at == Boundary::Canonicalized {
            fs::rename(&host, &moved)?;
            symlink(&external, &host)?;
        }
        Ok(())
    });
    fs::remove_file(&host).unwrap();
    fs::rename(&moved, &host).unwrap();
    assert!(result.unwrap_err().to_string().contains("root changed"));
}

#[test]
fn atomic_path_replacement_after_full_read_invalidates_unchanged_descriptor() {
    let fixture = Fixture::new();
    let original = fixture.write("note.md", "original bytes");
    let replacement = fixture.write("replacement.md", "replacement bytes");
    let settings = fixture.settings();
    let result = read_sources_checked(&settings, std::slice::from_ref(&original), |at, path| {
        if at == Boundary::Read {
            fs::rename(&replacement, path)?;
        }
        Ok(())
    });
    assert!(result.unwrap_err().to_string().contains("path changed"));
    assert_eq!(fs::read_to_string(original).unwrap(), "replacement bytes");
}

#[test]
fn pinned_root_identity_is_rechecked_after_full_read() {
    let fixture = Fixture::new();
    let host = fixture.0.join("host");
    let other = fixture.0.join("other");
    fs::create_dir(&host).unwrap();
    fs::create_dir(&other).unwrap();
    fs::write(host.join("note.md"), "facts").unwrap();
    let settings = Settings::new(&host, &[]).unwrap();
    let moved = fixture.0.join("original");
    let result = read_sources_checked(&settings, &[host.join("note.md")], |at, _| {
        if at == Boundary::Read {
            fs::rename(&host, &moved)?;
            fs::rename(&other, &host)?;
        }
        Ok(())
    });
    assert!(result.unwrap_err().to_string().contains("root changed"));
}

#[test]
fn timestamp_offsets_have_exact_layout_and_gregorian_dates() {
    let fixture = Fixture::new();
    let source = fixture.write("note.md", "facts");
    let settings = fixture.settings();
    let mut value = document(&read_sources(&settings, &[source]).unwrap());
    for valid in [
        "2026-11-01T12:34:56+00:00",
        "2026-11-01T12:34:56+0130",
        "2024-02-29T12:34:56.123456Z",
    ] {
        value["created_at"] = json!(valid);
        validate_value(&settings, &value).unwrap();
    }
    for invalid in [
        "2026-11-01T12:34:56+0:0:0:0",
        "2026-11-01T12:34:56+00::00",
        "2026-11-01T12:34:56+000:0",
        "2026-02-29T12:34:56+00:00",
        "2026-11-01T12:34:56+24:00",
        "2026-11-01T12:34:56",
    ] {
        value["created_at"] = json!(invalid);
        rejected(&settings, &value, "created_at");
    }
}
