//! The shell tool's workdir and the claim gate's protected checkout are
//! independent, even when the hook itself was installed by that checkout.

#[path = "../src/civil.rs"]
mod civil;
#[path = "../src/crg_gate.rs"]
mod crg_gate;
#[path = "../src/crg_refresh.rs"]
mod crg_refresh;
#[path = "../src/current_work_guard.rs"]
mod current_work_guard;
#[path = "../src/identity.rs"]
mod identity;
#[path = "../src/instant.rs"]
mod instant;
#[path = "../src/interpreter.rs"]
mod interpreter;
#[path = "../src/json_canon.rs"]
#[allow(dead_code)]
mod json_canon;
#[path = "../src/local_ai_policy.rs"]
mod local_ai_policy;
#[path = "../src/ownership.rs"]
mod ownership;
#[path = "../src/write_targets.rs"]
mod write_targets;

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "forge-hook-workdir-{}-{}",
            std::process::id(),
            crate::instant::now()
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn verdict(payload: &Value, protected: &Path) -> Value {
    let common = crg_gate::checkout_of(protected).unwrap().1;
    crg_gate::verify_bash(
        payload,
        "test-owner",
        protected,
        Some(&common),
        &HashMap::new(),
    )
}

fn blocked(payload: &Value, protected: &Path) -> bool {
    verdict(payload, protected)["hookSpecificOutput"]["permissionDecision"] == "deny"
}

#[test]
fn tool_workdir_scopes_relative_writes_but_absolute_and_linked_writes_stay_protected() {
    let scratch = Scratch::new();
    let protected = scratch.0.join("LLM");
    let forge = scratch.0.join("llm-forge");
    let linked = scratch.0.join("LLM-linked");
    std::fs::create_dir_all(protected.join(".git/worktrees/linked")).unwrap();
    std::fs::create_dir_all(&forge).unwrap();
    std::fs::create_dir_all(&linked).unwrap();
    std::fs::write(protected.join(".git/worktrees/linked/commondir"), "../..\n").unwrap();
    std::fs::write(
        linked.join(".git"),
        format!(
            "gitdir: {}\n",
            protected.join(".git/worktrees/linked").display()
        ),
    )
    .unwrap();

    let command = "rm -- src/conductor/test_one.py";
    let base = json!({"session_id": "workdir-scope", "cwd": protected, "tool_input": {"command": command}});
    let common = crg_gate::checkout_of(&protected).unwrap().1;
    assert_eq!(
        crg_gate::session_checkout(&base, &protected, Some(&common)),
        protected
    );
    assert!(
        blocked(&base, &protected),
        "legacy missing-workdir fallback must remain gated"
    );
    let mut missing_cwd = base.clone();
    missing_cwd.as_object_mut().unwrap().remove("cwd");
    assert!(
        blocked(&missing_cwd, &protected),
        "missing metadata falls back to the protected root"
    );

    let mut forge_call = base.clone();
    forge_call["tool_input"]["workdir"] = json!(forge);
    assert_eq!(verdict(&forge_call, &protected), Value::Null);

    forge_call["tool_input"]["cwd"] = json!(protected);
    assert_eq!(verdict(&forge_call, &protected), Value::Null);

    forge_call["tool_input"]["workdir"] = Value::Null;
    forge_call["tool_input"]["cwd"] = json!(forge);
    assert_eq!(verdict(&forge_call, &protected), Value::Null);

    forge_call["tool_input"]["cwd"] = Value::Null;
    forge_call["tool_input"]["workdir"] = json!(forge);
    forge_call["tool_input"]["command"] =
        json!(format!("rm -- {}", forge.join("other.py").display()));
    assert_eq!(verdict(&forge_call, &protected), Value::Null);

    forge_call["tool_input"]["command"] = json!(format!(
        "rm -- {}",
        protected.join("protected.py").display()
    ));
    assert!(blocked(&forge_call, &protected));

    forge_call["tool_input"]["command"] = json!("rm -- ../LLM/protected.py");
    assert!(blocked(&forge_call, &protected));

    std::os::unix::fs::symlink(&protected, forge.join("protected-link")).unwrap();
    forge_call["tool_input"]["command"] = json!("rm -- protected-link/protected.py");
    assert!(blocked(&forge_call, &protected));

    forge_call["tool_input"]["command"] =
        json!(format!("rm -- {}", linked.join("protected.py").display()));
    assert!(blocked(&forge_call, &protected));

    forge_call["tool_input"]["command"] =
        json!(format!("cd {} && rm -- protected.py", protected.display()));
    assert!(blocked(&forge_call, &protected));

    forge_call["tool_input"]["command"] =
        json!(format!("cd {} && rm -- other.py", forge.display()));
    assert_eq!(verdict(&forge_call, &protected), Value::Null);
}

#[test]
fn legacy_native_bridge_exposes_the_same_workdir_boundary() {
    let scratch = Scratch::new();
    let protected = scratch.0.join("LLM");
    let forge = scratch.0.join("llm-forge");
    std::fs::create_dir_all(protected.join(".git")).unwrap();
    std::fs::create_dir_all(&forge).unwrap();

    let run = |command: &str| {
        let request = json!({
            "repo_root": protected,
            "owner": "test-owner",
            "payload": {
                "session_id": "legacy-workdir-scope",
                "cwd": protected,
                "tool_input": {"command": command, "workdir": forge}
            }
        });
        let mut child = Command::new(env!("CARGO_BIN_EXE_forge"))
            .args(["legacy-hook", "gate-verify-bash"])
            .env("CRG_GATE_STATE_DIR", scratch.0.join("state"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(request.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };

    assert_eq!(run("rm -- relative.py"), Value::Null);
    let protected_write = format!("rm -- {}", protected.join("protected.py").display());
    assert_eq!(
        run(&protected_write)["hookSpecificOutput"]["permissionDecision"],
        "deny"
    );
}

#[test]
fn ordered_shell_commands_keep_the_cwd_of_each_write() {
    let scratch = Scratch::new();
    let protected = scratch.0.join("LLM");
    let forge = scratch.0.join("llm-forge");
    std::fs::create_dir_all(protected.join(".git")).unwrap();
    std::fs::create_dir_all(&forge).unwrap();
    let payload = |cwd: &Path, command: String| json!({"session_id": "ordered-workdir-scope", "cwd": cwd, "tool_input": {"command": command}});

    assert!(blocked(
        &payload(&protected, "rm -- protected.py; cd /tmp".into()),
        &protected
    ));
    assert!(blocked(
        &payload(&forge, "rm -- ~/Projects/LLM/protected.py".into()),
        &protected
    ));
    assert!(blocked(
        &payload(&forge, "cd ~/Projects/LLM && rm -- protected.py".into()),
        &protected
    ));
    assert!(!blocked(
        &payload(&forge, "echo \"$(date)\"".into()),
        &protected
    ));
    assert!(!blocked(
        &payload(&forge, "git rev-parse \"$(pwd)\"".into()),
        &protected
    ));
    assert!(!blocked(
        &payload(&forge, format!("pushd {} && pwd", protected.display())),
        &protected
    ));
    assert!(blocked(
        &payload(
            &forge,
            format!("echo \"$(rm -- {}/protected.py)\"", protected.display())
        ),
        &protected
    ));
    assert!(!blocked(
        &payload(
            &protected,
            format!("cd {} && rm -- foreign.py", forge.display())
        ),
        &protected
    ));
    assert!(blocked(
        &payload(
            &forge,
            format!("bash -c 'cd {} && rm -- protected.py'", protected.display())
        ),
        &protected
    ));
    assert!(blocked(
        &payload(
            &forge,
            format!(
                "bash <<'EOF'\ncd {}\nrm -- protected.py\nEOF",
                protected.display()
            )
        ),
        &protected
    ));
}

#[test]
fn opaque_and_wrapped_cwd_forms_keep_protected_writes_gated() {
    let scratch = Scratch::new();
    let protected = scratch.0.join("LLM");
    let forge = scratch.0.join("llm-forge");
    std::fs::create_dir_all(protected.join(".git")).unwrap();
    std::fs::create_dir_all(&forge).unwrap();
    let payload = |cwd: &Path, command: String| json!({"session_id": "wrapped-workdir-scope", "cwd": cwd, "tool_input": {"command": command}});

    assert!(blocked(
        &payload(
            &forge,
            format!("pushd {} && rm -- protected.py", protected.display())
        ),
        &protected
    ));
    assert!(blocked(
        &payload(
            &forge,
            format!("env -C {} rm -- protected.py", protected.display())
        ),
        &protected
    ));
    assert!(blocked(
        &payload(
            &forge,
            format!(
                "CDPATH={} cd LLM && rm -- protected.py",
                scratch.0.display()
            )
        ),
        &protected
    ));
    assert!(blocked(
        &payload(
            &forge,
            format!(
                "env CDPATH={} bash -c 'cd LLM && rm -- protected.py'",
                scratch.0.display()
            )
        ),
        &protected
    ));
    assert!(blocked(
        &payload(
            &forge,
            format!("eval 'cd {}'; rm -- protected.py", protected.display())
        ),
        &protected
    ));
    assert!(blocked(
        &payload(
            &forge,
            format!("FLAG=1 rm -- {}/protected.py", protected.display())
        ),
        &protected
    ));
    assert!(blocked(
        &payload(
            &forge,
            format!("command rm -- {}/protected.py", protected.display())
        ),
        &protected
    ));
    assert!(blocked(
        &payload(
            &forge,
            format!("exec rm -- {}/protected.py", protected.display())
        ),
        &protected
    ));
    assert!(blocked(
        &payload(&forge, "bash uninspected-script.sh".into()),
        &protected
    ));
    assert!(blocked(
        &payload(
            &forge,
            format!(
                "python3 -c \"import os; os.chdir('{}'); open('protected.py','w').write('x')\"",
                protected.display()
            )
        ),
        &protected
    ));
    assert!(blocked(
        &payload(
            &forge,
            format!(
                "cd {} && rm -- same.py; cd {} && rm -- same.py",
                forge.display(),
                protected.display()
            )
        ),
        &protected
    ));
}

#[test]
fn symlink_entry_and_deep_destination_are_both_protected() {
    let scratch = Scratch::new();
    let protected = scratch.0.join("LLM");
    let forge = scratch.0.join("llm-forge");
    std::fs::create_dir_all(protected.join(".git")).unwrap();
    std::fs::create_dir_all(&forge).unwrap();
    std::fs::write(forge.join("outside.py"), "x").unwrap();
    std::os::unix::fs::symlink(forge.join("outside.py"), protected.join("outbound-link")).unwrap();
    std::os::unix::fs::symlink(&protected, forge.join("inbound-link")).unwrap();
    std::os::unix::fs::symlink(protected.join("missing.py"), forge.join("dangling-link")).unwrap();
    let payload = |command: String| {
        json!({
            "session_id": "symlink-workdir-scope", "cwd": protected,
            "tool_input": {"workdir": forge, "command": command}
        })
    };

    assert!(blocked(
        &payload(format!(
            "rm -- {}",
            protected.join("outbound-link").display()
        )),
        &protected
    ));
    assert!(blocked(
        &payload(format!(
            "unlink {}",
            protected.join("outbound-link").display()
        )),
        &protected
    ));
    assert!(blocked(
        &payload(format!(
            "mv {} {}",
            protected.join("outbound-link").display(),
            forge.join("moved-link").display()
        )),
        &protected
    ));
    assert!(blocked(
        &payload("install -D /tmp/source inbound-link/new/sub/file".into()),
        &protected
    ));
    assert!(blocked(
        &payload("echo x > dangling-link".into()),
        &protected
    ));
}
