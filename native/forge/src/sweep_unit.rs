//! `forge sweep install`: a systemd user service and timer that run
//! `forge sweep run` in this checkout on the sweep config's `calendar`.
//!
//! The service is a nice'd oneshot under the host's `launcher`, so the cap
//! the host chose bounds the whole sweep; systemd never starts a second one
//! while the first is active. It records the installing shell's PATH: the
//! user manager's own PATH lacks per-user toolchains (node, uv) the steps call.
//! `Persistent=false`: a fire missed while the machine was off is skipped,
//! not run at boot.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::Args;

use super::SweepConfig;
use crate::land::Config;

#[derive(Args)]
pub struct InstallArgs {
    /// Print the units and the worst-case bound; write and enable nothing.
    #[arg(long)]
    print: bool,
}

pub fn install(here: &Path, land: &Config, sweep: &SweepConfig, args: &InstallArgs) -> Result<u8> {
    let name = unit_name(here)?;
    let exe = std::env::current_exe().context("locating the forge binary")?;
    let argv = command(here, &sweep.launcher, &exe);
    let path = dedup_path(&std::env::var("PATH").unwrap_or_default());
    let service = render_service(here, &argv, &path);
    let timer = render_timer(here, &sweep.calendar);
    let bound: u64 = land
        .setup
        .iter()
        .chain(sweep.checks(land))
        .map(|s| s.timeout_s)
        .sum();
    println!(
        "sweep: step timeouts bound one run at {bound} s ({:.1} h)",
        bound as f64 / 3600.0
    );
    if args.print {
        println!("# {name}.service\n{service}\n# {name}.timer\n{timer}");
        return Ok(0);
    }
    systemd(&["systemd-analyze", "calendar", &sweep.calendar])?;
    let dir = unit_dir()?;
    std::fs::create_dir_all(&dir)?;
    let mut paths = Vec::new();
    for (suffix, body) in [("service", &service), ("timer", &timer)] {
        let path = dir.join(format!("{name}.{suffix}"));
        std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
        paths.push(path.to_string_lossy().to_string());
    }
    // `enable --now` exits 0 even when the timer refuses to start because its
    // service has a bad setting, so the units are verified first.
    if let Err(err) = systemd(&["systemd-analyze", "--user", "verify", &paths[0], &paths[1]]) {
        for path in &paths {
            std::fs::remove_file(path).with_context(|| format!("removing {path}"))?;
        }
        return Err(err);
    }
    let timer_unit = format!("{name}.timer");
    systemd(&["systemctl", "--user", "daemon-reload"])?;
    systemd(&["systemctl", "--user", "enable", "--now", &timer_unit])?;
    systemd(&["systemctl", "--user", "is-active", "--quiet", &timer_unit]).with_context(|| {
        format!(
            "{timer_unit} is enabled but not active; see `systemctl --user status {name}.service`"
        )
    })?;
    println!("sweep: {timer_unit} active; `systemctl --user list-timers {timer_unit}` shows the next run");
    Ok(0)
}

/// `forge-sweep-<checkout dir name>`, reduced to unit-name-safe characters.
fn unit_name(here: &Path) -> Result<String> {
    let base = here
        .file_name()
        .and_then(|n| n.to_str())
        .context("checkout path has no usable directory name")?;
    let safe: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(format!("forge-sweep-{safe}"))
}

/// The launcher, its program resolved against the checkout when relative,
/// then this forge binary's `sweep run`.
fn command(here: &Path, launcher: &[String], exe: &Path) -> Vec<String> {
    let mut argv: Vec<String> = launcher.to_vec();
    if let Some(program) = argv.first_mut() {
        if program.contains('/') && !Path::new(program.as_str()).is_absolute() {
            *program = here.join(program.as_str()).to_string_lossy().to_string();
        }
    }
    argv.push(exe.to_string_lossy().to_string());
    argv.extend(["sweep".to_string(), "run".to_string()]);
    argv
}

fn dedup_path(path: &str) -> String {
    let mut seen = Vec::new();
    for dir in path.split(':').filter(|d| !d.is_empty()) {
        if !seen.contains(&dir) {
            seen.push(dir);
        }
    }
    seen.join(":")
}

/// systemd expands `%` specifiers everywhere and `$` variables in ExecStart.
/// Only ExecStart and Environment unquote: WorkingDirectory takes its value
/// verbatim, quotes included, so it gets `%%` alone.
fn quote(arg: &str, exec: bool) -> String {
    let mut escaped = arg
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    if exec {
        escaped = escaped.replace('$', "$$");
    }
    format!("\"{escaped}\"")
}

fn render_service(here: &Path, argv: &[String], path: &str) -> String {
    let exec: Vec<String> = argv.iter().map(|arg| quote(arg, true)).collect();
    format!(
        "[Unit]\nDescription=forge sweep of {here}\n\n\
         [Service]\nType=oneshot\nWorkingDirectory={dir}\n\
         Environment={env}\nExecStart={exec}\nNice=10\nTimeoutStartSec=infinity\n",
        here = here.display().to_string().replace('%', "%%"),
        dir = here.to_string_lossy().replace('%', "%%"),
        env = quote(&format!("PATH={path}"), false),
        exec = exec.join(" "),
    )
}

fn render_timer(here: &Path, calendar: &str) -> String {
    format!(
        "[Unit]\nDescription=forge sweep of {}\n\n\
         [Timer]\nOnCalendar={calendar}\nPersistent=false\n\n\
         [Install]\nWantedBy=timers.target\n",
        here.display().to_string().replace('%', "%%"),
    )
}

fn unit_dir() -> Result<PathBuf> {
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(config).join("systemd/user"));
    }
    let home = std::env::var_os("HOME").context("neither XDG_CONFIG_HOME nor HOME is set")?;
    Ok(PathBuf::from(home).join(".config/systemd/user"))
}

fn systemd(argv: &[&str]) -> Result<()> {
    let output = Command::new(argv[0])
        .args(&argv[1..])
        .output()
        .with_context(|| format!("spawning {}", argv[0]))?;
    if !output.status.success() {
        bail!(
            "{} failed: {}",
            argv.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_name_keeps_only_safe_characters() {
        assert_eq!(unit_name(Path::new("/h/LLM")).unwrap(), "forge-sweep-LLM");
        assert_eq!(
            unit_name(Path::new("/h/my repo.x")).unwrap(),
            "forge-sweep-my_repo_x"
        );
    }

    #[test]
    fn command_resolves_a_relative_launcher_and_appends_sweep_run() {
        let launcher = [
            "tools/cap.sh".to_string(),
            "-t".into(),
            "60".into(),
            "--".into(),
        ];
        let argv = command(Path::new("/h/LLM"), &launcher, Path::new("/bin/forge"));
        assert_eq!(
            argv,
            [
                "/h/LLM/tools/cap.sh",
                "-t",
                "60",
                "--",
                "/bin/forge",
                "sweep",
                "run"
            ]
        );
        let bare = command(
            Path::new("/h"),
            &["nice".to_string()],
            Path::new("/bin/forge"),
        );
        assert_eq!(bare[0], "nice", "a PATH program stays unresolved");
        assert_eq!(
            command(Path::new("/h"), &[], Path::new("/f")),
            ["/f", "sweep", "run"]
        );
    }

    #[test]
    fn service_escapes_specifiers_variables_and_quotes() {
        let argv = [
            "/a b/x".to_string(),
            "50%".into(),
            "$HOME".into(),
            "q\"".into(),
        ];
        let service = render_service(Path::new("/h/LLM"), &argv, "/u/bin:/bin");
        assert!(
            service.contains(r#"ExecStart="/a b/x" "50%%" "$$HOME" "q\"""#),
            "{service}"
        );
        assert!(service.contains("WorkingDirectory=/h/LLM\n"), "{service}");
        let spaced = render_service(Path::new("/h/a b%"), &argv, "/bin");
        assert!(spaced.contains("WorkingDirectory=/h/a b%%\n"), "{spaced}");
        assert!(
            service.contains("Environment=\"PATH=/u/bin:/bin\"\n"),
            "{service}"
        );
        assert!(service.contains("Type=oneshot\n") && service.contains("Nice=10\n"));
    }

    #[test]
    fn timer_fires_on_the_calendar_and_skips_missed_runs() {
        let timer = render_timer(Path::new("/h/LLM"), "*-*-* 01:00:00");
        assert!(timer.contains("OnCalendar=*-*-* 01:00:00\n"), "{timer}");
        assert!(timer.contains("Persistent=false\n"));
        assert!(timer.contains("WantedBy=timers.target\n"));
    }

    #[test]
    fn path_dedup_keeps_first_occurrence_order() {
        assert_eq!(dedup_path("/a:/b::/a:/c:/b"), "/a:/b:/c");
    }
}
