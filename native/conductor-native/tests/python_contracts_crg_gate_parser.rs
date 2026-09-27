#![cfg(feature = "python-compat-tests")]
//! Rust-owned assertions for the shipped Bash write-target compatibility API.

#[path = "python_contracts/crg_gate_support.rs"]
#[allow(dead_code)]
mod crg;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use crg::hook_module;
use pyo3::prelude::*;
use pyo3::types::PyModule;
use std::path::Path;
use support::{path, Case};

fn targets(parser: &Bound<'_, PyModule>, command: &str) -> Vec<String> {
    parser
        .getattr("write_targets")
        .unwrap()
        .call1((command,))
        .unwrap()
        .extract()
        .unwrap()
}

fn repo_targets(
    py: Python<'_>,
    parser: &Bound<'_, PyModule>,
    command: &str,
    repo: &Path,
) -> Vec<String> {
    parser
        .getattr("repo_write_targets")
        .unwrap()
        .call1((command, path(py, repo)))
        .unwrap()
        .extract()
        .unwrap()
}

fn check(command: &str, expected: &[&str]) {
    let _case = Case::new();
    Python::attach(|py| {
        let parser = hook_module(py, "bash_write_targets");
        assert_eq!(targets(&parser, command), expected, "command: {command}");
    });
}

fn opaque() -> &'static str {
    "<opaque-interpreter-write>"
}

#[test]
fn read_only_commands_have_no_write_targets() {
    for command in ["git status --porcelain", "ls -la; cat README.md | head -20"] {
        check(command, &[]);
    }
}

#[test]
fn descriptor_duplication_is_not_a_write() {
    check("make check 2>&1 | tail -5", &[]);
    check("echo hi > /dev/null 2>&1", &[]);
}

#[test]
fn interpreter_heredoc_resolves_a_literal_target() {
    check(
        "python3 - <<'EOF'\nimport pathlib\npathlib.Path('CLAUDE.md').write_bytes(body)\nEOF",
        &["CLAUDE.md"],
    );
}

#[test]
fn write_content_is_not_mistaken_for_a_path() {
    check(
        "python3 -c \"from pathlib import Path; Path('a.py').write_text('SECRET')\"",
        &["a.py"],
    );
}

#[test]
fn unresolvable_interpreter_write_is_reported_opaque() {
    check(
        "python3 - <<'EOF'\nimport pathlib\npathlib.Path(target).write_text('x')\nEOF",
        &[opaque()],
    );
}

#[test]
fn data_heredoc_body_is_not_scanned_for_writes() {
    check("cat <<'EOF'\nPath('x').write_text('y')\nEOF", &[]);
}

#[test]
fn paths_outside_the_repo_are_dropped() {
    let case = Case::new();
    let repo = case.mkdir("repo");
    Python::attach(|py| {
        let parser = hook_module(py, "bash_write_targets");
        assert_eq!(
            repo_targets(
                py,
                &parser,
                "git log > /tmp/x.log && echo y > inside.txt",
                &repo
            ),
            ["inside.txt"]
        );
    });
}

#[test]
fn stream_merge_redirect_is_a_write() {
    check("make check &> build.log", &["build.log"]);
}

#[test]
fn command_local_variable_is_expanded() {
    let case = Case::new();
    let repo = case.mkdir("repo");
    Python::attach(|py| {
        let parser = hook_module(py, "bash_write_targets");
        for form in ["$SP", "${SP}"] {
            let command = format!("SP=\"/tmp/scratch\"; echo hi > {form}/note.txt");
            assert_eq!(targets(&parser, &command), ["/tmp/scratch/note.txt"]);
            assert!(repo_targets(py, &parser, &command, &repo).is_empty());
        }
    });
}

#[test]
fn unresolvable_variable_is_reported_opaque() {
    check("echo hi > $HOME/note.txt", &[opaque()]);
}

#[test]
fn cd_outside_the_repo_moves_relative_targets() {
    let case = Case::new();
    let repo = case.mkdir("repo");
    Python::attach(|py| {
        let parser = hook_module(py, "bash_write_targets");
        assert!(
            repo_targets(py, &parser, "cd /tmp/scratch && echo hi > note.txt", &repo).is_empty()
        );
    });
}

#[test]
fn cd_into_the_repo_resolves_against_that_subdirectory() {
    let case = Case::new();
    let repo = case.mkdir("repo");
    Python::attach(|py| {
        let parser = hook_module(py, "bash_write_targets");
        assert_eq!(
            repo_targets(
                py,
                &parser,
                "cd conductor && sed -i s/a/b/ handoff.py",
                &repo
            ),
            ["conductor/handoff.py"]
        );
    });
}

#[test]
fn unresolvable_cd_makes_relative_targets_opaque() {
    let case = Case::new();
    let repo = case.mkdir("repo");
    Python::attach(|py| {
        let parser = hook_module(py, "bash_write_targets");
        for command in [
            "cd $SOMEWHERE && echo hi > note.txt",
            "cd && echo hi > note.txt",
            "cd - && echo hi > note.txt",
        ] {
            assert_eq!(repo_targets(py, &parser, command, &repo), [opaque()]);
        }
    });
}

#[test]
fn quoted_heredoc_mention_is_not_a_redirection() {
    check("python -m conductor.handoff append --title t --body \"line one\nmentions python3 - <<EOF\nand continues\" && echo done > out.txt", &["out.txt"]);
}

#[test]
fn commands_on_separate_lines_stay_separate() {
    check("cp a.py b.py\ngit -C /tmp status --porcelain", &["b.py"]);
}

#[test]
fn literal_loop_list_expands_to_every_target() {
    check(
        "for f in a.py b.py; do sed -i s/x/y/ \"$f\"; done",
        &["a.py", "b.py"],
    );
}

#[test]
fn computed_loop_list_stays_opaque() {
    check("for f in $(git ls-files); do rm \"$f\"; done", &[opaque()]);
}

#[test]
fn loop_list_hoisted_into_a_variable_still_expands() {
    check(
        "FILES='a.py b.py'\nfor f in $FILES; do sed -i s/x/y/ \"$f\"; done",
        &["a.py", "b.py"],
    );
}

#[test]
fn a_path_the_script_only_reads_is_not_a_target() {
    check("python3 -c 'from pathlib import Path; root = Path(\"/home/tim/Projects/LLM\"); out = Path(\"report.md\"); out.unlink()'", &["report.md"]);
}

macro_rules! command_family {
    ($name:ident, $command:expr, $expected:expr) => {
        #[test]
        fn $name() {
            check($command, &[$expected]);
        }
    };
}

command_family!(
    family_dd,
    "dd if=/dev/zero of=big.bin bs=1M count=1",
    "big.bin"
);
command_family!(family_tee, "grep -r x . | tee -a audit.log", "audit.log");
command_family!(
    family_git_checkout,
    "git checkout -- conductor/handoff.py",
    "conductor/handoff.py"
);
command_family!(family_git_apply, "git apply fix.patch", "fix.patch");
command_family!(family_patch, "patch -p1 target.c", "target.c");
command_family!(family_cp, "cp a.txt b.txt", "b.txt");
command_family!(family_mv, "mv a.txt b.txt", "b.txt");
command_family!(family_install, "install -m 644 a.txt b.txt", "b.txt");
command_family!(family_ln, "ln -s a.txt b.txt", "b.txt");
command_family!(family_rsync, "rsync -a src/ dest/", "dest/");
command_family!(family_truncate, "truncate -s 0 log.txt", "log.txt");
command_family!(family_sed, "sed --in-place s/a/b/ conf.ini", "conf.ini");
command_family!(
    family_rm_dash_name,
    "rm -f -- --weird-name.txt",
    "--weird-name.txt"
);
command_family!(
    family_bash_c,
    "bash -c \"echo x > nested.txt\"",
    "nested.txt"
);
command_family!(
    family_bash_heredoc,
    "bash <<'EOF'\nsed -i s/a/b/ inner.py\nEOF",
    "inner.py"
);
command_family!(
    family_backslash_newline,
    "echo one \\\n  > wrapped.txt",
    "wrapped.txt"
);

#[test]
fn unparseable_command_falls_back_to_write_shape() {
    check("echo 'unbalanced > out.txt", &[opaque()]);
    check("echo 'unbalanced | wc -l", &[]);
}

#[test]
fn a_heredoc_does_not_disturb_the_write_target() {
    check("tee out.txt <<'EOF'\nbody\nEOF", &["out.txt"]);
    check(
        "cat > conductor/x.py <<'EOF'\nprint('hi')\nEOF",
        &["conductor/x.py"],
    );
}
