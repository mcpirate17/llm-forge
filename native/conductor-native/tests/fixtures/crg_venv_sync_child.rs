//! Test-only interpreter for CRG venv-sync subprocess probes.

use std::env;
use std::path::Path;
use std::process;

fn main() {
    let executable = env::args().next().unwrap_or_default();
    if Path::new(&executable)
        .file_name()
        .is_some_and(|name| name == "broken-python")
    {
        process::exit(3);
    }
    let args: Vec<String> = env::args().collect();
    let command = args.get(2).map(String::as_str).unwrap_or_default();
    if command.contains("sysconfig") {
        println!("{}", env::var("FAKE_PURELIB").unwrap_or_default());
    } else if command.contains("crg_server") {
        if let Ok(error) = env::var("FAKE_IMPORT_ERROR") {
            if !error.is_empty() {
                eprintln!("Traceback (most recent call last):");
                eprintln!("ImportError: {error}");
                process::exit(1);
            }
        }
    } else {
        eprintln!("unexpected: {command}");
        process::exit(2);
    }
}
