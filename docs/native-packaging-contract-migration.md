# Native packaging and installed layout

The Python wheel-resource API (`conductor.package_resources`) and its dedicated
compatibility suite have been retired. Forge does not ship a Rust wheel-resource
loader as a replacement: the native executable embeds the small defaults it
needs at build time, including the memory catalog and embedding route table.
Project-context Rust primitives remain available independently; see
[native-project-context-migration.md](native-project-context-migration.md).

The installed-layout contract still checks that an installed Forge command
resolves host configuration rather than leaking its site-install path. Its
current Rust-owned assertion is in
`native/conductor-native/tests/python_contracts_installed_layout.rs` and
exercises `conductor.gate --help` against a private fake host. The tooling
boundary suite remains separately covered by
`native/conductor-native/tests/python_contracts_tooling_boundary.rs`.

Historical resource-wheel case tables and pass statements from previous
revisions described the retired Python loader and are not current tests or
validation claims. Embedded defaults must be updated in their source files and
remain referenced by the native `include_str!` sites; no runtime wheel
traversal or installed-resource verification API is provided.
