//! Strict native-analysis output protocol after operator-confined execution.
//! Selection chooses a data protocol; it does not attest executable identity.
//! Operator program grants and verified supervisor reports remain separate.
use super::*;

#[cfg(test)]
const IDS: &[&str] = &[
    "config-parse",
    "python-ast",
    "import-declaration",
    "duplicate-function-bodies",
    "structure-audit",
];
fn prefix(argv: &[Value], id: &str) -> bool {
    argv.len() >= 5
        && argv[0] == "forge"
        && argv[1] == "candidate"
        && argv[2] == "analyze"
        && argv[3] == "--check"
        && argv[4] == id
}
pub(super) fn selected(value: &Value) -> bool {
    let Some(id) = value["check_id"]
        .as_str()
        .filter(|id| crate::candidate_analysis::Mode::from_id(id).is_some())
    else {
        return false;
    };
    let Some(command) = value["command"].as_array() else {
        return false;
    };
    prefix(command, id)
}
fn version_template(value: &Value, check: &Check) -> Result<()> {
    let version = value["version_command"]
        .as_array()
        .context("native analysis version template missing")?;
    ensure!(
        version.len() == 6 && prefix(version, &check.id) && version[5] == "--tool-version",
        "native analysis version template mismatch"
    );
    Ok(())
}

fn version(check: &Check, version: &str) -> Result<()> {
    let words = version.split_whitespace().collect::<Vec<_>>();
    ensure!(
        words.len() == 4
            && words[0] == "forge-native-analysis"
            && words[1] == "1"
            && words[3] == check.id,
        "native analysis version protocol mismatch"
    );
    Ok(())
}
fn oid(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn invocation<'a>(check: &Check, files: &[String], command: &'a [String]) -> Result<&'a str> {
    ensure!(
        command.len() >= 5
            && command[0] == "forge"
            && command[1] == "candidate"
            && command[2] == "analyze"
            && command[3] == "--check"
            && command[4] == check.id,
        "native analysis command protocol mismatch"
    );
    let end = command
        .iter()
        .position(|arg| arg == "--")
        .context("missing native file delimiter")?;
    let mut options = std::collections::BTreeMap::new();
    let mut execute = false;
    let mut index = 5;
    while index < end {
        let name = command[index].as_str();
        if name == "--execute" {
            ensure!(!execute, "repeated native analysis execute flag");
            execute = true;
            index += 1;
            continue;
        }
        ensure!(
            matches!(
                name,
                "--repo" | "--base" | "--tree" | "--policy" | "--today"
            ),
            "unknown native analysis command option"
        );
        let value = command
            .get(index + 1)
            .filter(|_| index + 1 < end)
            .filter(|value| !value.is_empty())
            .context("native analysis option value missing")?;
        ensure!(
            options.insert(name, value.as_str()).is_none(),
            "repeated native analysis option"
        );
        index += 2;
    }
    ensure!(execute, "native analysis was not executed");
    let base = options
        .get("--base")
        .context("native analysis base missing")?;
    let tree = *options
        .get("--tree")
        .context("native analysis tree missing")?;
    ensure!(
        oid(base) && oid(tree),
        "native analysis base/tree must be full object IDs"
    );
    let selected = &command[end + 1..];
    ensure!(
        selected.len() == files.len() && normalized_files(selected)? == files,
        "native analysis argv selected files mismatch"
    );
    Ok(tree)
}

fn envelope(
    check: &Check,
    files: &[String],
    tree: &str,
    report: &Report,
) -> Result<(Value, Vec<String>)> {
    ensure!(
        report.outcome == Outcome::Exited
            && report.signal.is_none()
            && matches!(report.exit_code, Some(0 | 1))
            && report.cleanup_complete
            && report.resources.verified
            && report.errors.is_empty(),
        "native analysis execution incomplete"
    );
    // The tail normalizes UTF-8/newlines. Byte equality proves no discarded prefix
    // only when replacement decoding cannot mask lost bytes. A literal U+FFFD is
    // conservatively incomplete too; the supervisor does not retain raw stdout.
    ensure!(
        report.stdout_bytes == report.stdout.len() as u64
            && !report.stdout.contains('\u{fffd}')
            && report.stderr_bytes == 0
            && report.stderr.is_empty(),
        "native analysis output capture incomplete"
    );
    let value = crate::tool_schema::parse_document_with_limit(&report.stdout, MAX_MESSAGE_BYTES)?;
    let object = value
        .as_object()
        .context("native analysis output must be an object")?;
    ensure!(
        object.len() == 5
            && ["schema_version", "check", "executed", "plan", "result"]
                .iter()
                .all(|key| object.contains_key(*key))
            && value["schema_version"] == 1
            && value["check"] == check.id
            && value["executed"] == true,
        "native analysis envelope mismatch"
    );
    let plan = value["plan"]
        .as_object()
        .context("native analysis plan missing")?;
    let selected: Vec<String> = serde_json::from_value(value["plan"]["selected_files"].clone())?;
    ensure!(
        selected.len() <= 4096
            && selected.len() == files.len()
            && normalized_files(&selected)? == files,
        "native analysis selected files mismatch"
    );
    let candidate = plan
        .get("candidate_tree_oid")
        .and_then(Value::as_str)
        .context("native candidate tree missing")?;
    ensure!(
        oid(candidate) && candidate.eq_ignore_ascii_case(tree),
        "native analysis candidate tree mismatch"
    );
    let base = plan
        .get("base_tree_oid")
        .and_then(Value::as_str)
        .context("native base tree missing")?;
    // argv base may be a commit; the envelope always reports a resolved tree.
    // This pure mapper cannot resolve that commit, so it validates shape only.
    ensure!(
        oid(base),
        "native analysis base tree must be a full object ID"
    );
    let included = partition(&value["plan"], files, &check.id)?;
    Ok((value, included))
}
fn partition(plan: &Value, selected: &[String], check: &str) -> Result<Vec<String>> {
    let included: Vec<String> = serde_json::from_value(plan["included_files"].clone())?;
    let exclusions = plan["excluded_files"]
        .as_array()
        .context("native exclusions missing")?;
    ensure!(
        included.len() <= 4096 && exclusions.len() <= 4096,
        "native partition exceeds file limit"
    );
    let mut all = included.clone();
    for exclusion in exclusions {
        let object = exclusion
            .as_object()
            .context("native exclusion must be an object")?;
        ensure!(object.len() == 2, "native exclusion fields mismatch");
        let path = exclusion["path"]
            .as_str()
            .context("native exclusion path missing")?;
        let reason = exclusion["reason"]
            .as_str()
            .context("native exclusion reason missing")?;
        let allowed = matches!(reason, "deleted" | "class-not-selected")
            || (reason == "test-class" && check == "import-declaration")
            || (reason == "peer-directory-excluded"
                && matches!(check, "duplicate-function-bodies" | "structure-audit"));
        ensure!(allowed, "unknown or incompatible native exclusion reason");
        all.push(path.to_owned());
    }
    ensure!(
        all.len() == selected.len() && normalized_files(&all)? == selected,
        "native included/excluded files do not partition selected inventory"
    );
    // Equal total count and equal unique inventory exclude duplicates and overlap.
    normalized_files(&included)
}
fn validate_files(value: &Value, included: &[String]) -> Result<Vec<String>> {
    let files: Vec<String> = serde_json::from_value(value.clone())?;
    ensure!(
        files.len() <= 4096,
        "native analysis result files exceed 4096"
    );
    let normalized = normalized_files(&files)?;
    ensure!(
        normalized.len() == files.len() && normalized == included,
        "native analysis result files differ from included inventory"
    );
    Ok(normalized)
}
fn validate_finding(check: &Check, files: &[String], finding: &mut Finding) -> Result<()> {
    ensure!(
        finding.check_id == check.id
            && !finding.rule_id.is_empty()
            && finding.rule_id.len() <= 256
            && !finding.rule_id.chars().any(char::is_control)
            && finding.message.len() <= MAX_MESSAGE_BYTES,
        "native analysis finding identity or message invalid"
    );
    if let Some(path) = &finding.path {
        ensure!(
            files.binary_search(path).is_ok(),
            "native finding outside result files"
        );
    }
    ensure!(
        finding.line.is_none_or(|line| line > 0) && finding.column.is_none_or(|column| column >= 0),
        "invalid native analysis location"
    );
    crate::candidate_assessment::finalize_finding(finding);
    Ok(())
}
fn binding(
    check: &Check,
    files: &[String],
    command: &[String],
    plan: &Value,
    context: &AnalysisContext<'_>,
) -> Result<()> {
    let candidate = context.candidate;
    ensure!(
        plan["candidate_tree_oid"] == candidate.tree_oid
            && plan["base_tree_oid"] == candidate.base_tree_oid,
        "native analysis parent candidate binding mismatch"
    );
    ensure!(
        plan["policy_path"] == context.policy.path
            && plan["policy_sha256"] == context.policy.digest,
        "native analysis parent policy binding mismatch"
    );
    let base_position = command
        .iter()
        .position(|arg| arg == "--base")
        .context("native base missing")?;
    let base = command
        .get(base_position + 1)
        .context("native base value missing")?;
    ensure!(
        base.eq_ignore_ascii_case(&candidate.base_tree_oid)
            || candidate
                .base_commit_oid
                .as_ref()
                .is_some_and(|oid| base.eq_ignore_ascii_case(oid)),
        "native analysis argv base differs from parent candidate"
    );
    let mode =
        crate::candidate_analysis::Mode::from_id(&check.id).context("unknown native mode")?;
    let (included, excluded) = crate::candidate_analysis::select(candidate, mode, files)?;
    ensure!(
        plan["included_files"] == json!(included) && plan["excluded_files"] == json!(excluded),
        "native analysis partition differs from classified parent candidate"
    );
    Ok(())
}
fn import_scope(result: &Value, included: &[String], plan: &Value) -> Result<()> {
    let scope = result["provider_scope"]
        .as_object()
        .context("native import provider scope missing")?;
    ensure!(
        scope.get("complete") == Some(&Value::Bool(false))
            && scope.get("runtime_verified") == Some(&Value::Bool(false)),
        "native import scope overclaims completeness or runtime verification"
    );
    for key in ["stdlib_count", "module_count"] {
        ensure!(
            scope
                .get(key)
                .and_then(Value::as_u64)
                .is_some_and(|n| n <= 65_536),
            "invalid native provider count"
        );
    }
    let provenance = result["provider_scope"]["provenance"]
        .as_object()
        .context("native import provenance missing")?;
    ensure!(
        provenance.get("schema_version") == Some(&json!(1)),
        "invalid native provider provenance schema"
    );
    for key in ["implementation", "version", "platform"] {
        ensure!(
            provenance
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty()
                    && s.len() <= 256
                    && !s.chars().any(char::is_control)),
            "invalid native provider provenance text"
        );
    }
    let sha = provenance
        .get("sha256")
        .and_then(Value::as_str)
        .context("native provider hash missing")?;
    ensure!(
        sha.len() == 64
            && sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && plan["provider_metadata"]["sha256"] == sha,
        "native provider observation hash mismatch"
    );
    let metadata = plan["provider_metadata"]
        .as_object()
        .context("native provider observation metadata missing")?;
    let observed_path = metadata
        .get("path")
        .and_then(Value::as_str)
        .context("native provider observation path missing")?;
    ensure!(
        metadata.len() == 2
            && observed_path.starts_with('/')
            && observed_path.len() <= 4096
            && !observed_path.chars().any(char::is_control),
        "invalid native provider observation metadata"
    );
    let unresolved = result["unresolved"]
        .as_array()
        .context("native unresolved imports missing")?;
    ensure!(
        result["metrics"]["unresolved_imports"].as_u64() == Some(unresolved.len() as u64),
        "native unresolved import count mismatch"
    );
    for row in unresolved {
        let object = row
            .as_object()
            .context("native unresolved import row must be an object")?;
        let path = row["path"]
            .as_str()
            .context("native unresolved import path missing")?;
        let module = row["module"]
            .as_str()
            .context("native unresolved import module missing")?;
        ensure!(
            object.len() == 2
                && included.binary_search_by(|s| s.as_str().cmp(path)).is_ok()
                && !module.is_empty()
                && module.len() <= MAX_MESSAGE_BYTES
                && !module.chars().any(char::is_control),
            "invalid native unresolved import row"
        );
    }
    Ok(())
}
struct Decoded {
    findings: Vec<Finding>,
    metadata: Value,
}
fn decode(
    check: &Check,
    files: &[String],
    command: &[String],
    tool_version: &str,
    report: &Report,
    context: &AnalysisContext<'_>,
) -> Result<Decoded> {
    version(check, tool_version)?;
    let tree = invocation(check, files, command)?;
    let (mut value, included) = envelope(check, files, tree, report)?;
    binding(check, files, command, &value["plan"], context)?;
    let result = value["result"]
        .as_object()
        .context("native analysis result missing")?;
    ensure!(
        result.get("metrics").is_some_and(Value::is_object),
        "native analysis metrics missing"
    );
    let result_files = validate_files(&value["result"]["files"], &included)?;
    if check.id == "import-declaration" && !included.is_empty() {
        import_scope(&value["result"], &included, &value["plan"])?;
    }
    let rows = value["result"]["findings"]
        .as_array()
        .context("native analysis findings missing")?;
    ensure!(rows.len() <= 4096, "native analysis findings exceed 4096");
    ensure!(
        (report.exit_code == Some(0)) == rows.is_empty(),
        "native analysis exit/findings mismatch"
    );
    let rows = value["result"]
        .as_object_mut()
        .unwrap()
        .remove("findings")
        .unwrap();
    let mut findings: Vec<Finding> = serde_json::from_value(rows)?;
    for finding in &mut findings {
        validate_finding(check, &result_files, finding)?;
    }
    let metadata = json!({"plan":value["plan"].take(),"result":value["result"].take()});
    Ok(Decoded { findings, metadata })
}

pub(super) fn map(
    normalized: &Value,
    check: &Check,
    files: &[String],
    command: &[String],
    version: Option<&str>,
    report: &Report,
    context: Option<&AnalysisContext<'_>>,
) -> (Vec<Finding>, Option<Value>) {
    let decoded = version_template(normalized, check).and_then(|_| {
        version
            .context("native analysis version evidence unavailable")
            .and_then(|version| {
                let context = context.context("native analysis parent context missing")?;
                decode(check, files, command, version, report, context)
            })
    });
    match decoded {
        Ok(decoded) => (decoded.findings, Some(decoded.metadata)),
        Err(error) => {
            let diagnostic = format!("{error:#}").chars().take(1000).collect::<String>();
            let mut failure = finding(
                check,
                "analyzer-crash",
                Severity::Critical,
                format!("native analysis incomplete: {diagnostic}"),
                exit_code(report),
            );
            crate::candidate_assessment::finalize_finding(&mut failure);
            (vec![failure], None)
        }
    }
}

#[cfg(test)]
#[path = "candidate_execution_result_analysis_tests.rs"]
mod tests;
