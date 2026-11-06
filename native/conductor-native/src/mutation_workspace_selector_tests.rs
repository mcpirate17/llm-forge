use super::*;
use serde_json::json;

fn options(profile: Option<&str>, selector: Value) -> Map<String, Value> {
    let mut map = Map::new();
    if let Some(profile) = profile {
        map.insert("execution_profile".into(), json!(profile));
    }
    map.insert("workspace_selector".into(), selector);
    map
}
#[test]
fn v3_selector_builds_fixed_arguments() {
    let value = json!({"schema_version":1,"target":{"kind":"test","name":"contract-test"},"filter":"module::tests::"});
    let selector = parse(&options(Some("cargo-workspace-v3"), value))
        .unwrap()
        .unwrap();
    assert_eq!(selector.cargo_args(), ["--test=contract-test"]);
    assert_eq!(selector.libtest_args(), ["--", "module::tests::"]);
    assert_eq!(
        selector.engine_args(),
        [
            "--cargo-arg=--test=contract-test",
            "--cargo-test-arg=--",
            "--cargo-test-arg=module::tests::"
        ]
    );
}
#[test]
fn profile_and_selector_shapes_fail_closed() {
    let good = json!({"schema_version":1,"target":{"kind":"lib"},"filter":null});
    assert!(parse(&options(Some("cargo-workspace-v2"), good.clone())).is_err());
    assert!(parse(&options(None, good.clone())).is_err());
    assert!(parse(&options(Some("unknown"), good.clone())).is_err());
    assert!(parse(&Map::from_iter([(
        "execution_profile".into(),
        json!("cargo-workspace-v3")
    )]))
    .is_err());
    for value in [
        json!({"schema_version":1,"target":{"kind":"lib","name":"x"},"filter":null}),
        json!({"schema_version":1,"target":{"kind":"test","name":"--bad"},"filter":null}),
        json!({"schema_version":2,"target":{"kind":"lib"},"filter":null}),
        json!({"schema_version":1,"target":{"kind":"lib"},"filter":"foo::--help"}),
        json!({"schema_version":1,"target":{"kind":"lib"},"filter":""}),
        json!({"schema_version":1,"target":{"kind":"lib"},"filter":null,"unknown":true}),
    ] {
        assert!(
            parse(&options(Some("cargo-workspace-v3"), value.clone())).is_err(),
            "{value}"
        );
    }
}

#[test]
fn direct_selector_deserialization_refuses_unknown_target_fields() {
    for target in [
        json!({"kind":"lib","name":"undeclared"}),
        json!({"kind":"test","name":"contract","extra":true}),
        json!({"kind":"bin","name":"forge"}),
    ] {
        let value = json!({"schema_version":1,"target":target,"filter":null});
        assert!(
            serde_json::from_value::<Selector>(value.clone()).is_err(),
            "{value}"
        );
    }
    let value = json!({"schema_version":1,"target":{"kind":"lib"},"filter":null});
    assert_eq!(
        serde_json::from_value::<Selector>(value).unwrap().target,
        Target::Lib {}
    );
}

#[test]
fn library_metadata_kinds_are_explicitly_bounded() {
    for kind in ["lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"] {
        assert!(is_library_kind(kind));
    }
    for kind in ["bin", "test", "example", "bench", "custom-build", "unknown"] {
        assert!(!is_library_kind(kind));
    }
}
