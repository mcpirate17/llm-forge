use super::*;
fn call(
    command: &mut Command,
    scratch: &Path,
    tag: &str,
    slot: &mut Value,
) -> Result<measured::Observation> {
    slot["status"] = json!("started");
    match measured::run(command, scratch, tag, Duration::from_secs(30)) {
        Ok(observed) => {
            slot["observation"] = observed.record();
            slot["status"] = json!(if observed.timed_out || observed.cancelled {
                "censored"
            } else if observed.success() {
                "completed"
            } else {
                "failed"
            });
            observed.require_success()
        }
        Err(error) => {
            slot["status"] = json!(if measured::expired() || measured::signal().is_some() {
                "censored"
            } else {
                "failed"
            });
            slot["error"] = json!(format!("{error:#}"));
            Err(error)
        }
    }
}
pub fn workflow(args: &Args, scratch: &Path, report: &mut Value) -> Result<()> {
    report["status"] = json!("started");
    let result = inner(args, scratch, report);
    match &result {
        Ok(()) => report["status"] = json!("completed"),
        Err(error) => {
            report["status"] = json!("failed");
            report["error"] = json!(format!("{error:#}"));
        }
    }
    result
}
fn inner(args: &Args, scratch: &Path, report: &mut Value) -> Result<()> {
    let path = args.repo.join(&args.source);
    ensure!(
        !args.source.is_absolute()
            && !args
                .source
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "source must be repository relative"
    );
    let source = fs::read(&path)?;
    ensure!(source.len() <= 1024 * 1024, "context source exceeds1MiB");
    std::str::from_utf8(&source)?;
    let digest = fixture::hash(&source);
    let mut command = command(&args.forge, scratch);
    command
        .args(["code", "--root"])
        .arg(&args.repo)
        .args([
            "context",
            "--symbol",
            &args.symbol,
            "--no-graph",
            "--json",
            "--expected-hash",
            &digest,
            "--",
        ])
        .arg(&args.source);
    let output = call(
        &mut command,
        scratch,
        "context",
        &mut report["calls"]["projection"],
    )?;
    let projection: Value = serde_json::from_slice(&output.stdout)?;
    ensure!(
        projection["source_sha256"] == digest && projection["graph_status"] == "disabled",
        "context source identity/graph status mismatch"
    );
    ensure!(
        projection["symbols"]
            .as_array()
            .context("missing symbols")?
            .iter()
            .any(|s| s
                .as_str()
                .is_some_and(|s| s == args.symbol || s.ends_with(&format!("::{}", args.symbol)))),
        "selected symbol absent"
    );
    let skeleton = projection["skeleton"]
        .as_str()
        .context("missing skeleton")?;
    for fact in [&args.symbol, "source_venv", "destination_venv", "cancel"] {
        ensure!(skeleton.contains(fact), "signature fact absent: {fact}");
    }
    report["workflow"] = json!("real_source_signature_lookup");
    report["file"] = json!(args.source);
    report["source_sha256"] = json!(digest);
    report["source_bytes"] = json!(source.len());
    report["projection_json_bytes"] = json!(output.stdout.len());
    report["projection_skeleton_bytes"] = json!(skeleton.len());
    report["saved_bytes"] = json!(source.len() as i64 - output.stdout.len() as i64);
    report["projection_stdout"] = json!(String::from_utf8(output.stdout.clone())?);
    report["format"] = projection["format"].clone();
    report["facts_checked"] = json!([
        "requested symbol",
        "source_venv/destination_venv/cancel parameter spellings",
        "source SHA256",
        "graph disabled"
    ]);
    report["scope"]=json!("raw full source versus complete emitted JSON; signature lookup only; omitted bodies are not equivalent full-source semantics");
    report["tokens"] = Value::Null;
    report["token_status"] = json!("unavailable; no estimates substituted");
    if let Some(config) = &args.tokenizer_config {
        tokens(args, scratch, config, &path, &output.stdout, report)?;
    }
    ensure!(
        fs::read(path)? == source,
        "real source changed during workflow"
    );
    report["model_inference_performed"] = json!(false);
    report["task_answer_quality_measured"] = json!(false);
    Ok(())
}
fn tokens(
    args: &Args,
    scratch: &Path,
    config: &Path,
    path: &Path,
    projection: &[u8],
    report: &mut Value,
) -> Result<()> {
    let label = args
        .tokenizer_name
        .as_deref()
        .context("--tokenizer-name required with config")?;
    let config_bytes = fs::read(config)?;
    ensure!(config_bytes.len() <= 65536, "tokenizer config exceeds64KiB");
    let prompt = scratch.join("projection.json");
    fs::write(&prompt, projection)?;
    let raw = count(
        args,
        scratch,
        config,
        path,
        "tokens-raw",
        &mut report["calls"]["raw_tokens"],
    )?;
    let compact = count(
        args,
        scratch,
        config,
        &prompt,
        "tokens-projection",
        &mut report["calls"]["projection_tokens"],
    )?;
    ensure!(
        raw["tokenizer"] == compact["tokenizer"],
        "tokenizer identity changed between inputs"
    );
    ensure!(
        raw["input_bytes"] == fs::metadata(path)?.len()
            && compact["input_bytes"] == projection.len(),
        "token input byte mismatch"
    );
    ensure!(fs::read(config)? == config_bytes, "tokenizer config drift");
    let before = raw["tokens"].as_i64().context("missing raw token count")?;
    let after = compact["tokens"]
        .as_i64()
        .context("missing output token count")?;
    report["tokens"] = json!({"operator_supplied_label":label,"artifact_pinned_identity":raw["tokenizer"],"model_identity_scope":"model SHA is config supplied; in the published GPT2 fixture 0707... is a synthetic sentinel, not a weight hash; no weights loaded or deployed model identified",
        "raw_source":before,"complete_projection_json":after,"saved":before-after,"reduction_percent":if before>0 {100.0*(before-after) as f64/before as f64}else{0.0},
        "config_sha256":fixture::hash(&config_bytes),"scope":"exact supplied strings; external chat templates/envelopes and inference excluded"});
    report["token_status"] = json!("exact artifact-pinned tokenizer; operator label");
    Ok(())
}
fn count(
    args: &Args,
    scratch: &Path,
    config: &Path,
    input: &Path,
    tag: &str,
    slot: &mut Value,
) -> Result<Value> {
    let mut command = command(&args.forge, scratch);
    command
        .args(["context", "tokens", "--tokenizer"])
        .arg(config)
        .arg("--input")
        .arg(input);
    let output = call(&mut command, scratch, tag, slot)?;
    serde_json::from_slice(&output.stdout).context("invalid native tokenizer report")
}
