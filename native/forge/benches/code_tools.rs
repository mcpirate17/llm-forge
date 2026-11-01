//! Current-only warm measurements. Includes parsing/IO where named, never model latency.
use conductor_native::syntax::{self, Language};
use forge::{
    patch::{self, Edit, Request},
    source_file::SourceRoot,
};
use std::{hint::black_box, time::Instant};

fn measure(name: &str, bytes: usize, mut operation: impl FnMut()) {
    for _ in 0..20 {
        operation();
    }
    let mut samples = Vec::with_capacity(31);
    for _ in 0..31 {
        let start = Instant::now();
        for _ in 0..100 {
            operation();
        }
        samples.push(start.elapsed().as_nanos() as f64 / 100.0);
    }
    samples.sort_by(f64::total_cmp);
    println!("{{\"workload\":{name:?},\"bytes\":{bytes},\"iterations\":3100,\"batch_mean_ns_p50\":{},\"batch_mean_ns_p95\":{}}}", samples[15], samples[29]);
}

fn main() {
    let small = "fn helper() -> u32 { 42 }\nfn caller() -> u32 { helper() }\n";
    measure("rust_inspect_small", small.len(), || {
        black_box(syntax::inspect(black_box(small), Language::Rust)).unwrap();
    });
    let large = (0..1024)
        .map(|i| format!("fn function_{i}(value: u32) -> u32 {{ value.wrapping_add({i}) }}\n"))
        .collect::<String>();
    measure("rust_inspect_1024_functions", large.len(), || {
        black_box(syntax::inspect(black_box(&large), Language::Rust)).unwrap();
    });
    let inspection = syntax::inspect(&large, Language::Rust).unwrap();
    measure("fold_1024_functions", large.len(), || {
        black_box(inspection.fold(black_box(&large))).unwrap();
    });
    let directory = conductor_native::state_paths::state_root()
        .unwrap()
        .join(format!("bench-code-{}", std::process::id()));
    std::fs::create_dir_all(directory.parent().unwrap()).unwrap();
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("lib.rs"), small).unwrap();
    let root = SourceRoot::open(&directory).unwrap();
    let request = Request {
        path: "lib.rs".into(),
        expected_sha256: syntax::fingerprint(small),
        language: None,
        edit: Edit::Replace {
            search: "42".into(),
            replacement: "43".into(),
            expected_matches: 1,
        },
    };
    measure("read_hash_prepare_validate_patch", small.len(), || {
        black_box(patch::prepare(&root, black_box(&request))).unwrap();
    });
    std::fs::remove_dir_all(directory).unwrap();
}
