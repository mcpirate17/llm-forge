//! Current-only pinned GPT-2 tokenization and recoverable compaction costs.
use anyhow::{ensure, Context, Result};
use forge::context_store::{Store, DEFAULT_QUOTA};
use forge::context_window::{compact, Entry, Kind, Request};
use forge::tokenizer::{Config, Counter};
use serde_json::json;
use std::{fs, hint::black_box, path::PathBuf, time::Instant};

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_dir_all(&self.0) {
            eprintln!("context benchmark cleanup: {e}");
        }
    }
}
fn measure(
    name: &str,
    bytes: usize,
    operations: usize,
    mut operation: impl FnMut() -> Result<()>,
) -> Result<()> {
    operation()?;
    let mut samples = Vec::new();
    for _ in 0..11 {
        let start = Instant::now();
        for _ in 0..operations {
            operation()?;
        }
        samples.push(start.elapsed().as_nanos() / operations as u128);
    }
    samples.sort_unstable();
    println!(
        "{}",
        json!({"workload":name,"input_bytes":bytes,"batches":11,"operations_per_batch":operations,
        "median_batch_mean_ns":samples[5],"p95_batch_mean_ns":samples[10],"samples":samples})
    );
    Ok(())
}
fn entry(id: &str, kind: Kind, content: String) -> Entry {
    Entry {
        id: id.into(),
        kind,
        content,
        protected: false,
        exchange: None,
        language: None,
    }
}
fn main() -> Result<()> {
    let path = std::env::var_os("FORGE_GPT2_TOKENIZER")
        .context("set FORGE_GPT2_TOKENIZER to the pinned offline artifact")?;
    let config = Config {
        path: path.into(),
        tokenizer_sha256: conductor_native::sha256_hex::parse(
            "8414cab924d8b9b33013f0d221c5862f365ee9be39c5c2bfae8a5a9e970478a6",
        )
        .map_err(anyhow::Error::msg)?,
        model_sha256: [7; 32],
        add_special_tokens: false,
    };
    measure(
        "load_and_verify_pinned_gpt2_warm_filesystem",
        fs::metadata(&config.path)?.len() as usize,
        1,
        || {
            black_box(Counter::load(&config)?);
            Ok(())
        },
    )?;
    let counter = Counter::load(&config)?;
    println!(
        "{}",
        json!({"measurement":"current-only; shared CPU host; one native caller; no BPE word cache; no model inference",
        "tokenizer":counter.identity(),"model_identity":"tokenizer-only fixture identity; no weights loaded"})
    );
    for (size, operations) in [(4096, 20), (65536, 5), (1048576, 1)] {
        let mut source =
            "fn answer(value: u32) -> u32 { value + 42 } // 東京 🦀\n".repeat(size / 64 + 1);
        while source.len() < size {
            source.push_str("fn main() {}\n");
        }
        let mut end = size;
        while !source.is_char_boundary(end) {
            end -= 1;
        }
        source.truncate(end);
        let count = counter.count(&source)?;
        measure(
            "count_exact_rendered_source",
            source.len(),
            operations,
            || {
                ensure!(
                    black_box(counter.count_uncached(black_box(&source))?) == count,
                    "unstable tokens"
                );
                Ok(())
            },
        )?;
        measure(
            "count_repeated_text_hash_cache",
            source.len(),
            operations * 10,
            || {
                ensure!(
                    black_box(counter.count(black_box(&source))?) == count,
                    "cached count changed"
                );
                Ok(())
            },
        )?;
    }
    compaction(&counter)
}

fn compaction(counter: &Counter) -> Result<()> {
    let base = conductor_native::state_paths::state_root()?
        .join(format!("bench-context-{}", std::process::id()));
    fs::create_dir_all(base.parent().context("benchmark root has no parent")?)?;
    fs::create_dir(&base)?;
    let _directory = Directory(base.clone());
    let store = Store::create(&base.join("recovery"), DEFAULT_QUOTA)?;
    let mut entries = vec![entry(
        "policy",
        Kind::Policy,
        "Retain original instructions and diagnostic errors.".into(),
    )];
    for i in 0..32 {
        entries.push(entry(
            &format!("history-{i}"),
            Kind::Text,
            "Prior tool result: hello world!\n".repeat(128),
        ));
    }
    let mut code = entry(
        "code",
        Kind::Code,
        format!(
            "pub fn answer() -> usize {{\n{}42\n}}",
            "let _ = \"東京🦀\";\n".repeat(1024)
        ),
    );
    code.language = Some(conductor_native::syntax::Language::Rust);
    entries.push(code);
    entries.push(entry(
        "log",
        Kind::Log,
        format!(
            "{}error[E0308]: wrong type\n --> src/main.rs:5:9\n{}",
            "Compiling dependency\n".repeat(2048),
            "build progress\n".repeat(1024)
        ),
    ));
    entries.push(entry(
        "latest",
        Kind::Text,
        "Fix the compiler error while retaining the API.".into(),
    ));
    let request = Request {
        entries,
        max_tokens: 4096,
        max_bytes: 32768,
        reserved_tokens: 512,
        keep_recent: 1,
    };
    let start = Instant::now();
    let first = compact(counter, &store, &request)?;
    ensure!(!first.protected_overflow, "benchmark must fit");
    println!(
        "{}",
        json!({"workload":"first_compaction_including_fsync","elapsed_ns":start.elapsed().as_nanos(),"input_bytes":first.input_bytes,
        "input_tokens":first.input_tokens,"output_tokens":first.output_tokens,"output_bytes":first.output_bytes,"omitted_groups":first.omitted_ids.len()})
    );
    measure(
        "compaction_reusing_verified_recovery_artifact",
        first.input_bytes,
        3,
        || {
            let out = black_box(compact(counter, &store, &request)?);
            ensure!(!out.protected_overflow, "unexpected overflow");
            Ok(())
        },
    )?;
    let status = fs::read_to_string("/proc/self/status")?;
    println!(
        "{}",
        json!({"memory":status.lines().filter(|line|line.starts_with("VmRSS:")||line.starts_with("VmHWM:")||line.starts_with("Threads:")).collect::<Vec<_>>()})
    );
    Ok(())
}
