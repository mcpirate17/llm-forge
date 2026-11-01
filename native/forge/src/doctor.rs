//! Verify native settings, installed binary versions, ledger storage, embedded
//! policy and a real bounded hook roundtrip. No interpreter probe is required.

use crate::{hooks_install, ledger, route};
use anyhow::{Context, Result};
use clap::Args;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

#[derive(Args)]
pub struct DoctorArgs {
    /// The host project root to verify (the dir containing `.claude/`).
    #[arg(long)]
    host: PathBuf,
    /// Print the checks as one JSON object instead of one line per check.
    #[arg(long)]
    json: bool,
}

/// One check's verdict. `detail` is always human-readable and always ends
/// in the concrete path, rev or error the verdict rests on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    Pass,
    Fail,
    Skip,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::Skip => "SKIP",
        }
    }
}

struct Check {
    name: &'static str,
    status: Status,
    detail: String,
}

impl Check {
    fn pass(name: &'static str, detail: String) -> Check {
        Check {
            name,
            status: Status::Pass,
            detail,
        }
    }
    fn fail(name: &'static str, detail: String) -> Check {
        Check {
            name,
            status: Status::Fail,
            detail,
        }
    }
    fn skip(name: &'static str, detail: String) -> Check {
        Check {
            name,
            status: Status::Skip,
            detail,
        }
    }
}

pub fn run(args: &DoctorArgs) -> Result<u8> {
    let host = args.host.as_path();
    let (settings, entries) = check_settings(host);
    let self_path =
        std::env::current_exe().context("locating the running forge binary for hook-roundtrip")?;
    let checks = vec![
        settings,
        check_binaries(&entries),
        check_ledger(),
        check_policy(),
        check_roundtrip(&self_path, host),
    ];
    let ok = !checks.iter().any(|c| c.status == Status::Fail);
    if args.json {
        println!("{}", render_json(&checks));
    } else {
        println!("{}", render_text(&checks));
    }
    Ok(if ok { 0 } else { 1 })
}

// ── settings ──────────────────────────────────────────────────────────────

/// Every forge hook entry in the settings, across all five events. Reuses
/// `hooks_install`'s parser so "what counts as forge's" can never disagree
/// between install, uninstall, status and doctor.
fn installed_entries(settings: &Value) -> Vec<hooks_install::ParsedHook> {
    settings
        .get("hooks")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|hooks| hooks.values())
        .filter_map(Value::as_array)
        .flatten()
        .flat_map(hooks_install::entry_parsed_hooks)
        .collect()
}

fn check_settings(host: &Path) -> (Check, Vec<hooks_install::ParsedHook>) {
    let parsed = hooks_install::load_settings(host);
    let entries = match &parsed {
        Ok((_, settings)) => installed_entries(settings),
        Err(_) => Vec::new(),
    };
    if entries.is_empty() {
        let detail = match parsed {
            Err(ref err) => format!("unreadable: {err:#}"),
            Ok(_) => format!(
                "no forge hook entries in {}",
                hooks_install::settings_path(host).display()
            ),
        };
        return (Check::fail("settings", detail), entries);
    }
    let events: Vec<&str> = hooks_install::EVENTS
        .iter()
        .copied()
        .filter(|event| entries.iter().any(|e| e.event == *event))
        .collect();
    let first = &entries[0];
    let detail = format!(
        "{} entries over [{}], mode={}, standalone={}",
        entries.len(),
        events.join(", "),
        first.mode,
        first.standalone
    );
    (Check::pass("settings", detail), entries)
}

// ── binary ────────────────────────────────────────────────────────────────

fn check_binaries(entries: &[hooks_install::ParsedHook]) -> Check {
    if entries.is_empty() {
        return Check::skip(
            "binary",
            "no forge entries installed (settings FAILED above)".to_string(),
        );
    }
    let mut binaries: Vec<&str> = Vec::new();
    for entry in entries {
        if !binaries.contains(&entry.binary.as_str()) {
            binaries.push(&entry.binary);
        }
    }
    for binary in &binaries {
        if let Some(check) = binary_problem(binary) {
            return check;
        }
    }
    Check::pass(
        "binary",
        format!(
            "{} {} checked, versions match {}",
            binaries.len(),
            if binaries.len() > 1 {
                "binaries"
            } else {
                "binary"
            },
            hooks_install::VERSION_LINE
        ),
    )
}

/// The first thing wrong with one installed binary, as a FAIL check --
/// `None` when it exists, is executable, and answers `--version` with this
/// running binary's rev.
fn binary_problem(binary: &str) -> Option<Check> {
    let path = Path::new(binary);
    if !path.is_file() {
        return Some(Check::fail("binary", format!("{binary} does not exist")));
    }
    if !is_executable(path) {
        return Some(Check::fail("binary", format!("{binary} is not executable")));
    }
    let mut cmd = Command::new(binary);
    cmd.arg("--version");
    let output = match run_capture_bounded(cmd, None, BINARY_VERSION_TIMEOUT) {
        Ok(output) => output,
        Err(err) => {
            return Some(Check::fail(
                "binary",
                format!("{binary} --version could not run: {err:#}"),
            ))
        }
    };
    if !output.status.success() {
        return Some(Check::fail(
            "binary",
            format!("{binary} --version exited {}", output.status),
        ));
    }
    let line = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !hooks_install::versions_match(&line) {
        return Some(Check::fail(
            "binary",
            format!(
                "version mismatch: installed {line}, running {}",
                hooks_install::VERSION_LINE
            ),
        ));
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

// ── python ────────────────────────────────────────────────────────────────

// ── ledger ────────────────────────────────────────────────────────────────

fn check_ledger() -> Check {
    let root = match ledger::resolve_ledger_root(None) {
        Ok(root) => root,
        Err(error) => return Check::fail("ledger", error.to_string()),
    };
    let live = root.join("live");
    if let Err(err) = std::fs::create_dir_all(&live) {
        return Check::fail(
            "ledger",
            format!("{} is not usable ({err})", root.display()),
        );
    }
    let probe = live.join(format!("doctor-probe-{}.json", std::process::id()));
    let wrote = std::fs::write(&probe, b"{}\n");
    let cleaned = std::fs::remove_file(&probe);
    if let Err(err) = wrote.or(cleaned) {
        return Check::fail(
            "ledger",
            format!(
                "{} is not writable (probe {}): {err}",
                root.display(),
                probe.display()
            ),
        );
    }
    Check::pass(
        "ledger",
        format!(
            "{} is writable (probe under live/ created and deleted)",
            root.display()
        ),
    )
}

// ── policy ────────────────────────────────────────────────────────────────

fn check_policy() -> Check {
    policy_check(route::Policy::embedded(), "embedded")
}

/// `check_policy` with the parse result supplied, so a broken policy can be
/// fed in a test without rebuilding the binary.
fn policy_check(parsed: Result<route::Policy>, source: &str) -> Check {
    match parsed {
        Ok(policy) => Check::pass(
            "policy",
            format!(
                "{source} routing_policy.toml parses: policy_version {}, {} classes",
                policy.policy_version,
                policy.classes.len()
            ),
        ),
        Err(err) => Check::fail(
            "policy",
            format!("{source} routing_policy.toml does not parse: {err:#}"),
        ),
    }
}

// ── hook-roundtrip ────────────────────────────────────────────────────────

const ROUNDTRIP_TIMEOUT: Duration = Duration::from_secs(30);
const BINARY_VERSION_TIMEOUT: Duration = Duration::from_secs(15);

fn check_roundtrip(self_path: &Path, host: &Path) -> Check {
    let payload = json!({
        "session_id": "forge-doctor-probe",
        "tool_name": "Bash",
        "tool_input": {"command": "true"},
        "cwd": host.display().to_string(),
        "hook_event_name": "PreToolUse",
    });
    let mut cmd = Command::new(self_path);
    cmd.arg("hook")
        .arg("PreToolUse")
        .current_dir(host)
        .env("CLAUDE_PROJECT_DIR", host)
        .env("FORGE_MODE", "warn")
        .env("FORGE_HOOK_STANDALONE", "1")
        .env("FORGE_LEDGER_DISABLE", "1")
        .env("CONTEXT_TELEMETRY_PATH", "");
    let start = Instant::now();
    let ran = run_capture_bounded(cmd, Some(payload.to_string().as_bytes()), ROUNDTRIP_TIMEOUT);
    let elapsed_ms = start.elapsed().as_millis();
    match ran {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let trimmed = stdout.trim();
            if trimmed.is_empty() || serde_json::from_str::<Value>(trimmed).is_ok() {
                Check::pass(
                    "hook-roundtrip",
                    format!(
                        "exit 0, stdout {}, {elapsed_ms} ms",
                        if trimmed.is_empty() {
                            "empty"
                        } else {
                            "valid JSON"
                        }
                    ),
                )
            } else {
                Check::fail(
                    "hook-roundtrip",
                    format!("stdout is neither empty nor JSON: {}", tail(trimmed, 120)),
                )
            }
        }
        Ok(output) => Check::fail(
            "hook-roundtrip",
            format!(
                "exited {}: {}",
                output
                    .status
                    .code()
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "by signal".to_string()),
                tail(&String::from_utf8_lossy(&output.stderr), 200)
            ),
        ),
        Err(err) => Check::fail(
            "hook-roundtrip",
            format!("{}: {err:#}", self_path.display()),
        ),
    }
}

// ── output ────────────────────────────────────────────────────────────────

fn render_text(checks: &[Check]) -> String {
    checks
        .iter()
        .map(|c| format!("{} {}: {}", c.status.as_str(), c.name, c.detail))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_json(checks: &[Check]) -> String {
    let rows: Vec<Value> = checks
        .iter()
        .map(|c| {
            json!({
                "name": c.name,
                "status": c.status.as_str(),
                "detail": c.detail,
            })
        })
        .collect();
    let ok = !checks.iter().any(|c| c.status == Status::Fail);
    serde_json::to_string_pretty(&json!({ "checks": rows, "ok": ok }))
        .expect("plain strings always serialize")
}

// ── shared process plumbing ───────────────────────────────────────────────

fn run_capture_bounded(
    mut command: Command,
    stdin: Option<&[u8]>,
    timeout: Duration,
) -> Result<Output> {
    match stdin {
        Some(bytes) => {
            forge::process_probe::run_with_input(&mut command, bytes, timeout, 256 * 1024)
        }
        None => forge::process_probe::run(&mut command, timeout, 256 * 1024),
    }
}

/// The last `max` characters of `text`, trimmed -- the readable tail of an
/// error, never the whole wall of a Python traceback.
fn tail(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    let count = trimmed.chars().count();
    if count <= max {
        return trimmed.to_string();
    }
    let start = trimmed
        .char_indices()
        .nth(count - max)
        .map(|(i, _)| i)
        .expect("count > max guarantees the index exists");
    trimmed[start..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ScratchDir(PathBuf);
    impl ScratchDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "forge-doctor-test-{tag}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            ScratchDir(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn no_settings_file_means_settings_fail_and_exit_one() {
        let scratch = ScratchDir::new("no-settings");
        let (check, entries) = check_settings(scratch.path());
        assert_eq!(check.status, Status::Fail);
        assert!(
            check.detail.contains("no forge hook entries"),
            "{}",
            check.detail
        );
        assert!(check.detail.contains("settings.json"), "{}", check.detail);
        assert!(entries.is_empty());
        let code = run(&DoctorArgs {
            host: scratch.path().to_path_buf(),
            json: false,
        })
        .unwrap();
        assert_eq!(code, 1);
    }

    #[test]
    fn installed_via_hooks_install_helpers_means_settings_pass() {
        let scratch = ScratchDir::new("installed");
        hooks_install::install(&hooks_install::InstallArgs {
            host: scratch.path().to_path_buf(),
            mode: "warn".to_string(),
            standalone: true,
            binary: Some(PathBuf::from("/opt/forge/bin/forge")),
            dry_run: false,
            takeover: false,
        })
        .unwrap();
        let (check, entries) = check_settings(scratch.path());
        assert_eq!(check.status, Status::Pass, "{}", check.detail);
        assert!(
            check.detail.contains("PreToolUse, SubagentStop"),
            "{}",
            check.detail
        );
        assert!(check.detail.contains("mode=warn"), "{}", check.detail);
        assert!(check.detail.contains("standalone=true"), "{}", check.detail);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn an_unparsable_policy_fails_and_the_embedded_one_passes() {
        let broken = policy_check(route::Policy::parse("{{ not toml"), "test");
        assert_eq!(broken.status, Status::Fail);
        assert!(
            broken.detail.contains("does not parse"),
            "{}",
            broken.detail
        );
        let embedded = check_policy();
        assert_eq!(embedded.status, Status::Pass, "{}", embedded.detail);
        assert!(embedded.detail.contains("classes"), "{}", embedded.detail);
    }

    #[test]
    fn json_output_carries_every_check_with_name_status_and_detail() {
        // The ledger check resolves LEDGER_ROOT from this process's env; pin
        // it to a scratch dir under the env lock so the test can never probe
        // (or write into) whatever ledger root the machine running it has.
        let _guard = crate::ledger::test_env::ENV_LOCK.lock().unwrap();
        let scratch = ScratchDir::new("json");
        let ambient = std::env::var("LEDGER_ROOT").ok();
        std::env::set_var("LEDGER_ROOT", scratch.path().join("ledger"));
        let (settings, entries) = check_settings(scratch.path());
        let checks = vec![
            settings,
            check_binaries(&entries),
            check_ledger(),
            check_policy(),
        ];
        match ambient {
            Some(value) => std::env::set_var("LEDGER_ROOT", value),
            None => std::env::remove_var("LEDGER_ROOT"),
        }
        let text = render_json(&checks);
        let parsed: Value = serde_json::from_str(&text).unwrap();
        let rows = parsed["checks"].as_array().unwrap();
        assert_eq!(rows.len(), checks.len());
        for row in rows {
            assert!(row["name"].is_string());
            assert!(matches!(
                row["status"].as_str(),
                Some("PASS") | Some("FAIL") | Some("SKIP")
            ));
            assert!(row["detail"].is_string());
        }
        assert_eq!(parsed["ok"], json!(false), "the settings FAIL must flip ok");
    }

    /// Every native fixture consumes stdin before quiet success, invalid-output
    /// success, or a nonzero exit, so process-exit timing cannot mask diagnostics.
    #[test]
    fn roundtrip_check_passes_a_quiet_child_and_fails_a_noisy_one() {
        let scratch = ScratchDir::new("roundtrip");
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/doctor_roundtrip.rs");
        let executable = scratch.path().join("roundtrip-fixture");
        let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let compiled = Command::new(compiler)
            .args([
                "--edition=2021",
                "--crate-name=forge_doctor_roundtrip_fixture",
            ])
            .args(["-C", "codegen-units=1", "-C", "debuginfo=0"])
            .arg(source)
            .arg("-o")
            .arg(&executable)
            .output()
            .expect("launch native doctor fixture compiler");
        assert!(
            compiled.status.success(),
            "doctor fixture compilation failed: {}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let quiet = scratch.path().join("quiet");
        let noisy = scratch.path().join("noisy");
        let failed = scratch.path().join("failed");
        for path in [&quiet, &noisy, &failed] {
            std::fs::hard_link(&executable, path).unwrap();
        }
        let pass = check_roundtrip(&quiet, scratch.path());
        assert_eq!(pass.status, Status::Pass, "{}", pass.detail);
        assert!(pass.detail.contains("stdout empty"), "{}", pass.detail);
        assert!(pass.detail.contains("ms"), "{}", pass.detail);
        let fail = check_roundtrip(&noisy, scratch.path());
        assert_eq!(fail.status, Status::Fail);
        assert!(
            fail.detail.contains("neither empty nor JSON"),
            "{}",
            fail.detail
        );
        let failed = check_roundtrip(&failed, scratch.path());
        assert_eq!(failed.status, Status::Fail);
        assert!(failed.detail.contains("exited 1"), "{}", failed.detail);
        assert!(
            failed.detail.contains("native fixture failure"),
            "{}",
            failed.detail
        );
    }
}
