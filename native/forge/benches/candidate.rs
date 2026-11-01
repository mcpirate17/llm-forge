//! Current-only native candidate costs. No interpreter, external Git, or model.
use forge::{
    candidate_receipt, candidate_snapshot,
    candidate_source::{Kind, Source},
};
use std::{hint::black_box, path::PathBuf, time::Instant};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn measure(name: &str, iterations: u32, mut operation: impl FnMut()) {
    for _ in 0..100 {
        operation();
    }
    let mut batches = Vec::new();
    for _ in 0..31 {
        let start = Instant::now();
        for _ in 0..iterations {
            operation();
        }
        batches.push(start.elapsed().as_nanos() as f64 / f64::from(iterations));
    }
    batches.sort_by(f64::total_cmp);
    println!(
        "{name}: median_ns={:.2} p95_ns={:.2} batches=31 iterations={iterations}",
        batches[15], batches[29]
    );
}

fn main() {
    let root = std::env::temp_dir().join(forge::resource_id("forge-candidate-bench").unwrap());
    std::fs::create_dir(&root).unwrap();
    let fixture = Fixture(root);
    let repository = git2::Repository::init(fixture.0.join("repo")).unwrap();
    std::fs::write(fixture.0.join("repo/file"), "fn example() {}\n").unwrap();
    let mut index = repository.index().unwrap();
    index.add_path(std::path::Path::new("file")).unwrap();
    index.write().unwrap();
    let tree = index.write_tree().unwrap().to_string();
    let source = Source::open(&fixture.0.join("repo")).unwrap();
    let reference: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../tests/fixtures/candidate_receipt/reference.json"
    ))
    .unwrap();
    let receipt = serde_json::to_vec(&reference["digest_valid_failed_receipt"]).unwrap();
    let output = fixture.0.join("snapshot");
    measure("receipt_digest", 1000, || {
        black_box(candidate_receipt::parse_and_verify(black_box(&receipt)).unwrap());
    });
    measure("staged_candidate_one_file", 100, || {
        black_box(source.resolve(Kind::Index, "HEAD", None).unwrap());
    });
    measure("snapshot_preflight_one_file", 100, || {
        black_box(
            candidate_snapshot::preflight(
                &source,
                &tree,
                &output,
                candidate_snapshot::Limits::default(),
            )
            .unwrap(),
        );
    });
    classification();
}

fn classification() {
    use conductor_native::candidate_policy::{classify, PreparedClassifier};
    use serde_json::json;
    let globs = json!({"class_globs":{"rust":["native/*.rs"],"test":["native/*/test_*.rs","native/*_test.rs"],
        "docs":["docs/*.md"],"source":["native/*","src/*"]},"generated_globs":["native/generated/*"],"high_risk_globs":["native/security/*"]});
    let changes: Vec<_> = (0..128).map(|index| json!({"path":format!("native/module{index}/test_file.rs"),
        "old_path":format!("src/module{index}/test_file.rs"),"old_mode":"100644","new_mode":"100644"})).collect();
    let mut index = 0usize;
    measure("policy_stateless_change", 1000, || {
        black_box(classify(&changes[index % changes.len()], &globs).unwrap());
        index += 1;
    });
    let mut prepared = PreparedClassifier::new(&globs).unwrap();
    let mut index = 0usize;
    measure("policy_prepared_change", 1000, || {
        black_box(prepared.classify(&changes[index % changes.len()]).unwrap());
        index += 1;
    });
}
