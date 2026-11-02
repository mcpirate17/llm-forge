#[allow(dead_code)]
mod support;
use serde_json::Value;
use std::fs;
use std::process::Output;

fn call(host: &support::Host, check: &str, files: &[&str]) -> Output {
    host.command()
        .args([
            "analyze",
            "--root",
            host.0.to_str().unwrap(),
            "paths",
            "--check",
            check,
            "--",
        ])
        .args(files)
        .env_remove("CONDUCTOR_NOTES_ROOT")
        .env("PATH", "")
        .env("PYO3_PYTHON", "/forbidden-python")
        .env("CONDUCTOR_PYTHON", "/forbidden-python")
        .output()
        .unwrap()
}

fn decoded(output: &Output, code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn root_patterns_blessed_names_case_and_subdirectories_match_legacy() {
    let host = support::Host::new();
    let bad = [
        "BIG_PLAN.md",
        "HANDOFF.md",
        "run.log",
        "metrics.jsonl",
        "unused.py",
    ];
    let value = decoded(&call(&host, "root-junk", &bad), 1);
    let found: Vec<_> = value["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["path"].as_str().unwrap())
        .collect();
    assert_eq!(found, bad);
    assert!(value["findings"][0]["message"]
        .as_str()
        .unwrap()
        .contains("move to tasks/"));
    let good = [
        "tasks/BIG_PLAN.md",
        "research/reports/run.log",
        "a/b/metrics.jsonl",
        "COMMANDS_DO_NOT_DELETE.txt",
        "MY_PLAN_DO_NOT_DELETE.md",
        "pyproject.toml",
        "CLAUDE.md",
        "Makefile",
        "my_plan.md",
    ];
    assert_eq!(
        decoded(&call(&host, "root-junk", &good), 0)["finding_count"],
        0
    );
    assert_eq!(
        decoded(&call(&host, "root-junk", &[]), 0)["finding_count"],
        0
    );
}

#[test]
fn lexical_paths_do_not_require_files_and_preserve_posix_edge_cases() {
    let host = support::Host::new();
    let paths = [
        "./PLAN.md",
        ".//PLAN.md/",
        "PLAN.md",
        "PLAN.md",
        "unused.",
        ".log",
        "/PLAN.md",
        "//PLAN.md",
        "../PLAN.md",
        "a/../PLAN.md",
        "a\\PLAN.md",
    ];
    let value = decoded(&call(&host, "root-junk", &paths), 1);
    assert_eq!(value["findings"].as_array().unwrap().len(), 7);
    assert_eq!(value["findings"][6]["path"], "a\\PLAN.md");
    assert!(!host.0.join("PLAN.md").exists());
}

#[test]
fn notes_suffixes_top_level_only_and_hidden_suffix_edge_cases_match_legacy() {
    let host = support::Host::new();
    let paths = [
        "research/notes/a.json",
        "research/notes/b.jsonl",
        "research/notes/c.csv",
        "./research//notes/./d.json/",
        "research/notes/.hidden.json",
        "research/notes/kb.md",
        "research/notes/archive/old.csv",
        "research/data/input.json",
        "research/notes_extra/x.json",
        "notes/x.json",
        "research/notes/.json",
        "research/notes/a.JSON",
        "research/notes/a.json.",
        "/research/notes/a.json",
    ];
    let value = decoded(&call(&host, "json-in-notes", &paths), 1);
    assert_eq!(value["finding_count"], 5);
    assert!(value["findings"][0]["message"]
        .as_str()
        .unwrap()
        .contains("research/data/"));
    assert_eq!(
        decoded(&call(&host, "json-in-notes", &[]), 0)["finding_count"],
        0
    );
}

#[test]
fn notes_configuration_uses_native_manifest_precedence_and_environment_override() {
    let host = support::Host::new();
    fs::write(
        host.0.join("pyproject.toml"),
        "[tool.conductor]\nnotes_root='kb/cards'\n",
    )
    .unwrap();
    assert_eq!(
        decoded(
            &call(
                &host,
                "json-in-notes",
                &["kb/cards/x.csv", "research/notes/x.csv"]
            ),
            1
        )["finding_count"],
        1
    );
    fs::write(
        host.0.join("forge.toml"),
        "schema_version=1\n[conductor]\nnotes_root='knowledge'\n",
    )
    .unwrap();
    let value = decoded(
        &call(
            &host,
            "json-in-notes",
            &["kb/cards/x.csv", "knowledge/x.json"],
        ),
        1,
    );
    assert_eq!(value["findings"][0]["path"], "knowledge/x.json");
    let output = host
        .command()
        .args([
            "analyze",
            "paths",
            "--check",
            "json-in-notes",
            "--",
            "chosen/x.csv",
        ])
        .env("CONDUCTOR_NOTES_ROOT", "chosen")
        .env("PATH", "")
        .output()
        .unwrap();
    assert_eq!(decoded(&output, 1)["findings"][0]["path"], "chosen/x.csv");
}

#[test]
fn malformed_configuration_limits_and_unknown_checks_fail_loud_without_json() {
    let host = support::Host::new();
    fs::write(host.0.join("forge.toml"), "not toml [").unwrap();
    for output in [
        call(&host, "json-in-notes", &[]),
        call(&host, "unknown", &[]),
        call(&host, "root-junk", &[&"a".repeat(4097)]),
    ] {
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    assert_eq!(
        decoded(&call(&host, "root-junk", &["PLAN.md"]), 1)["finding_count"],
        1
    );
    let many: Vec<_> = std::iter::repeat_n("a", 4097).collect();
    assert_eq!(call(&host, "root-junk", &many).status.code(), Some(2));
    let long = "a".repeat(1024);
    let large: Vec<_> = std::iter::repeat_n(long.as_str(), 1025).collect();
    let output = call(&host, "root-junk", &large);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}

#[test]
fn version_and_rules_are_native_and_output_budget_never_silently_truncates() {
    let host = support::Host::new();
    fs::write(host.0.join("forge.toml"), "not toml [").unwrap();
    let output = host
        .command()
        .args([
            "analyze",
            "paths",
            "--check",
            "json-in-notes",
            "--tool-version",
        ])
        .env("PATH", "")
        .env("PYO3_PYTHON", "/forbidden-python")
        .output()
        .unwrap();
    assert!(decoded(&output, 0)["tool_version"]
        .as_str()
        .unwrap()
        .starts_with("forge-native-path-policy 1 "));
    let rules = host.ok(&["analyze", "rules"]);
    assert_eq!(
        rules["path_checks"],
        serde_json::json!(["root-junk", "json-in-notes"])
    );
    let paths: Vec<_> = std::iter::repeat_n("PLAN.md", 20).collect();
    let output = host
        .command()
        .args([
            "analyze",
            "--max-bytes",
            "512",
            "paths",
            "--check",
            "root-junk",
            "--",
        ])
        .args(paths)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}
