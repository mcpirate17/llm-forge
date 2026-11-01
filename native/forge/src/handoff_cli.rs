use super::*;
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args)]
pub struct HandoffArgs {
    /// Host project root. Live state is read here; no claims are changed.
    #[arg(long, global = true, default_value = ".")]
    host: PathBuf,
    #[command(subcommand)]
    action: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Write a prepared envelope at an explicit path.
    Prepare {
        #[command(flatten)]
        content: Content,
        #[arg(long, default_value = "{}")]
        runtime_json: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Revalidate governance, activate idempotently and print the context.
    Activate {
        #[arg(long)]
        input: PathBuf,
    },
    /// Stage the next session's handoff, linking to its predecessor.
    Stage {
        #[command(flatten)]
        content: Content,
        #[arg(long)]
        identity: Option<String>,
    },
    /// Emit SessionStart JSON; a staged envelope is injected at most once.
    Hook {
        #[arg(long)]
        identity: Option<String>,
    },
}
#[derive(Args)]
struct Content {
    #[arg(long)]
    task: String,
    #[arg(long, num_args=0..)]
    paths: Vec<String>,
    #[arg(long, conflicts_with = "context_file")]
    context: Option<String>,
    #[arg(long)]
    context_file: Option<PathBuf>,
}

pub(super) fn resolve_identity(root: &Path, explicit: Option<&str>) -> Result<String> {
    let name = if let Some(name) = explicit {
        name.to_owned()
    } else if let Some(name) = std::env::var_os("A2A_AGENT_NAME") {
        name.into_string()
            .map_err(|_| anyhow::anyhow!("A2A_AGENT_NAME is not UTF-8"))?
    } else {
        let path = root.join(".agents/a2a/default_identity");
        match std::fs::symlink_metadata(&path) {
            Ok(_) => String::from_utf8(forge::bounded_file::read(&path, 4096)?)?
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .into(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => "claude".into(),
            Err(e) => return Err(e.into()),
        }
    };
    validate_identity(&name)?;
    Ok(name)
}

fn content_text(
    root: &Path,
    content: &Content,
    state: &ActiveState,
    identity: Option<&str>,
) -> Result<String> {
    if let Some(path) = &content.context_file {
        return Ok(String::from_utf8(forge::bounded_file::read(
            path,
            1024 * 1024,
        )?)?);
    }
    if let Some(text) = &content.context {
        return Ok(text.clone());
    }
    crate::session_brief::for_handoff(root, &content.task, &content.paths, state, identity)
}

fn write_prepared(
    root: &Path,
    content: Content,
    runtime: BTreeMap<String, Value>,
    path: &Path,
    staged: bool,
) -> Result<Envelope> {
    let _lock = store::lock(path)?;
    let parent = if staged {
        match std::fs::symlink_metadata(path) {
            Ok(_) => Some(store::load(path)?.handoff_id),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        }
    } else {
        None
    };
    let state = active_state::generate_active_state(root)?;
    let context = content_text(
        root,
        &content,
        &state,
        runtime.get("identity").and_then(Value::as_str),
    )?;
    let envelope = prepare(
        Preparation {
            task: &content.task,
            paths: content.paths,
            runtime,
            context: &context,
            parent,
        },
        serde_json::to_value(state)?,
        instant::now(),
    )?;
    store::save(path, &envelope)?;
    Ok(envelope)
}

pub fn run(args: HandoffArgs) -> Result<u8> {
    let root = std::fs::canonicalize(args.host)?;
    match args.action {
        Action::Prepare {
            content,
            runtime_json,
            output,
        } => {
            let runtime =
                serde_json::from_value(forge::tool_schema::parse_document(&runtime_json)?)?;
            let envelope = write_prepared(&root, content, runtime, &output, false)?;
            println!(
                "{}",
                json!({"handoff_id":envelope.handoff_id,"status":envelope.status})
            );
        }
        Action::Stage { content, identity } => {
            let identity = resolve_identity(&root, identity.as_deref())?;
            let path = staged_path(&root, &identity);
            let runtime = BTreeMap::from([
                ("adapter".into(), json!("claude-code-session-start")),
                ("supports_inplace_replace".into(), json!(true)),
                ("identity".into(), json!(identity)),
            ]);
            let envelope = write_prepared(&root, content, runtime, &path, true)?;
            println!(
                "{}",
                json!({"handoff_id":envelope.handoff_id,"staged":path,"context_chars":envelope.context.chars().count()})
            );
        }
        Action::Activate { input } => {
            let state = serde_json::to_value(active_state::generate_active_state(&root)?)?;
            println!(
                "{}",
                activate(&input, &state, instant::now())?.0.live_context()
            );
        }
        Action::Hook { identity } => {
            println!(
                "{}",
                hook_output(&hook_context(
                    &root,
                    &resolve_identity(&root, identity.as_deref())?
                )?)
            );
        }
    }
    Ok(0)
}
