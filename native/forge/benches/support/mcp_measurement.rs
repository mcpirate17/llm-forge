//! One matched CLI/raw-MCP workload, with retained failure accounting.
#[path = "../../tests/support/mcp_client.rs"]
mod mcp_client;
#[path = "../../tests/support/process_usage.rs"]
mod process_usage;
use anyhow::{ensure, Context, Result};
use process_usage::process;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

pub const BATCHES: usize = 31;
pub const OPERATIONS: usize = 10;
pub const WARMUPS: usize = 10;
const CALLS: usize = 1 + WARMUPS + BATCHES * OPERATIONS;
const CALL_SECONDS: u64 = 2;

pub fn counts(planned: usize) -> Value {
    json!({"planned":planned,"started":0,"completed":0,"failed":0,"censored":0,"unstarted":planned})
}
#[derive(Debug)]
struct Deadline;
impl std::fmt::Display for Deadline {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str("cooperative measurement deadline exceeded")
    }
}
impl std::error::Error for Deadline {}
fn add(value: &mut Value, count: u64) {
    *value = json!(value.as_u64().unwrap() + count);
}
pub fn start(value: &mut Value) {
    add(&mut value["started"], 1);
}
pub fn record<T>(value: &mut Value, result: &Result<T>) {
    let field = match result {
        Ok(_) => "completed",
        Err(error)
            if error.is::<tokio::time::error::Elapsed>()
                || error.is::<forge::process_probe::TimedOut>()
                || error.is::<Deadline>() =>
        {
            "censored"
        }
        Err(_) => "failed",
    };
    add(&mut value[field], 1);
}
fn finalize(value: &mut Value) {
    let started = value["started"].as_u64().unwrap();
    let finished: u64 = ["completed", "failed", "censored"]
        .iter()
        .map(|key| value[*key].as_u64().unwrap())
        .sum();
    add(&mut value["censored"], started - finished);
    value["unstarted"] = json!(value["planned"].as_u64().unwrap() - started);
}
pub fn limit(deadline: Instant) -> Result<Duration> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or(Deadline)?;
    if remaining.is_zero() {
        return Err(Deadline.into());
    }
    Ok(remaining.min(Duration::from_secs(CALL_SECONDS)))
}
pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= 1024 * 1024 * 1024,
        "unbounded identity input"
    );
    let mut hash = Sha256::new();
    let mut bytes = [0; 65536];
    let mut total = 0u64;
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .context("identity input byte overflow")?;
        ensure!(
            total <= 1024 * 1024 * 1024,
            "identity input grew beyond byte bound"
        );
        hash.update(&bytes[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn digest(value: &Value) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}
pub fn input_identity(state: &Path, binary: &Path) -> Result<Value> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let base = state.parent().context("state parent missing")?;
    let mut files = serde_json::Map::new();
    for name in [
        "source/main.rs",
        "query.json",
        "memory.jsonl",
        "memory.mapped",
        "state/policy.json",
    ] {
        files.insert(name.into(), json!(hash_file(&base.join(name))?));
    }
    let mut build_inputs = serde_json::Map::new();
    for name in [
        "native/forge/Cargo.toml",
        "native/forge/Cargo.lock",
        "native/forge-cli/Cargo.toml",
        "native/forge/benches/harness.rs",
        "native/forge/benches/support/mcp_measurement.rs",
        "native/forge/benches/support/memory_fixture.rs",
        "native/forge/tests/support/mcp_client.rs",
        "native/forge/tests/support/forge_binary.rs",
    ] {
        build_inputs.insert(name.into(), json!(hash_file(&repo.join(name))?));
    }
    let inventory = forge::source_fingerprint::files(&repo)?;
    let repository = git2::Repository::discover(&repo)?;
    let head = repository
        .head()?
        .target()
        .context("detached or unborn repository identity missing")?
        .to_string();
    Ok(json!({"head":head,"binary_sha256":hash_file(binary)?,
        "benchmark_binary_sha256":hash_file(&std::env::current_exe()?)?,
        "source_inventory_sha256":forge::source_fingerprint::fingerprint(&repo,&inventory)?,
        "build_input_sha256":build_inputs,"fixture_sha256":files,
        "request_sha256":digest(&call())?,"architecture":std::env::consts::ARCH,
        "scope":"observed source/build inputs and executables; not compiler authenticity attestation"}))
}
pub fn call() -> Value {
    json!({"name":"code_fold","arguments":{"path":"main.rs","max_bytes":32768}})
}
fn children() -> Value {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage) } != 0 {
        return json!({"available":false,"reason":std::io::Error::last_os_error().to_string()});
    }
    let micros = (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as u64 * 1_000_000
        + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as u64;
    json!({"available":true,"cpu_us":micros})
}
fn resources(adapter: u32, service: u32) -> Value {
    json!({"driver":process(std::process::id()),"adapter":process(adapter),"service":process(service),"waited_children":children()})
}
fn delta(before: Value, after: Value) -> Value {
    let mut result = serde_json::Map::new();
    for key in ["driver", "adapter", "service"] {
        let (a, b) = (&before[key], &after[key]);
        let ticks = (a["available"] == true
            && b["available"] == true
            && a["start_ticks"] == b["start_ticks"])
            .then(|| {
                b["cpu_ticks"]
                    .as_u64()?
                    .checked_sub(a["cpu_ticks"].as_u64()?)
            })
            .flatten();
        result.insert(key.into(), json!({"cpu_ticks":ticks,"before":a,"after":b}));
    }
    let micros = after["waited_children"]["cpu_us"]
        .as_u64()
        .and_then(|end| end.checked_sub(before["waited_children"]["cpu_us"].as_u64()?));
    result.insert("waited_child_cpu_us".into(), json!(micros));
    Value::Object(result)
}
fn arm() -> Value {
    json!({"accounting":counts(CALLS),"warmups":WARMUPS,"batches":BATCHES,
    "operations_per_batch":OPERATIONS,"sample_batch_means_ns":[],"resources_per_batch":[]})
}
pub struct Measurement {
    pub report: Value,
    pub deadline: Instant,
    client: Option<mcp_client::Mcp>,
    expected: Value,
    next_id: u64,
}
impl Measurement {
    pub fn new() -> Self {
        let legacy: Vec<_> = ["code_fold", "session_brief", "workspace_recall"]
            .iter()
            .map(|name| json!({"name":name,"accounting":counts(CALLS)}))
            .collect();
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        Self {
            report: json!({"valid":false,"measurement":"current-only matched native CLI/raw MCP; no task or token saving claim",
            "measurement_deadline_seconds_from_driver_start":90,"sample_deadline_seconds":CALL_SECONDS,"service_lifetime_seconds":120,
            "deadline_scope":"cooperative measurement deadline; setup, synchronous filesystem work and cleanup are not preempted. Use an outer forge performance benchmark timeout for a whole-process bound.",
            "identity_scope":"before/after point-in-time hashes; transient replace-and-restore and compiler authenticity are not attested",
            "protocol_accounting":counts(4),"mcp":arm(),"cli":arm(),"unix_diagnostic":legacy,
            "clock_ticks_per_second":if ticks>0 {Some(ticks)} else {None},
            "resource_scope":"per-process /proc warm batch deltas and RSS/HWM observations; no summed tree peak. Waited-child CPU covers reaped children. Outer performance receipt excludes systemd daemon; not total MCP resources.",
            "protocol_accounting_scope":"initialize and two tools/list replies validated; initialized notification counted as completed write, not acknowledged processing",
            "cache_scope":"process-cold setup and first calls; kernel/filesystem caches uncontrolled",
            "cleanup":{"adapter":"not_started","service":"not_stopped"}}),
            deadline: Instant::now() + Duration::from_secs(90),
            client: None,
            expected: Value::Null,
            next_id: 3,
        }
    }
    async fn exchange(&mut self, request: Value, reply: bool) -> Result<Value> {
        let duration = limit(self.deadline)?;
        let client = self.client.as_mut().context("MCP adapter absent")?;
        tokio::time::timeout(duration, async {
            client.try_send(request).await?;
            if reply {
                client.try_receive().await
            } else {
                Ok(Value::Null)
            }
        })
        .await
        .context("MCP sample deadline exceeded")?
    }
    async fn protocol(&mut self, request: Value, reply: bool) -> Result<Value> {
        start(&mut self.report["protocol_accounting"]);
        let method = request["method"].as_str().unwrap_or("").to_owned();
        let id = request.get("id").cloned();
        let result = self.exchange(request, reply).await.and_then(|response| {
            if reply {
                ensure!(
                    response.get("id") == id.as_ref() && response.get("error").is_none(),
                    "MCP protocol request failed: {response}"
                );
                if method == "initialize" {
                    ensure!(
                        response["result"]["protocolVersion"] == "2025-11-25",
                        "MCP version mismatch"
                    );
                }
                if method == "tools/list" {
                    forge::tool_schema::ToolRegistry::from_json(&response["result"].to_string())?
                        .validate_call(&call().to_string(), false)?;
                    if let Some(expected) = self.report["catalog_sha256"].as_str() {
                        ensure!(
                            digest(&response["result"])? == expected,
                            "MCP catalog drifted"
                        );
                    }
                }
            }
            Ok(response)
        });
        record(&mut self.report["protocol_accounting"], &result);
        result
    }
    async fn initialize(&mut self, state: &Path, binary: &Path) -> Result<()> {
        let setup_started = Instant::now();
        self.report["cleanup"]["adapter"] = json!("launch_attempted");
        self.client = Some(mcp_client::Mcp::try_new(binary, &state.join("socket"))?);
        let init=self.protocol(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"forge-harness-benchmark","version":"1"}}}),true).await?;
        ensure!(
            init["id"] == 1 && init["result"]["protocolVersion"] == "2025-11-25",
            "MCP initialization failed: {init}"
        );
        self.report["adapter_launch_initialize_ns"] = json!(setup_started.elapsed().as_nanos());
        self.protocol(
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            false,
        )
        .await?;
        let started = Instant::now();
        let listed = self
            .protocol(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}), true)
            .await?;
        ensure!(
            listed["id"] == 2 && listed["result"]["tools"].is_array(),
            "MCP discovery failed: {listed}"
        );
        self.report["catalog_sha256"] = json!(digest(&listed["result"])?);
        self.report["catalog_bytes"] = json!(serde_json::to_vec(&listed["result"])?.len());
        self.report["discovery_ns"] = json!(started.elapsed().as_nanos());
        self.report["adapter_setup_total_ns"] = json!(setup_started.elapsed().as_nanos());
        Ok(())
    }
    async fn mcp_call(&mut self) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let response = self
            .exchange(
                json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":call()}),
                true,
            )
            .await?;
        ensure!(
            response["id"] == id && response["result"]["isError"] == false,
            "MCP call failed: {response}"
        );
        let output = &response["result"]["structuredContent"]["result"];
        ensure!(
            *output == self.expected,
            "complete MCP payload/source hash differs from CLI-compatible reference"
        );
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .context("MCP text missing")?;
        ensure!(
            serde_json::from_str::<Value>(text)? == response["result"]["structuredContent"],
            "MCP text/structured payload differ"
        );
        Ok(output.clone())
    }
    fn cli_call(&self, source: &Path, binary: &Path) -> Result<Value> {
        let output = forge::process_probe::run(
            std::process::Command::new(binary)
                .args(["code", "--root"])
                .arg(source)
                .args(["fold", "main.rs", "--max-bytes", "32768"])
                .env("PATH", ""),
            limit(self.deadline)?,
            65536,
        )?;
        ensure!(
            output.status.success(),
            "CLI failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout)?;
        ensure!(
            value == self.expected,
            "complete CLI payload/source hash differs from MCP-compatible reference"
        );
        Ok(value)
    }
    async fn operation(&mut self, mcp: bool, source: &Path, binary: &Path) -> Result<u128> {
        limit(self.deadline)?;
        let key = if mcp { "mcp" } else { "cli" };
        start(&mut self.report[key]["accounting"]);
        let started = Instant::now();
        let result = if mcp {
            self.mcp_call().await
        } else {
            self.cli_call(source, binary)
        };
        record(&mut self.report[key]["accounting"], &result);
        result?;
        Ok(started.elapsed().as_nanos())
    }
    async fn batch(
        &mut self,
        mcp: bool,
        source: &Path,
        binary: &Path,
        service_pid: u32,
    ) -> Result<()> {
        let pid = self.client.as_ref().context("MCP adapter absent")?.pid();
        let before = resources(pid, service_pid);
        let started = Instant::now();
        for _ in 0..OPERATIONS {
            self.operation(mcp, source, binary).await?;
            tokio::task::yield_now().await;
        }
        let mean = started.elapsed().as_nanos() / OPERATIONS as u128;
        let after = resources(pid, service_pid);
        let key = if mcp { "mcp" } else { "cli" };
        self.report[key]["sample_batch_means_ns"]
            .as_array_mut()
            .unwrap()
            .push(json!(mean));
        self.report[key]["resources_per_batch"]
            .as_array_mut()
            .unwrap()
            .push(delta(before, after));
        Ok(())
    }
    pub async fn run(&mut self, state: &Path, binary: &Path) -> Result<()> {
        let source = state
            .parent()
            .context("state parent missing")?
            .join("source");
        let text = fs::read_to_string(source.join("main.rs"))?;
        let inspection =
            conductor_native::syntax::inspect(&text, conductor_native::syntax::Language::Rust)?;
        self.expected = json!({"source_sha256":inspection.source_sha256,"language":"rust","context_only":true,"source":inspection.fold(&text)?});
        self.report["expected_payload_sha256"] = json!(digest(&self.expected)?);
        self.report["source_sha256"] = self.expected["source_sha256"].clone();
        self.initialize(state, binary).await?;
        let ready: Value = serde_json::from_slice(&fs::read(state.join("ready.json"))?)?;
        let service_pid = u32::try_from(ready["pid"].as_u64().context("service PID absent")?)?;
        for mcp in [true, false] {
            let ns = self.operation(mcp, &source, binary).await?;
            self.report[if mcp { "mcp" } else { "cli" }]["first_call_ns"] = json!(ns);
        }
        for _ in 0..WARMUPS {
            for mcp in [false, true] {
                self.operation(mcp, &source, binary).await?;
            }
        }
        for batch in 0..BATCHES {
            let order = if batch % 2 == 0 {
                [true, false]
            } else {
                [false, true]
            };
            for mcp in order {
                self.batch(mcp, &source, binary, service_pid).await?;
            }
        }
        let id = self.next_id;
        let listed = self
            .protocol(json!({"jsonrpc":"2.0","id":id,"method":"tools/list"}), true)
            .await?;
        ensure!(
            listed["id"] == id
                && digest(&listed["result"])? == self.report["catalog_sha256"].as_str().unwrap(),
            "MCP catalog drifted"
        );
        self.report["catalog_reverified"] = json!(true);
        Ok(())
    }
    pub async fn finish_adapter(&mut self) -> Result<()> {
        let Some(client) = self.client.take() else {
            if self.report["cleanup"]["adapter"] != "not_started" {
                self.report["cleanup"]["adapter"] = json!("startup_cleanup_unverified");
                anyhow::bail!("adapter startup failed without verified cleanup");
            }
            return Ok(());
        };
        let result = client.try_finish().await;
        self.report["cleanup"]["adapter"] = match &result {
            Ok(()) => json!("verified_eof_exit"),
            Err(error) => json!({"failed":format!("{error:#}")}),
        };
        result
    }
    pub fn finish_report(&mut self, error: Option<&str>) {
        finalize(&mut self.report["protocol_accounting"]);
        for key in ["mcp", "cli"] {
            finalize(&mut self.report[key]["accounting"]);
            let mut samples: Vec<_> = self.report[key]["sample_batch_means_ns"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_u64)
                .collect();
            samples.sort_unstable();
            if samples.len() == BATCHES {
                self.report[key]["median_batch_mean_ns"] = json!(samples[15]);
                self.report[key]["p95_batch_mean_ns"] = json!(samples[29]);
            }
        }
        for row in self.report["unix_diagnostic"].as_array_mut().unwrap() {
            finalize(&mut row["accounting"]);
        }
        let complete = std::iter::once(&self.report["protocol_accounting"])
            .chain([
                &self.report["mcp"]["accounting"],
                &self.report["cli"]["accounting"],
            ])
            .chain(
                self.report["unix_diagnostic"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| &row["accounting"]),
            )
            .all(|counts| {
                counts["completed"] == counts["planned"]
                    && counts["failed"] == 0
                    && counts["censored"] == 0
            });
        self.report["accounting_complete"] = json!(complete);
        self.report["valid"] = json!(error.is_none() && complete);
        self.report["error"] = json!(error);
    }
}
