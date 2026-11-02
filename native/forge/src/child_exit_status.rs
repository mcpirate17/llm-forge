//! Preserve a failing guest status through anyhow context without changing callers
//! that handle failures as ordinary errors.
use anyhow::Result;
use std::{fmt, os::unix::process::ExitStatusExt, process::ExitStatus};

#[derive(Debug)]
pub(crate) struct ChildExit {
    pub status: ExitStatus,
    action: String,
}
impl fmt::Display for ChildExit {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(output, "{} failed: {}", self.action, self.status)
    }
}
impl std::error::Error for ChildExit {}

pub(crate) fn require_success(status: ExitStatus, action: &str) -> Result<()> {
    if status.success() {
        return Ok(());
    }
    Err(ChildExit {
        status,
        action: action.to_owned(),
    }
    .into())
}

pub(crate) fn code(status: ExitStatus) -> u8 {
    match (status.code(), status.signal()) {
        (Some(code), _) => u8::try_from(code).unwrap_or(1),
        (_, Some(signal)) => 128u8.saturating_add(signal as u8),
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failing_status_survives_context_and_signal_mapping() {
        let error = require_success(ExitStatus::from_raw(37 << 8), "fixture")
            .map_err(|error| error.context("outer native operation"))
            .unwrap_err();
        assert_eq!(code(error.downcast_ref::<ChildExit>().unwrap().status), 37);
        assert_eq!(code(ExitStatus::from_raw(libc::SIGTERM)), 143);
        assert!(require_success(ExitStatus::from_raw(0), "fixture").is_ok());
    }
}
