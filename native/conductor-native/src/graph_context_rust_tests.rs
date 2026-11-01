use super::*;
fn skeleton(source: &str, target: Option<&str>) -> Value {
    rust(source, target).unwrap()
}
fn output(value: &Value) -> &str {
    value["skeleton"].as_str().unwrap()
}

#[test]
fn rust_attributes_signatures_containers_and_data_items_are_preserved() {
    let source = r#"use std::fmt;
pub mod outer {
    use super::*;
    /// Public trait.
    pub trait Worker {
        /// Run trait work.
        fn run(&self, input: &str) -> usize;
    }
    #[derive(Debug)]
    pub struct Engine { value: usize }
    pub enum State { Ready, Waiting(u32) }
    impl Worker for Engine {
        /// Run implementation work.
        #[inline]
        fn run(
            &self,
            input: &str,
        ) -> usize { self.value + input.len() }
    }
    mod child;
}
"#;
    let result = skeleton(source, None);
    let text = output(&result);
    for expected in [
        "use std::fmt;",
        "pub mod outer {",
        "pub trait Worker {",
        "/// Public trait.",
        "fn run(&self, input: &str) -> usize;",
        "#[derive(Debug)]",
        "pub struct Engine { value: usize }",
        "pub enum State { Ready, Waiting(u32) }",
        "impl Worker for Engine {",
        "#[inline]",
        "/// Run implementation work.",
        "input: &str,",
        "mod child;",
        "{ ... }",
    ] {
        assert!(text.contains(expected), "missing {expected:?}: {text}");
    }
    assert!(!text.contains("self.value + input.len()"));
    assert_eq!(result["format"], "rust-signatures-v1");
    assert_eq!(result["language"], "rust");
    let selected = skeleton(source, Some("outer::Engine::run"));
    assert!(output(&selected).contains("impl Worker for Engine"));
    assert!(!output(&selected).contains("pub struct Engine"));
    assert!(!output(&selected).contains("pub trait Worker"));
}

#[test]
fn unicode_before_and_inside_signatures_uses_exact_byte_spans() {
    let source="// Unicode prefix 界 Ελληνικά\n/// Description café 🦀\npub fn café(\n    entrée: &str,\n) -> &'static str { \"界\" }\n";
    let result = skeleton(source, None);
    let text = output(&result);
    assert!(text.contains("/// Description café 🦀"));
    assert!(
        text.contains("pub fn café(\n    entrée: &str,\n) -> &'static str { ... }"),
        "{text}"
    );
    assert!(!text.contains("\"界\""));
    assert_eq!(result["symbols"], json!(["café"]));
}

#[test]
fn many_function_projection_has_counted_linear_work_without_prefix_rescans() {
    let source = (0..2048)
        .map(|n| format!("/// Function {n} 界\npub fn f{n}(x: u64) -> u64 {{ x + {n} }}\n"))
        .collect::<String>();
    let (result, stats) = project(&source, None, Limits::default()).unwrap();
    assert_eq!(result["symbols"].as_array().unwrap().len(), 2048);
    assert_eq!(stats.definitions, 2048);
    assert!(stats.nodes < Limits::default().nodes);
    assert!(
        stats.work < source.len() * 12,
        "counted work {:?} for {} source bytes",
        stats,
        source.len()
    );
    assert!(output(&result).contains("pub fn f2047(x: u64) -> u64 { ... }"));
    assert!(!output(&result).contains("x + 2047"));
}

#[test]
fn source_depth_nodes_definitions_work_and_output_fail_loudly_at_bounds() {
    assert!(rust(&" ".repeat(1024 * 1024 + 1), None)
        .unwrap_err()
        .contains("exceeds"));
    let deep = format!("{}fn leaf() {{}}{}", "mod m {".repeat(100), "}".repeat(100));
    let message = rust(&deep, None).unwrap_err();
    assert!(
        message.contains("depth limit") || message.contains("budget"),
        "{message}"
    );
    let source = "pub fn a() {}\npub fn b() {}\n";
    for (limits, reason) in [
        (
            Limits {
                nodes: 2,
                ..Limits::default()
            },
            "node",
        ),
        (
            Limits {
                definitions: 1,
                ..Limits::default()
            },
            "definition count",
        ),
        (
            Limits {
                work: 1,
                ..Limits::default()
            },
            "work",
        ),
        (
            Limits {
                output: 8,
                ..Limits::default()
            },
            "output byte",
        ),
    ] {
        let error = project(source, None, limits).unwrap_err().to_string();
        assert!(error.contains(reason), "{error}");
    }
}

#[test]
fn nested_implementation_items_are_not_exposed_and_missing_selector_refuses() {
    let source = "pub fn outer() { fn hidden() {} hidden(); }\n";
    let result = skeleton(source, None);
    assert_eq!(result["symbols"], json!(["outer"]));
    assert!(!output(&result).contains("hidden"));
    assert!(rust(source, Some("hidden"))
        .unwrap_err()
        .contains("not found"));
    assert!(rust(source, Some(&"x".repeat(4097)))
        .unwrap_err()
        .contains("selector"));
    assert!(rust("fn broken( {", None)
        .unwrap_err()
        .contains("syntax error"));
}

#[test]
fn one_line_many_functions_after_non_ascii_prefix_have_linear_counted_work() {
    let mut source =
        "const PREFIX: &str = \"界🦀\"; pub fn café() -> &'static str { PREFIX } ".to_owned();
    for n in 0..2048 {
        source.push_str(&format!("pub fn f{n}(x:usize)->usize{{x+{n}}} "));
    }
    let (result, stats) = project(&source, None, Limits::default()).unwrap();
    let text = output(&result);
    assert_eq!(result["symbols"].as_array().unwrap().len(), 2049);
    assert!(
        text.contains("pub fn café() -> &'static str { ... }"),
        "{text}"
    );
    assert!(text.contains("pub fn f2047(x:usize)->usize { ... }"));
    assert!(!text.contains("x+2047"));
    assert!(
        stats.work < source.len() * 12,
        "counted work {:?} for {} bytes",
        stats,
        source.len()
    );
}
