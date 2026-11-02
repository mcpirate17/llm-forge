# Native packaging and installed layout

The Python wheel-resource API (`conductor.package_resources`) and its dedicated
compatibility suite have been retired. Forge does not ship a Rust wheel-resource
loader as a replacement: the native executable embeds the small defaults it
needs at build time, including the memory catalog and embedding route table.
Project-context Rust primitives remain available independently; see
[native-project-context-migration.md](native-project-context-migration.md).

The native gate help contract is exercised from a clean host by
`native/forge/tests/native_candidate_gate.rs`; it does not consult an installed
Python site. The tooling boundary suite remains separately covered by
`native/conductor-native/tests/python_contracts_tooling_boundary.rs`.

Historical resource-wheel case tables and pass statements from previous
revisions described the retired Python loader and are not current tests or
validation claims. Embedded defaults must be updated in their source files and
remain referenced by the native `include_str!` sites; no runtime wheel
traversal or installed-resource verification API is provided.
