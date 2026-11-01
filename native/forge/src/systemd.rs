//! Shared native connection and bounded commands for the local user service manager.
use anyhow::{ensure, Context, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

#[derive(Clone)]
pub struct Manager {
    environment: Vec<(OsString, OsString)>,
}

impl Manager {
    pub fn connect(minimum_version: u32) -> Result<Self> {
        let manager = Self {
            environment: user_environment()?,
        };
        let mut command = manager.control(&["show", "--property=Version", "--value"]);
        let runtime = manager
            .successful(&mut command)
            .context("systemd user manager is unavailable; no task was launched")?;
        let runtime = version(String::from_utf8(runtime.stdout)?.trim())?;
        ensure!(
            runtime >= minimum_version,
            "systemd manager version {runtime} is older than required {minimum_version}"
        );
        let client = manager
            .successful(Command::new("systemd-run").arg("--version"))
            .context("systemd-run is unavailable; no task was launched")?;
        let client = String::from_utf8(client.stdout)?;
        let client = version(
            client
                .split_whitespace()
                .nth(1)
                .context("missing systemd-run version")?,
        )?;
        ensure!(
            client >= minimum_version,
            "systemd-run version {client} is older than required {minimum_version}"
        );
        Ok(manager)
    }

    pub fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }

    pub fn control(&self, args: &[&str]) -> Command {
        let mut command = Command::new("systemctl");
        command
            .args(["--user", "--no-pager", "--no-ask-password"])
            .args(args)
            .envs(self.environment.iter().cloned());
        command
    }

    pub fn successful(&self, command: &mut Command) -> Result<Output> {
        self.successful_for(command, Duration::from_secs(3))
    }

    pub(crate) fn successful_for(
        &self,
        command: &mut Command,
        timeout: Duration,
    ) -> Result<Output> {
        let output = crate::process_probe::run(command, timeout, 65536)?;
        ensure!(
            output.status.success(),
            "resource backend command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(output)
    }
}

fn version(text: &str) -> Result<u32> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    ensure!(digits > 0, "invalid service-manager version");
    Ok(text[..digits].parse()?)
}

#[cfg(unix)]
fn verified_runtime(runtime: &Path, uid: u32) -> Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let directory = std::fs::symlink_metadata(runtime).context("user runtime directory")?;
    ensure!(
        directory.is_dir() && directory.uid() == uid,
        "user runtime directory is not owned by this UID"
    );
    let socket =
        std::fs::symlink_metadata(runtime.join("bus")).context("user manager bus socket")?;
    ensure!(
        socket.file_type().is_socket() && socket.uid() == uid,
        "user bus is not a socket owned by this UID"
    );
    Ok(())
}

fn user_environment() -> Result<Vec<(OsString, OsString)>> {
    #[cfg(unix)]
    {
        let uid = unsafe { libc::getuid() };
        let configured = std::env::var_os("XDG_RUNTIME_DIR");
        let runtime = configured
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}")));
        let mut environment = Vec::new();
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            verified_runtime(&runtime, uid)?;
            let mut address = OsString::from("unix:path=");
            address.push(runtime.join("bus"));
            environment.push(("DBUS_SESSION_BUS_ADDRESS".into(), address));
            if configured.is_none() {
                environment.push(("XDG_RUNTIME_DIR".into(), runtime.into_os_string()));
            }
        } else if configured.is_none() && verified_runtime(&runtime, uid).is_ok() {
            environment.push(("XDG_RUNTIME_DIR".into(), runtime.into_os_string()));
        }
        Ok(environment)
    }
    #[cfg(not(unix))]
    bail!("systemd process-tree limits require Linux")
}
