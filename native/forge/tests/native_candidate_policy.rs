#![cfg(target_os = "linux")]
#[path = "support/candidate_policy_fixture.rs"]
#[allow(dead_code)]
mod candidate_policy_fixture;
use candidate_policy_fixture::{policy, Fixture};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::process::{Command, Output};

fn value(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn plan_uses_staged_policy_and_objects_preserves_order_and_never_runs_checks() {
    let fixture = Fixture::new();
    let original = fixture.configured();
    std::fs::write(
        fixture.root.join("repo/policies/main.toml"),
        "invalid working policy",
    )
    .unwrap();
    std::fs::write(
        fixture.root.join("repo/baseline.txt"),
        "unreviewed dirty baseline",
    )
    .unwrap();
    let plan = value(fixture.command().output().unwrap());
    assert_eq!(plan["plan_only"], true);
    assert_eq!(plan["policy"]["path"], "policies/main.toml");
    assert_eq!(
        plan["policy"]["sha256"],
        format!("{:x}", Sha256::digest(original.as_bytes()))
    );
    assert_eq!(plan["checks"][0]["check"]["check_id"], "zeta");
    assert_eq!(plan["checks"][1]["check"]["check_id"], "alpha");
    assert_eq!(
        plan["checks"][0]["files"],
        serde_json::json!(["src/security/test_unit.rs"])
    );
    assert_eq!(plan["checks"][0]["scheduled"], true);
    assert_eq!(plan["checks"][1]["scheduled"], false);
    let change = plan["candidate"]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|change| change["path"] == "src/security/test_unit.rs")
        .unwrap();
    assert_eq!(change["risk"], "high");
    for class in ["native", "rust", "source", "test"] {
        assert!(change["classes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == class));
    }
    assert_eq!(
        plan["baselines"][0]["sha256"],
        format!("{:x}", Sha256::digest(b"baseline bytes\n"))
    );
    fixture.clean_scratch();
}

#[test]
fn profiles_and_policy_overrides_keep_their_exact_precedence() {
    let fixture = Fixture::new();
    fixture.configured();
    fixture.stage("alternate.toml", &policy("alternative"));
    let full = value(
        fixture
            .command()
            .args(["--profile", "full"])
            .output()
            .unwrap(),
    );
    assert_eq!(full["checks"].as_array().unwrap().len(), 2);
    assert_eq!(full["checks"][1]["check"]["check_id"], "full_only");
    let explicit = value(
        fixture
            .command()
            .args(["--policy", "alternate.toml"])
            .env("CONDUCTOR_POLICY", "missing.toml")
            .output()
            .unwrap(),
    );
    assert_eq!(explicit["checks"][0]["check"]["check_id"], "alternative");
    let env = value(
        fixture
            .command()
            .env("CONDUCTOR_POLICY", "alternate.toml")
            .output()
            .unwrap(),
    );
    assert_eq!(env["policy"]["path"], "alternate.toml");
    fixture.stage("forge.toml", "invalid staged config [[");
    let configured_env = value(
        fixture
            .command()
            .env("CONDUCTOR_CANDIDATE_POLICY", "alternate.toml")
            .output()
            .unwrap(),
    );
    assert_eq!(configured_env["policy"]["path"], "alternate.toml");
    fixture.clean_scratch();
}

#[test]
fn missing_candidate_policy_never_falls_back_to_working_tree() {
    let fixture = Fixture::new();
    fixture.stage(
        "forge.toml",
        "schema_version=1\n[conductor]\ncandidate_policy='unstaged.toml'\n",
    );
    std::fs::write(
        fixture.root.join("repo/unstaged.toml"),
        policy("working_only"),
    )
    .unwrap();
    let output = fixture.command().output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("absent"));
    fixture.clean_scratch();
}

#[test]
fn required_baseline_absence_and_escape_are_errors_with_cleanup() {
    let fixture = Fixture::new();
    fixture.configured();
    for path in ["missing.txt", "../escape.txt", "/etc/passwd"] {
        fixture.stage(
            "policies/main.toml",
            &policy("zeta").replace("path='baseline.txt'", &format!("path='{path}'")),
        );
        assert_eq!(fixture.command().output().unwrap().status.code(), Some(1));
        fixture.clean_scratch();
    }
}

#[test]
fn internal_policy_and_baseline_symlinks_are_confined_and_supported() {
    let fixture = Fixture::new();
    fixture.configured();
    std::os::unix::fs::symlink("policies/main.toml", fixture.root.join("repo/alias.toml")).unwrap();
    fixture.add("alias.toml");
    fixture.stage("data/base.txt", "linked baseline\n");
    std::fs::remove_file(fixture.root.join("repo/baseline.txt")).unwrap();
    std::os::unix::fs::symlink("./data/base.txt", fixture.root.join("repo/baseline.txt")).unwrap();
    fixture.add("baseline.txt");
    let plan = value(
        fixture
            .command()
            .args(["--policy", "alias.toml"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        plan["baselines"][0]["sha256"],
        format!("{:x}", Sha256::digest(b"linked baseline\n"))
    );
    fixture.clean_scratch();
    std::fs::remove_file(fixture.root.join("repo/alias.toml")).unwrap();
    std::os::unix::fs::symlink("../outside.toml", fixture.root.join("repo/alias.toml")).unwrap();
    fixture.add("alias.toml");
    assert_eq!(
        fixture
            .command()
            .args(["--policy", "alias.toml"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(1)
    );
    fixture.clean_scratch();
}

#[test]
fn date_refusal_and_parallel_plans_leave_no_private_snapshots() {
    let fixture = Fixture::new();
    fixture.configured();
    let mut invalid = Command::new(env!("CARGO_BIN_EXE_forge"));
    let output = invalid
        .args([
            "candidate",
            "plan",
            "--kind",
            "index",
            "--today",
            "2030-02-30",
            "--scratch-parent",
        ])
        .arg(fixture.root.join("scratch"))
        .current_dir(fixture.root.join("repo"))
        .env("PATH", "")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    fixture.clean_scratch();
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let mut command = fixture.command();
            std::thread::spawn(move || command.output().unwrap())
        })
        .collect();
    for worker in workers {
        assert_eq!(value(worker.join().unwrap())["plan_only"], true);
    }
    fixture.clean_scratch();
}

#[test]
fn invalid_environment_path_cannot_select_a_fallback_policy() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    fixture.configured();
    let invalid = std::ffi::OsString::from_vec(b"bad\xff".to_vec());
    let refused = fixture
        .command()
        .env("CONDUCTOR_CANDIDATE_POLICY", &invalid)
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&refused.stderr)
        .contains("CONDUCTOR_CANDIDATE_POLICY must be UTF-8"));
    fixture.clean_scratch();
    let explicit = value(
        fixture
            .command()
            .args(["--policy", "policies/main.toml"])
            .env("CONDUCTOR_CANDIDATE_POLICY", &invalid)
            .env("CONDUCTOR_POLICY", &invalid)
            .output()
            .unwrap(),
    );
    assert_eq!(explicit["policy"]["path"], "policies/main.toml");
    let higher_env = value(
        fixture
            .command()
            .env("CONDUCTOR_POLICY", "policies/main.toml")
            .env("CONDUCTOR_CANDIDATE_POLICY", &invalid)
            .output()
            .unwrap(),
    );
    assert_eq!(higher_env["policy"]["path"], "policies/main.toml");
    fixture.clean_scratch();
}
