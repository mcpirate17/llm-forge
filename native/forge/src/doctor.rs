//! `forge doctor --host DIR`: verify a host install end to end
//! (`docs/roadmap.md` Phase 4, item 2c). `forge hooks status` proves the
//! settings entries exist and their binary answers `--version`; it says
//! nothing about the other half of an install -- that the Python package
//! forge delegates to is importable from the host, that the ledger root is
//! writable, that the routing policy parses, and that a real hook call
//! answers at all. One command, one line per check, exit 1 on any FAIL:
//! Tim runs it on a host and either sees green or the exact broken piece.
//!
//! Checks, in order (all of them always run and always print):
//!
//! * `settings` -- forge's entries in `DIR/.claude/settings.json`, reusing
//!   `hooks_install`'s loader and command parser.
//! * `binary` -- every installed entry's binary exists, is executable, and
//!   its `--version` rev matches this running binary.
//! * `python` -- the interpreter `interpreter::resolve_python` would pick
//!   for `DIR` can `import conductor, tooling.hooks.dispatch` with `cwd` at
//!   `DIR` (what `dispatch::delegate` spawns on every non-standalone call).
//!   SKIP when every installed entry is standalone: forge never delegates.
//! * `ledger` -- the ledger root `ledger::resolve_ledger_root` picks
//!   (`LEDGER_ROOT` env override honoured) is writable, proven by
//!   creating and deleting a probe file under `live/` (where `cap_enforce`
//!   keeps its per-agent state).
//! * `policy` -- the routing policy this binary actually answers from
//!   (embedded at build time by `route.rs`) parses; report the class count.
//! * `hook-roundtrip` -- this binary re-invoked as a child exactly as the
//!   standalone install wires it (`FORGE_MODE=warn FORGE_HOOK_STANDALONE=1
//!   <self> hook PreToolUse`), fed a synthetic Bash payload on stdin. PASS
//!   is exit 0 with empty or JSON stdout. The probe writes nothing:
//!   `FORGE_LEDGER_DISABLE=1` for the rollup paths and an explicitly empty
//!   `CONTEXT_TELEMETRY_PATH` (telemetry's default lives under the ledger
//!   root; the empty path is its documented no-write state).

use crate::{hooks_install, interpreter, ledger, route};
use anyhow::{Context, Result};
use clap::Args;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
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
        check_python(host, &entries),
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
            "{} binar{} checked, versions match {}",
            binaries.len(),
            if binaries.len() > 1 { "ies" } else { "y" },
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

fn check_python(host: &Path, entries: &[hooks_install::ParsedHook]) -> Check {
    if entries.is_empty() {
        return Check::skip(
            "python",
            "no forge entries installed (settings FAILED above)".to_string(),
        );
    }
    if entries.iter().all(|e| e.standalone) {
        return Check::skip(
            "python",
            "every installed entry is standalone; forge never delegates to Python".to_string(),
        );
    }
    let python = interpreter::resolve_python(host);
    let mut cmd = Command::new(&python);
    cmd.arg("-c")
        .arg("import conductor, tooling.hooks.dispatch")
        .current_dir(host);
    match run_capture_bounded(cmd, None, PYTHON_IMPORT_TIMEOUT) {
        Ok(output) if output.status.success() => Check::pass(
            "python",
            format!(
                "{} imports conductor and tooling.hooks.dispatch from {}",
                python.display(),
                host.display()
            ),
        ),
        Ok(output) => Check::fail(
            "python",
            format!(
                "{} -c 'import conductor, tooling.hooks.dispatch' (cwd {}) failed: {}",
                python.display(),
                host.display(),
                tail(&String::from_utf8_lossy(&output.stderr), 200)
            ),
        ),
        Err(err) => Check::fail(
            "python",
            format!("could not run {}: {err:#}", python.display()),
        ),
    }
}

// ── ledger ────────────────────────────────────────────────────────────────

fn check_ledger() -> Check {
    let root = ledger::resolve_ledger_root(None);
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
const PYTHON_IMPORT_TIMEOUT: Duration = Duration::from_secs(60);

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

/// Runs `command` to completion, capturing stdout/stderr, feeding `stdin`
/// when given, and killing the child by pid when it outlives `timeout` --
/// the same deadline-by-thread pattern as `bounded_child::run_bounded`,
/// which cannot be reused here because it fixes the executable to
/// `current_exe`, nulls stdout, and takes no stdin or env.
fn run_capture_bounded(
    mut command: Command,
    stdin: Option<&[u8]>,
    timeout: Duration,
) -> Result<Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    match stdin {
        Some(bytes) => {
            command.stdin(Stdio::piped());
            let mut child = command.spawn().context("spawning the probe child")?;
            {
                let mut stdin = child.stdin.take().expect("piped stdin");
                // A child that exits before reading its payload (a broken
                // binary -- exactly what the roundtrip check exists to
                // catch) closes the pipe and turns this write into EPIPE.
                // That is the child's verdict to report, not a probe
                // failure: swallow BrokenPipe and let the wait below judge
                // by exit status and stdout. Every other write error is a
                // real feeding failure and fails the check.
                if let Err(err) = stdin.write_all(bytes) {
                    if err.kind() != std::io::ErrorKind::BrokenPipe {
                        return Err(err).context("feeding the probe child its stdin payload");
                    }
                }
            }
            wait_bounded(child, timeout)
        }
        None => {
            command.stdin(Stdio::null());
            let child = command.spawn().context("spawning the probe child")?;
            wait_bounded(child, timeout)
        }
    }
}

/// The deadline half of `run_capture_bounded`: the blocking wait happens on
/// a thread, and the main thread kills the child by pid if the deadline
/// passes first (`Child::wait` blocks with no deadline of its own).
fn wait_bounded(child: std::process::Child, timeout: Duration) -> Result<Output> {
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let outcome = match rx.recv_timeout(timeout) {
        Ok(received) => received.context("waiting for the probe child"),
        Err(_) => {
            let _ = Command::new("kill")
                .arg(pid.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            Err(anyhow::anyhow!(
                "timed out after {} ms (child killed)",
                timeout.as_millis()
            ))
        }
    };
    let _ = handle.join();
    outcome
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

    fn stub_script(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms).unwrap();
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
            check_python(scratch.path(), &entries),
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

    /// One roundtrip-shape sanity check on the shared plumbing: a stub child
    /// that exits 0 with empty stdout is the exact PASS shape, one that
    /// writes garbage is the FAIL shape. The real-binary roundtrip is
    /// exercised live in item 5 of the slice (the LLM-host run).
    #[test]
    fn roundtrip_check_passes_a_quiet_child_and_fails_a_noisy_one() {
        let scratch = ScratchDir::new("roundtrip");
        let quiet = scratch.path().join("forge-quiet");
        stub_script(&quiet, "#!/bin/sh\n");
        let pass = check_roundtrip(&quiet, scratch.path());
        assert_eq!(pass.status, Status::Pass, "{}", pass.detail);
        assert!(pass.detail.contains("ms"), "{}", pass.detail);
        let noisy = scratch.path().join("forge-noisy");
        stub_script(&noisy, "#!/bin/sh\necho 'not json'\n");
        let fail = check_roundtrip(&noisy, scratch.path());
        assert_eq!(fail.status, Status::Fail);
        assert!(
            fail.detail.contains("neither empty nor JSON"),
            "{}",
            fail.detail
        );
        let dead = scratch.path().join("forge-dead");
        stub_script(&dead, "#!/bin/sh\nexit 3\n");
        let failed = check_roundtrip(&dead, scratch.path());
        assert_eq!(failed.status, Status::Fail);
        assert!(failed.detail.contains("exited 3"), "{}", failed.detail);
    }
}
