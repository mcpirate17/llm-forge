# Embedding test coverage after broker retirement

The old Python route and HTTP adapter suites, and their PyO3 compatibility
targets, are retired. The current broker implementation and CLI live in
`native/forge/src/embedding_broker/`; the operator surface is documented in
[native-embedding-broker.md](native-embedding-broker.md).

Current native coverage is organized around routing configuration and legacy
fingerprints, quality receipts and pinned vector spaces, backend response
validation and normalization, paid-route admission and reservation, local
Ollama behavior, service request limits, lifecycle ownership, and verified TLS
transport. Unit tests live beside broker modules. TLS fixtures are under
`native/forge/tests/fixtures/embedding_broker/` and are included by
`transport_tls_tests.rs`. These tests exercise native code and private local
fixtures; this document makes no exact parity claim against the removed Python
targets and records no new test result.

The active default route file is `src/conductor/embedding_routes.toml`, read by
the native broker at build time. Keep its historical route fingerprints and
accepted configuration semantics stable when changing route identity fields.
See [native-embedding-broker.md](native-embedding-broker.md) for the current
contract and limits.

The CRG embedding-text compatibility target is outside this broker retirement
and remains a separate Python boundary; its status is tracked by the current
Cargo target and discovery inventory.
