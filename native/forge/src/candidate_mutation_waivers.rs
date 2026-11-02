//! Exact base/hash/date-bound exceptions retained as visible findings.
use super::*;
pub(super) fn receipt_states(
    capture: &mut Capture<'_>,
    candidate: &Candidate,
    policy: &Policy,
) -> Result<Value> {
    let mut rows = Vec::new();
    for waiver in list(&policy.document["mutation_waivers"])? {
        let path = string(waiver, "path")?;
        let bound = candidate.waiver_base() == waiver["integration_base"].as_str();
        let file_ok = bound && capture.optional_hash(path)?.as_deref() == waiver["sha256"].as_str();
        let mut sources = Vec::new();
        if file_ok {
            for source in list(&waiver["sources"])? {
                let path = string(source, "path")?;
                let ok = capture.optional_hash(path)?.as_deref() == source["sha256"].as_str();
                sources.push(json!({"path":path,"ok":ok}));
                if !ok {
                    break;
                }
            }
        }
        rows.push(json!({"id":waiver["waiver_id"],"path":path,"integration_base":waiver["integration_base"],"file_ok":file_ok,"sources":sources}));
    }
    let mut states = decide(
        "waiver_states",
        &json!({"base":candidate.waiver_base(),"waivers":rows}),
    )?;
    let date = local_date()?;
    for (state, waiver) in states
        .as_array_mut()
        .context("waiver states must be an array")?
        .iter_mut()
        .zip(list(&policy.document["mutation_waivers"])?)
    {
        if string(waiver, "expires")? < date.as_str() {
            state["active"] = json!(false);
            state["reason"] = json!(format!("expired on {}", string(waiver, "expires")?));
        }
    }
    Ok(states)
}
pub(super) fn value_states(policy: &Policy, candidate: &Candidate, date: &str) -> Result<Value> {
    let mut rows = Vec::new();
    for row in list(&policy.document["value_waivers"])? {
        let reason = if candidate.waiver_base() != row["integration_base"].as_str() {
            "candidate base commit is not the pinned integration base".into()
        } else if row["expires"].is_null() {
            "no expiry is declared".into()
        } else if string(row, "expires")? < date {
            format!("expired on {}", string(row, "expires")?)
        } else {
            String::new()
        };
        rows.push(json!({"integration_base":row["integration_base"],"nodeids":row["nodeids"],"active":reason.is_empty(),"reason":reason}));
    }
    Ok(json!(rows))
}
pub(super) fn apply(
    findings: &mut [Finding],
    policy: &Policy,
    candidate: &Candidate,
    date: &str,
) -> Result<Value> {
    let states = value_states(policy, candidate, date)?;
    for finding in findings {
        if finding.rule_id != "new-test-value-not-admitted" {
            continue;
        }
        let Some(node) = finding.evidence.get("nodeid").and_then(Value::as_str) else {
            continue;
        };
        for (waiver, state) in list(&policy.document["value_waivers"])?
            .iter()
            .zip(list(&states)?)
        {
            if state["active"] != true
                || !list(&waiver["nodeids"])?
                    .iter()
                    .any(|v| v.as_str() == Some(node))
            {
                continue;
            }
            let expires = string(waiver, "expires")?;
            let base = string(waiver, "integration_base")?;
            finding.message = format!(
                "WAIVED {} -- {} (approved by {} on {}, expires {}, base {})",
                finding.message,
                string(waiver, "reason")?,
                string(waiver, "approved_by")?,
                string(waiver, "approved_on")?,
                expires,
                &base[..12]
            );
            finding.rule_id = "new-test-value-waived".into();
            finding.severity = crate::candidate_assessment::Severity::Info;
            finding.evidence.insert(
                "waived_by".into(),
                json!({"integration_base":base,"approved_by":waiver["approved_by"],
                "approved_on":waiver["approved_on"],"expires":expires,"reason":waiver["reason"]}),
            );
            break;
        }
    }
    Ok(states)
}
pub(super) fn local_date() -> Result<String> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let stamp: libc::time_t = seconds.try_into()?;
    let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: localtime_r writes a complete tm on success; both pointers live for the call.
    ensure!(
        !unsafe { libc::localtime_r(&stamp, local.as_mut_ptr()) }.is_null(),
        "local waiver date unavailable"
    );
    // SAFETY: successful localtime_r initialized local above.
    let local = unsafe { local.assume_init() };
    let date = format!(
        "{:04}-{:02}-{:02}",
        local.tm_year + 1900,
        local.tm_mon + 1,
        local.tm_mday
    );
    crate::candidate_policy::validate_today(&date)?;
    Ok(date)
}
