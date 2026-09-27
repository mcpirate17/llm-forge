#![cfg(feature = "python-compat-tests")]
//! Hook doctor contracts with Rust-owned fixtures and assertions.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{buffer_text, capture, py_json};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyList};
use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use support::{module, path, Case};

const GOOD: &str = "#!/usr/bin/env python3\nimport json\nprint(json.dumps({\"hookSpecificOutput\": {\"hookEventName\": \"PreToolUse\"}}))\n";

fn doctor(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "tooling.hooks.dispatch.doctor")
}

fn hook(root: &Path, name: &str, text: &str, executable: bool) -> PathBuf {
    let file = root.join(".claude/hooks").join(name);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, text).unwrap();
    fs::set_permissions(
        &file,
        fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
    )
    .unwrap();
    file
}

fn declared<'py>(
    py: Python<'py>,
    name: &str,
    event: &str,
    matcher: &str,
    timeout: i32,
) -> Bound<'py, PyAny> {
    doctor(py)
        .getattr("Declared")
        .unwrap()
        .call1((
            event,
            matcher,
            format!("$CLAUDE_PROJECT_DIR/.claude/hooks/{name}"),
            timeout,
        ))
        .unwrap()
}

fn standard<'py>(py: Python<'py>, name: &str) -> Bound<'py, PyAny> {
    declared(py, name, "PreToolUse", "Bash", 3)
}

fn static_report<'py>(py: Python<'py>, root: &Path, item: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let doc = doctor(py);
    let report = doc.getattr("Report").unwrap().call1((item,)).unwrap();
    let resolved = doc
        .getattr("resolve")
        .unwrap()
        .call1((item.getattr("command").unwrap(), path(py, root)))
        .unwrap();
    doc.getattr("static_checks")
        .unwrap()
        .call1((resolved, &report, path(py, root)))
        .unwrap();
    report
}

fn run_report<'py>(py: Python<'py>, root: &Path, item: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let doc = doctor(py);
    let report = doc.getattr("Report").unwrap().call1((item,)).unwrap();
    doc.getattr("run_check")
        .unwrap()
        .call1((
            item,
            &report,
            path(py, root),
            path(py, &root.join("scratch")),
        ))
        .unwrap();
    report
}

fn problems(report: &Bound<'_, PyAny>) -> Vec<String> {
    report.getattr("problems").unwrap().extract().unwrap()
}

fn status(report: &Bound<'_, PyAny>, expected: &str) {
    assert!(report.getattr("status").unwrap().eq(expected).unwrap());
}

#[test]
fn good_hook_passes_static_and_run() {
    let case = Case::new();
    hook(case.root(), "good.py", GOOD, true);
    Python::attach(|py| {
        let item = standard(py, "good.py");
        status(&static_report(py, case.root(), &item), "OK");
        let report = run_report(py, case.root(), &item);
        status(&report, "OK");
        let elapsed: f64 = report.getattr("elapsed_ms").unwrap().extract().unwrap();
        assert!(elapsed > 0.0);
    });
}

#[test]
fn mode_644_hook_is_dead_and_exits_126() {
    let case = Case::new();
    hook(case.root(), "budget.py", GOOD, false);
    Python::attach(|py| {
        let item = standard(py, "budget.py");
        let static_result = static_report(py, case.root(), &item);
        status(&static_result, "DEAD");
        assert!(problems(&static_result)[0].contains("not executable (mode 644)"));
        let run = run_report(py, case.root(), &item);
        status(&run, "DEAD");
        assert!(problems(&run)[0].starts_with("exit 126"));
    });
}

#[test]
fn nonzero_exit_with_no_output_is_dead() {
    let case = Case::new();
    hook(
        case.root(),
        "crash.py",
        "#!/usr/bin/env python3\nimport sys\nsys.exit(1)\n",
        true,
    );
    Python::attach(|py| {
        let report = run_report(py, case.root(), &standard(py, "crash.py"));
        status(&report, "DEAD");
        assert_eq!(problems(&report), ["exit 1; stderr: <empty>"]);
    });
}

#[test]
fn garbage_stdout_is_dead() {
    let case = Case::new();
    hook(
        case.root(),
        "garbage.sh",
        "#!/bin/bash\necho 'Traceback (most recent call last):'\n",
        true,
    );
    Python::attach(|py| {
        let report = run_report(py, case.root(), &standard(py, "garbage.sh"));
        status(&report, "DEAD");
        assert!(problems(&report)[0].starts_with("stdout is not JSON"));
    });
}

#[test]
fn silent_hook_warns_unless_registered_quiet() {
    let case = Case::new();
    hook(case.root(), "silent.sh", "#!/bin/bash\nexit 0\n", true);
    Python::attach(|py| {
        let report = run_report(py, case.root(), &standard(py, "silent.sh"));
        status(&report, "WARN");
        assert_eq!(problems(&report), ["wrote nothing to stdout"]);
    });
}

#[test]
fn timeout_is_dead() {
    let case = Case::new();
    hook(case.root(), "slow.sh", "#!/bin/bash\nsleep 5\n", true);
    Python::attach(|py| {
        let report = run_report(
            py,
            case.root(),
            &declared(py, "slow.sh", "PreToolUse", "Bash", 1),
        );
        status(&report, "DEAD");
        assert_eq!(problems(&report), ["timed out after 1s"]);
    });
}

#[test]
fn missing_empty_and_broken_scripts_are_dead() {
    let case = Case::new();
    hook(case.root(), "empty.py", "", true);
    hook(case.root(), "noshebang.py", "print(1)\n", true);
    hook(
        case.root(),
        "syntax.py",
        "#!/usr/bin/env python3\ndef (:\n",
        true,
    );
    Python::attach(|py| {
        assert!(
            problems(&static_report(py, case.root(), &standard(py, "absent.py")))[0]
                .starts_with("missing:")
        );
        assert!(
            problems(&static_report(py, case.root(), &standard(py, "empty.py")))[0]
                .starts_with("empty file:")
        );
        assert!(problems(&static_report(
            py,
            case.root(),
            &standard(py, "noshebang.py")
        ))
        .is_empty());
        assert!(
            problems(&static_report(py, case.root(), &standard(py, "syntax.py")))[0]
                .starts_with("does not compile:")
        );
    });
}

#[test]
fn static_liveness_for_binaries_and_bad_shebangs() {
    let case = Case::new();
    let binary = hook(case.root(), "forge", "", true);
    let mut bytes = b"\x7fELF".to_vec();
    bytes.extend([0; 60]);
    fs::write(&binary, bytes).unwrap();
    let real = case.root().join(".claude/hooks/true");
    fs::copy("/bin/true", &real).unwrap();
    hook(
        case.root(),
        "flat.sh",
        "#!/usr/bin/env python3\nexit 0\n",
        false,
    );
    hook(
        case.root(),
        "badinterp.sh",
        "#!/no/such/interp-9f1e\nexit 0\n",
        true,
    );
    Python::attach(|py| {
        for name in ["forge", "true"] {
            assert!(problems(&static_report(py, case.root(), &standard(py, name))).is_empty());
        }
        assert!(
            problems(&static_report(py, case.root(), &standard(py, "flat.sh")))[0]
                .starts_with("not executable")
        );
        assert!(problems(&static_report(
            py,
            case.root(),
            &standard(py, "badinterp.sh")
        ))[0]
            .starts_with("shebang interpreter not resolvable:"));
    });
}

#[test]
fn resolve_strips_env_prefix_and_interpreter() {
    let case = Case::new();
    Python::attach(|py| {
        let resolved = doctor(py).getattr("resolve").unwrap().call1((
            "env GOVERNANCE_OWNER=\"${GOVERNANCE_OWNER:-claude}\" $CLAUDE_PROJECT_DIR/x.py verify",
            path(py, case.root()),
        )).unwrap();
        assert!(resolved
            .getattr("env")
            .unwrap()
            .eq(py_json(
                py,
                json!({"GOVERNANCE_OWNER": "${GOVERNANCE_OWNER:-claude}"})
            ))
            .unwrap());
        assert!(resolved
            .getattr("script")
            .unwrap()
            .eq(path(py, &case.root().join("x.py")))
            .unwrap());
        assert!(resolved.getattr("interpreter").unwrap().is_none());
        let module_result = doctor(py)
            .getattr("resolve")
            .unwrap()
            .call1((
                "python3 -m conductor.context_telemetry",
                path(py, case.root()),
            ))
            .unwrap();
        assert!(module_result
            .getattr("module")
            .unwrap()
            .eq("conductor.context_telemetry")
            .unwrap());
        assert!(module_result
            .getattr("interpreter")
            .unwrap()
            .eq("python3")
            .unwrap());
    });
}

#[test]
fn unregistered_command_is_dead_and_main_exits_one() {
    let case = Case::new();
    hook(case.root(), "rogue.py", GOOD, true);
    let settings = case.root().join(".claude/settings.json");
    fs::write(&settings, json!({"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "$CLAUDE_PROJECT_DIR/.claude/hooks/rogue.py", "timeout": 3}]}]}}).to_string()).unwrap();
    Python::attach(|py| {
        let (stdout, _guard) = capture(py, "stdout");
        let argv = PyList::new(py, ["--project-dir", case.root().to_str().unwrap()]).unwrap();
        let code = doctor(py).getattr("main").unwrap().call1((argv,)).unwrap();
        assert!(code.eq(1).unwrap());
        let output = buffer_text(&stdout);
        assert!(output.contains("DEAD"));
        assert!(output.contains("resolves to no registered hook"));
        assert!(output
            .trim_end()
            .ends_with("hook-doctor | FAIL dead=1 warn=0 total=1"));
    });
}

#[test]
fn main_without_settings_fails() {
    let case = Case::new();
    Python::attach(|py| {
        let (stderr, _guard) = capture(py, "stderr");
        let argv = PyList::new(py, ["--project-dir", case.root().to_str().unwrap()]).unwrap();
        let code = doctor(py).getattr("main").unwrap().call1((argv,)).unwrap();
        assert!(code.eq(1).unwrap());
        assert!(buffer_text(&stderr).contains("no settings file"));
    });
}

#[test]
fn session_events_get_a_synthetic_payload() {
    for event in ["SessionStart", "SessionEnd"] {
        let case = Case::new();
        let text = format!("#!/bin/bash\ncat >/dev/null; echo '{{\"hookSpecificOutput\":{{\"hookEventName\":\"{event}\"}}}}'\n");
        hook(case.root(), "s.sh", &text, true);
        Python::attach(|py| {
            let item = declared(py, "s.sh", event, "", 3);
            status(&run_report(py, case.root(), &item), "OK");
        });
    }
}
