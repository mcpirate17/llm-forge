//! Extension point for native hook bodies. Step 1 wires no handlers -- every event
//! still delegates whole to Python (`dispatch::run_hook`). Later PRs port one hook
//! body at a time (migration order: `research/rust_port_plan.md` section 4) and
//! register it here; `dispatch::run_hook` then skips Python for whatever event has
//! full native coverage and keeps delegating for the rest.

use anyhow::Result;
use serde_json::Value;

/// A hook body ported to native Rust. No hook has been ported yet (`registry()` is
/// empty), so the trait has zero implementors and rustc's dead_code lint would
/// otherwise flag its methods -- allowed here rather than deleted, since deleting
/// the shape is exactly what would make the next PR redesign it from scratch.
#[allow(dead_code)]
pub trait NativeHandler {
    /// Registry key, e.g. `"PreToolUse/crg_gate_verify_bash"` -- mirrors the name
    /// `HOOK_DISPATCH_TRACE=1` prints for the Python adapter it replaces.
    fn name(&self) -> &'static str;

    /// The Claude Code event this handler answers, e.g. `"PreToolUse"`.
    fn event(&self) -> &'static str;

    /// Run against the raw hook JSON payload, returning the JSON this hook body
    /// contributes to the merged output -- the same shape its Python adapter
    /// returns today.
    fn run(&self, payload: &Value) -> Result<Value>;
}

/// Handlers ported so far. Empty until a later PR ports the first hook body.
pub fn registry() -> Vec<Box<dyn NativeHandler>> {
    Vec::new()
}

/// True once every hook matched on `event` is covered by `registry()` and
/// `dispatch::run_hook` can skip Python entirely for it. Always false today --
/// asserted against `registry()` so a handler added without updating this stays a
/// build-time-visible bug, not a silent no-op.
pub fn fully_native(_event: &str) -> bool {
    debug_assert!(
        registry().is_empty(),
        "a NativeHandler was registered but handlers::fully_native() was not \
         updated to route to it -- see research/rust_port_plan.md migration order"
    );
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_events_are_natively_covered_yet() {
        assert!(!fully_native("PreToolUse"));
        assert!(!fully_native("PostToolUse"));
        assert!(registry().is_empty());
    }
}
