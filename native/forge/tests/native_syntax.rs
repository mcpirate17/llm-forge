use conductor_native::syntax::{self, Language, MAX_SOURCE_BYTES};

#[test]
fn parses_definitions_calls_and_scopes_in_all_registered_languages() {
    for (language, source, name) in [
        (Language::Rust, "mod sample { fn helper() {} fn caller() { helper(); } }", "sample::caller"),
        (Language::Python, "class Sample:\n    def helper(self):\n        return 1\n    def caller(self):\n        return self.helper()\n", "Sample.caller"),
        (Language::C, "int helper(void) { return 1; } int caller(void) { return helper(); }", "caller"),
        (Language::Cpp, "namespace sample { int helper() { return 1; } int caller() { return helper(); } }", "sample::caller"),
        (Language::Javascript, "function helper() { return 1; } const caller = () => helper();", "caller"),
        (Language::Typescript, "function helper(): number { return 1; } function caller(): number { return helper(); }", "caller"),
        (Language::Tsx, "function caller() { return <div>{helper()}</div>; }", "caller"),
    ] {
        let result = syntax::inspect(source, language).unwrap();
        let found = result.definitions(name);
        assert_eq!(found.len(), 1, "{}: {result:?}", language.name());
        assert!(found[0].signature.contains("caller"));
        assert!(result.references.iter().any(|r| r.role == "call" && r.scope == name), "{result:?}");
        let folded = result.fold(source).unwrap();
        assert!(folded.contains("body omitted"));
        syntax::parse(&folded, language).unwrap();
        assert!(result.fold(&(source.to_owned()+"\n")).is_err());
    }
}

#[test]
fn malformed_sources_and_resource_limits_fail_with_source_locations() {
    for (language, source) in [
        (Language::Rust, "fn x( {"),
        (Language::Python, "def x(:\n"),
        (Language::C, "int x( {"),
        (Language::Cpp, "class X {"),
        (Language::Javascript, "function x( {"),
        (Language::Typescript, "interface X {"),
    ] {
        let error = syntax::inspect(source, language).unwrap_err().to_string();
        assert!(error.contains("syntax error at line"), "{error}");
    }
    assert!(syntax::inspect(&" ".repeat(MAX_SOURCE_BYTES + 1), Language::Rust).is_err());
    assert!(syntax::inspect("\0", Language::Rust).is_err());
    assert!(Language::from_name("unknown").is_err());
}

#[test]
fn dependency_spellings_and_unicode_byte_offsets_are_preserved() {
    let js = "import { x } from './dependency.js';\nfunction café() { return x(); }\n";
    let result = syntax::inspect(js, Language::Javascript).unwrap();
    assert_eq!(result.dependencies[0].target, "./dependency.js");
    let symbol = result.definitions("café")[0];
    assert_eq!(
        &js[symbol.byte_start..symbol.byte_end],
        "function café() { return x(); }"
    );
    let c = syntax::inspect(
        "#include \"local.h\"\nint x(void) { return 0; }\n",
        Language::C,
    )
    .unwrap();
    assert_eq!(c.dependencies[0].target, "local.h");
    let py = syntax::inspect(
        "import os, sys as system\nfrom .local import helper\n",
        Language::Python,
    )
    .unwrap();
    assert_eq!(
        py.dependencies
            .iter()
            .map(|d| d.target.as_str())
            .collect::<Vec<_>>(),
        ["os", "sys", ".local"]
    );
    let rust = syntax::inspect("use crate::local::{helper, second};\n", Language::Rust).unwrap();
    assert_eq!(
        rust.dependencies[0].target,
        "crate::local::{helper, second}"
    );
}

#[test]
fn ambiguity_and_nested_folds_are_explicit() {
    let source = "function a() { function nested() { return 1; } return nested(); }\nfunction a() { return 2; }\n";
    let result = syntax::inspect(source, Language::Javascript).unwrap();
    assert_eq!(result.definitions("a").len(), 2);
    assert_eq!(result.definitions("a.nested").len(), 1);
    let folded = result.fold(source).unwrap();
    assert_eq!(folded.matches("body omitted").count(), 2);
    assert!(!folded.contains("nested"));
}

#[test]
fn deserialized_spans_are_checked_and_graph_signatures_respect_byte_limits() {
    let source = "fn café() {}";
    let inspection = syntax::inspect(source, Language::Rust).unwrap();
    for span in [
        (0, usize::MAX),
        (source.len(), source.len()),
        (7, 8),
        (10, 2),
    ] {
        let mut invalid = inspection.clone();
        invalid.symbols[0].body = Some(span);
        assert!(
            invalid.fold(source).is_err(),
            "accepted invalid span {span:?}"
        );
    }
    let mut inspection = inspection;
    inspection.symbols[0].signature = "é".repeat(300);
    let facts = inspection.graph_facts();
    assert_eq!(facts.definitions[0].signature.len(), 512);
    assert!(facts.cross_file_incomplete);
}
