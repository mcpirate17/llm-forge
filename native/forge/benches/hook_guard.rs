//! Current-only cooperative guard timings; excludes JSON parsing, process
//! startup, other hook handlers and model latency. No command is executed.
#[path = "../src/current_work_guard.rs"]
#[allow(dead_code)]
mod current_work_guard;
#[path = "../src/local_ai_policy.rs"]
// `harness = false` omits their test functions while all-target checking still
// enables cfg(test), leaving the source-local test imports unused here.
#[allow(dead_code, unused_imports)]
mod local_ai_policy;
#[path = "../src/write_targets.rs"]
#[allow(dead_code, unused_imports)]
mod write_targets;

use serde_json::json;
use std::hint::black_box;
use std::time::Instant;

fn measure(name: &str, mut operation: impl FnMut()) {
    for _ in 0..100 {
        operation();
    }
    let mut samples = Vec::with_capacity(31);
    for _ in 0..31 {
        let start = Instant::now();
        for _ in 0..1000 {
            operation();
        }
        samples.push(start.elapsed().as_nanos() as f64 / 1000.0);
    }
    samples.sort_by(f64::total_cmp);
    println!(
        "{}",
        json!({"workload":name,"batches":31,"operations_per_batch":1000,
        "warmup_operations":100,"median_batch_mean_ns":samples[15],"p95_batch_mean_ns":samples[29]})
    );
}

fn main() {
    // Bind the guard policy without touching any managed runtime or credentials.
    std::env::set_var("LOCAL_AI_RUNTIME", "0");
    let mut canonical = json!({"tool_name":"Read","tool_input":{"file_path":"safe.rs","content":"x".repeat(65536)}});
    measure(
        "canonical normalization, 64 KiB content, borrowed input",
        || {
            current_work_guard::normalize_tool_input(black_box(&mut canonical)).unwrap();
        },
    );
    let mut alternate =
        json!({"tool_name":"Read","tool_input":{},"arguments":{"file_path":".current_work.md"}});
    measure(
        "alternate normalization, includes canonical reset and clone",
        || {
            alternate["tool_input"] = json!({});
            current_work_guard::normalize_tool_input(black_box(&mut alternate)).unwrap();
        },
    );
    for (name, payload) in [
        (
            "Read protected path",
            json!({"tool_name":"Read","tool_input":{"file_path":".current_work.md"}}),
        ),
        (
            "Write unrelated path, 64 KiB content",
            json!({"tool_name":"Write","tool_input":{"file_path":"safe.rs","content":"x".repeat(65536)}}),
        ),
        (
            "Bash protected path",
            json!({"tool_name":"Bash","tool_input":{"command":"cat .current_work.md"}}),
        ),
    ] {
        measure(name, || {
            black_box(current_work_guard::run(black_box(&payload)));
        });
    }
}
