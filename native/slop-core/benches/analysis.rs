//! Warm native parse/scan timings. Synthetic source, no model or Python process.
use sha2::{Digest, Sha256};
use slop_core::{
    ablation, detector_scan, hot_loop_scan, style_scan, test_value_scan, verbosity_scan,
};
use std::{hint::black_box, time::Instant};

fn measure(name: &str, source: &str, mut operation: impl FnMut() -> usize) {
    for _ in 0..5 {
        black_box(operation());
    }
    let mut samples = Vec::new();
    let mut result_count = 0;
    for _ in 0..31 {
        let start = Instant::now();
        for _ in 0..10 {
            result_count = black_box(operation());
        }
        samples.push(start.elapsed().as_secs_f64() * 1e6 / 10.0);
    }
    samples.sort_by(f64::total_cmp);
    println!(
        "{}",
        serde_json::json!({"workload":name,"source_bytes":source.len(),
        "source_sha256":format!("{:x}", Sha256::digest(source)),"batch_mean_us_p50":samples[15],
        "batch_mean_us_p95":samples[29],"iterations":310,"result_count":result_count,
        "scope":"warm synthetic source; current native implementation only"})
    );
}

fn main() {
    let source: String = (0..128).map(|i| format!(
        "def function_{i}(x: Tensor):\n    try:\n        for j in range(len(x)):\n            print(x[j].item())\n        return x.clamp(min=0)\n    except Exception:\n        return 0\n\n"
    )).collect();
    measure("fallback_128_functions", &source, || {
        detector_scan::scan_source("models/a.py", black_box(&source))
            .unwrap()
            .len()
    });
    measure("five_python_scans_128_functions", &source, || {
        let parsed = slop_core::source::PythonSource::new(black_box(&source)).unwrap();
        detector_scan::scan_parsed("models/a.py", &parsed).len()
            + style_scan::scan_parsed("models/a.py", &parsed).len()
            + test_value_scan::scan_parsed("models/a.py", &parsed).len()
            + verbosity_scan::scan_parsed("models/a.py", &parsed).len()
            + hot_loop_scan::scan_parsed("models/a.py", &parsed).len()
    });
    let selected = ablation::select(Some(&["drop_clamp".into()]), &[]).unwrap();
    measure("checked_ablation_collection_128_functions", &source, || {
        ablation::collect(black_box(&source), &selected)
            .unwrap()
            .len()
    });
    let small = "def f(x):\n    return x.clamp(min=0)\n";
    let proposal = ablation::collect(small, &selected).unwrap().remove(0);
    measure("checked_ablation_apply_small", small, || {
        ablation::apply(black_box(small), &proposal).unwrap().len()
    });
}
