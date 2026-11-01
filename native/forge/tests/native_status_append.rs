#[allow(dead_code)]
mod support;
use forge::session_status::append_status;
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::time::Duration;

fn host() -> support::Host {
    let host = support::Host::new();
    git2::Repository::init(&host.0).unwrap();
    fs::write(host.0.join("pyproject.toml"), "").unwrap();
    host
}

#[test]
fn newest_insertion_preserves_every_existing_byte_and_refreshes_headings() {
    let host = host();
    let path = host.0.join(".current_work.md");
    let prefix = "# Existing banner\r\n\r\n";
    let suffix = "## Earlier — timestamp, old-owner\r\n\r\nExisting 🦀 body.\r\n";
    fs::write(&path, format!("{prefix}{suffix}")).unwrap();
    let result = append_status(
        &host.0,
        "native-agent",
        "  Ready\n  for review  ",
        "Bounded result.",
    )
    .unwrap();
    assert_eq!(result.exit_code(), 0, "{:?}", result.refresh_error);
    assert!(result.entry.starts_with("## Ready for review — "));
    assert!(result
        .entry
        .contains(" UTC, native-agent\n\nBounded result.\n\n"));
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        format!("{prefix}{}{suffix}", result.entry)
    );
    let state: Value =
        serde_json::from_slice(&fs::read(host.0.join("conductor/active_state.json")).unwrap())
            .unwrap();
    assert!(state["active_headings"][0]
        .as_str()
        .unwrap()
        .starts_with("Ready for review — "));
    assert_eq!(
        state["active_headings"][1],
        "Earlier — timestamp, old-owner"
    );
}

#[test]
fn missing_and_heading_free_logs_use_bounded_default_insertion() {
    let host = host();
    let path = host.0.join(".current_work.md");
    let result = append_status(&host.0, "agent", "First", "one").unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        format!("# Active Coordination\n\n{}", result.entry)
    );
    for existing in ["", "Existing preamble", "Existing preamble\n"] {
        fs::write(&path, existing).unwrap();
        let result = append_status(&host.0, "agent", "Next", "two").unwrap();
        let separator = if existing.is_empty() || existing.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("{existing}{separator}{}", result.entry)
        );
    }
}

#[test]
fn bounds_count_unicode_characters_and_reject_heading_injection_without_writes() {
    let host = host();
    let title = "🦀".repeat(120);
    let body = "é".repeat(1200);
    assert_eq!(
        append_status(&host.0, "agent", &title, &body)
            .unwrap()
            .exit_code(),
        0
    );
    let path = host.0.join(".current_work.md");
    let original = fs::read(&path).unwrap();
    for (owner, title, body) in [
        ("bad\n## owner", "title".into(), "body".into()),
        ("../owner", "title".into(), "body".into()),
        ("agent", "🦀".repeat(121), "body".into()),
        ("agent", "title".into(), "é".repeat(1201)),
        ("agent", "title".into(), vec!["line"; 13].join("\n")),
        ("agent", "title".into(), vec!["line"; 13].join("\u{2028}")),
        ("agent", "title".into(), "body\n## forged heading".into()),
        ("agent", "title".into(), "body\n  ##\tforged heading".into()),
        ("agent", "title\u{1b}".into(), "body".into()),
        ("agent", "title".into(), "".into()),
    ] {
        assert!(append_status(&host.0, owner, &title, &body).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
    }
    assert!(append_status(&host.0, "agent", "Twelve lines", &["line"; 12].join("\r\n")).is_ok());
}

#[test]
fn unsafe_status_destinations_and_invalid_utf8_are_rejected_without_clobbering() {
    let host = host();
    let path = host.0.join(".current_work.md");
    let target = host.0.join("retained");
    fs::write(&target, "retained bytes").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(append_status(&host.0, "agent", "title", "body").is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "retained bytes");
    fs::remove_file(&path).unwrap();
    fs::hard_link(&target, &path).unwrap();
    assert!(append_status(&host.0, "agent", "title", "body").is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "retained bytes");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(append_status(&host.0, "agent", "title", "body").is_err());
    fs::remove_dir(&path).unwrap();
    let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(append_status(&host.0, "agent", "title", "body").is_err());
    fs::remove_file(&path).unwrap();
    for bytes in [vec![0xff], vec![b'x'; 1024 * 1024 + 1]] {
        fs::write(&path, &bytes).unwrap();
        assert!(append_status(&host.0, "agent", "title", "body").is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn concurrent_native_writers_preserve_all_entries_and_latest_state() {
    let host = host();
    let barrier = Arc::new(Barrier::new(8));
    let threads = (0..8)
        .map(|index| {
            let path = host.0.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                append_status(
                    &path,
                    &format!("agent-{index}"),
                    &format!("result {index}"),
                    "bounded body",
                )
                .unwrap()
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        assert_eq!(thread.join().unwrap().exit_code(), 0);
    }
    let text = fs::read_to_string(host.0.join(".current_work.md")).unwrap();
    for index in 0..8 {
        assert_eq!(text.matches(&format!("## result {index} — ")).count(), 1);
    }
    let state: Value =
        serde_json::from_slice(&fs::read(host.0.join("conductor/active_state.json")).unwrap())
            .unwrap();
    let first = text
        .lines()
        .find(|line| line.starts_with("## "))
        .unwrap()
        .trim_start_matches("## ");
    assert_eq!(state["active_headings"][0], first);
}

#[test]
fn unsafe_lock_is_rejected_and_a_busy_lock_has_a_finite_deadline() {
    let host = host();
    let path = host.0.join(".current_work.lock");
    let target = host.0.join("retained");
    fs::write(&target, "retained bytes").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(append_status(&host.0, "agent", "title", "body").is_err());
    assert!(!host.0.join(".current_work.md").exists());
    fs::remove_file(&path).unwrap();
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    let _lock = forge::file_lock::Exclusive::try_new(file, "fixture").unwrap();
    let started = std::time::Instant::now();
    let error = append_status(&host.0, "agent", "title", "body").unwrap_err();
    assert!(error.to_string().contains("deadline"));
    assert!(started.elapsed() < Duration::from_secs(6));
    assert!(!host.0.join(".current_work.md").exists());
}

#[test]
fn cli_without_path_reports_durable_append_with_failed_refresh_distinctly() {
    let host = host();
    let outside = host.0.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("active_state.json"), "retained bytes").unwrap();
    std::os::unix::fs::symlink(&outside, host.0.join("conductor")).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
    command
        .env_clear()
        .env("PATH", "")
        .env("HOME", &host.0)
        .args(["session", "append", "--host"])
        .arg(&host.0)
        .args([
            "--owner",
            "native-agent",
            "--title",
            "Durable status",
            "--body",
            "Written once",
            "--json",
        ]);
    let output = forge::process_probe::run(&mut command, Duration::from_secs(8), 65536).unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["active_state_refreshed"], false);
    assert!(result["refresh_error"]
        .as_str()
        .unwrap()
        .contains("symlink"));
    assert!(fs::read_to_string(host.0.join(".current_work.md"))
        .unwrap()
        .contains("Written once"));
    assert_eq!(
        fs::read_to_string(outside.join("active_state.json")).unwrap(),
        "retained bytes"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("written, but"));
}
