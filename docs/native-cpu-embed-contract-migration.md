# Native embedding broker

The Python broker modules `conductor.cpu_embed`, `conductor.embedding_contract`,
and `conductor.http_transport` have been retired. The active provider-neutral
broker is implemented by the native `forge` executable; current behavior and
operator commands are documented in [native-embedding-broker.md](native-embedding-broker.md).

The former Python contract target and its exclusive Rust helpers were removed
after native broker coverage was added. Historical case inventories and pass
claims in earlier revisions of this document described those retired wrappers
and are not current coverage claims. The current broker has Rust unit and
integration fixtures under `native/forge/src/embedding_broker/` and
`native/forge/tests/fixtures/embedding_broker/`; this migration note does not
claim case-for-case parity or report a validation result.

`src/conductor/embedding_routes.toml` remains the default route configuration
embedded into the native binary. See the broker guide for route identity,
quality policy, paid-route limits, service bounds, and startup behavior.
