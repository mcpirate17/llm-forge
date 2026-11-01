//! Native test fixture: basename selects behavior after stdin reaches EOF.
use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> io::Result<ExitCode> {
    if io::copy(&mut io::stdin().lock(), &mut io::sink())? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fixture requires the hook payload on stdin",
        ));
    }
    let executable = std::env::current_exe()?;
    match executable.file_name().and_then(|name| name.to_str()) {
        Some("quiet") => Ok(ExitCode::SUCCESS),
        Some("noisy") => {
            writeln!(io::stdout().lock(), "native fixture output is not JSON")?;
            Ok(ExitCode::SUCCESS)
        }
        Some("failed") => {
            writeln!(io::stderr().lock(), "native fixture failure")?;
            Ok(ExitCode::from(1))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unknown fixture basename",
        )),
    }
}
