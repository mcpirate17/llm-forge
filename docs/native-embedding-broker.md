# Native embedding broker

Forge provides a provider-neutral embedding service in the native `forge`
binary. It replaces the retired Python `conductor.cpu_embed`,
`conductor.embedding_contract`, and `conductor.http_transport` modules.
`src/conductor/embedding_routes.toml` remains the default route document and is
embedded in the binary.

## Commands

```text
forge embedding-broker serve [--host HOST] [--port PORT] [--routes PATH]
    [--quality PATH] [--timeout-seconds SECONDS] [--lifetime-seconds SECONDS]
    [--no-ollama-start]
forge embedding-broker ensure [same options] [--state-dir PATH]
forge embedding-broker status [same options]
forge embedding-broker warm [same options] [--state-dir PATH]
```

The service binds to loopback `127.0.0.1:7317` by default. `serve` runs until
SIGINT/SIGTERM or an optional `--lifetime-seconds` expires; its default lifetime
is persistent. `ensure` starts this executable only when no listener is
present and refuses to replace an occupied or incompatible service. `status`
only reads health. `warm` explicitly ensures the service and makes one
embedding request. The `--state-dir` used by lifecycle commands is created as
needed and must resolve to a user-owned private directory (mode 0700 or
stricter); the default runtime directory is private and scoped to the current
user.

No command downloads a model. Startup is enabled for the configured default
Ollama endpoint at `127.0.0.1:11434`; `--no-ollama-start` disables it. Other
configured backend endpoints must already be running. Route and quality files are optional overrides; defaults
come from the embedded route table and the normal quality policy.

## Route identity and selection

Route parsing is strict, bounded, and preserves the established public route
fingerprints for accepted configurations. Configuration that changes the
fingerprinted vector-space identity produces a different fingerprint. Quality
receipts are checked against that exact fingerprint. Quality failure may select
an explicitly configured eligible fallback; a caller-supplied workspace pin
requires the exact fingerprint and never falls back to another vector space.
Routes may also require explicit environment enablement and credentials. Paid
routes require HTTPS, verified TLS, and an API key from the named environment
variable. Endpoints reject URL credentials, query strings, and fragments; the
transport disables proxies and redirects. Error messages do not include
credentials or server response bodies.

For each request, paid-route spending uses estimated cost. Every paid attempt
must fit that route's configured cap and the cumulative reservation ceiling,
which is the largest eligible paid-route cap for that request. A reservation
is retained after a failed attempt. Response metadata reports estimates and
reservations; it is not provider billing data or an actual charge receipt.

## Request and resource bounds

The broker exposes `/health`, `/v1/models`, and `/v1/embeddings`. Embedding
requests accept one text or up to 128 texts. The request body limit is 1 MiB and
the response limit is 32 MiB. At most four backend jobs run concurrently and
the listener admits at most 16 connections. Initial headers have an absolute
five-second deadline; request-body reads also have a five-second deadline.
The overall backend timeout defaults to 300 seconds and is configurable from
1 through 3600 seconds. These limits apply to the native service and its bounded
transport.

## Implementation and tests

Native routing, quality, backend dispatch, transport, lifecycle, and HTTP service
are under `native/forge/src/embedding_broker/`. Native unit tests and private
TLS fixtures exercise those components. The retired PyO3 compatibility targets
and their exclusive Python helpers were removed; this guide does not assert
case-for-case parity or report validation results. See
[`native-cpu-embed-contract-migration.md`](native-cpu-embed-contract-migration.md)
and [`native-embedding-tests-migration.md`](native-embedding-tests-migration.md)
for the current coverage boundary.
