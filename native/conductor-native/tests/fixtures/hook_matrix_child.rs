//! Native executable fixture for the hook matrix's two launcher protocols.

use std::env;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let invoked = PathBuf::from(env::args_os().next().ok_or("missing argv[0]")?);
    match invoked.file_name().and_then(|name| name.to_str()) {
        Some("obsidian_sync.py") => {
            io::copy(&mut io::stdin(), &mut io::sink())?;
            println!(r#"{{"hookSpecificOutput":{{"hookEventName":"PostToolUse"}}}}"#);
            Ok(())
        }
        Some("crg_gate.py") => {
            let root = invoked
                .parent()
                .and_then(|dir| dir.parent())
                .ok_or("gate root")?;
            let body = root.join("tooling/hooks/agent/crg_gate.py");
            let python = env::var_os("HOOK_MATRIX_PYTHON").ok_or("HOOK_MATRIX_PYTHON unset")?;
            let error = Command::new(python)
                .arg(body)
                .args(env::args_os().skip(1))
                .env("PROJECT_DIR", root)
                .exec();
            Err(error.into())
        }
        other => Err(format!("unknown hook matrix protocol: {other:?}").into()),
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("hook_matrix_child: {error}");
        std::process::exit(2);
    }
}
