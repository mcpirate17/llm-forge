//! Candidate pytest configuration collection using isolated granted stages.
use super::{stage, types::Phase};
use crate::{
    candidate_execution_plan::Selection,
    candidate_snapshot::Snapshot,
    candidate_source::{Source, TreeEntry},
    confined_execution::Outcome,
    process_signals::Interrupts,
};
use anyhow::{Context, Result};
use serde_json::json;
use std::path::Path;
fn config_name(path: &str) -> bool {
    !path
        .split('/')
        .any(|part| matches!(part, ".venv" | "node_modules"))
        && matches!(
            path.rsplit('/').next(),
            Some("pytest.ini" | "setup.cfg" | "tox.ini" | "pyproject.toml")
        )
}
fn sample<'a>(entries: &'a [TreeEntry], config: &str) -> Option<&'a str> {
    let parent = Path::new(config).parent().unwrap_or(Path::new(""));
    entries
        .iter()
        .filter(|entry| entry.mode == "100644" || entry.mode == "100755")
        .filter(|entry| {
            !entry
                .path
                .split('/')
                .any(|p| matches!(p, ".venv" | "node_modules"))
        })
        .filter(|entry| Path::new(&entry.path).starts_with(parent))
        .filter(|entry| {
            Path::new(&entry.path)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("test_") && name.ends_with(".py"))
        })
        .min_by(|left, right| {
            left.size
                .unwrap_or(u64::MAX)
                .cmp(&right.size.unwrap_or(u64::MAX))
                .then_with(|| left.path.cmp(&right.path))
        })
        .map(|entry| entry.path.as_str())
}
fn run_one(
    selection: &Selection,
    interrupts: &Interrupts,
    runtime: &tokio::runtime::Runtime,
    python: &str,
    config: &str,
    target: Option<&str>,
) -> Result<Option<String>> {
    let timeout = if target.is_some() { 300 } else { 120 };
    let report = stage::run(
        selection,
        interrupts,
        runtime,
        timeout,
        target.is_none(),
        |workspace| {
            let root = workspace.path();
            let config = root.join(config);
            let target =
                target.map_or_else(|| root.join(".gate-empty-collect"), |path| root.join(path));
            Ok(vec![
                python.into(),
                "-m".into(),
                "pytest".into(),
                "-c".into(),
                config.to_string_lossy().into_owned(),
                "--collect-only".into(),
                "-q".into(),
                "--no-header".into(),
                target.to_string_lossy().into_owned(),
            ])
        },
    )?;
    if report.outcome == Outcome::Exited && matches!(report.exit_code, Some(0 | 5)) {
        return Ok(None);
    }
    Ok(Some(format!(
        "pytest outcome {:?} exit {:?}: {}",
        report.outcome,
        report.exit_code,
        failure_context(&report.stdout, &report.stderr)
    )))
}
// This summarizes the bounded text supplied by Stage, not an unbounded child
// stream. A head already discarded by Stage's capture cannot be reconstructed.
const DIAGNOSTIC_BYTES: usize = 4096;
const TRUNCATED_MIDDLE: &str = "\n[... middle truncated ...]\n";
fn failure_context(stdout: &str, stderr: &str) -> String {
    let streams = [("stderr", stderr.trim()), ("stdout", stdout.trim())];
    let count = streams.iter().filter(|(_, text)| !text.is_empty()).count();
    if count == 0 {
        return "no output".into();
    }
    // Each label is six ASCII bytes plus ':\n'; between streams is one LF.
    let budget = (DIAGNOSTIC_BYTES - count * 8 - (count - 1)) / count;
    let mut summary = String::with_capacity(DIAGNOSTIC_BYTES);
    for (label, text) in streams.into_iter().filter(|(_, text)| !text.is_empty()) {
        if !summary.is_empty() {
            summary.push('\n');
        }
        summary.push_str(label);
        summary.push_str(":\n");
        push_excerpt(&mut summary, text, budget);
    }
    summary
}
fn push_excerpt(summary: &mut String, text: &str, budget: usize) {
    if text.len() <= budget {
        summary.push_str(text);
        return;
    }
    let kept = budget - TRUNCATED_MIDDLE.len();
    let mut head = kept.div_ceil(2);
    let mut tail = text.len() - kept / 2;
    // Move inward by at most three bytes per UTF-8 boundary, never split a scalar.
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    summary.push_str(&text[..head]);
    summary.push_str(TRUNCATED_MIDDLE);
    summary.push_str(&text[tail..]);
}

pub(super) struct Observation {
    pub phase: Phase,
    held: Vec<crate::candidate_inputs::BoundFile>,
}
impl Observation {
    pub fn verify(&self) -> Result<()> {
        for item in &self.held {
            item.verify()?;
        }
        Ok(())
    }
}
pub(super) fn run(
    selection: &Selection,
    source: &Source,
    snapshot: &Snapshot,
    interrupts: &Interrupts,
    runtime: &tokio::runtime::Runtime,
    skip: bool,
) -> Result<Observation> {
    if skip {
        return Ok(Observation {
            phase: Phase::new(
                "pytest-config",
                true,
                "pytest config probe explicitly skipped",
                json!({"skipped":true}),
            ),
            held: Vec::new(),
        });
    }
    let entries = source.list_tree(&selection.candidate.tree_oid)?;
    anyhow::ensure!(
        entries
            .iter()
            .filter(|entry| config_name(&entry.path))
            .count()
            <= 64,
        "pytest config inventory exceeds 64"
    );
    let mut checked = Vec::new();
    let mut problems = Vec::new();
    let mut held = Vec::new();
    for entry in entries.iter().filter(|entry| config_name(&entry.path)) {
        anyhow::ensure!(
            matches!(entry.mode.as_str(), "100644" | "100755"),
            "pytest config must be a regular candidate blob"
        );
        let (binding, bytes) = crate::candidate_inputs::snapshot(
            &snapshot.path(),
            &entry.path,
            &entry.oid,
            2 * 1024 * 1024,
        )?;
        held.push(binding);
        if entry.path.ends_with("pyproject.toml")
            && !String::from_utf8_lossy(&bytes).contains("[tool.pytest")
        {
            continue;
        }
        let python = selection
            .grants
            .python
            .as_deref()
            .context("pytest config probe requires a granted Python program")?;
        checked.push(entry.path.clone());
        if let Some(problem) = run_one(selection, interrupts, runtime, python, &entry.path, None)? {
            problems.push(format!(
                "{}: addopts/plugins do not parse: {problem}",
                entry.path
            ));
            continue;
        }
        if let Some(test) = sample(&entries, &entry.path) {
            if let Some(problem) = run_one(
                selection,
                interrupts,
                runtime,
                python,
                &entry.path,
                Some(test),
            )? {
                problems.push(format!(
                    "{}: conftest/plugin import failed on {test}: {problem}",
                    entry.path
                ));
            }
        }
    }
    let phase = if problems.is_empty() {
        Phase::new(
            "pytest-config",
            true,
            format!("{} pytest config(s) collect cleanly", checked.len()),
            json!({"configs_checked":checked}),
        )
    } else {
        Phase::new(
            "pytest-config",
            false,
            problems.join("; "),
            json!({"configs_checked":checked,"problems":problems}),
        )
    };
    let observation = Observation { phase, held };
    observation.verify()?;
    Ok(observation)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn entry(path: &str, size: u64) -> TreeEntry {
        TreeEntry {
            path: path.into(),
            mode: "100644".into(),
            object_type: "blob".into(),
            oid: "a".repeat(40),
            size: Some(size),
        }
    }
    #[test]
    fn configs_skip_vendor_trees_and_sample_is_smallest_in_scope() {
        assert!(config_name("pkg/pytest.ini"));
        assert!(config_name("pyproject.toml"));
        assert!(!config_name("pkg/.venv/pytest.ini"));
        assert!(!config_name("node_modules/tox.ini"));
        let entries = vec![
            entry("pkg/test_z.py", 10),
            entry("pkg/nested/test_b.py", 2),
            entry("else/test_a.py", 1),
        ];
        assert_eq!(
            sample(&entries, "pkg/pytest.ini"),
            Some("pkg/nested/test_b.py")
        );
        assert_eq!(sample(&entries, "other/pytest.ini"), None);
    }
    #[test]
    fn unknown_argument_survives_trailing_pytest_context() {
        let stderr = "ERROR: usage: python -m pytest [options]\npython: error: unrecognized arguments: --definitely-not-a-pytest-option\n  inifile: /candidate/pytest.ini\n  rootdir: /candidate\n";
        let summary = failure_context("", stderr);
        assert!(summary.contains("unrecognized arguments: --definitely-not-a-pytest-option"));
        assert!(summary.ends_with("rootdir: /candidate"));
        assert!(!summary.contains(TRUNCATED_MIDDLE));
        assert!(summary.len() <= DIAGNOSTIC_BYTES);
    }
    #[test]
    fn long_unicode_traceback_keeps_head_cause_and_byte_bound() {
        let first = "ERROR: invalid initial argument --bad-option\n";
        let cause = "\nImportError: terminal cause 雪🙂é";
        let stderr = format!("{first}{}{cause}", "trace 雪🙂é\n".repeat(2000));
        let summary = failure_context("", &stderr);
        assert!(summary.starts_with(&format!("stderr:\n{first}")));
        assert!(summary.ends_with(cause));
        assert_eq!(summary.matches(TRUNCATED_MIDDLE).count(), 1);
        assert!(!summary.contains('\u{fffd}'));
        assert!(summary.len() <= DIAGNOSTIC_BYTES);
        // Each multibyte scalar contributes, but cannot make the bounded slice panic.
        for scalar in ['é', '雪', '🙂'] {
            let text = format!("head{}tail", scalar.to_string().repeat(DIAGNOSTIC_BYTES));
            let excerpt = failure_context("", &text);
            assert!(excerpt.starts_with("stderr:\nhead"));
            assert!(excerpt.ends_with("tail"));
            assert!(excerpt.len() <= DIAGNOSTIC_BYTES);
        }
    }
    #[test]
    fn both_streams_share_one_bound_and_empty_control_is_explicit() {
        assert_eq!(failure_context(" \r\n", "\t"), "no output");
        assert_eq!(
            failure_context("collected 0 items\n", ""),
            "stdout:\ncollected 0 items"
        );
        let stdout = format!("stdout first\n{}\nstdout cause", "界".repeat(5000));
        let stderr = format!("stderr first\n{}\nstderr cause", "🙂".repeat(5000));
        let summary = failure_context(&stdout, &stderr);
        for context in [
            "stderr first",
            "stderr cause",
            "stdout first",
            "stdout cause",
        ] {
            assert!(summary.contains(context));
        }
        assert_eq!(summary.matches(TRUNCATED_MIDDLE).count(), 2);
        assert!(summary.len() <= DIAGNOSTIC_BYTES);
    }
}
