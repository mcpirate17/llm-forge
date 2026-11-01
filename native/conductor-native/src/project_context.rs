//! Read-only project context decisions and strict configuration parsing.

#[path = "project_context_config.rs"]
mod config;
#[path = "project_context_decision.rs"]
mod decision;
#[path = "project_context_fs.rs"]
mod fs;

pub use config::parse_config;

pub fn evaluate(operation: &str, payload: &serde_json::Value) -> serde_json::Value {
    if operation == "read-config-bytes" {
        return fs::evaluate(payload);
    }
    decision::evaluate(operation, payload)
}
