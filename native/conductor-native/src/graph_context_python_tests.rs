use super::*;
fn skeleton(source: &str, target: Option<&str>) -> Value {
    python(source, target).unwrap()
}
fn rendered(value: &Value) -> &str {
    value["skeleton"].as_str().unwrap()
}

#[test]
fn decorators_async_headers_annotations_and_docs_survive_without_implementation() {
    let source = r#""""Module documentation."""
from __future__ import annotations
import os

@trace("source")
@cache(
    maxsize=32,
)
async def fetch(
    url: str,
    default: tuple[int, str] = (1, "界"),
) -> dict[str, str]:
    """Fetch data.

    Preserve uncertainty and Unicode 界.
    """
    response = await call_network(url)
    return response
"#;
    let result = skeleton(source, None);
    let output = rendered(&result);
    for expected in [
        "\"\"\"Module documentation.\"\"\"",
        "from __future__ import annotations",
        "import os",
        "@trace(\"source\")\n@cache(\n    maxsize=32,\n)",
        "async def fetch(\n",
        "default: tuple[int, str] = (1, \"界\")",
        ") -> dict[str, str]:",
        "Preserve uncertainty and Unicode 界.",
        "    ...",
    ] {
        assert!(output.contains(expected), "missing {expected:?}: {output}");
    }
    assert!(!output.contains("call_network"));
    assert!(!output.contains("return response"));
    assert_eq!(result["symbols"], json!(["fetch"]));
    assert_eq!(result["format"], "source-preserving-v1");
    syntax::parse(output, Language::Python).unwrap();
}

#[test]
fn bare_method_and_qualified_selection_keep_enclosing_class_and_only_matches() {
    let source = r#"class First(Base, metaclass=Meta):
    """First class."""
    count: int = 3
    config = make_configuration()
    @trace
    async def run(self, x: int) -> bool:
        """Run first."""
        return important_implementation()
    def other(self):
        return ignored()

class Second:
    def run(self):
        return another_implementation()
"#;
    let bare = skeleton(source, Some("run"));
    let output = rendered(&bare);
    for text in [
        "class First(Base, metaclass=Meta):",
        "class Second:",
        "@trace",
        "async def run",
        "config = make_configuration()",
        "\"\"\"Run first.\"\"\"",
    ] {
        assert!(output.contains(text), "{output}");
    }
    assert!(!output.contains("def other"));
    assert!(!output.contains("important_implementation"));
    let qualified = skeleton(source, Some("First.run"));
    assert!(rendered(&qualified).contains("class First"));
    assert!(!rendered(&qualified).contains("class Second"));
    assert_eq!(
        qualified["symbols"],
        json!(["First", "run", "other", "Second", "run"])
    );
    syntax::parse(rendered(&qualified), Language::Python).unwrap();
}

#[test]
fn selecting_class_includes_its_methods_and_scoped_nested_classes() {
    let source = "class Outer:\n    def one(self): return 1\n    class Inner:\n        def two(self): return 2\n\nclass Other:\n    def three(self): return 3\n";
    let result = skeleton(source, Some("Outer"));
    let output = rendered(&result);
    for text in [
        "class Outer:",
        "def one(self):",
        "class Inner:",
        "def two(self):",
    ] {
        assert!(output.contains(text), "{output}");
    }
    assert!(!output.contains("class Other"));
    assert!(!output.contains("return 1"));
    let method = skeleton(source, Some("Outer.Inner.two"));
    assert!(rendered(&method).contains("class Outer:"));
    assert!(rendered(&method).contains("class Inner:"));
    assert!(!rendered(&method).contains("def one"));
    syntax::parse(rendered(&method), Language::Python).unwrap();
}

#[test]
fn module_assignments_keep_scalars_annotations_and_omit_runtime_statements() {
    let source = r#"COUNT = 5
NEGATIVE = -1
LABEL = r"quoted 界"
READY = True
EMPTY = None
CHAIN = ALIAS = {"a": 1}
SPEC = build_spec()
FORMAT = rf"{build_spec()}"
ONLY_ANNOTATION: list[str]
ANNOTATED: int = compute_value()
print("module side effect")
if True:
    def conditional_definition(): return 1
"#;
    let output = skeleton(source, None);
    let text = rendered(&output);
    for expected in [
        "COUNT = 5",
        "NEGATIVE = -1",
        r#"LABEL = r"quoted 界""#,
        "READY = True",
        "EMPTY = None",
        "CHAIN = ALIAS = ...",
        "SPEC = ...",
        "FORMAT = ...",
        "ONLY_ANNOTATION: list[str]",
        "ANNOTATED: int = compute_value()",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    assert!(!text.contains("module side effect"));
    assert!(!text.contains("conditional_definition"));
    assert_eq!(output["symbols"], json!([]));
    syntax::parse(text, Language::Python).unwrap();
}

#[test]
fn implementation_nested_functions_are_not_exported_or_selectable() {
    let source = "def outer():\n    def hidden():\n        return 1\n    return hidden()\n";
    let result = skeleton(source, None);
    assert_eq!(result["symbols"], json!(["outer"]));
    assert!(!rendered(&result).contains("hidden"));
    assert!(python(source, Some("hidden"))
        .unwrap_err()
        .contains("not found"));
    assert!(python(source, Some("outer.hidden"))
        .unwrap_err()
        .contains("not found"));
}

#[test]
fn definition_lines_include_decorators_and_use_one_based_inclusive_bounds() {
    let source =
        "# comment\n@decorator\ndef f():\n    return 1\n\nclass C:\n    def m(self): return 2\n";
    let result = skeleton(source, None);
    let defs = result["definitions"].as_array().unwrap();
    assert_eq!(
        (defs[0]["line_start"].as_u64(), defs[0]["line_end"].as_u64()),
        (Some(2), Some(4))
    );
    assert_eq!(
        (defs[1]["line_start"].as_u64(), defs[1]["line_end"].as_u64()),
        (Some(6), Some(7))
    );
    assert_eq!(defs[2]["qualified_name"], "C.m");
    assert_eq!(
        (defs[2]["line_start"].as_u64(), defs[2]["line_end"].as_u64()),
        (Some(7), Some(7))
    );
}

#[test]
fn missing_symbols_syntax_nul_byte_and_selector_bounds_fail_loudly() {
    assert!(python("def f(: pass", None)
        .unwrap_err()
        .contains("syntax error"));
    assert!(python("x = 1\0", None).unwrap_err().contains("NUL"));
    assert!(python("def f(): pass\n", Some("absent"))
        .unwrap_err()
        .contains("not found"));
    assert!(python("", Some(&"x".repeat(4097)))
        .unwrap_err()
        .contains("selector"));
    assert!(python(&" ".repeat(syntax::MAX_SOURCE_BYTES + 1), None)
        .unwrap_err()
        .contains("exceeds"));
}

#[test]
fn depth_and_definition_admission_are_bounded() {
    let deeply_nested = format!("x = {}0{}\n", "(".repeat(300), ")".repeat(300));
    let error = python(&deeply_nested, None).unwrap_err();
    assert!(
        error.contains("limit") || error.contains("budget"),
        "{error}"
    );
    let definitions = (0..MAX_DEFINITIONS + 1)
        .map(|n| format!("def f{n}(): pass\n"))
        .collect::<String>();
    let error = python(&definitions, None).unwrap_err();
    assert!(
        error.contains("count exceeded") || error.contains("budget") || error.contains("deadline"),
        "{error}"
    );
}

#[test]
fn comments_before_docs_and_empty_classes_remain_valid_context() {
    let source = "class Empty:\n    pass\n\ndef f():\n    # explanation\n    'source quote style'\n    return 1\n";
    let result = skeleton(source, None);
    let output = rendered(&result);
    assert!(output.contains("class Empty:\n    ..."));
    assert!(output.contains("'source quote style'"));
    assert!(!output.contains("return 1"));
    syntax::parse(output, Language::Python).unwrap();
}

#[test]
fn output_budget_refuses_instead_of_cutting_signatures_or_docs() {
    let source = "@decorator\ndef detailed(argument: tuple[str, int]) -> bool:\n    'Keep this documentation.'\n    return True\n";
    let complete = project_with_limit(source, None, MAX_OUTPUT).unwrap();
    assert!(rendered(&complete).contains("'Keep this documentation.'"));
    let error = project_with_limit(source, None, 32)
        .unwrap_err()
        .to_string();
    assert!(error.contains("output byte limit"));
    let parenthesized = skeleton("VALUE = (5)\nLABEL = 'a' 'b'\n", None);
    assert!(rendered(&parenthesized).contains("VALUE = (5)"));
    assert!(rendered(&parenthesized).contains("LABEL = 'a' 'b'"));
}

#[test]
fn inline_class_simple_suites_receive_body_indentation() {
    for (source, expected) in [
        ("class C: x = 1\n", "class C:\n    x = 1"),
        (
            "class C: 'class documentation'\n",
            "class C:\n    'class documentation'",
        ),
        ("class C: import os\n", "class C:\n    import os"),
        (
            "class C: from os import path\n",
            "class C:\n    from os import path",
        ),
        (
            "class C: 'docs'; x = 1; import os; y: int = 2\n",
            "class C:\n    'docs'\n    x = 1\n    import os\n    y: int = 2",
        ),
        (
            "class Outer:\n  class Inner: 'docs'; x = 1\n",
            "class Outer:\n  class Inner:\n      'docs'\n      x = 1",
        ),
        ("class C:\n  x = 1\n  y = 2\n", "class C:\n  x = 1\n  y = 2"),
        (
            "class C:\n  x = 1; y = 2; import os\n",
            "class C:\n  x = 1\n  y = 2\n  import os",
        ),
    ] {
        let projected = skeleton(source, None);
        assert_eq!(rendered(&projected), expected);
        syntax::parse(rendered(&projected), Language::Python).unwrap();
    }
}

#[test]
fn parenthesized_real_docs_survive_and_fstrings_or_bytes_are_not_docs() {
    let source = r#"("Module documentation")
class C:
    (("class " "documentation"))
    def method(self):
        ("method documentation")
        return 1

def f():
    (("function documentation"))
    pass
"#;
    let projected = skeleton(source, None);
    let output = rendered(&projected);
    for text in [
        r#"("Module documentation")"#,
        r#"(("class " "documentation"))"#,
        r#"("method documentation")"#,
        r#"(("function documentation"))"#,
    ] {
        assert!(output.contains(text), "missing {text}: {output}");
    }
    syntax::parse(output, Language::Python).unwrap();
    let excluded = r#"class NotDoc:
    (b"bytes are not documentation")

def f():
    (f"interpolation {implementation()}")
    pass
"#;
    let projected = skeleton(excluded, None);
    let output = rendered(&projected);
    assert!(!output.contains("bytes are not documentation"));
    assert!(!output.contains("interpolation"));
    assert!(output.contains("class NotDoc:\n    ..."));
    assert!(output.contains("def f():\n    ..."));
    syntax::parse(output, Language::Python).unwrap();
}

#[test]
fn nested_class_formfeed_indentation_is_preserved_and_parseable() {
    let source = "class A:\n\x0c    class B:\n\x0c        pass\n";
    // If the native grammar rejects legal formfeed indentation, this fails at
    // source admission explicitly rather than approximating it silently.
    syntax::parse(source, Language::Python).expect("native parser admits formfeed indentation");
    let projected = skeleton(source, None);
    let output = rendered(&projected);
    assert_eq!(output, "class A:\n\x0c    class B:\n\x0c        ...");
    syntax::parse(output, Language::Python).expect("projected formfeed indentation remains valid");
}
