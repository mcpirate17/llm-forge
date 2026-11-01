//! Spawn and exit observation, including native process startup; current-only timings.
use forge::process_io::ChildExit;
use sha2::{Digest, Sha256};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Instant;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--worker") {
        return;
    }
    let executable = std::env::current_exe().unwrap();
    let digest = format!("{:x}", Sha256::digest(std::fs::read(&executable).unwrap()));
    println!("{{\"binary_sha256\":{digest:?},\"profile\":\"release\",\"isolated_host\":false}}");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut samples = Vec::with_capacity(31);
    runtime.block_on(async {
        for batch in 0..32 {
            let start = Instant::now();
            for _ in 0..50 {
                let mut child = Command::new(&executable)
                    .arg("--worker")
                    .process_group(0)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap();
                let watcher = ChildExit::new(&child).unwrap();
                assert!(watcher.wait().await.unwrap().success());
                assert!(child.wait().unwrap().success());
            }
            if batch > 0 {
                samples.push(start.elapsed().as_nanos() as f64 / 50.0);
            }
        }
    });
    samples.sort_by(f64::total_cmp);
    println!("{{\"workload\":\"native_spawn_pidfd_wait_reap\",\"iterations\":1550,\"batch_mean_ns_p50\":{},\"batch_mean_ns_p95\":{}}}", samples[15], samples[29]);
}
