#![cfg(feature = "python-compat-tests")]
//! Merge and source template contracts for `conductor init`.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/project_init_support.rs"]
#[allow(dead_code)]
mod init_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::py_json;
use init_support::{config, events, init, json_dumps, json_loads, repo, settings_hooks};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PySet, PyString};
use serde_json::json;
use std::fs;
use support::{assert_error, module, path, text, Case};

fn settings(py: Python<'_>, existing: Option<&str>, force: bool) -> String {
    init(py)
        .getattr("render_settings")
        .unwrap()
        .call1((existing, force))
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn settings_merge_keeps_non_hook_keys_and_foreign_events() {
    let _case = Case::new();
    Python::attach(|py| {
        let existing = py_json(
            py,
            json!({"permissions":{"allow":["Bash(ls:*)"]},"env":{"FOO":"1"},
                "hooks":{"Notification":[{"hooks":[{"type":"command","command":"x"}]}]}}),
        );
        let merged = json_loads(
            py,
            PyString::new(py, &settings(py, Some(&json_dumps(&existing)), false)).as_any(),
        );
        for key in ["permissions", "env"] {
            assert!(merged
                .get_item(key)
                .unwrap()
                .eq(existing.get_item(key).unwrap())
                .unwrap());
        }
        assert!(merged
            .get_item("hooks")
            .unwrap()
            .get_item("Notification")
            .unwrap()
            .eq(existing
                .get_item("hooks")
                .unwrap()
                .get_item("Notification")
                .unwrap())
            .unwrap());
        let template = settings_hooks(py);
        for event in events(py) {
            assert!(merged
                .get_item("hooks")
                .unwrap()
                .get_item(&event)
                .unwrap()
                .eq(template.get_item(&event).unwrap())
                .unwrap());
        }
        // Use the production registry's dynamic template, not a hard-coded copy.
        let payload = PyDict::new(py);
        payload.set_item("hooks", template).unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("indent", 2).unwrap();
        let expected: String = module(py, "json")
            .getattr("dumps")
            .unwrap()
            .call((payload,), Some(&kwargs))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(settings(py, None, false), format!("{expected}\n"));
    });
}

#[test]
fn settings_conflict_refused_without_force_replaced_with_force() {
    let _case = Case::new();
    Python::attach(|py| {
        let existing = py_json(
            py,
            json!({"model":"opus","hooks":{"PreToolUse":[
                {"matcher":"Bash","hooks":[{"type":"command","command":"legacy.sh"}]}
            ]}}),
        );
        let serialized = json_dumps(&existing);
        let pi = init(py);
        let error = pi
            .getattr("render_settings")
            .unwrap()
            .call1((&serialized, false))
            .unwrap_err();
        assert_error(
            py,
            error,
            &pi.getattr("InitError").unwrap(),
            "hooks.PreToolUse",
        );
        let forced = settings(py, Some(&serialized), true);
        let merged = json_loads(py, PyString::new(py, &forced).as_any());
        assert!(merged.get_item("model").unwrap().eq("opus").unwrap());
        assert!(merged
            .get_item("hooks")
            .unwrap()
            .eq(settings_hooks(py))
            .unwrap());
    });
}

#[test]
fn settings_identical_event_is_not_a_conflict() {
    let _case = Case::new();
    Python::attach(|py| {
        let template = settings_hooks(py);
        let hooks = PyDict::new(py);
        hooks
            .set_item("PreToolUse", template.get_item("PreToolUse").unwrap())
            .unwrap();
        let existing = PyDict::new(py);
        existing.set_item("hooks", hooks).unwrap();
        existing.set_item("k", 1).unwrap();
        let merged = json_loads(
            py,
            PyString::new(
                py,
                &settings(py, Some(&json_dumps(existing.as_any())), false),
            )
            .as_any(),
        );
        let expected = PyDict::new(py);
        expected.set_item("hooks", template).unwrap();
        expected.set_item("k", 1).unwrap();
        assert!(merged.eq(expected).unwrap());
    });
}

#[test]
fn settings_malformed_json_refused() {
    let _case = Case::new();
    Python::attach(|py| {
        let pi = init(py);
        for (source, message) in [("{not json", "not JSON"), ("[]", "JSON object")] {
            let error = pi
                .getattr("render_settings")
                .unwrap()
                .call1((source, true))
                .unwrap_err();
            assert_error(py, error, &pi.getattr("InitError").unwrap(), message);
        }
    });
}

#[test]
fn an_already_wired_settings_file_is_returned_byte_for_byte() {
    let _case = Case::new();
    Python::attach(|py| {
        let existing = format!(
            "{{\n\t\"model\": \"opus\",\n\t\"hooks\": {}\n}}\t\n\n",
            json_dumps(&settings_hooks(py))
        );
        for force in [false, true] {
            assert_eq!(settings(py, Some(&existing), force), existing);
        }
    });
}

#[test]
fn wiring_in_a_hook_does_not_escape_the_rest_of_the_file() {
    let _case = Case::new();
    Python::attach(|py| {
        let existing = "{\"env\": {\"GREETING\": \"café → ☕\"}}";
        let rendered = settings(py, Some(existing), false);
        assert!(rendered.contains("café → ☕"));
        let merged = json_loads(py, PyString::new(py, &rendered).as_any());
        assert!(merged
            .get_item("env")
            .unwrap()
            .eq(py_json(py, json!({"GREETING":"café → ☕"})))
            .unwrap());
    });
}

#[test]
fn a_partially_wired_settings_file_is_still_rewritten() {
    let _case = Case::new();
    Python::attach(|py| {
        let hooks = PyDict::new(py);
        hooks
            .set_item(
                "PreToolUse",
                settings_hooks(py).get_item("PreToolUse").unwrap(),
            )
            .unwrap();
        let input = PyDict::new(py);
        input.set_item("hooks", hooks).unwrap();
        let existing = json_dumps(input.as_any());
        let rendered = settings(py, Some(&existing), false);
        assert_ne!(rendered, existing);
        let merged = json_loads(py, PyString::new(py, &rendered).as_any());
        assert!(merged
            .get_item("hooks")
            .unwrap()
            .eq(settings_hooks(py))
            .unwrap());
    });
}

#[test]
fn an_absent_settings_file_is_created_not_preserved() {
    let _case = Case::new();
    Python::attach(|py| {
        let rendered = settings(py, None, false);
        assert_ne!(rendered, "");
        let merged = json_loads(py, PyString::new(py, &rendered).as_any());
        let expected = PyDict::new(py);
        expected.set_item("hooks", settings_hooks(py)).unwrap();
        assert!(merged.eq(expected).unwrap());
    });
}

#[test]
fn mcp_merge_keeps_other_servers_and_refuses_conflict() {
    let case = Case::new();
    let project = repo(&case);
    Python::attach(|py| {
        let pi = init(py);
        let initial_config = config(py, &project, None, false, false, false);
        let existing = py_json(
            py,
            json!({"mcpServers":{"other":{"command":"x"},
            "code-review-graph":{"command":"stale"}},"extra":true}),
        );
        let serialized = json_dumps(&existing);
        let error = pi
            .getattr("render_mcp")
            .unwrap()
            .call1((&serialized, &initial_config))
            .unwrap_err();
        assert_error(
            py,
            error,
            &pi.getattr("InitError").unwrap(),
            "mcpServers.code-review-graph",
        );
        let forced = config(py, &project, None, true, false, false);
        let rendered = pi
            .getattr("render_mcp")
            .unwrap()
            .call1((&serialized, forced))
            .unwrap();
        let merged = json_loads(py, &rendered);
        assert!(merged
            .get_item("extra")
            .unwrap()
            .is(pyo3::types::PyBool::new(py, true)));
        let servers = merged.get_item("mcpServers").unwrap();
        assert!(servers
            .get_item("other")
            .unwrap()
            .eq(py_json(py, json!({"command":"x"})))
            .unwrap());
        let entry = servers.get_item("code-review-graph").unwrap();
        let expected_args = py_json(
            py,
            json!([
                "-m",
                "conductor.crg_server",
                "--repo",
                project.to_str().unwrap()
            ]),
        );
        assert!(entry.get_item("args").unwrap().eq(expected_args).unwrap());
        assert!(entry
            .get_item("command")
            .unwrap()
            .eq(init_support::python_executable(py).to_str().unwrap())
            .unwrap());
        assert!(entry
            .get_item("env")
            .unwrap()
            .eq(py_json(py, json!({"CRG_ROLE":"review"})))
            .unwrap());
    });
}

#[test]
fn an_already_wired_mcp_file_is_returned_byte_for_byte() {
    let case = Case::new();
    let project = repo(&case);
    Python::attach(|py| {
        let pi = init(py);
        let initial_config = config(py, &project, None, false, false, false);
        let entry = pi
            .getattr("mcp_entry")
            .unwrap()
            .call1((
                path(py, &project),
                initial_config.getattr("python").unwrap(),
            ))
            .unwrap();
        let existing = format!(
            "{{\"mcpServers\": {{\"code-review-graph\": {}}}}}",
            json_dumps(&entry)
        );
        for force in [false, true] {
            let config = config(py, &project, None, force, false, false);
            let rendered: String = pi
                .getattr("render_mcp")
                .unwrap()
                .call1((&existing, config))
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(rendered, existing);
        }
    });
}

#[test]
fn gitignore_block_appended_once_and_replaced_when_stale() {
    let _case = Case::new();
    Python::attach(|py| {
        let pi = init(py);
        let render = pi.getattr("render_gitignore").unwrap();
        let first: String = render.call1(("*.pyc\n",)).unwrap().extract().unwrap();
        let begin = text(&pi.getattr("MARK_BEGIN").unwrap());
        let end = text(&pi.getattr("MARK_END").unwrap());
        assert!(first.starts_with(&format!("*.pyc\n\n{begin}")));
        assert_eq!(first.matches(&begin).count(), 1);
        assert_eq!(
            render
                .call1((&first,))
                .unwrap()
                .extract::<String>()
                .unwrap(),
            first
        );
        let stale = format!("{}build/\n", first.replace(".mcp.json\n", ""));
        let rewritten: String = render.call1((stale,)).unwrap().extract().unwrap();
        assert_eq!(rewritten.matches(&begin).count(), 1);
        assert!(rewritten.contains(".mcp.json\n"));
        assert!(rewritten.ends_with(&format!("{end}\nbuild/\n")));
        let absent: String = render.call1((py.None(),)).unwrap().extract().unwrap();
        assert_eq!(absent.matches(&end).count(), 1);
    });
}

#[test]
fn policy_template_is_loadable() {
    let case = Case::new();
    let policy = case.root().join("candidate_policy.toml");
    Python::attach(|py| {
        let datetime = module(py, "datetime");
        let utc = datetime.getattr("UTC").unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("tz", utc).unwrap();
        let now = datetime
            .getattr("datetime")
            .unwrap()
            .getattr("now")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        let today = now.call_method0("date").unwrap();
        let rendered: String = init(py)
            .getattr("render_policy")
            .unwrap()
            .call1((today,))
            .unwrap()
            .extract()
            .unwrap();
        fs::write(&policy, rendered).unwrap();
        let loaded = module(py, "conductor.candidate_review.policy")
            .getattr("load_policy")
            .unwrap()
            .call1((path(py, &policy),))
            .unwrap();
        assert!(loaded.getattr("block_at").unwrap().eq("high").unwrap());
        let checks = loaded.getattr("checks").unwrap();
        let ids = checks
            .try_iter()
            .unwrap()
            .map(|check| text(&check.unwrap().getattr("check_id").unwrap()))
            .collect::<Vec<_>>();
        let actual = PySet::new(py, ids).unwrap();
        let expected =
            PySet::new(py, ["candidate-integrity", "config-parse", "python-ast"]).unwrap();
        assert!(actual.eq(expected).unwrap());
    });
}
