#[allow(dead_code)]
mod support;
use serde_json::Value;
use std::fs;
use std::process::Output;

fn call(host: &support::Host, arguments: &[&str]) -> Output {
    host.command()
        .args(["analyze", "--root", host.0.to_str().unwrap()])
        .args(arguments)
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
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn mixed_native_scans_report_hashes_checks_and_deterministic_findings() {
    let host = support::Host::new();
    fs::write(
        host.0.join("a.py"),
        "try:\n    risky()\nexcept Exception:\n    pass\n",
    )
    .unwrap();
    fs::write(
        host.0.join("b.rs"),
        "fn f() { let x = Some(1); x.unwrap(); }\n",
    )
    .unwrap();
    let first = call(&host, &["scan", "b.rs", "a.py", "a.py"]);
    let value = decoded(&first, 1);
    assert_eq!(value["sources"].as_array().unwrap().len(), 2);
    assert_eq!(value["sources"][0]["path"], "a.py");
    assert_eq!(
        value["sources"][0]["source_sha256"].as_str().unwrap().len(),
        64
    );
    assert!(value["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["rule"] == "failure/silent-fallback"));
    assert!(value["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["rule"] == "failure/rust-unwrap"));
    assert_eq!(first.stdout, call(&host, &["scan", "a.py", "b.rs"]).stdout);
}

#[test]
fn clean_files_exit_zero_and_truncation_never_hides_the_total() {
    let host = support::Host::new();
    fs::write(host.0.join("clean.rs"), "fn f() -> i32 { 42 }\n").unwrap();
    assert_eq!(
        decoded(&call(&host, &["scan", "clean.rs"]), 0)["finding_count"],
        0
    );
    fs::write(
        host.0.join("bad.rs"),
        "fn f() { Some(1).unwrap(); None::<i32>.unwrap(); }\n",
    )
    .unwrap();
    let value = decoded(&call(&host, &["scan", "bad.rs", "--max-results", "1"]), 1);
    assert_eq!(value["finding_count"], 2);
    assert_eq!(value["findings"].as_array().unwrap().len(), 1);
    assert_eq!(value["truncated"], true);
}

#[test]
fn syntax_missing_files_and_inapplicable_checks_fail_without_partial_json() {
    let host = support::Host::new();
    fs::write(host.0.join("a.rs"), "fn f() { Some(1).unwrap(); }").unwrap();
    fs::write(host.0.join("z.py"), "def broken(:\n").unwrap();
    for args in [
        vec!["scan", "a.rs", "z.py"],
        vec!["scan", "none.py"],
        vec!["scan", "a.rs", "--checks", "fallback"],
        vec!["scan", "a.rs", "--checks", "made-up"],
    ] {
        let output = call(&host, &args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn root_capability_rejects_escapes_symlinks_and_nonregular_sources() {
    use std::os::unix::fs::symlink;
    let host = support::Host::new();
    let outside = support::Host::new();
    fs::write(outside.0.join("outside.py"), "x=1").unwrap();
    symlink(outside.0.join("outside.py"), host.0.join("link.py")).unwrap();
    fs::create_dir(host.0.join("dir.py")).unwrap();
    for path in [
        "../outside.py",
        "link.py",
        "dir.py",
        outside.0.join("outside.py").to_str().unwrap(),
    ] {
        let output = call(&host, &["scan", path]);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn source_and_output_budgets_fail_explicitly() {
    let host = support::Host::new();
    fs::write(host.0.join("huge.py"), "#".repeat(1024 * 1024 + 1)).unwrap();
    fs::write(
        host.0.join("a.rs"),
        "fn f() { Some(1).unwrap(); Some(2).unwrap(); }",
    )
    .unwrap();
    for args in [
        vec!["scan", "huge.py"],
        vec!["scan", "a.rs", "--max-results", "0"],
        vec!["scan", "a.rs", "--max-bytes", "512"],
        vec!["scan", "a.rs", "--max-bytes", "4194305"],
    ] {
        let output = call(&host, &args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn hot_loop_checks_require_explicit_applicable_paths() {
    let host = support::Host::new();
    fs::create_dir(host.0.join("models")).unwrap();
    fs::write(
        host.0.join("models/a.py"),
        "def f(x: Tensor):\n    for i in range(len(x)):\n        print(x[i].item())\n",
    )
    .unwrap();
    assert_eq!(
        call(&host, &["scan", "models/a.py", "--checks", "hot-loop"])
            .status
            .code(),
        Some(2)
    );
    let value = decoded(
        &call(
            &host,
            &[
                "scan",
                "models/a.py",
                "--checks",
                "hot-loop",
                "--hot-path",
                "models",
            ],
        ),
        1,
    );
    assert_eq!(value["findings"][0]["rule"], "perf/python-tensor-loop");
    assert_eq!(
        call(&host, &["scan", "models/a.py", "--hot-path", "../models"])
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn native_registry_matches_the_available_check_families() {
    let host = support::Host::new();
    let value = decoded(&call(&host, &["rules"]), 0);
    assert_eq!(value["checks"].as_object().unwrap().len(), 6);
    assert!(value["ablations"]["default"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "drop_clamp"));
    assert!(value["ablations"]["optional"].as_array().unwrap().len() > 1);
}

#[test]
fn ablations_are_hash_bound_read_only_and_reparsable() {
    let host = support::Host::new();
    let source =
        "def f(x):\n    return x.clamp(min=1e-6)\n\ndef g(y):\n    return y.clamp(min=1e-7)\n";
    let path = host.0.join("a.py");
    fs::write(&path, source).unwrap();
    let value = decoded(
        &call(
            &host,
            &[
                "ablations",
                "a.py",
                "--rule",
                "drop_clamp",
                "--qualname",
                "f",
            ],
        ),
        0,
    );
    assert_eq!(value["executed"], false);
    assert_eq!(value["candidate_count"], 1);
    assert_eq!(value["ablations"][0]["qualname"], "f");
    let edit = &value["ablations"][0]["edits"][0];
    let mut changed = source.to_owned();
    changed.replace_range(
        edit["start"].as_u64().unwrap() as usize..edit["end"].as_u64().unwrap() as usize,
        edit["replacement"].as_str().unwrap(),
    );
    assert!(changed.contains("return x\n"));
    assert!(!slop_core::engine::parse(&changed)
        .unwrap()
        .root_node()
        .has_error());
    assert_eq!(fs::read_to_string(path).unwrap(), source);
    let hash = value["source_sha256"].as_str().unwrap();
    decoded(
        &call(
            &host,
            &[
                "ablations",
                "a.py",
                "--rule",
                "drop_clamp",
                "--expected-hash",
                hash,
            ],
        ),
        0,
    );
    fs::write(host.0.join("a.py"), "x=2\n").unwrap();
    assert_eq!(
        call(&host, &["ablations", "a.py", "--expected-hash", hash])
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn unknown_rules_invalid_source_and_proposal_limits_are_explicit() {
    let host = support::Host::new();
    fs::write(
        host.0.join("a.py"),
        "def f(x):\n    return x.clamp(min=0)\ndef g(x):\n    return x.clamp(min=0)\n",
    )
    .unwrap();
    for args in [
        vec!["ablations", "a.py", "--rule", "unknown"],
        vec!["ablations", "a.py", "--extra", "drop_clamp"],
        vec!["ablations", "a.py", "--max-results", "4097"],
    ] {
        assert_eq!(call(&host, &args).status.code(), Some(2));
    }
    let value = decoded(
        &call(
            &host,
            &[
                "ablations",
                "a.py",
                "--rule",
                "drop_clamp",
                "--max-results",
                "1",
            ],
        ),
        0,
    );
    assert_eq!(value["candidate_count"], 2);
    assert_eq!(value["truncated"], true);
    fs::write(host.0.join("a.py"), "def f(:").unwrap();
    assert_eq!(call(&host, &["ablations", "a.py"]).status.code(), Some(2));
}
