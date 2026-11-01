use super::*;
use serde_json::json;

fn parsed(raw: &[u8]) -> Value {
    serde_json::from_slice(&sanitize(raw).unwrap()).unwrap()
}

#[test]
fn default_and_common_options_preserve_types() {
    assert_eq!(parsed(b""), json!({"InheritParentConfig":false}));
    assert_eq!(parsed(b"Checks: '-*,modernize-*'\nInheritParentConfig: false\nCheckOptions:\n  - key: modernize-x.Option\n    value: 'yes'\nWarningsAsErrors: '*'\n"),
        json!({"Checks":"-*,modernize-*","InheritParentConfig":false,"CheckOptions":[{"key":"modernize-x.Option","value":"yes"}],"WarningsAsErrors":"*"}));
}

#[test]
fn scalar_lexemes_survive_inside_options_and_other_fields() {
    let value = parsed(b"UseColor: false\nCheckOptions:\n  check.one: 001\n  check.hex: 0xFF\n  check.float: 0.123456789123456789\n  check.quoted: '001'\n  check.true: true\n  check.null: null\n  InheritParentConfig: false\n");
    assert_eq!(value["UseColor"], "false");
    assert_eq!(
        value["CheckOptions"],
        json!({"check.one":"001", "check.hex":"0xFF",
        "check.float":"0.123456789123456789", "check.quoted":"001", "check.true":"true",
        "check.null":"null", "InheritParentConfig":"false"})
    );
    assert_eq!(value["InheritParentConfig"], false);
}

#[test]
fn decoded_keys_and_duplicate_nested_keys_are_refused() {
    assert!(sanitize(b"ExtraArgs: []\n").is_err());
    assert!(sanitize(b"\"Extra\\x41rgs\": []\n").is_err());
    assert!(sanitize(b"RemovedArgs: []\n").is_err());
    assert!(sanitize(b"ExtraArgsBefore: []\n").is_err());
    assert!(sanitize(b"CheckOptions: { Foo: x, \"F\\x6fo\": y }\n").is_err());
    assert!(sanitize(b"true: value\n").is_err());
    assert!(sanitize(b"? [a, b]\n: value\n").is_err());
}

#[test]
fn inheritance_and_unsupported_yaml_fail_closed() {
    assert!(sanitize(b"InheritParentConfig: true\n").is_err());
    assert!(sanitize(b"InheritParentConfig: 'false'\n").is_err());
    assert!(sanitize(b"Checks: &anchor '*'\nOther: *anchor\n").is_err());
    assert!(sanitize(b"Checks: !tag '*'\n").is_err());
    assert!(sanitize(b"---\nChecks: a\n---\nChecks: b\n").is_err());
    let deep = format!("X: {}a{}", "[".repeat(33), "]".repeat(33));
    assert!(sanitize(deep.as_bytes()).is_err());
}

#[test]
fn bounded_input_and_output() {
    assert!(sanitize(&vec![b' '; MAX_BYTES + 1]).is_err());
    let many = format!("Checks: \"{}\"", "\\n".repeat(20_000));
    assert!(sanitize(many.as_bytes()).is_ok());
    let oversized_output = format!("Checks: '{}'", "\t".repeat(40_000));
    assert!(decode(&oversized_output).is_ok());
    assert!(sanitize(oversized_output.as_bytes()).is_err());
    let events = format!("Checks: [{}]", "[],".repeat(10_001));
    assert!(events.len() < MAX_BYTES);
    assert!(sanitize(events.as_bytes()).is_err());
}

#[test]
#[ignore = "requires explicitly provisioned FORGE_TEST_CLANG_TIDY native ELF"]
fn real_llvm_dump_preserves_config_scalar_lexemes() {
    let root = tempfile::tempdir().unwrap();
    let tool = crate::candidate_clang_format_cli::tool::Tool::from_env_key(
        root.path(),
        "FORGE_TEST_CLANG_TIDY",
    )
    .unwrap();
    let raw = b"Checks: '-*,readability-identifier-naming'\nUseColor: false\nSystemHeaders: true\nInheritParentConfig: false\nCheckOptions:\n  readability-identifier-naming.VariablePrefix: 001\n  readability-identifier-naming.FunctionPrefix: 0xFF\n  readability-identifier-naming.ConstantPrefix: 0.123456789123456789\n  readability-identifier-naming.MemberPrefix: true\n  readability-identifier-naming.ClassPrefix: null\n";
    let sanitized = sanitize(raw).unwrap();
    let mut values = Vec::new();
    for (name, bytes) in [
        ("raw.yaml", raw.as_slice()),
        ("normalized.json", &sanitized),
    ] {
        let path = root.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        let mut command = tool.command(root.path()).unwrap();
        command
            .arg("--dump-config")
            .arg(format!("--config-file={}", path.display()));
        let output =
            crate::process_probe::run(&mut command, std::time::Duration::from_secs(30), 128 * 1024)
                .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        values.push(parsed(&output.stdout));
    }
    assert_eq!(values[0], values[1]);
    assert_eq!(
        values[1]["CheckOptions"]["readability-identifier-naming.VariablePrefix"],
        "001"
    );
    assert_eq!(
        values[1]["CheckOptions"]["readability-identifier-naming.FunctionPrefix"],
        "0xFF"
    );
    assert_eq!(
        values[1]["CheckOptions"]["readability-identifier-naming.ConstantPrefix"],
        "0.123456789123456789"
    );
}
