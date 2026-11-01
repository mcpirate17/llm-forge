use super::*;
use std::fs;
use std::os::unix::{ffi::OsStrExt, fs::symlink};
use std::sync::atomic::{AtomicU64, Ordering};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "forge-syntactic-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, data: &str) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn limits() -> Limits {
    Limits {
        bytes: BYTES,
        entries: ENTRIES,
        hits: HITS,
        depth: DEPTH,
    }
}
#[test]
fn deterministic_literal_regex_hidden_and_target_exclusion() {
    let f = Fixture::new();
    f.write("z.py", "f.x()\nf+x()\n");
    f.write("a.py", "f.x()\n");
    f.write(".hidden/a.py", "f.x()\n");
    f.write("target.py", "f.x()\n");
    let rows = run(&f.0, "f.x", "target.py", limits(), |_| Ok(())).unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row.qualified_name.as_str())
            .collect::<Vec<_>>(),
        ["a.py:1", "z.py:1"]
    );
    assert!(rows.iter().all(|row| row.kind == "calls (syntactic)"));
}
#[test]
fn discovered_symlinks_are_excluded_and_visible_python_fifo_refused() {
    let f = Fixture::new();
    let external = Fixture::new();
    external.write("note.py", "target()\n");
    symlink(external.0.join("note.py"), f.0.join("alias.py")).unwrap();
    symlink(&external.0, f.0.join("directory-link")).unwrap();
    symlink(external.0.join("note.py"), f.0.join("fixture-link")).unwrap();
    f.write("visible.py", "target()\n");
    let rows = run(&f.0, "target", "target.py", limits(), |_| Ok(())).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].file_path, "visible.py");
    let name = std::ffi::CString::new(f.0.join("pipe.py").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(run(&f.0, "target", "target.py", limits(), |_| Ok(())).is_err());
    assert_eq!(
        fs::read_to_string(external.0.join("note.py")).unwrap(),
        "target()\n"
    );
}
#[test]
fn parent_replacement_during_walk_refused() {
    let f = Fixture::new();
    f.write("dir/note.py", "target()\n");
    f.write("replacement/note.py", "target()\n");
    let result = run(&f.0, "target", "target.py", limits(), |path| {
        if path == Path::new("dir/note.py") {
            fs::rename(f.0.join("dir"), f.0.join("moved"))?;
            fs::rename(f.0.join("replacement"), f.0.join("dir"))?;
        }
        Ok(())
    });
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("directory changed"));
}
#[test]
fn entry_byte_depth_and_match_limits_fail_without_partial_results() {
    let f = Fixture::new();
    f.write("a/note.py", "target()\ntarget()\n");
    f.write("b.py", "target()\n");
    let mut l = limits();
    l.entries = 1;
    assert!(run(&f.0, "target", "none.py", l, |_| Ok(())).is_err());
    l = limits();
    l.bytes = 2;
    assert!(run(&f.0, "target", "none.py", l, |_| Ok(())).is_err());
    l = limits();
    l.depth = 0;
    assert!(run(&f.0, "target", "none.py", l, |_| Ok(())).is_err());
    l = limits();
    l.hits = 1;
    assert!(run(&f.0, "target", "none.py", l, |_| Ok(())).is_err());
}
#[test]
fn oversized_and_invalid_text_are_explicit_errors() {
    let f = Fixture::new();
    f.write("big.py", &"x".repeat(MAX_SOURCE_BYTES + 1));
    assert!(run(&f.0, "target", "none.py", limits(), |_| Ok(())).is_err());
    fs::write(f.0.join("big.py"), [0xff]).unwrap();
    assert!(run(&f.0, "target", "none.py", limits(), |_| Ok(())).is_err());
}

#[test]
fn invalid_symbol_or_target_fails_before_directory_enumeration() {
    let f = Fixture::new();
    symlink("missing", f.0.join("unsafe-link")).unwrap();
    for symbol in ["", "bad\0name", &"x".repeat(513)] {
        let error = syntactic_callers(&f.0, symbol, "target.py").unwrap_err();
        assert!(error.contains("invalid syntactic caller symbol"), "{error}");
    }
    for target in ["../outside.py", "/outside.py", "a/../target.py", "bad\0.py"] {
        let error = syntactic_callers(&f.0, "target", target).unwrap_err();
        assert!(
            !error.contains("symlink"),
            "target must be rejected before walking: {error}"
        );
    }
}

#[test]
fn authored_ignore_precedence_negation_and_nested_rules() {
    let f = Fixture::new();
    f.write(".gitignore", "*.py\n!root.py\nignored/\n");
    f.write(".ignore", "!nested/*.py\n");
    f.write(".rgignore", "!nested/allowed.py\nnested/blocked.py\n");
    f.write("root.py", "target()\n");
    f.write("skip.py", "target()\n");
    f.write("nested/.gitignore", "!blocked.py\nallowed.py\n");
    f.write("nested/allowed.py", "target()\n");
    f.write("nested/blocked.py", "target()\n");
    f.write("ignored/deep.py", "target()\n");
    let rows = run(&f.0, "target", "target.py", limits(), |_| Ok(())).unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| r.file_path.as_str())
            .collect::<Vec<_>>(),
        ["nested/allowed.py", "root.py"]
    );
}

#[test]
fn ignored_tree_is_not_opened_but_visible_unsafe_files_are_refused() {
    let f = Fixture::new();
    f.write(".gitignore", "target/\n");
    f.write("target/binary.py", "dummy");
    fs::write(f.0.join("target/binary.py"), [0xff]).unwrap();
    let fifo = std::ffi::CString::new(f.0.join("target/pipe.py").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    f.write("visible.py", "target()\n");
    assert_eq!(
        run(&f.0, "target", "none.py", limits(), |_| Ok(()))
            .unwrap()
            .len(),
        1
    );
    fs::write(f.0.join("visible.py"), [0xff]).unwrap();
    assert!(run(&f.0, "target", "none.py", limits(), |_| Ok(())).is_err());
}

#[test]
fn malformed_or_unsafe_ignore_files_fail_and_consulted_changes_are_detected() {
    let f = Fixture::new();
    f.write(".gitignore", "[z-a]\n");
    f.write("visible.py", "target()\n");
    assert!(run(&f.0, "target", "none.py", limits(), |_| Ok(())).is_err());
    fs::write(f.0.join(".gitignore"), [0xff]).unwrap();
    assert!(run(&f.0, "target", "none.py", limits(), |_| Ok(())).is_err());
    fs::remove_file(f.0.join(".gitignore")).unwrap();
    symlink("visible.py", f.0.join(".gitignore")).unwrap();
    assert!(run(&f.0, "target", "none.py", limits(), |_| Ok(())).is_err());
    fs::remove_file(f.0.join(".gitignore")).unwrap();
    let result = run(&f.0, "target", "none.py", limits(), |path| {
        if path == Path::new("visible.py") {
            fs::write(f.0.join(".ignore"), "visible.py\n")?;
        }
        Ok(())
    });
    assert!(result.unwrap_err().to_string().contains("ignore file"));
}

#[test]
fn ignore_size_and_rule_count_are_bounded() {
    let f = Fixture::new();
    f.write(".ignore", &"x".repeat(64 * 1024 + 1));
    assert!(run(&f.0, "target", "none.py", limits(), |_| Ok(())).is_err());
    f.write(".ignore", &"x\n".repeat(20_001));
    assert!(run(&f.0, "target", "none.py", limits(), |_| Ok(())).is_err());
}
