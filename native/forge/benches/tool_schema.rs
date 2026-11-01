//! Run with `cargo bench --manifest-path native/forge/Cargo.toml --bench tool_schema`.
//! In-process validation only; no claim about model or subprocess latency.
use forge::tool_schema::{CompiledSchema, ToolRegistry};
use std::hint::black_box;
use std::time::Instant;

fn measure(name: &str, bytes: usize, mut operation: impl FnMut()) {
    for _ in 0..10_000 {
        operation();
    }
    let mut samples = Vec::with_capacity(31);
    for _ in 0..31 {
        let start = Instant::now();
        for _ in 0..10_000 {
            operation();
        }
        samples.push(start.elapsed().as_nanos() as f64 / 10_000.0);
    }
    samples.sort_by(f64::total_cmp);
    println!("{{\"workload\":{name:?},\"bytes\":{bytes},\"iterations\":310000,\"batch_mean_ns_p50\":{},\"batch_mean_ns_p95\":{}}}", samples[15], samples[29]);
}

fn main() {
    let spec = r#"{"type":"object","additionalProperties":false,"properties":{"path":{"type":"string","minLength":1},"limit":{"type":"integer","minimum":1,"maximum":100},"tags":{"type":"array","items":{"type":"string"},"maxItems":4}},"required":["path"]}"#;
    let start = Instant::now();
    let schema = CompiledSchema::from_json(spec).unwrap();
    println!("{{\"schema_compile_ns\":{}}}", start.elapsed().as_nanos());
    let input = r#"{"path":"src/\u006dain.rs","limit":25,"tags":["rust","tooling"]}"#;
    measure("escaped_arguments", input.len(), || {
        black_box(schema.validate(black_box(input))).unwrap();
    });
    let invalid = r#"{"path":"src/main.rs","limit":101}"#;
    measure("rejected_range", invalid.len(), || {
        assert!(black_box(schema.validate(black_box(invalid))).is_err());
    });
    let catalog = format!(r#"{{"tools":[{{"name":"inspect","inputSchema":{spec}}}]}}"#);
    let registry = ToolRegistry::from_json(&catalog).unwrap();
    let call = format!(r#"{{"name":"inspect","arguments":{input}}}"#);
    measure("registered_call", call.len(), || {
        black_box(registry.validate_call(black_box(&call), false)).unwrap();
    });
    let tools: Vec<_> = (0..256)
        .map(|index| {
            serde_json::json!({
                "name":format!("tool{index:03}"),
                "inputSchema":serde_json::from_str::<serde_json::Value>(spec).unwrap()
            })
        })
        .collect();
    let registry =
        ToolRegistry::from_json(&serde_json::json!({"tools":tools}).to_string()).unwrap();
    let call = format!(r#"{{"name":"tool255","arguments":{input}}}"#);
    measure("registered_call_256_tools", call.len(), || {
        black_box(registry.validate_call(black_box(&call), false)).unwrap();
    });
}
