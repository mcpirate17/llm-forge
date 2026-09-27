//! Native construction of the four-agent hook matrix used by policy contracts.

use crate::support::{module, path, text, AttrPatch, Case};
use pyo3::prelude::*;
use serde_json::json;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

const AGENTS: [(&str, &str, &str, &str); 4] = [
    ("codex", "Read", "Bash", ""),
    ("claude", "Read", "Bash", ""),
    (
        "qwen",
        "read_file",
        "run_shell_command",
        "export LOCAL_AI_RUNTIME=1\n",
    ),
    ("grok", "read_file", "run_shell_command", ""),
];

const PRE_BASH: &str = "#!/usr/bin/env bash\n\
# PreToolUse/Bash: deny history-destroying git commands.\n\
set -euo pipefail\n\
payload=$(cat)\n\
case \"$payload\" in\n\
  *'git reset --hard'*) echo 'BLOCKED: git reset --hard destroys uncommitted work' ;;\n\
esac\n";

const POST_HOOK: &str = "#!/usr/bin/env bash\n\
# PostToolUse: bounded no-op.\n\
set -euo pipefail\n\
cat >/dev/null\n\
echo '{\"hookSpecificOutput\":{\"hookEventName\":\"PostToolUse\"}}'\n";

pub struct HookMatrix {
    root: PathBuf,
    _active_root: AttrPatch,
}

impl HookMatrix {
    pub fn new(py: Python<'_>, case: &mut Case) -> Self {
        let root = case.mkdir("repo");
        let init = Command::new("git")
            .args(["init", "-q"])
            .arg(&root)
            .output()
            .expect("start git init for hook matrix");
        assert!(
            init.status.success(),
            "git init hook matrix: {}",
            String::from_utf8_lossy(&init.stderr)
        );

        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../src")
            .canonicalize()
            .expect("resolve Forge Python source tree");
        let python = text(&module(py, "sys").getattr("executable").unwrap());
        case.set_env("HOOK_MATRIX_PYTHON", &python);
        case.set_env(
            "GROK_INSPECT_COMMAND",
            &format!(
                "{} -m conductor.grok_inspect_stub {}",
                shell_quote(&python),
                shell_quote(&root.to_string_lossy())
            ),
        );

        for (agent, read_tool, shell_tool, exports) in AGENTS {
            write_agent(
                &root, &source, &python, agent, read_tool, shell_tool, exports,
            );
        }
        write_post_hooks(&root);
        let gate = root.join(".agent_hooks/crg_gate.py");
        link_child(&gate);
        let gate_body = root.join("tooling/hooks/agent/crg_gate.py");
        fs::create_dir_all(gate_body.parent().unwrap()).expect("create gate body directory");
        fs::copy(source.join("tooling/hooks/agent/crg_gate.py"), &gate_body)
            .expect("copy shipped gate body into hook matrix");

        let active_state = module(py, "conductor.active_state");
        let active_root =
            AttrPatch::replace(active_state.as_any(), "ROOT", path(py, &root).as_any());
        active_state
            .getattr("save_active_state")
            .unwrap()
            .call1((path(py, &root.join("conductor/active_state.json")),))
            .expect("save production active state in hook matrix");
        Self {
            root,
            _active_root: active_root,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn write_agent(
    root: &Path,
    source: &Path,
    python: &str,
    agent: &str,
    read_tool: &str,
    shell_tool: &str,
    exports: &str,
) {
    let hooks = root.join(format!(".{agent}/hooks"));
    fs::create_dir_all(&hooks).expect("create agent hook directory");
    let pre_edit = format!(
        "#!/usr/bin/env bash\n\
# PreToolUse: route every read and edit through the shared current-work guard.\n\
set -euo pipefail\n\
{exports}export PYTHONPATH={}${{PYTHONPATH:+:$PYTHONPATH}}\n\
exec {} -m conductor.current_work_guard\n",
        shell_quote(&source.to_string_lossy()),
        shell_quote(python),
    );
    write_program(&hooks.join("pre-edit.sh"), &pre_edit);
    let config_path = match agent {
        "codex" => root.join(".codex/hooks.json"),
        "claude" => root.join(".claude/settings.json"),
        "qwen" => root.join(".qwen/settings.json"),
        "grok" => root.join(".grok/hooks/workspace.json"),
        _ => unreachable!("fixed agent matrix"),
    };
    let config = json!({"hooks":{"PreToolUse":[
        {"matcher":read_tool,"hooks":[{"type":"command","command":hooks.join("pre-edit.sh")} ]},
        {"matcher":shell_tool,"hooks":[{"type":"command","command":format!(
            "GOVERNANCE_OWNER={agent} {} verify", root.join(".agent_hooks/crg_gate.py").display()
        )}]}
    ]}});
    fs::write(
        config_path,
        format!("{}\n", serde_json::to_string_pretty(&config).unwrap()),
    )
    .expect("write agent hook config");
}

fn write_post_hooks(root: &Path) {
    for agent in ["codex", "claude"] {
        let hooks = root.join(format!(".{agent}/hooks"));
        write_program(&hooks.join("pre-bash.sh"), PRE_BASH);
        write_program(&hooks.join("post-edit.sh"), POST_HOOK);
        link_child(&hooks.join("obsidian_sync.py"));
    }
    write_program(&root.join(".claude/hooks/post-bash-graph.sh"), POST_HOOK);
}

fn write_program(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write hook fixture program");
    let mut mode = fs::metadata(path).unwrap().permissions();
    mode.set_mode(0o755);
    fs::set_permissions(path, mode).expect("mark hook fixture program executable");
}

fn link_child(path: &Path) {
    fs::create_dir_all(path.parent().expect("hook launcher parent"))
        .expect("create hook launcher directory");
    symlink(env!("CARGO_BIN_EXE_hook_matrix_child"), path).expect("link native hook launcher");
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
