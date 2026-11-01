//! Current-only pure preparation/capture measurements. No process or sandbox launch.
use forge::{
    candidate_command::{self, ExternalCheck, OperatorInputs},
    candidate_source::{Candidate, Kind},
    process_capture_tail::TailBuffer,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    hint::black_box,
    io::Read,
    path::Path,
    time::{Duration, Instant},
};

const BATCHES: usize = 31;
const CHARS: usize = 12_000;
const DEADLINE: Duration = Duration::from_secs(30);
fn executable_sha() -> anyhow::Result<String> {
    let mut file = std::fs::File::open(std::env::current_exe()?)?;
    let mut digest = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        digest.update(&bytes[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn stats(name: &str, samples: &mut [f64], iterations: u32, size: Value) -> Value {
    samples.sort_by(f64::total_cmp);
    json!({"name":name,"unit":"ns_per_operation","batch_mean_median":samples[15],
        "batch_mean_p95":samples[29],"batches":BATCHES,"iterations_per_batch":iterations,"workload":size})
}
fn measure(
    name: &str,
    iterations: u32,
    size: Value,
    started: Instant,
    mut op: impl FnMut(),
) -> Value {
    for _ in 0..10 {
        op();
    }
    let mut samples = Vec::with_capacity(BATCHES);
    for _ in 0..BATCHES {
        let t = Instant::now();
        for _ in 0..iterations {
            op();
        }
        samples.push(t.elapsed().as_nanos() as f64 / f64::from(iterations));
        assert!(
            started.elapsed() < DEADLINE,
            "benchmark exceeded 30 seconds"
        );
    }
    stats(name, &mut samples, iterations, size)
}
fn candidate() -> Candidate {
    Candidate {
        kind: Kind::Index,
        tree_oid: "a".repeat(40),
        base_tree_oid: "b".repeat(40),
        base_commit_oid: Some("c".repeat(40)),
        commit_oid: None,
        target_ref: None,
        changes: vec![],
        integration_base_oid: None,
        integration_base_detail: String::new(),
    }
}
fn check(command: Vec<String>) -> ExternalCheck {
    ExternalCheck::from_normalized(&json!({"kind":"command","check_id":"bench",
        "max_output_chars":CHARS,"command":command,"version_command":["tool","--version"]}))
    .unwrap()
    .unwrap()
}
fn inputs() -> OperatorInputs<'static> {
    OperatorInputs {
        repo: Path::new("/operator/repo"),
        snapshot: Path::new("/operator/snapshot"),
        runtime_dir: Path::new("/operator/runtime"),
        python: Some("python"),
    }
}
fn preparation(start: Instant) -> Vec<Value> {
    let candidate = candidate();
    let files: Vec<String> = (0..32)
        .map(|i| format!("src/module/file_{i:02}.py"))
        .collect();
    let simple = check(vec!["tool".into(), "{files}".into()]);
    let dense = check(vec![
        "tool".into(),
        "{files}".into(),
        "{repo}:{snapshot}:{base}:{tree}:{python}:{changed_files_file}".repeat(8),
    ]);
    let reference = candidate_command::prepare(&simple, &candidate, &inputs(), &files).unwrap();
    assert_eq!(&reference.command.argv[1..], files.as_slice());
    assert_eq!(reference.version.argv, ["tool", "--version"]);
    let reference = candidate_command::prepare(&dense, &candidate, &inputs(), &files).unwrap();
    let artifact = reference.command.changed_files.as_ref().unwrap();
    assert_eq!(artifact.bytes, format!("{}\n", files.join("\n")).as_bytes());
    assert!(reference
        .command
        .argv
        .last()
        .unwrap()
        .contains("/operator/runtime/changed-files-bench.txt"));
    [&simple, &dense]
        .into_iter()
        .zip(["argv_32_files", "argv_32_files_dense_placeholders"])
        .map(|(check, name)| {
            measure(
                name,
                100,
                json!({"files":32,"dense_repetitions":if name.contains("dense") {8} else {0}}),
                start,
                || {
                    black_box(
                        candidate_command::prepare(
                            black_box(check),
                            black_box(&candidate),
                            &inputs(),
                            black_box(&files),
                        )
                        .unwrap(),
                    );
                },
            )
        })
        .collect()
}
fn chunk() -> Vec<u8> {
    let value = format!("{}aéz\r\n", "aé🦀\r\nz".repeat(409));
    assert_eq!(value.len(), 4096);
    value.into_bytes()
}
fn filled(chunk: &[u8]) -> TailBuffer {
    let mut tail = TailBuffer::new(CHARS).unwrap();
    for _ in 0..16 {
        tail.push(chunk);
    }
    tail
}
fn tail_samples(start: Instant) -> Vec<Value> {
    let chunk = chunk();
    let mut verify = TailBuffer::new(4).unwrap();
    verify.push("é🦀\r\naβ".as_bytes());
    assert_eq!(verify.finish(), "🦀\naβ");
    let output = filled(&chunk).finish();
    assert_eq!(output.chars().count(), CHARS);
    assert!(!output.contains('\r'));
    let mut tail = filled(&chunk);
    let push = measure(
        "tail_push_steady",
        1000,
        json!({"tail_chars":CHARS,"chunk_bytes":4096}),
        start,
        || black_box(&mut tail).push(black_box(&chunk)),
    );
    let mut values = Vec::with_capacity(BATCHES);
    for _ in 0..10 {
        black_box(filled(&chunk).finish());
    }
    for _ in 0..BATCHES {
        let buffers: Vec<_> = (0..10).map(|_| filled(&chunk)).collect();
        let t = Instant::now();
        for buffer in buffers {
            black_box(buffer.finish());
        }
        values.push(t.elapsed().as_nanos() as f64 / 10.0);
        assert!(start.elapsed() < DEADLINE, "benchmark exceeded 30 seconds");
    }
    let finish = stats(
        "tail_finish_full_mixed_utf8_crlf",
        &mut values,
        10,
        json!({"tail_chars":CHARS,"chunk_bytes":4096,"setup_outside_timing":true}),
    );
    vec![push, finish]
}
fn main() -> anyhow::Result<()> {
    let digest = executable_sha()?;
    let start = Instant::now();
    let mut results = preparation(start);
    results.extend(tail_samples(start));
    println!(
        "{}",
        json!({"schema_version":1,"binary_sha256":digest,
        "profile":if cfg!(debug_assertions) {"debug"} else {"release"},
        "isolated_host":false,"comparison":"current_only",
        "scope":"pure argv preparation and tail capture; excludes process and sandbox", "results":results})
    );
    Ok(())
}
