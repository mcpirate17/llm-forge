//! Native random identities for owned transient resources; never an authorization token.
use anyhow::{ensure, Context, Result};
use std::io;
pub fn new(prefix: &str) -> Result<String> {
    ensure!(
        !prefix.is_empty()
            && prefix.len() <= 32
            && prefix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
        "invalid native resource ID prefix"
    );
    let mut random = [0u8; 16];
    let mut offset = 0;
    while offset < random.len() {
        let count = unsafe {
            libc::getrandom(
                random[offset..].as_mut_ptr().cast(),
                random.len() - offset,
                0,
            )
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error).context("generate native resource nonce");
        }
        ensure!(count > 0, "empty native resource nonce read");
        offset += count as usize;
    }
    use std::fmt::Write;
    let mut name = format!("{prefix}-{}-", std::process::id());
    for byte in random {
        write!(&mut name, "{byte:02x}")?;
    }
    Ok(name)
}
