//! Native child process for the SessionStart hook's external command seam.
//!
//! The hook invokes a selected Python for three conductor modules and a gate
//! executable. This fixture models only those process protocols; the Rust
//! integration target supplies every assertion and fixture host.

use std::env;
use std::fs;
use std::io::{self, Read, Write};

fn input() -> io::Result<String> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    Ok(input)
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.as_slice() {
        [module, name, command]
            if module == "-m" && name == "conductor.active_state" && command == "update" => {}
        [module, name, command, flag, _]
            if module == "-m"
                && name == "conductor.session_preamble"
                && command == "hook"
                && flag == "--a2a-name" =>
        {
            println!(
                r#"{{"hookSpecificOutput":{{"hookEventName":"SessionStart","additionalContext":"GENERIC"}}}}"#
            );
        }
        [module, name, command, hook, hook_name, category, category_name]
            if module == "-m"
                && name == "conductor.context_telemetry"
                && command == "hook-context"
                && hook == "--hook"
                && hook_name == "session-start"
                && category == "--category"
                && category_name == "instructions" =>
        {
            io::stdout().write_all(input()?.as_bytes())?;
        }
        [command] if command == "start" => {
            let _ = input()?;
        }
        [session, preamble, host, _, name, _]
            if session == "session"
                && preamble == "preamble"
                && host == "--host"
                && name == "--a2a-name" =>
        {
            if let Some(marker) = env::var_os("SEAM_FORGE_ARGS") {
                let marker = std::path::PathBuf::from(marker);
                fs::write(&marker, format!("{}\n", args.join("\n")))?;
                fs::write(marker.with_extension("exe"), env::args().next().unwrap())?;
            }
            println!(
                r#"{{"hookSpecificOutput":{{"hookEventName":"SessionStart","additionalContext":"FORGE-PREAMBLE"}}}}"#
            );
        }
        _ => return Err(format!("unrecognized hook seam child arguments: {args:?}").into()),
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("hook_project_seam_child: {error}");
        std::process::exit(2);
    }
}
