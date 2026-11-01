//! Current-only native in-process Unicode and candidate-integrity measurements.
//! Excludes source/snapshot I/O, sandbox, processes, assessment and result caching.
use anyhow::{ensure, Result};
use conductor_native::{candidate_checks, candidate_unicode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    hint::black_box,
    io::Read,
    time::{Duration, Instant},
};
const SAMPLES: usize = 31;
const WARMUP: u32 = 10;
const DEADLINE: Duration = Duration::from_secs(30);

fn within(start: Instant) -> Result<()> {
    ensure!(
        start.elapsed() < DEADLINE,
        "benchmark exceeded cooperative 30-second budget"
    );
    Ok(())
}
fn executable_sha(start: Instant) -> Result<String> {
    let mut file = std::fs::File::open(std::env::current_exe()?)?;
    ensure!(
        file.metadata()?.len() <= 256 * 1024 * 1024,
        "benchmark executable exceeds 256 MiB"
    );
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    for _ in 0..4097 {
        within(start)?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            return Ok(format!("{:x}", digest.finalize()));
        }
        digest.update(&buffer[..n]);
    }
    anyhow::bail!("benchmark executable grew beyond 256 MiB")
}
fn measure(
    name: &str,
    iterations: u32,
    workload: Value,
    start: Instant,
    mut op: impl FnMut(),
) -> Result<Value> {
    for _ in 0..WARMUP {
        within(start)?;
        op();
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        within(start)?;
        let began = Instant::now();
        for _ in 0..iterations {
            op();
        }
        samples.push(began.elapsed().as_nanos() as f64 / f64::from(iterations));
        within(start)?;
    }
    samples.sort_by(f64::total_cmp);
    Ok(
        json!({"name":name,"unit":"ns_per_operation","median_batch_mean":samples[15],
        "p95_batch_mean":samples[29],"samples":SAMPLES,"iterations_per_sample":iterations,
        "warmup_operations":WARMUP,"workload":workload}),
    )
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
struct Corpus {
    name: &'static str,
    paths: Vec<String>,
}
fn corpora() -> Vec<Corpus> {
    let ascii = [
        "src/Candidate_Checks.rs",
        "assets/MODEL.bin",
        "pkg/Cargo.lock",
        "deep/tree/file_0123.py",
    ];
    let mixed = [
        "Straße/Σςİﬃ.rs",
        "quote'雪\u{7}.py",
        "both'\"\\line\n\t.rs",
        "new\u{1c89}.txt",
    ];
    let large_unicode = format!("{}file.rs", "Straße/Σςİﬃ/".repeat(227));
    assert_eq!(large_unicode.len(), 4093);
    vec![
        Corpus {
            name: "ascii_paths",
            paths: ascii.map(str::to_owned).into(),
        },
        Corpus {
            name: "unicode_quotes_controls",
            paths: mixed.map(str::to_owned).into(),
        },
        Corpus {
            name: "near_4k_paths",
            paths: vec!["A".repeat(4096), large_unicode],
        },
    ]
}
fn unicode_oracle(corpora: &[Corpus]) {
    assert_eq!(candidate_unicode::UNICODE_VERSION, (15, 0, 0));
    let expected_fold = [
        "strasse/σσi\u{307}ffi.rs",
        "quote'雪\u{7}.py",
        "both'\"\\line\n\t.rs",
        "new\u{1c89}.txt",
    ];
    let expected_repr = [
        "'Straße/Σςİﬃ.rs'",
        "\"quote'雪\\x07.py\"",
        r#"'both\'"\\line\n\t.rs'"#,
        "'new\\u1c89.txt'",
    ];
    for ((path, fold), repr) in corpora[1]
        .paths
        .iter()
        .zip(expected_fold)
        .zip(expected_repr)
    {
        assert_eq!(candidate_unicode::casefold(path).unwrap(), fold);
        assert_eq!(candidate_unicode::python_repr(path).unwrap(), repr);
    }
    assert_eq!(
        candidate_unicode::casefold(&corpora[0].paths[0]).unwrap(),
        "src/candidate_checks.rs"
    );
    assert_eq!(
        candidate_unicode::python_repr(&corpora[0].paths[0]).unwrap(),
        "'src/Candidate_Checks.rs'"
    );
    assert_eq!(
        candidate_unicode::casefold(&corpora[2].paths[0]).unwrap(),
        "a".repeat(4096)
    );
    assert_eq!(
        candidate_unicode::python_repr(&corpora[2].paths[0]).unwrap(),
        format!("'{}'", "A".repeat(4096))
    );
    assert!(candidate_unicode::casefold(&"A".repeat(4097)).is_err());
    assert!(candidate_unicode::python_repr(&"A".repeat(4097)).is_err());
}
fn unicode_samples(start: Instant) -> Result<Vec<Value>> {
    let corpora = corpora();
    unicode_oracle(&corpora);
    let mut results = Vec::with_capacity(6);
    for corpus in &corpora {
        let lengths: Vec<_> = corpus.paths.iter().map(String::len).collect();
        let scalars: Vec<_> = corpus
            .paths
            .iter()
            .map(|path| path.chars().count())
            .collect();
        let metadata = json!({"paths":corpus.paths.len(),"input_bytes":lengths.iter().sum::<usize>(),
            "path_bytes":lengths,"path_scalars":scalars,"input_sha256":sha(&serde_json::to_vec(&corpus.paths)?),
            "operation":"one complete corpus, including output allocation and drop","setup_outside_timing":true});
        for (operation, function) in [
            (
                "casefold",
                candidate_unicode::casefold as fn(&str) -> anyhow::Result<String>,
            ),
            (
                "python_repr",
                candidate_unicode::python_repr as fn(&str) -> anyhow::Result<String>,
            ),
        ] {
            let name = format!("{operation}_{}", corpus.name);
            results.push(measure(&name, 100, metadata.clone(), start, || {
                for path in &corpus.paths {
                    black_box(function(black_box(path)).unwrap());
                }
            })?);
        }
    }
    Ok(results)
}
fn path(index: usize) -> String {
    match index {
        0..64 => format!("protected/unit_{index:04}.rs"),
        64..96 => format!("assets/unit_{index:04}.bin"),
        _ => format!("src/unit_{index:04}.rs"),
    }
}
fn integrity_payload() -> Value {
    let entries: Vec<_> = (0..1024)
        .map(|index| {
            let path = path(index);
            json!({"folded":candidate_unicode::casefold(&path).unwrap(),
            "repr":candidate_unicode::python_repr(&path).unwrap(),"path":path,"mode":"100644"})
        })
        .collect();
    let changes: Vec<_> = (0..128).map(|index|json!({"path":path(index),
        "old_path":if (96..112).contains(&index){Some(format!("protected/old_{index:04}.rs"))}else{None},
        "status":match index{0..32=>"M",32..96=>"A",96..112=>"R100",_=>"M"},
        "new_mode":"100644","new_oid":"a".repeat(40),"classes":if(64..96).contains(&index){vec!["binary"]}else{vec!["source"]},
        "deleted":false,"exists":index<112,"size":if(64..96).contains(&index){4096}else{128}})).collect();
    json!({"entries":entries,"changes":changes,"surface":"manual","max_file_bytes":1024,"max_binary_bytes":2048,
        "protected_globs":["protected/*","generated/**/manifest?.json","release/[ab]*.lock","models/**/champion.json",
            "reports/*/status?.json","src/**/[!t]*.py","keep/*.toml","assets/**/pin.json"]})
}
fn integrity_oracle(payload: &Value) -> Value {
    let output = candidate_checks::evaluate("candidate-integrity", payload).unwrap();
    assert_eq!(
        output["metrics"],
        json!({"tree_entries":1024,"changes":128})
    );
    let findings = output["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 128);
    for finding in &findings[..32] {
        assert_eq!(finding["rule_id"], "protected-overwrite");
        assert_eq!(finding["severity"], "high");
    }
    for pair in findings[32..96].as_chunks::<2>().0 {
        assert_eq!(pair[0]["rule_id"], "oversized-artifact");
        assert_eq!(
            pair[0]["evidence"],
            json!({"size_bytes":4096,"limit_bytes":2048})
        );
        assert_eq!(pair[1]["rule_id"], "binary-admission");
    }
    for finding in &findings[96..112] {
        assert_eq!(finding["rule_id"], "protected-delete-or-move");
        assert_eq!(finding["severity"], "critical");
    }
    for finding in &findings[112..] {
        assert_eq!(finding["rule_id"], "incomplete-snapshot");
        assert_eq!(finding["severity"], "critical");
    }
    assert_eq!(findings[0]["path"], "protected/unit_0000.rs");
    assert_eq!(findings[96]["path"], "protected/old_0096.rs");
    assert_eq!(findings[127]["path"], "src/unit_0127.rs");
    output
}
fn integrity_sample(start: Instant) -> Result<Value> {
    let payload = integrity_payload();
    let expected = integrity_oracle(&payload);
    let encoded = serde_json::to_vec(&payload)?;
    let metadata = json!({"tree_entries":1024,"changes":128,"protected_globs":8,"findings":128,
        "input_json_bytes":encoded.len(),"input_sha256":sha(&encoded),"output_sha256":sha(&serde_json::to_vec(&expected)?),
        "included":"admission, borrowed payload decoding, tree map, prepared glob compilation/matching, findings and JSON Value output allocation/drop",
        "excluded":"payload construction, Unicode entry metadata construction, JSON text encoding, filesystem, sandbox, process execution, fingerprints and assessment"});
    measure(
        "candidate_integrity_1024_entries_128_changes",
        20,
        metadata,
        start,
        || {
            black_box(
                candidate_checks::evaluate("candidate-integrity", black_box(&payload)).unwrap(),
            );
        },
    )
}
fn main() -> Result<()> {
    let start = Instant::now();
    let binary = executable_sha(start)?;
    let mut results = unicode_samples(start)?;
    results.push(integrity_sample(start)?);
    within(start)?;
    println!(
        "{}",
        json!({"schema_version":1,"comparison":"current_only","isolated_host":false,
        "profile":if cfg!(debug_assertions){"debug"}else{"release"},"unicode_version":"15.0.0",
        "binary_sha256":binary,"cooperative_deadline_seconds":30,"scope":"native pure in-process APIs; no source/snapshot filesystem or sandbox/process work",
        "results":results})
    );
    Ok(())
}
