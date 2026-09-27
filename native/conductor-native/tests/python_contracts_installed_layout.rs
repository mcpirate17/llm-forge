#![cfg(feature = "python-compat-tests")]
//! Rust-owned parity for test_installed_layout.py (one integration case).

#[path = "python_contracts/package_resources_support.rs"]
#[allow(dead_code)]
mod fixtures;

use fixtures::rust_test_temp;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

fn python() -> PathBuf {
    std::env::var_os("PYO3_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("python3"))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("resolve Forge repository root")
}

fn install_site(site: &Path) {
    let output = Command::new("timeout")
        .args(["120", "uv", "pip", "install", "--no-deps", "--target"])
        .arg(site)
        .arg(repo_root())
        .output()
        .expect("run uv pip install");
    assert_ne!(
        output.status.code(),
        Some(124),
        "uv pip install exceeded 120s timeout"
    );
    assert!(
        output.status.success(),
        "uv pip install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fake_host(root: &Path) -> PathBuf {
    let host = root.join("host");
    fs::create_dir(&host).unwrap();
    let git = Command::new("git")
        .args(["init", "--quiet"])
        .arg(&host)
        .output()
        .expect("initialize isolated fake host");
    assert!(
        git.status.success(),
        "git init: {}",
        String::from_utf8_lossy(&git.stderr)
    );
    fs::write(host.join("Makefile"), "gate:\n\t@echo gate\n").unwrap();
    fs::write(
        host.join("pyproject.toml"),
        "[tool.conductor.session]\npreamble = [\"INSTALLED-LAYOUT-FAKE-HOST-MISSION: ship the resolver.\"]\nstanding_mandates = [\"MAND-1: never resolve against site-packages\"]\n",
    )
    .unwrap();
    fs::create_dir_all(host.join("research/notes")).unwrap();
    let campaigns = host.join("tasks/mutation_campaigns");
    fs::create_dir_all(&campaigns).unwrap();
    fs::write(
        campaigns.join("registry.json"),
        r#"{"campaigns": [{"name": "fake-campaign", "status": "active"}]}"#,
    )
    .unwrap();
    fs::create_dir(host.join(".claude")).unwrap();
    host
}

fn run(module_args: &[&str], cwd: &Path, site: &Path) -> Output {
    let output = Command::new("timeout")
        .arg("30")
        .arg(python())
        .arg("-m")
        .args(module_args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("PYTHONPATH", site)
        .output()
        .expect("run installed CLI");
    assert_ne!(
        output.status.code(),
        Some(124),
        "installed CLI exceeded 30s timeout"
    );
    output
}

fn assert_no_site_leak(result: &Output, site: &Path, label: &str) -> String {
    let stdout = String::from_utf8(result.stdout.clone()).unwrap();
    let stderr = String::from_utf8(result.stderr.clone()).unwrap();
    let site = site.to_string_lossy();
    for (stream_name, stream) in [("stdout", stdout.as_str()), ("stderr", stderr.as_str())] {
        for line in stream.lines() {
            assert!(
                !line.contains(site.as_ref()),
                "{label} {stream_name} leaks install site path: {line:?}"
            );
        }
    }
    assert!(
        result.status.success(),
        "{label} exited {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        result.status.code()
    );
    stdout
}

#[test]
fn installed_layout_resolves_host_not_site() {
    let case = rust_test_temp("installed-layout");
    let site = case.root().join("site");
    fs::create_dir(&site).unwrap();
    install_site(&site);
    let host = fake_host(case.root());
    let started = Instant::now();

    let preamble = run(&["conductor.session_preamble", "text"], &host, &site);
    let output = assert_no_site_leak(&preamble, &site, "session_preamble");
    assert!(output.contains("INSTALLED-LAYOUT-FAKE-HOST-MISSION"));

    let dump = run(&["conductor.active_state", "dump"], &host, &site);
    assert_no_site_leak(&dump, &site, "active_state dump");

    let gate_help = run(&["conductor.gate", "--help"], &host, &site);
    assert_no_site_leak(&gate_help, &site, "gate --help");

    assert!(
        started.elapsed().as_secs_f64() < 20.0,
        "installed-layout subprocess run took {:.1}s; tighten it or raise the bound deliberately",
        started.elapsed().as_secs_f64()
    );
}
