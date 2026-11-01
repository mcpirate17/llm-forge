//! Current-only pure API measurements; no tool, filesystem planning or sandbox.
use anyhow::{ensure, Result};
use conductor_native::{
    candidate_clang_args::{rebase, Command, InputKind, Mapping, MappingKind},
    candidate_clang_db::{parse_database, Database},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    hint::black_box,
    path::Path,
    time::{Duration, Instant},
};
const DIRECTORY: &str = "/original/project/build";
const FILE: &str = "../src/unit.c";
const SAMPLES: usize = 31;
const ITERATIONS: usize = 20;
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn arguments(file: &str, define: &str) -> Vec<String> {
    [
        "cc",
        "-std=c17",
        "-Wall",
        "-O2",
        define,
        "-I../include",
        "-isystem",
        "/original/sdk/include",
        "-include",
        "../include/config.h",
        "-MMD",
        "-MF",
        "unit.d",
        "-c",
        file,
        "-o",
        "unit.o",
    ]
    .iter()
    .map(|arg| (*arg).into())
    .collect()
}
fn database_bytes() -> Result<Vec<u8>> {
    let mut records = Vec::with_capacity(1000);
    for tu in 0..500 {
        let file = format!("../src/module{tu:03}/unit_雪.c");
        let args = arguments(&file, "-DDEBUG");
        records.push(json!({"directory":DIRECTORY,"file":file,"arguments":args}));
        let args = arguments(&file, "-DNDEBUG");
        records.push(json!({"directory":DIRECTORY,"file":file,"command":args.join(" ")}));
    }
    Ok(serde_json::to_vec(&records)?)
}
fn mappings() -> Vec<Mapping> {
    vec![
        Mapping {
            original: "/original/project".into(),
            admitted: "/snapshot/project".into(),
            kind: MappingKind::Source,
        },
        Mapping {
            original: "/original/sdk".into(),
            admitted: "/operator/sdk".into(),
            kind: MappingKind::External,
        },
    ]
}
fn database_oracle(db: &Database) -> Result<()> {
    ensure!(db.entries.len() == 1000, "database record count oracle");
    let unique: BTreeSet<_> = db.entries.iter().map(|entry| &entry.file).collect();
    ensure!(unique.len() == 500, "translation-unit count oracle");
    ensure!(
        db.entries[0].file == "../src/module000/unit_雪.c"
            && db.entries[999].file == "../src/module499/unit_雪.c",
        "database order oracle"
    );
    for pair in db.entries.chunks_exact(2) {
        ensure!(
            pair[0].file == pair[1].file
                && pair.iter().all(|entry| entry.directory == DIRECTORY
                    && entry.arguments.len() == 17
                    && entry.arguments[0] == "cc"),
            "retained config oracle"
        );
        ensure!(
            pair[0].arguments[4] == "-DDEBUG" && pair[1].arguments[4] == "-DNDEBUG",
            "config distinction oracle"
        );
    }
    Ok(())
}
fn command_oracle(command: &Command) -> Result<()> {
    ensure!(
        command.directory == Path::new("/snapshot/project/build")
            && command.file == Path::new("/snapshot/project/src/unit.c"),
        "command path oracle"
    );
    ensure!(
        command.arguments
            == [
                "clang",
                "-std=c17",
                "-Wall",
                "-O2",
                "-DNDEBUG",
                "-I",
                "/snapshot/project/include",
                "-isystem",
                "/operator/sdk/include",
                "-include",
                "/snapshot/project/include/config.h",
                "/snapshot/project/src/unit.c"
            ],
        "exact normalized argv oracle"
    );
    ensure!(
        command.inputs.len() == 5
            && command
                .inputs
                .iter()
                .any(|input| input.path == Path::new("/operator/sdk/include")
                    && input.kind == InputKind::Directory
                    && input.mapping == MappingKind::External)
            && command.inputs.iter().any(|input| input.path
                == Path::new("/snapshot/project/include/config.h")
                && input.kind == InputKind::File
                && input.mapping == MappingKind::Source),
        "typed input oracle"
    );
    Ok(())
}
fn command_value(command: &Command) -> Value {
    let inputs: Vec<_> = command
        .inputs
        .iter()
        .map(|input| {
            json!({"path":input.path,
        "kind":format!("{:?}",input.kind),"mapping":format!("{:?}",input.mapping)})
        })
        .collect();
    json!({"directory":command.directory,"file":command.file,"arguments":command.arguments,"inputs":inputs})
}
fn measure<T>(
    name: &str,
    start: Instant,
    mut call: impl FnMut() -> Result<T, String>,
) -> Result<Value> {
    for _ in 0..5 {
        drop(black_box(call().map_err(anyhow::Error::msg)?));
    }
    let mut times = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        ensure!(
            start.elapsed() < Duration::from_secs(30),
            "cooperative benchmark deadline"
        );
        let began = Instant::now();
        for _ in 0..ITERATIONS {
            // Explicit drop is inside the timed loop: allocations are freed here.
            drop(black_box(call().map_err(anyhow::Error::msg)?));
        }
        times.push(began.elapsed().as_secs_f64() * 1_000_000_000.0 / ITERATIONS as f64);
    }
    ensure!(
        start.elapsed() < Duration::from_secs(30),
        "cooperative benchmark deadline"
    );
    times.sort_by(f64::total_cmp);
    Ok(
        json!({"name":name,"warmup_calls":5,"samples":SAMPLES,"iterations_per_sample":ITERATIONS,
        "timed_calls":SAMPLES*ITERATIONS,"median_sample_mean_ns":times[15],
        "p95_sample_mean_ns":times[29],"mean_ns":times.iter().sum::<f64>()/SAMPLES as f64}),
    )
}
fn main() -> Result<()> {
    let started = Instant::now();
    let bytes = database_bytes()?;
    let db = parse_database(&bytes).map_err(anyhow::Error::msg)?;
    database_oracle(&db)?;
    let args = arguments(FILE, "-DNDEBUG");
    let maps = mappings();
    let command = rebase(DIRECTORY, FILE, &args, &maps, "clang").map_err(anyhow::Error::msg)?;
    command_oracle(&command)?;
    let rebase_input = json!({"directory":DIRECTORY,"file":FILE,"arguments":args,"compiler":"clang",
        "mappings":[{"original":"/original/project","admitted":"/snapshot/project","kind":"source"},
            {"original":"/original/sdk","admitted":"/operator/sdk","kind":"external"}]});
    let measurements = vec![
        measure("parse_1000_records_500_translation_units", started, || {
            parse_database(black_box(&bytes))
        })?,
        measure("rebase_17_argument_compile_record", started, || {
            rebase(DIRECTORY, FILE, black_box(&args), black_box(&maps), "clang")
        })?,
    ];
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "scope":"current-only synthetic pure API; no inferred baseline improvement or CLI latency claim",
            "timing":"API including Result handling and object destruction; parse includes UTF8/JSON decoding/tokenization; excludes fixture/oracle/serialization/hash/filesystem/process/sandbox/CLI costs",
            "database":{"records":1000,"translation_units":500,"configs_per_tu":2,"arguments_records":500,
                "command_records":500,"input_bytes":bytes.len(),"input_sha256":sha(&bytes),
                "normalized_sha256":sha(&serde_json::to_vec(&db)?)},
            "rebase":{"arguments":args.len(),"mappings":maps.len(),"input_sha256":sha(&serde_json::to_vec(&rebase_input)?),
                "output_sha256":sha(&serde_json::to_vec(&command_value(&command))?)},
            "build_profile":if cfg!(debug_assertions){"debug"}else{"release-like"},
            "measurements":measurements,"elapsed_seconds":started.elapsed().as_secs_f64(),"deadline_seconds":30
        }))?
    );
    Ok(())
}
