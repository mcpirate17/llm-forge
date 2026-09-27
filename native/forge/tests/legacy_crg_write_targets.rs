//! Rust equivalents of the shell write-target cases originally collected in
//! `test_crg_gate.py`. These literal command/target pairs pin the guard's
//! public behavior, including commands that only read, opaque writes, and
//! path resolution across `cd` and repository boundaries.

#[path = "../src/write_targets.rs"]
mod write_targets;

use std::path::Path;
use write_targets::{repo_write_targets, write_targets, OPAQUE_WRITE};

#[test]
fn read_only_and_descriptor_redirects_have_no_write_targets() {
    for command in [
        "git status --porcelain",
        "ls -la; cat README.md | head -20",
        "make check 2>&1 | tail -5",
        "echo hi > /dev/null 2>&1",
        "cat <<'EOF'\nPath('x').write_text('y')\nEOF",
    ] {
        assert!(write_targets(command).is_empty(), "{command}");
    }
}

#[test]
fn interpreter_source_reports_only_literal_write_targets() {
    let cases: &[(&str, &[&str])] = &[
        (
            "python3 - <<'EOF'\nimport pathlib\npathlib.Path('CLAUDE.md').write_bytes(body)\nEOF",
            &["CLAUDE.md"],
        ),
        (
            "python3 -c \"from pathlib import Path; Path('a.py').write_text('SECRET')\"",
            &["a.py"],
        ),
        (
            "python3 -c 'from pathlib import Path; root = Path(\"/repo\"); out = Path(\"report.md\"); out.unlink()'",
            &["report.md"],
        ),
    ];
    for (command, expected) in cases {
        let actual = write_targets(command);
        assert_eq!(
            actual,
            expected.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "{command}"
        );
    }
    assert_eq!(
        write_targets(
            "python3 - <<'EOF'\nimport pathlib\npathlib.Path(target).write_text('x')\nEOF"
        ),
        vec![OPAQUE_WRITE]
    );
}

#[test]
fn shell_syntax_preserves_real_targets_and_ignores_heredoc_data() {
    let cases: &[(&str, &[&str])] = &[
        ("make check &> build.log", &["build.log"]),
        ("cp a.py b.py\ngit -C /tmp status --porcelain", &["b.py"]),
        (
            "for f in a.py b.py; do sed -i s/x/y/ \"$f\"; done",
            &["a.py", "b.py"],
        ),
        (
            "FILES='a.py b.py'\nfor f in $FILES; do sed -i s/x/y/ \"$f\"; done",
            &["a.py", "b.py"],
        ),
        ("tee out.txt <<'EOF'\nbody\nEOF", &["out.txt"]),
        (
            "cat > conductor/x.py <<'EOF'\nprint('hi')\nEOF",
            &["conductor/x.py"],
        ),
        ("echo one \\\n  > wrapped.txt", &["wrapped.txt"]),
    ];
    for (command, expected) in cases {
        assert_eq!(
            write_targets(command),
            expected.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "{command}"
        );
    }
    let quoted_mention = concat!(
        "python -m conductor.handoff append --title t --body \"line one\n",
        "mentions python3 - <<EOF\n",
        "and continues\" && echo done > out.txt"
    );
    assert_eq!(write_targets(quoted_mention), vec!["out.txt"]);
}

#[test]
fn opaque_targets_are_reported_only_for_unresolved_writes() {
    for command in [
        "echo hi > $HOME/note.txt",
        "for f in $(git ls-files); do rm \"$f\"; done",
        "echo 'unbalanced > out.txt",
    ] {
        assert_eq!(write_targets(command), vec![OPAQUE_WRITE], "{command}");
    }
    assert!(write_targets("echo 'unbalanced | wc -l").is_empty());
}

#[test]
fn write_command_families_keep_their_documented_operands() {
    let cases: &[(&str, &[&str])] = &[
        ("dd if=/dev/zero of=big.bin bs=1M count=1", &["big.bin"]),
        ("grep -r x . | tee -a audit.log", &["audit.log"]),
        (
            "git checkout -- conductor/handoff.py",
            &["conductor/handoff.py"],
        ),
        ("git apply fix.patch", &["fix.patch"]),
        ("patch -p1 target.c", &["target.c"]),
        ("cp a.txt b.txt", &["b.txt"]),
        ("mv a.txt b.txt", &["b.txt"]),
        ("install -m 644 a.txt b.txt", &["b.txt"]),
        ("ln -s a.txt b.txt", &["b.txt"]),
        ("rsync -a src/ dest/", &["dest/"]),
        ("truncate -s 0 log.txt", &["log.txt"]),
        ("sed --in-place s/a/b/ conf.ini", &["conf.ini"]),
        ("rm -f -- --weird-name.txt", &["--weird-name.txt"]),
        ("bash -c \"echo x > nested.txt\"", &["nested.txt"]),
        ("bash <<'EOF'\nsed -i s/a/b/ inner.py\nEOF", &["inner.py"]),
    ];
    for (command, expected) in cases {
        assert_eq!(
            write_targets(command),
            expected.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "{command}"
        );
    }
}

#[test]
fn repository_resolution_handles_external_paths_variables_and_cd() {
    let repo = Path::new("/repo");
    assert_eq!(
        repo_write_targets("git log > /tmp/x.log && echo y > inside.txt", repo),
        vec!["inside.txt"]
    );
    for form in ["$SP", "${SP}"] {
        let command = format!("SP=\"/tmp/scratch\"; echo hi > {form}/note.txt");
        assert_eq!(write_targets(&command), vec!["/tmp/scratch/note.txt"]);
        assert!(repo_write_targets(&command, repo).is_empty());
    }
    assert!(repo_write_targets("cd /tmp/scratch && echo hi > note.txt", repo).is_empty());
    assert_eq!(
        repo_write_targets("cd conductor && sed -i s/a/b/ handoff.py", repo),
        vec!["conductor/handoff.py"]
    );
    for command in [
        "cd $SOMEWHERE && echo hi > note.txt",
        "cd && echo hi > note.txt",
        "cd - && echo hi > note.txt",
    ] {
        assert_eq!(repo_write_targets(command, repo), vec![OPAQUE_WRITE]);
    }
}
