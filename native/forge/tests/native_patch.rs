#[path = "support/forge_binary.rs"]
mod forge_binary;
use conductor_native::syntax::fingerprint;
use forge::{
    patch::{self, Edit, Request},
    source_file::SourceRoot,
};
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "forge-patch-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        git2::Repository::init(&path).unwrap();
        Self(path)
    }
    fn source(&self, path: &str, text: &str) -> SourceRoot {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
        SourceRoot::open(&self.0).unwrap()
    }
    fn content(&self, path: &str) -> String {
        fs::read_to_string(self.0.join(path)).unwrap()
    }
    fn diff(&self, source: &str, patch: &str, offset: usize) -> anyhow::Result<String> {
        let root = self.source("sample.txt", source);
        let request = Request {
            path: "sample.txt".into(),
            expected_sha256: fingerprint(source),
            language: None,
            edit: Edit::UnifiedDiff {
                patch: patch.to_owned(),
                max_offset_lines: offset,
            },
        };
        patch::prepare(&root, &request)?.apply(&root)?;
        Ok(self.content("sample.txt"))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn replacement(path: &str, source: &str, search: &str, replacement: &str) -> Request {
    Request {
        path: path.into(),
        expected_sha256: fingerprint(source),
        language: None,
        edit: Edit::Replace {
            search: search.into(),
            replacement: replacement.into(),
            expected_matches: 1,
        },
    }
}

#[test]
fn preparation_is_read_only_and_apply_preserves_mode_and_exact_recovery_blob() {
    let fixture = Fixture::new();
    let source = "fn answer() -> u32 { 42 }\n";
    let root = fixture.source("src/lib.rs", source);
    fs::set_permissions(
        fixture.0.join("src/lib.rs"),
        fs::Permissions::from_mode(0o751),
    )
    .unwrap();
    fs::write(fixture.0.join("unrelated"), "dirty work").unwrap();
    let plan = patch::prepare(&root, &replacement("src/lib.rs", source, "42", "43")).unwrap();
    assert!(!plan.report().applied);
    assert_eq!(plan.report().validation, "native_ast");
    assert_eq!(fixture.content("src/lib.rs"), source);
    let report = plan.apply(&root).unwrap();
    assert!(report.applied);
    assert_eq!(
        report.candidate_sha256,
        fingerprint(&source.replace("42", "43"))
    );
    assert_eq!(fixture.content("unrelated"), "dirty work");
    assert_eq!(
        fs::metadata(fixture.0.join("src/lib.rs"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o751
    );
    let repo = git2::Repository::open(&fixture.0).unwrap();
    let oid = git2::Oid::from_str(report.git_snapshot.as_deref().unwrap()).unwrap();
    assert_eq!(repo.find_blob(oid).unwrap().content(), source.as_bytes());
    assert_eq!(
        repo.find_reference(&format!("refs/forge/undo/{oid}"))
            .unwrap()
            .target(),
        Some(oid)
    );
    assert_eq!(fs::read_dir(fixture.0.join("src")).unwrap().count(), 1);
    assert!(patch::prepare_restore(
        &root,
        Path::new("src/lib.rs"),
        &fingerprint(source),
        &oid.to_string()
    )
    .is_err());
    let recovery = patch::prepare_restore(
        &root,
        Path::new("src/lib.rs"),
        &report.candidate_sha256,
        &oid.to_string(),
    )
    .unwrap();
    assert_eq!(recovery.report().validation, "exact_git_recovery");
    recovery.apply(&root).unwrap();
    assert_eq!(fixture.content("src/lib.rs"), source);
}

#[test]
fn stale_preconditions_and_invalid_syntax_never_write() {
    let fixture = Fixture::new();
    let source = "fn answer() -> u32 { 42 }\n";
    let root = fixture.source("lib.rs", source);
    assert!(patch::prepare(&root, &replacement("lib.rs", source, "42", "(")).is_err());
    let mut request = replacement("lib.rs", source, "42", "43");
    request.expected_sha256 = "0".repeat(64);
    assert!(patch::prepare(&root, &request).is_err());
    let plan = patch::prepare(&root, &replacement("lib.rs", source, "42", "43")).unwrap();
    fs::write(fixture.0.join("lib.rs"), "// concurrent edit\n").unwrap();
    assert!(plan.apply(&root).is_err());
    assert_eq!(fixture.content("lib.rs"), "// concurrent edit\n");
    let source = "x x x";
    let root = fixture.source("a.txt", source);
    assert!(patch::prepare(&root, &replacement("a.txt", source, "x", "y")).is_err());
    assert!(patch::prepare(&root, &replacement("a.txt", source, "absent", "y")).is_err());
}

#[test]
fn unified_diff_preserves_crlf_unicode_and_final_newline_state() {
    let f = Fixture::new();
    assert_eq!(
        f.diff(
            "α\r\nβ\r\n",
            "--- a/sample.txt\n+++ b/sample.txt\n@@ -1,2 +1,2 @@\n α\r\n-β\r\n+γ\r\n",
            0
        )
        .unwrap(),
        "α\r\nγ\r\n"
    );
    assert_eq!(f.diff("α\nβ", "@@ -1,2 +1,2 @@\n α\n-β\n\\ No newline at end of file\n+γ\n\\ No newline at end of file\n", 0).unwrap(), "α\nγ");
    assert_eq!(
        f.diff("a\nb\n", "@@ -1,0 +2,1 @@\n+insert\n", 0).unwrap(),
        "a\ninsert\nb\n"
    );
    assert_eq!(f.diff("a\nb\n", "@@ -1 +0,0 @@\n-a\n", 0).unwrap(), "b\n");
    assert_eq!(f.diff("", "@@ -0,0 +1 @@\n+new\n", 0).unwrap(), "new\n");
    assert_eq!(
        f.diff(
            "a\nb\nc\n",
            "@@ -1 +1,2 @@\n-a\n+A\n+extra\n@@ -3 +4 @@\n-c\n+C\n",
            0
        )
        .unwrap(),
        "A\nextra\nb\nC\n"
    );
}

#[test]
fn fuzz_is_bounded_exact_and_unambiguous() {
    let f = Fixture::new();
    let patch = "@@ -1 +1 @@\n-target\n+changed\n";
    assert!(f.diff("prefix\ntarget\n", patch, 0).is_err());
    assert_eq!(
        f.diff("prefix\ntarget\n", patch, 1).unwrap(),
        "prefix\nchanged\n"
    );
    assert!(f.diff("prefix\ntarget\ntarget\n", patch, 2).is_err());
    assert!(f.diff("prefix\ntarget \n", patch, 1).is_err());
    assert!(f.diff("prefix\nother\ntarget\n", patch, 1).is_err());
    assert!(f.diff("prefix\ntarget\n", patch, 4097).is_err());
}

#[test]
fn diff_stress_matches_independent_line_splices() {
    let f = Fixture::new();
    let source: String = (0..200).map(|i| format!("line α {i}\n")).collect();
    let root = f.source("sample.txt", &source);
    let lines: Vec<_> = source.split_inclusive('\n').collect();
    for case in 0..1000 {
        let start = (case * 73) % 199;
        let removed = (case * 17) % (200 - start);
        let added = (case * 11) % 9;
        if removed == 0 && added == 0 {
            continue;
        }
        let replacement: String = (0..added)
            .map(|i| format!("added β {case} {i}\n"))
            .collect();
        let expected = lines[..start].concat() + &replacement + &lines[start + removed..].concat();
        let old_start = start + usize::from(removed != 0);
        let new_start = start + usize::from(added != 0);
        let mut diff = format!("@@ -{old_start},{removed} +{new_start},{added} @@\n");
        for line in &lines[start..start + removed] {
            diff.push('-');
            diff.push_str(line);
        }
        for line in replacement.split_inclusive('\n') {
            diff.push('+');
            diff.push_str(line);
        }
        let request = Request {
            path: "sample.txt".into(),
            expected_sha256: fingerprint(&source),
            language: None,
            edit: Edit::UnifiedDiff {
                patch: diff,
                max_offset_lines: 0,
            },
        };
        let plan = patch::prepare(&root, &request).unwrap();
        assert_eq!(
            plan.report().candidate_sha256,
            fingerprint(&expected),
            "case {case}"
        );
    }
    assert_eq!(f.content("sample.txt"), source);
}

#[test]
fn malformed_headers_counts_and_multi_file_patches_are_rejected() {
    let f = Fixture::new();
    for patch in [
        "",
        "@@ -0 +1 @@\n-a\n+b\n",
        "@@ -1,2 +1 @@\n-a\n+b\n",
        "@@ -1 +1 @@\n-a\n+b\n+c\n",
        "@@ -1 +2 @@\n-a\n+b\n",
        "--- a/other.txt\n+++ b/sample.txt\n@@ -1 +1 @@\n-a\n+b\n",
        "@@ -1 +1 @@\n-a\n+b\n--- a/other.txt\n+++ b/other.txt\n",
        "@@ -1 +1 @@\n-a\n+b\n@@ -1 +1 @@\n-a\n+c\n",
        "@@ -1 +1 @@\n\\ No newline at end of file\n-a\n+b\n",
        "@@ -999999999999999999999999999999 +1 @@\n-a\n+b\n",
    ] {
        assert!(f.diff("a\n", patch, 0).is_err(), "accepted {patch:?}");
    }
}

#[test]
fn capabilities_reject_escapes_metadata_symlinks_and_nonregular_sources() {
    let f = Fixture::new();
    let root = f.source("a.txt", "before");
    for path in ["../a.txt", "/etc/passwd", ".git/config", ""] {
        assert!(root.read(Path::new(path)).is_err());
    }
    symlink("/etc", f.0.join("outside")).unwrap();
    symlink("a.txt", f.0.join("link")).unwrap();
    assert!(root.read(Path::new("outside/passwd")).is_err());
    assert!(root.read(Path::new("link")).is_err());
    assert!(root.read(Path::new(".git")).is_err());
    let fifo = std::ffi::CString::new(f.0.join("fifo").as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: the NUL-terminated pathname belongs to the fixture, mode is valid.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(root.read(Path::new("fifo")).is_err());
    fs::hard_link(f.0.join("a.txt"), f.0.join("alias")).unwrap();
    let plan = patch::prepare(&root, &replacement("a.txt", "before", "before", "after")).unwrap();
    assert!(plan.apply(&root).is_err());
    assert_eq!(f.content("alias"), "before");
}

#[test]
fn missing_snapshot_store_and_competing_lock_leave_source_untouched() {
    let f = Fixture::new();
    let root = f.source("a.txt", "before");
    let directory = fs::File::open(&f.0).unwrap();
    use std::os::fd::AsRawFd;
    // SAFETY: descriptor is live until after the plan attempts to apply.
    assert_eq!(
        unsafe { libc::flock(directory.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let request = replacement("a.txt", "before", "before", "after");
    assert!(patch::prepare(&root, &request)
        .unwrap()
        .apply(&root)
        .is_err());
    drop(directory);
    fs::remove_dir_all(f.0.join(".git")).unwrap();
    assert!(patch::prepare(&root, &request)
        .unwrap()
        .apply(&root)
        .is_err());
    assert_eq!(f.content("a.txt"), "before");
    assert_eq!(fs::read_dir(&f.0).unwrap().count(), 1);
}

#[test]
fn cli_inspection_folding_and_patching_need_no_external_runtime() {
    let f = Fixture::new();
    let source = "fn answer() -> u32 { 42 }\n";
    f.source("lib.rs", source);
    let command = |args: &[&str]| {
        Command::new(forge_binary::path())
            .env("PATH", "")
            .args(["code", "--root"])
            .arg(&f.0)
            .args(args)
            .output()
            .unwrap()
    };
    for action in ["inspect", "fold"] {
        let output = command(&[action, "lib.rs"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["source_sha256"], fingerprint(source));
    }
    fs::write(
        f.0.join("request.json"),
        serde_json::json!({"path":"lib.rs", "expected_sha256":fingerprint(source),
        "edit":{"kind":"replace","search":"42","replacement":"43"}})
        .to_string(),
    )
    .unwrap();
    let request = f.0.join("request.json");
    let output = command(&["patch", "--request", request.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(f.content("lib.rs"), source);
    let output = command(&["patch", "--request", request.to_str().unwrap(), "--apply"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(f.content("lib.rs"), source.replace("42", "43"));
}
