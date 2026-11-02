//! Analysis-specific additions to the shared immutable private Git fixture.
use crate::{
    candidate_clang_fixture::{assert_code, run, Private},
    candidate_execution_fixture::Case,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};

pub const POLICY: &str = "policies/main.toml";
pub fn private(base: &[(&str, &[u8])], target: &[(&str, &[u8])]) -> Private {
    let case = Case::new(Path::new("/bin/true"));
    case.staged_policy(|policy| policy.replace("[classes]\n",
        "[classes]\npython=['*.py','**/*.py']\nconfig=['*.toml','*.json']\nnotebook=['*.ipynb']\ntest=['specimens/*.py']\nnovel=['novel_lane/*.py']\n")
        .replace("hot=[]","hot=['hot_lane/*.py']"));
    Private::from_case(case, base, target)
}
pub fn command(f: &Private, check: &str, files: &[&str], execute: bool) -> Command {
    let mut options = vec!["--check", check, "--policy", POLICY];
    if execute {
        options.push("--execute");
    }
    f.tool_command_options("analyze", &options, files)
}
pub fn report(output: Output, code: i32) -> Value {
    assert_code(&output, code);
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output.stdout)))
}
pub fn execute(f: &Private, check: &str, files: &[&str], code: i32) -> Value {
    let value = report(run(command(f, check, files, true)), code);
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["check"], check);
    assert_eq!(value["executed"], true);
    assert!(value["result"].is_object());
    value
}
pub fn findings(value: &Value) -> &[Value] {
    value["result"]["findings"].as_array().unwrap()
}
pub fn rule<'a>(value: &'a Value, path: &str, rule: &str) -> Option<&'a Value> {
    findings(value)
        .iter()
        .find(|finding| finding["path"] == path && finding["rule_id"] == rule)
}
pub fn provider(f: &Private) -> String {
    let value = json!({"schema_version":1,"implementation":"cpython","version":"3.12","platform":"linux",
        "stdlib":["os","sys","typing"],"modules":[{"name":"requests","distributions":["requests"]}]});
    let bytes = serde_json::to_vec(&value).unwrap();
    let directory = f.case.fixture.root.join("operator-providers");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.join("observation.json");
    fs::write(&path, &bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    json!({"path":path,"sha256":format!("{:x}",Sha256::digest(&bytes))}).to_string()
}
pub fn long_function(name: &str) -> String {
    format!("def {name}():\n    a = 1\n    b = 2\n    c = 3\n    d = 4\n    e = 5\n    f = 6\n    g = 7\n    h = 8\n    return a\n")
}
