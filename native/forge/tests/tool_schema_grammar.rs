//! Optional interoperability test against llama.cpp's actual GBNF engine.
//! Build `test-gbnf-validator` from llama.cpp and set FORGE_GBNF_VALIDATOR.
//! The upstream program exits 0 for both acceptance and rejection, so assert
//! its explicit verdict; an initialization failure is never a rejection PASS.
use forge::tool_schema::{CompiledSchema, ToolRegistry};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

struct Engine {
    binary: PathBuf,
    directory: PathBuf,
}

impl Engine {
    fn new() -> Self {
        let binary = std::env::var_os("FORGE_GBNF_VALIDATOR")
            .expect("set FORGE_GBNF_VALIDATOR to llama.cpp test-gbnf-validator")
            .into();
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "forge-grammar-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        Self { binary, directory }
    }

    fn check(&self, grammar: &str, input: &str, accepted: bool) {
        let grammar_path = self.directory.join("grammar.gbnf");
        let input_path = self.directory.join("input.json");
        fs::write(&grammar_path, grammar).unwrap();
        fs::write(&input_path, input).unwrap();
        let result = forge::process_probe::run(
            Command::new(&self.binary).args([&grammar_path, &input_path]),
            std::time::Duration::from_secs(20),
            1024 * 1024,
        )
        .unwrap();
        let stdout = String::from_utf8(result.stdout).unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let verdict = if accepted {
            "Input string is valid according to the grammar."
        } else {
            "Input string is invalid according to the grammar."
        };
        assert!(
            stdout.contains(verdict),
            "input={input:?}\n{stdout}\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
#[ignore = "requires external llama.cpp test-gbnf-validator (no model or GPU)"]
fn factored_repetitions_match_independent_length_predicates() {
    let engine = Engine::new();
    for (minimum, maximum, lengths) in [
        (0, 65, vec![0, 1, 32, 64, 65, 66]),
        (63, 129, vec![62, 63, 64, 65, 127, 128, 129, 130]),
        (8191, 8193, vec![8190, 8191, 8192, 8193, 8194]),
        (0, 524288, vec![0, 4097, 524288, 524289]),
    ] {
        let schema = CompiledSchema::compile(
            &json!({"type":"string","minLength":minimum,"maxLength":maximum}),
        )
        .unwrap();
        for length in lengths {
            let input = format!("\"{}\"", "x".repeat(length));
            engine.check(
                &schema.grammar(),
                &input,
                (minimum..=maximum).contains(&length),
            );
        }
    }
    let schema = CompiledSchema::compile(
        &json!({"type":"array","items":{"type":"null"},"minItems":66,"maxItems":130}),
    )
    .unwrap();
    for length in [65, 66, 67, 128, 130, 131] {
        engine.check(
            &schema.grammar(),
            &serde_json::to_string(&vec![Value::Null; length]).unwrap(),
            (66..=130).contains(&length),
        );
    }
    let schema = CompiledSchema::compile(&json!({"type":"string","minLength":65})).unwrap();
    for length in [64, 65, 100] {
        engine.check(
            &schema.grammar(),
            &format!("\"{}\"", "x".repeat(length)),
            length >= 65,
        );
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).unwrap();
    }
}

#[test]
#[ignore = "requires external llama.cpp test-gbnf-validator (no model or GPU)"]
fn generated_grammars_accept_valid_calls_and_reject_structural_violations() {
    let engine = Engine::new();
    let properties = json!({"a":{"type":"string"},"b":{"type":"boolean"},
        "c":{"type":"integer"},"d":{"type":"array","items":{"type":"null"},"maxItems":2},
        "e":{"type":"object","additionalProperties":false}});
    let schema = CompiledSchema::compile(
        &json!({"type":"object","additionalProperties":false,"properties":properties}),
    )
    .unwrap();
    let grammar = schema.grammar();
    let values = [json!("x"), json!(true), json!(42), json!([null]), json!({})];
    for mask in 0..32 {
        let mut object = serde_json::Map::new();
        for (index, name) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            if mask & (1 << index) != 0 {
                object.insert((*name).to_owned(), values[index].clone());
            }
        }
        let input = Value::Object(object).to_string();
        schema.validate(&input).unwrap();
        engine.check(&grammar, &input, true);
    }
    for input in [
        r#"{"a":null}"#,
        r#"{"f":1}"#,
        r#"{"a":"x","a":"y"}"#,
        r#"{"d":[null,null,null]}"#,
        "{,}",
        "{}{}",
    ] {
        assert!(schema.validate(input).is_err());
        engine.check(&grammar, input, false);
    }
    for (spec, valid, invalid) in [
        (
            json!({"type":"string","minLength":1,"maxLength":1}),
            r#""\ud83d\ude00""#,
            r#""\ud800""#,
        ),
        (
            json!({"type":"string","enum":["a\nb","x"]}),
            r#""a\nb""#,
            r#""y""#,
        ),
        (
            json!({"type":"string","minLength":0,"maxLength":0}),
            r#""""#,
            r#""a""#,
        ),
        (
            json!({"type":"array","items":{"type":"null"},"minItems":0,"maxItems":0}),
            "[]",
            "[null]",
        ),
        (
            json!({"type":"array","items":{"type":"null"},"minItems":2,"maxItems":3}),
            "[null,null]",
            "[null]",
        ),
        (
            json!({"type":"object","properties":{"a":{"type":"null"}},"required":["a"],"additionalProperties":false}),
            r#"{"a":null}"#,
            "{}",
        ),
        (json!({"type":"number"}), "1.25e-2", "01.2"),
    ] {
        let schema = CompiledSchema::compile(&spec).unwrap();
        engine.check(&schema.grammar(), valid, true);
        engine.check(&schema.grammar(), invalid, false);
    }
    let registry = ToolRegistry::from_json(r#"{"tools":[{"name":"a\"😀","inputSchema":{"type":"object","additionalProperties":false}},{"name":"b","inputSchema":{"type":"object","additionalProperties":false}}]}"#).unwrap();
    engine.check(
        &registry.grammar(),
        r#"{"name":"a\"😀","arguments":{}}"#,
        true,
    );
    engine.check(&registry.grammar(), r#"{"name":"b","arguments":{}}"#, true);
    engine.check(
        &registry.grammar(),
        r#"{"name":"unknown","arguments":{}}"#,
        false,
    );
    engine.check(
        &registry.grammar(),
        r#"{"name":"b","arguments":{"unknown":0}}"#,
        false,
    );
}
