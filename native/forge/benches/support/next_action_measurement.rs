//! Bounded source-location retrieval comparisons over actual MCP roundtrips.
use super::*;

const WARM: usize = 5;
const SAMPLES: usize = 31;
const PER_ARM: usize = 1 + WARM + SAMPLES;
pub(super) const NAMES: [&str; 4] = ["fused", "prompt_known", "known", "prompt_discovery"];

fn prompt(next: bool) -> Value {
    let mut args = json!({"mode":"suggest","prompt":"Inspect the reported Rust source function.","max_output_bytes":8192});
    if next {
        args["next_action"] = json!({"intent":"inspect_failure","allow_read_advice":true,
            "path":"main.rs","symbol":"answer","max_results":20,"max_bytes":2048});
    }
    json!({"name":"context_prompt","arguments":args})
}
fn known() -> Value {
    json!({"name":"code_inspect","arguments":{"path":"main.rs","symbol":"answer","max_results":20,"max_bytes":2048}})
}
fn row() -> Value {
    json!({"accounting":counts(PER_ARM),"warmup_count":WARM,"sample_count":SAMPLES,
        "observations":[],"in_flight":null,"scoring":"exact source SHA-256, symbol signature and line; no diagnosis or task success"})
}
pub(super) fn empty() -> Value {
    json!({"scope":"source location retrieval; no model/task or token cost claim",
        "first_scope":"first operation per arm on one shared warm service; not fresh process or cold filesystem",
        "policy_scope":"context capability and tokenizer change existing harness policy/catalog; compare only matching policy fixtures",
        "rpc_accounting_scope":"completed means valid protocol reply and tool success; workflow scoring separately validates contents",
        "byte_scope":"complete request/reply JSONL frames including newline; incomplete or undecodable reply byte count is unknown",
        "schema":null,"fused":row(),"prompt_known":row(),"known":row(),"prompt_discovery":row()})
}
pub(super) fn request_fingerprint() -> Result<String> {
    digest(
        &json!({"prompt_fused":prompt(true),"prompt_plain":prompt(false),"known":known(),
        "discovery":{"method":"tools/list","params":{}}}),
    )
}
pub(super) fn finish(report: &mut Value) {
    for name in NAMES {
        finalize(&mut report[name]["accounting"]);
        if report[name]["in_flight"].is_object() {
            finalize(&mut report[name]["in_flight"]["rpc_accounting"]);
            report[name]["in_flight"]["interrupted"] = json!(true);
        }
    }
}
pub(super) fn initialize(measurement: &mut Measurement, catalog: &Value) -> Result<()> {
    let tools = catalog["tools"]
        .as_array()
        .context("catalog tools missing")?;
    let prompt_tool = tools
        .iter()
        .find(|tool| tool["name"] == "context_prompt")
        .context("context_prompt missing from catalog")?;
    let schema = &prompt_tool["inputSchema"];
    measurement.report["next_action"]["schema"] = json!({
        "catalog_frame_bytes":measurement.last_response_bytes,
        "prompt_schema_compact_json_bytes":serde_json::to_vec(schema)?.len(),
        "prompt_schema_sha256":digest(schema)?,
        "field_present":schema["properties"].get("next_action").is_some()});
    Ok(())
}

struct Calls {
    arm: &'static str,
    phase: &'static str,
    rows: Vec<Value>,
    accounting: Value,
}
impl Calls {
    fn new(arm: &'static str, phase: &'static str) -> Self {
        let planned = match arm {
            "known" => 1,
            "prompt_discovery" => 3,
            _ => 2,
        };
        Self {
            arm,
            phase,
            rows: Vec::new(),
            accounting: counts(planned),
        }
    }
    fn persist(&self, measurement: &mut Measurement) {
        measurement.report["next_action"][self.arm]["in_flight"] = json!({
            "phase":self.phase,"roundtrips":self.rows,"rpc_accounting":self.accounting,
            "interrupted":false});
    }
}
async fn roundtrip(
    measurement: &mut Measurement,
    method: &str,
    params: Value,
    calls: &mut Calls,
) -> Result<Value> {
    let id = measurement.next_id;
    measurement.next_id += 1;
    let stage = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(method)
        .to_owned();
    let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
    let request_bytes = serde_json::to_vec(&request)?.len() + 1;
    start(&mut calls.accounting);
    calls.persist(measurement);
    measurement.report["next_action"][calls.arm]["in_flight"]["pending"] = json!({
        "stage":stage,"method":method,"request_frame_bytes":request_bytes,
        "response_frame_bytes":null});
    measurement.last_response_bytes = 0;
    let started = Instant::now();
    let result = measurement.exchange(request, true).await.and_then(|reply| {
        ensure!(
            reply["id"] == id && reply.get("error").is_none(),
            "MCP response invalid: {reply}"
        );
        if method == "tools/call" {
            ensure!(
                reply["result"]["isError"] == false,
                "MCP tool rejected: {}",
                reply["result"]
            );
        }
        Ok(reply["result"].clone())
    });
    record(&mut calls.accounting, &result);
    calls.rows.push(json!({"stage":stage,"method":method,"request_frame_bytes":request_bytes,
        "response_frame_bytes":if measurement.last_response_bytes == 0 {None} else {Some(measurement.last_response_bytes)},
        "roundtrip_ns":started.elapsed().as_nanos(),"error":result.as_ref().err().map(|e|format!("{e:#}"))}));
    calls.persist(measurement);
    result
}
async fn tool(measurement: &mut Measurement, call: Value, calls: &mut Calls) -> Result<Value> {
    let result = roundtrip(measurement, "tools/call", call, calls).await?;
    ensure!(result["isError"] == false, "MCP tool rejected: {result}");
    let body = &result["structuredContent"];
    let text = result["content"][0]["text"]
        .as_str()
        .context("MCP text absent")?;
    ensure!(
        serde_json::from_str::<Value>(text)? == *body,
        "MCP text/structured mismatch"
    );
    Ok(body["result"].clone())
}
fn score(value: &Value, source: &str) -> Result<()> {
    ensure!(
        value["source_sha256"] == source,
        "inspection source fingerprint mismatch"
    );
    ensure!(value["truncated"] == false, "inspection truncated");
    let symbols = value["symbols"]
        .as_array()
        .context("inspection symbols missing")?;
    ensure!(symbols.len() == 1, "inspection result empty or ambiguous");
    let item = &symbols[0];
    ensure!(
        item["name"] == "answer"
            && item["signature"] == "fn answer() -> u32"
            && item["line_start"] == 1
            && item["line_end"] == 1,
        "source signature/location mismatch"
    );
    Ok(())
}
async fn operation(
    measurement: &mut Measurement,
    arm: &str,
    reference: &mut Option<Value>,
    calls: &mut Calls,
) -> Result<()> {
    let mut selected = known();
    if arm != "known" {
        let value = tool(measurement, prompt(arm == "fused"), calls).await?;
        if arm == "fused" {
            ensure!(
                value["next_action"]["decision"] == "inspect_source",
                "advice was not inspect_source"
            );
            selected = value["next_action"]["call"].clone();
            ensure!(
                selected == known(),
                "advised call differs from selected request"
            );
            let mut comparable = value.clone();
            comparable
                .as_object_mut()
                .context("prompt output must be object")?
                .remove("next_action");
            if let Some(expected) = reference {
                ensure!(*expected == comparable, "base prompt output changed");
            } else {
                *reference = Some(comparable);
            }
        } else {
            ensure!(
                value.get("next_action").is_none(),
                "absent next_action unexpectedly returned"
            );
            if let Some(expected) = reference {
                ensure!(*expected == value, "base prompt output changed");
            } else {
                *reference = Some(value);
            }
        }
    }
    if arm == "prompt_discovery" {
        let catalog = roundtrip(measurement, "tools/list", json!({}), calls).await?;
        ensure!(
            digest(&catalog)? == measurement.report["catalog_sha256"].as_str().unwrap(),
            "catalog drifted"
        );
        ensure!(
            catalog["tools"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|x| x["name"] == "code_inspect")),
            "inspect not discovered"
        );
    }
    let inspected = tool(measurement, selected, calls).await?;
    score(
        &inspected,
        measurement.report["source_sha256"]
            .as_str()
            .context("source hash absent")?,
    )?;
    Ok(())
}
pub(super) async fn run(measurement: &mut Measurement) -> Result<()> {
    let mut reference = None;
    let available = measurement.report["next_action"]["schema"]["field_present"] == true;
    if !available {
        measurement.report["next_action"]["fused"]["unavailable_reason"] =
            json!("catalog does not expose the next_action field");
    }
    for iteration in 0..PER_ARM {
        for index in 0..NAMES.len() {
            let name = NAMES[(index + iteration) % NAMES.len()];
            if name == "fused" && !available {
                continue;
            }
            start(&mut measurement.report["next_action"][name]["accounting"]);
            let started = Instant::now();
            let phase = if iteration == 0 {
                "first"
            } else if iteration <= WARM {
                "warmup"
            } else {
                "sample"
            };
            let mut calls = Calls::new(name, phase);
            calls.persist(measurement);
            let result = operation(measurement, name, &mut reference, &mut calls).await;
            record(
                &mut measurement.report["next_action"][name]["accounting"],
                &result,
            );
            finalize(&mut calls.accounting);
            let sample = json!({"phase":phase,"workflow_ns":started.elapsed().as_nanos(),
                "roundtrips":calls.rows,"rpc_accounting":calls.accounting,
                "error":result.as_ref().err().map(|e|format!("{e:#}"))});
            measurement.report["next_action"][name]["observations"]
                .as_array_mut()
                .unwrap()
                .push(sample);
            measurement.report["next_action"][name]["in_flight"] = Value::Null;
            if let Err(error) = result {
                for arm in NAMES {
                    finalize(&mut measurement.report["next_action"][arm]["accounting"]);
                }
                anyhow::bail!("next-action {name} measurement stopped: {error:#}");
            }
        }
    }
    for name in NAMES {
        finalize(&mut measurement.report["next_action"][name]["accounting"]);
    }
    ensure!(
        available,
        "baseline catalog lacks next_action; fused workflow unavailable"
    );
    Ok(())
}
