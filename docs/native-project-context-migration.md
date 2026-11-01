# Native project context

Project-context decisions and filesystem checks are implemented in the
`conductor-native` Rust crate. The public pure Rust entry points remain
`conductor_native::project_context::evaluate` and
`conductor_native::project_context::parse_config`; descriptor-based filesystem
coverage is in `native/conductor-native/tests/project_context_fs.rs`, with
decision and parser unit tests alongside their private implementation modules.

The former `conductor.project_context` Python facade and its dedicated
compatibility target have been retired. The old Python API, patchable helper
seams, and case-by-case migration table described in earlier revisions are no
longer current interfaces or coverage claims. The Rust primitives and their
native tests remain. No replacement Python wheel loader or wrapper was added.

The native decisions preserve bounded Git subprocess behavior, path
containment, configuration validation, and descriptor-read safety. This page
records the current ownership boundary; it does not claim current validation
results or exact parity with every retired Python assertion.
