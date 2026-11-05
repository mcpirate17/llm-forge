//! Single-observation artifact; never a speedup or task-benefit claim.
use super::process_usage::process;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::Instant,
};
pub struct Report {
    file: Option<fs::File>,
    pub data: Value,
}
impl Report {
    pub fn new() -> Self {
        let path = std::path::PathBuf::from(
            std::env::var_os("FORGE_TEST_OUTBOUND_REPORT")
                .expect("measurement requires explicit FORGE_TEST_OUTBOUND_REPORT"),
        );
        assert!(path.is_absolute(), "report path must be absolute");
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .expect("report must not exist");
        let names = [
            "service_ready",
            "stdio_initialize",
            "tools_discovery",
            "input_capture",
            "input_recheck",
            "adapter_eof",
            "service_cleanup",
        ];
        let mut phases: Vec<Value> = names
            .iter()
            .map(|name| json!({"name":name,"state":"unstarted"}))
            .collect();
        for i in 0..32 {
            phases.push(json!({"name":format!("call_{i}"),"state":"unstarted"}));
        }
        Self {
            file: Some(file),
            data: json!({"schema":"forge.outbound-mcp-observation.v1","valid":false,
            "phases":phases,"calls":[],"cleanup":{"verified":false},"scope":
            "current-only fixed 1 first + 31 sequential warm roundtrips, zero retries; transport includes send/receive framing and JSON conversion, oracle separate; no before/after or task-benefit claim"}),
        }
    }
    pub fn start(&mut self, name: &str) -> Instant {
        let phase = self.phase(name);
        assert_eq!(phase["state"], "unstarted");
        phase["state"] = json!("running");
        Instant::now()
    }
    fn phase(&mut self, name: &str) -> &mut Value {
        self.data["phases"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|p| p["name"] == name)
            .unwrap()
    }
    pub fn done(&mut self, name: &str, start: Instant) {
        let phase = self.phase(name);
        assert_eq!(phase["state"], "running");
        phase["state"] = json!("completed");
        phase["elapsed_ns"] = json!(start.elapsed().as_nanos() as u64);
    }
    pub fn censored(&mut self, name: &str) {
        self.phase(name)["state"] = json!("censored");
    }
    pub fn deadline(&mut self) {
        for phase in self.data["phases"].as_array_mut().unwrap() {
            if phase["state"] == "running" {
                phase["state"] = json!("censored");
            }
        }
        self.data["failure_scope"] = json!(
            "cooperative observation deadline; cleanup unverified unless explicitly recorded"
        );
    }
    pub fn finish(mut self) {
        assert!(self.data["phases"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["state"] == "completed"));
        self.data["valid"] = json!(true);
        self.persist().expect("writing observation artifact");
    }
    fn persist(&mut self) -> std::io::Result<()> {
        let mut counts = json!({"planned":39,"completed":0,"failed":0,"censored":0,"unstarted":0});
        for phase in self.data["phases"].as_array_mut().unwrap() {
            if phase["state"] == "running" {
                phase["state"] = json!(if std::thread::panicking() {
                    "failed"
                } else {
                    "censored"
                });
            }
            let state = phase["state"].as_str().unwrap();
            let n = counts[state].as_u64().unwrap();
            counts[state] = json!(n + 1);
        }
        counts["started"] = json!(39 - counts["unstarted"].as_u64().unwrap());
        self.data["accounting"] = counts;
        let mut calls =
            json!({"planned":32,"started":0,"completed":0,"failed":0,"censored":0,"unstarted":0});
        for phase in self.data["phases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["name"].as_str().unwrap().starts_with("call_"))
        {
            let state = phase["state"].as_str().unwrap();
            let n = calls[state].as_u64().unwrap();
            calls[state] = json!(n + 1);
        }
        calls["started"] = json!(32 - calls["unstarted"].as_u64().unwrap());
        self.data["call_accounting"] = calls;
        if self.data["valid"] != true && self.data["failure_scope"].is_null() {
            self.data["failure_scope"]=json!("incomplete observation; see test stderr; retained fixture paths are not verified clean");
        }
        let bytes = serde_json::to_vec(&self.data)?;
        if bytes.len() > 131072 {
            return Err(std::io::Error::other("observation exceeds 128KiB"));
        }
        // One create_new owner, one final write: never replace a previous artifact.
        if let Some(mut file) = self.file.take() {
            file.write_all(&bytes)?;
            file.flush()?;
            file.sync_all()?;
        }
        Ok(())
    }
}
impl Drop for Report {
    fn drop(&mut self) {
        if self.file.is_some() {
            if let Err(error) = self.persist() {
                eprintln!("observation artifact incomplete: {error}");
            }
        }
    }
}
pub fn hash_file(path: &Path) -> String {
    let mut file = fs::File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    let mut total = 0;
    loop {
        let n = file.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        total += n;
        assert!(total <= 1024 * 1024 * 1024);
        hash.update(&buf[..n]);
    }
    format!("{:x}", hash.finalize())
}
pub fn identity(state: &Path, binary: &Path, fixture: &Path, request: &Value) -> Value {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let inventory = forge::source_fingerprint::files(&repo).unwrap();
    let git = git2::Repository::open(&repo).unwrap();
    let mut build = serde_json::Map::new();
    for path in [
        "native/forge/Cargo.toml",
        "native/forge/Cargo.lock",
        "native/forge-cli/Cargo.toml",
        "native/forge/tests/native_outbound_mcp.rs",
        "native/forge/tests/support/outbound_mcp_server.rs",
        "native/forge/tests/support/outbound_observation.rs",
        "native/forge/tests/support/process_usage.rs",
        "native/forge/tests/support/mcp_client.rs",
        "native/forge/tests/support/forge_binary.rs",
    ] {
        build.insert(path.into(), json!(hash_file(&repo.join(path))));
    }
    json!({"git_head":git.head().unwrap().target().unwrap().to_string(),
        "forge_binary_sha256":hash_file(binary),"test_binary_sha256":hash_file(&std::env::current_exe().unwrap()),
        "fixture_binary_sha256":hash_file(fixture),"source_inventory_sha256":forge::source_fingerprint::fingerprint(&repo,&inventory).unwrap(),
        "build_inputs":build,"policy_sha256":hash_file(&state.join("policy.json")),
        "allowed_input_sha256":hash_file(&state.parent().unwrap().join("allowed.txt")),
        "request_sha256":format!("{:x}",Sha256::digest(serde_json::to_vec(request).unwrap())),
        "scope":"point-in-time observations; not held immutable identity or compiler authenticity attestation"})
}
pub fn resources(adapter: u32, service: u32, endpoint: Option<u32>) -> Value {
    json!({"driver":process(std::process::id()),"adapter":process(adapter),"service":process(service),
        "endpoint":endpoint.map(process),"clock_ticks_per_second":unsafe{libc::sysconf(libc::_SC_CLK_TCK)},
        "scope":"per-process snapshots, driver/adapter/service after discovery vs after last call; endpoint after first vs after last; endpoint PID is remote-reported and stable, not independently bound to cgroup; cooperative fixture observation only; no tree peak/allocation/per-call CPU claim"})
}
