//! Opt-in multi-module controls using the existing exact guest/Stage owners.
use super::*;

const SECOND: &str = "src/weak_target.py";
const EXPECTED: [&str; 2] = ["src/probe_target.py", SECOND];
const WEAK_TEST: &str = "\nfrom src.weak_target import clamp as weak_clamp\n\ndef test_weak_clamp():\n    assert weak_clamp(torch.tensor([2.0])).item() == 2.0\n";

fn multi_harness(ready: bool) -> Harness {
    let driver = format!(
        "{}{WEAK_TEST}",
        if ready {
            LIVE_READY_DRIVER
        } else {
            LIVE_DRIVER
        }
    );
    let h = live_harness(&driver, 90.0);
    h.fixture
        .stage(SECOND, "raise RuntimeError('staged second-module decoy')\n");
    private_file(&h.repo.join(SECOND), SUBJECT.as_bytes());
    h
}

// Preserve all existing Harness argv/environment, replacing only module selection.
fn selected_command(h: &Harness, selection: &[&str]) -> Command {
    let original = h.live_command(120);
    let mut command = Command::new(original.get_program());
    let mut args = original.get_args();
    while let Some(arg) = args.next() {
        if arg == "--module" {
            args.next().expect("Harness module argument");
        } else {
            command.arg(arg);
        }
    }
    command.args(selection);
    for (name, value) in original.get_envs() {
        if let Some(value) = value {
            command.env(name, value);
        } else {
            command.env_remove(name);
        }
    }
    command
}

fn host_identity(h: &Harness) -> (Vec<u8>, String) {
    let repository = git2::Repository::open(&h.repo).unwrap();
    let head = repository
        .head()
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .id()
        .to_string();
    (fs::read(h.repo.join(".git/index")).unwrap(), head)
}

fn assert_host_identity(h: &Harness, expected: &(Vec<u8>, String)) {
    assert_eq!(host_identity(h), *expected);
    assert_eq!(
        fs::read(h.repo.join("src/probe_target.py")).unwrap(),
        SUBJECT.as_bytes()
    );
    assert_eq!(fs::read(h.repo.join(SECOND)).unwrap(), SUBJECT.as_bytes());
}

fn assert_multi_result(h: &Harness, value: &Value, index: &[u8]) {
    assert_eq!(value["modules"], json!(EXPECTED));
    assert!(value["module"].is_null());
    assert_eq!(value["selection"]["module_count"], 2);
    assert_eq!(value["inputs"]["scope"], "whole_modules");
    assert_eq!(
        value["inputs"]["changed"],
        json!([[EXPECTED[0], null], [SECOND, null]])
    );
    assert_eq!(value["summary"]["modules_probed"], 2);
    assert!(value["summary"]["incomplete"]
        .as_array()
        .unwrap()
        .is_empty());
    let blocking = value["summary"]["blocking"].as_array().unwrap();
    assert!(blocking.iter().any(|row| row["module"] == SECOND
        && row["qualname"] == "clamp"
        && row["rule"] == "drop_clamp_min"
        && row["verdict"] == "REACHABLE_BUT_UNTESTED"));
    assert!(blocking.iter().all(|row| row["module"] != EXPECTED[0]));
    assert!(value["summary"]["live"].as_u64().unwrap() > 0);
    assert_eq!(value["live_capture_cleanup_complete"], true);
    assert_eq!(
        value["live_inputs"]["host_index_sha256"],
        format!("{:x}", Sha256::digest(index))
    );
    assert_eq!(value["live_inputs"]["snapshot_tree"], value["tree_oid"]);
    assert_eq!(value["execution_read_scope"]["policy"], "operator_grants");
    // One Stage result/resource owner carries the complete two-module plan.
    assert!(value["stage"].is_object());
    assert!(value["execution"].is_object());
    assert_clean_result(h, value);
    let artifact = Path::new(value["published_artifact"].as_str().unwrap());
    assert_eq!(artifact.parent(), Some(h.artifacts.as_path()));
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(artifact).unwrap()).unwrap(),
        value["summary"]
    );
}

fn run_multi(h: &Harness, selection: &[&str]) -> Value {
    let identity = host_identity(h);
    let label = selection.first().unwrap().trim_start_matches("--");
    let stdout = h.fixture.root.join(format!("stdout-{label}.json"));
    let stderr = h.fixture.root.join(format!("stderr-{label}.log"));
    assert!(
        !stdout.exists() && !stderr.exists(),
        "refuse observer log reuse"
    );
    let mut child = OwnedChild::spawn_command(h, selected_command(h, selection));
    assert_eq!(child.wait(150).code(), Some(1), "{}", child.diagnostic());
    let value: Value = serde_json::from_slice(&fs::read(&child.stdout).unwrap()).unwrap();
    fs::copy(&child.stdout, stdout).unwrap();
    fs::copy(&child.stderr, stderr).unwrap();
    assert_multi_result(h, &value, &identity.0);
    assert_host_identity(h, &identity);
    value
}

#[test]
#[ignore = "requires existing real guest pins and fresh owned delegated unit; sequential control"]
fn repeated_modules_execute_one_live_stage_with_strong_and_weak_oracles() {
    let h = multi_harness(false);
    let value = run_multi(&h, &["--module", EXPECTED[0], "--module", SECOND]);
    assert_eq!(value["selection"]["mode"], "explicit");
    assert_eq!(fs::read_dir(&h.artifacts).unwrap().count(), 1);
}

#[test]
#[ignore = "requires existing real guest pins and fresh owned delegated unit; sequential control"]
fn automatic_changed_and_all_tracked_match_live_modules_without_mutating_head_or_index() {
    let h = multi_harness(false);
    let changed = run_multi(&h, &["--changed-from", &h.commit]);
    let all = run_multi(&h, &["--all-tracked"]);
    assert_eq!(changed["selection"]["binding"]["mode"], "changed_from");
    assert_eq!(all["selection"]["binding"]["mode"], "all_tracked");
    assert_eq!(changed["modules"], all["modules"]);
    assert_eq!(changed["tree_oid"], all["tree_oid"]);
    assert_eq!(changed["inputs"], all["inputs"]);
    assert_eq!(changed["summary"], all["summary"]);
    assert_ne!(
        changed["execution"]["resources"]["path"],
        all["execution"]["resources"]["path"]
    );
    assert_eq!(fs::read_dir(&h.artifacts).unwrap().count(), 2);
}

fn move_base(h: &Harness, name: &str) -> String {
    let repository = git2::Repository::open(&h.repo).unwrap();
    let parent = repository
        .find_commit(git2::Oid::from_str(&h.commit).unwrap())
        .unwrap();
    let tree = parent.tree().unwrap();
    let signature = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
    repository
        .commit(
            Some(name),
            &signature,
            &signature,
            "operator moves comparison base",
            &tree,
            &[&parent],
        )
        .unwrap()
        .to_string()
}

#[test]
#[ignore = "requires existing real guest pins and fresh owned delegated unit; sequential control"]
fn automatic_selection_refuses_original_index_or_base_drift_after_ready_and_cleans_all_owners() {
    for base_drift in [false, true] {
        let h = multi_harness(true);
        let original = host_identity(&h);
        let name = "refs/heads/comparison-base";
        let repository = git2::Repository::open(&h.repo).unwrap();
        repository
            .reference(
                name,
                git2::Oid::from_str(&h.commit).unwrap(),
                false,
                "authored base",
            )
            .unwrap();
        let selection = if base_drift {
            vec!["--changed-from", name]
        } else {
            vec!["--all-tracked"]
        };
        let mut child = OwnedChild::spawn_command(&h, selected_command(&h, &selection));
        let (witness, group, roots) = live_ready(&h, &mut child);
        let expected = if base_drift {
            Some(move_base(&h, name))
        } else {
            // Stage a new non-Python fixture only after the guest is ready.
            h.fixture.stage("late-index.txt", "operator index drift\n");
            None
        };
        release_live(&h);
        assert!(!child.wait(150).success(), "{}", child.diagnostic());
        let error = fs::read_to_string(&child.stderr).unwrap();
        assert!(
            error.contains("live path selection changed")
                || error.contains("live host input selection changed"),
            "{error}"
        );
        assert_eq!(fs::read_dir(&h.artifacts).unwrap().count(), 0);
        assert_removed(&group);
        for root in roots {
            assert_removed(&root);
        }
        h.assert_no_scratch();
        assert_gone_or_reused(
            witness["pid"].as_u64().unwrap() as u32,
            witness["start"].as_str().unwrap(),
        );
        assert_eq!(host_identity(&h).1, original.1);
        if let Some(expected) = expected {
            assert_eq!(host_identity(&h).0, original.0);
            assert_eq!(
                repository
                    .find_reference(name)
                    .unwrap()
                    .target()
                    .unwrap()
                    .to_string(),
                expected
            );
        } else {
            assert_ne!(host_identity(&h).0, original.0);
            assert_eq!(
                fs::read(h.repo.join("late-index.txt")).unwrap(),
                b"operator index drift\n"
            );
        }
    }
}
