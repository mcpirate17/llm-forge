//! Explicit CLI/environment adaptation for the native clerical core.
use super::{
    draft, inventory, preflight_output, validate_document, write_document, GenerateOptions,
    Settings, MODEL,
};
use anyhow::{ensure, Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_PATH: usize = 4096;
const MAX_PATHS: usize = 1024;
const MAX_ROOTS: usize = 64;

#[derive(Args)]
pub struct ClerkArgs {
    #[arg(long, global = true, default_value = ".")]
    host: PathBuf,
    #[command(subcommand)]
    action: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Hash and inventory explicit files without contacting a model.
    Inventory {
        #[arg(required = true, num_args = 1..)]
        paths: Vec<PathBuf>,
    },
    /// Request a bounded local clerical draft, never an approval.
    Draft {
        #[arg(required = true, num_args = 1..)]
        paths: Vec<PathBuf>,
        #[arg(long, default_value = MODEL)]
        model: String,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Rehash sources and validate a saved draft without model inference.
    Validate { path: PathBuf },
}

/// Captured once by the CLI; pure configuration tests supply values directly.
#[derive(Clone, Debug, Default)]
pub struct ClerkEnv {
    pub home: Option<OsString>,
    pub roots: Option<OsString>,
    pub ollama_host: Option<OsString>,
    pub local_ai_task: Option<OsString>,
}
impl ClerkEnv {
    fn capture() -> Self {
        Self {
            home: std::env::var_os("HOME"),
            roots: std::env::var_os("CONDUCTOR_CLERK_ROOTS"),
            ollama_host: std::env::var_os("OLLAMA_HOST"),
            local_ai_task: std::env::var_os("LOCAL_AI_TASK"),
        }
    }
    fn home(&self) -> Result<PathBuf> {
        let raw = self
            .home
            .as_ref()
            .context("HOME is required for clerk home paths")?;
        let path = checked_path(Path::new(raw))?;
        ensure!(path.is_absolute(), "HOME must be an absolute path");
        Ok(path)
    }
}
fn checked_path(path: &Path) -> Result<PathBuf> {
    let text = path.to_str().context("clerk paths must be UTF-8")?;
    ensure!(
        !text.contains('\0') && text.len() <= MAX_PATH,
        "invalid clerk path or byte limit"
    );
    Ok(path.to_owned())
}
fn expanded(path: &Path, env: &ClerkEnv) -> Result<PathBuf> {
    let path = checked_path(path)?;
    let text = path.to_str().expect("checked UTF-8");
    let value = if text == "~" {
        env.home()?
    } else if let Some(rest) = text.strip_prefix("~/") {
        env.home()?.join(rest)
    } else {
        ensure!(
            !text.starts_with('~'),
            "named-user tilde paths are unsupported"
        );
        path
    };
    checked_path(&value)
}
fn resolved(path: &Path, host: &Path, env: &ClerkEnv) -> Result<PathBuf> {
    let path = expanded(path, env)?;
    checked_path(&if path.is_absolute() {
        path
    } else {
        host.join(path)
    })
}
fn env_text<'a>(value: Option<&'a OsString>, default: &'a str, limit: usize) -> Result<&'a str> {
    let value = value
        .map(|v| v.to_str().context("clerk environment value must be UTF-8"))
        .transpose()?
        .unwrap_or(default);
    ensure!(
        value.len() <= limit && !value.contains('\0'),
        "clerk environment value exceeds bounds"
    );
    Ok(value)
}

/// Pure environment/path adapter, with no global environment mutation or inference.
pub fn settings(host: &Path, env: &ClerkEnv) -> Result<Settings> {
    let host = expanded(host, env)?
        .canonicalize()
        .context("resolve clerk host")?;
    let roots = if let Some(raw) = &env.roots {
        let text = env_text(Some(raw), "", MAX_ROOTS * (MAX_PATH + 1))?;
        let paths: Vec<_> = text.split(':').filter(|item| !item.is_empty()).collect();
        ensure!(paths.len() <= MAX_ROOTS, "too many clerk roots");
        paths
            .into_iter()
            .map(|path| resolved(Path::new(path), &host, env))
            .collect::<Result<Vec<_>>>()?
    } else if env.home.is_some() {
        let home = env.home()?;
        [".claude/tasks", ".codex/memories", "Documents/CodexVault"]
            .into_iter()
            .map(|name| checked_path(&home.join(name)))
            .collect::<Result<Vec<_>>>()?
    } else {
        // An absent home means no personal default roots; explicit ~/ paths fail.
        Vec::new()
    };
    Settings::new(&host, &roots)
}
fn source_paths(paths: &[PathBuf], settings: &Settings, env: &ClerkEnv) -> Result<Vec<PathBuf>> {
    ensure!(
        !paths.is_empty() && paths.len() <= MAX_PATHS,
        "clerk requires 1..1024 source paths"
    );
    paths
        .iter()
        .map(|path| resolved(path, settings.host(), env))
        .collect()
}
fn execute(args: ClerkArgs, env: &ClerkEnv) -> Result<Value> {
    let settings = settings(&args.host, env)?;
    match args.action {
        Action::Inventory { paths } => inventory(&settings, &source_paths(&paths, &settings, env)?),
        Action::Validate { path } => {
            validate_document(&settings, &resolved(&path, settings.host(), env)?)
        }
        Action::Draft {
            paths,
            model,
            output,
        } => {
            let output = output.map(|path| expanded(&path, env)).transpose()?;
            if let Some(path) = &output {
                preflight_output(&settings, path)?;
            }
            let options = GenerateOptions {
                model,
                task_class: env_text(env.local_ai_task.as_ref(), "", 128)?.to_owned(),
                endpoint: env_text(env.ollama_host.as_ref(), "http://127.0.0.1:11434", 4096)?
                    .to_owned(),
                timeout: Duration::from_secs(120),
            };
            let document = draft(&settings, &source_paths(&paths, &settings, env)?, &options)?;
            if let Some(path) = output {
                // Relative outputs remain publisher-relative, matching the legacy CLI.
                write_document(&settings, &path, &document)
            } else {
                Ok(document)
            }
        }
    }
}
fn emit(writer: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}
/// Runtime failures use the clerk's JSON/exit-2 contract; stdio failures propagate.
pub fn run(args: ClerkArgs) -> Result<u8> {
    match execute(args, &ClerkEnv::capture()) {
        Ok(value) => {
            emit(&mut std::io::stdout().lock(), &value)?;
            Ok(0)
        }
        Err(error) => {
            emit(
                &mut std::io::stderr().lock(),
                &json!({"error":format!("{error:#}")}),
            )?;
            Ok(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;
    #[test]
    fn expansion_is_pure_and_home_is_only_required_when_used() {
        let empty = ClerkEnv::default();
        assert_eq!(
            expanded(Path::new("relative"), &empty).unwrap(),
            Path::new("relative")
        );
        assert!(expanded(Path::new("~/note"), &empty).is_err());
        let env = ClerkEnv {
            home: Some("/fixture/home".into()),
            ..ClerkEnv::default()
        };
        assert_eq!(
            resolved(Path::new("~/note"), Path::new("/host"), &env).unwrap(),
            Path::new("/fixture/home/note")
        );
        assert_eq!(
            resolved(Path::new("note"), Path::new("/host"), &env).unwrap(),
            Path::new("/host/note")
        );
        for invalid in ["~other/note", "bad\0path"] {
            assert!(expanded(Path::new(invalid), &env).is_err());
        }
        let invalid = OsString::from_vec(vec![255]);
        assert!(expanded(Path::new(&invalid), &env).is_err());
        assert!(expanded(Path::new(&"x".repeat(MAX_PATH + 1)), &env).is_err());
    }
}
