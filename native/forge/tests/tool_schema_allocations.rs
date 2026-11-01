//! This binary instruments allocations only on this test thread and only after
//! registration. Transport/serialization are deliberately outside the claim.
use forge::tool_schema::{CompiledSchema, ToolRegistry};
use std::hint::black_box;
#[path = "support/allocation_counter.rs"]
mod allocation_counter;

#[test]
fn success_failure_unicode_repair_and_registered_dispatch_allocate_nothing() {
    let spec = r#"{"type":"object","additionalProperties":false,"properties":{"path":{"type":"string","enum":["a😀"]},"limit":{"type":"integer","minimum":1,"maximum":100}},"required":["path"]}"#;
    let schema = CompiledSchema::from_json(spec).unwrap();
    let registry = ToolRegistry::from_json(&format!(
        r#"{{"tools":[{{"name":"inspect","inputSchema":{spec}}}]}}"#
    ))
    .unwrap();
    let valid = r#"{"p\u0061th":"a\ud83d\ude00","limit":1e2}"#;
    let call = format!(r#"{{"name":"ins\u0070ect","arguments":{valid}}}"#);
    let fenced = format!("```json\n{valid}\n```");
    let fenced_call = format!("```json\n{call}\n```");
    let (correct, allocations) = allocation_counter::measure(|| {
        let mut correct = true;
        for _ in 0..1000 {
            correct &= black_box(schema.validate(black_box(valid))).is_ok();
            correct &= black_box(schema.validate(r#"{"path":"b"}"#)).is_err();
            correct &= black_box(schema.validate(r#"{"path":"a😀","limit":101}"#)).is_err();
            correct &= black_box(schema.validate(r#"{"path":"a😀","path":"a😀"}"#)).is_err();
            correct &= black_box(schema.validate(r#"{"path":"\ud800"}"#)).is_err();
            correct &= black_box(schema.validate_with_repair(black_box(&fenced), true)).is_ok();
            correct &= black_box(registry.validate_call(black_box(&call), false)).is_ok();
            correct &= black_box(registry.validate_call(black_box(&fenced_call), true)).is_ok();
            correct &=
                black_box(registry.validate_call(r#"{"name":"missing","arguments":{}}"#, false))
                    .is_err();
        }
        correct
    });
    assert!(correct);
    assert_eq!(allocations, 0);
}
