# Local agent messaging

`agent_a2a` is a durable local mailbox transport. It coordinates agents; receiving
a message never authorizes an action or supplies a governance approval. Endpoints
bind to loopback, registry tokens protect recipient writes, and sender names remain
cooperative identity metadata within the local fleet trust domain.

The registry lives at `<host>/.agents/a2a/agents.json`; each registered identity has
`<identity>/store.sqlite`. `--state-dir` selects another existing registry. Keep
registry tokens private. Provision identities explicitly with `init --name NAME
--port PORT`; serving one identity does not launch an AI agent.

## Inspect and acknowledge with Rust

`forge mailbox` opens the same SQLite stores directly. It needs no Python
interpreter and performs no network operations:

```sh
forge mailbox --host /path/to/project inbox --as-name recipient --compact --json
forge mailbox --host /path/to/project show --as-name recipient MESSAGE_ID --json
forge mailbox --host /path/to/project read --as-name recipient MESSAGE_ID
forge mailbox --host /path/to/project history --as-name sender --limit 20
```

`inbox` uses the existing `bounded-a2a-inbox` JSON schema and bounds the number of
messages, preview characters and total rendered characters. It does not mark
messages read. `show` retrieves one complete message explicitly, with a default
1 MiB limit (`--max-bytes`, maximum 16 MiB); larger records are refused rather
than silently truncated. Use `--direction outbound` to inspect an outbound fact.
`read` atomically acknowledges an unread inbound message; repeated acknowledgment
returns the original durable acknowledgment and timestamp. A self-send's outbound
record is preserved.

`--state-dir` selects an existing alternative state directory. Inspection never
creates a database or migrates its schema. Missing mailbox stores and corrupt
stores produce errors for inbox, show and read. These commands can inspect a
retained store after its registry entry is removed. `history` requires a registered
identity and reports unavailable history when its store or event table is absent.
Lifecycle and presentation state remain owned by the transport.

## Queue an outbound message with Rust

```sh
forge mailbox --host /path/to/project enqueue --from-name sender --to recipient --body-file message.txt
forge mailbox --host /path/to/project enqueue --from-name sender --to recipient --body 'Review is ready' --data-file coordination.json
```

`enqueue` records a durable outbound message and returns its message ID with a
`queued` receipt. It performs no network operation. Both identities must already
be registered, and the sender must have an initialized store with the current
message, lifecycle and delivery-event tables. The command does not initialize or
migrate a store.

Bodies are limited to 256 KiB of UTF-8; structured data is limited to 1 MiB.
The receipt includes a bounded summary and digests, without full body or data
fields. Inputs that exceed the limits fail before a message is written.

The command uses the transport's sender lock and atomically writes the message,
lifecycle metadata and delivery event. The existing dispatcher can then deliver
queued messages in order. Structured `coordination-v2` payloads are validated
before writing. A queued receipt is evidence of local storage; delivery requires
a later successful transport receipt.

## Deliver through the transport

```sh
python -m conductor.agent_a2a send --from-name sender --to recipient --body-file message.txt
python -m conductor.agent_a2a flush --as-name sender --max-messages 100
```

Structured `coordination-v2` sends can queue while the peer is offline. A reachable
peer that does not advertise the protocol is refused before a new send is recorded.
Before queued delivery, the recipient's current Agent Card must advertise the
protocol; incompatible peers and malformed replies produce terminal failure events.
No protocol downgrade happens. Transport failures stay queued; `--no-queue` makes an
unavailable send fail instead.

HTTP 408, 429 and 5xx responses stay queued even when their bodies are not JSON.
Retry timing is durable in an auxiliary `outbound_retries` table, created on the
first retry. Backoff starts at 250 ms, doubles to 60 seconds with deterministic
per-message jitter (up to 25%), and honors `Retry-After` delta seconds or HTTP dates,
bounded to one hour. An explicit flush does not override a future due time. A
delayed head blocks later messages to that recipient, while other recipients can
progress. There is no terminal retry-count threshold: transient failure retains
the durable request for recovery. Authentication, invalid protocol, invalid payload
and acknowledgment mismatches remain terminal failures.

Each sender has one delivery lock. Older messages to a recipient are retried first;
new sends cannot overtake an undelivered backlog. A process interrupted between
recording and delivery leaves `pending` evidence that the next flush retries with
the same message ID. Receiver deduplication makes a lost acknowledgement safe to
retry. Delivery is at least once over the wire and stored once by message ID.
Identical inbound retries succeed; reusing an ID with a different sender, recipient,
body or data fails without overwriting the original fact.

Both native `send`/`enqueue` and Python `send` accept `--idempotency-key KEY` (1–256
bytes). The key is scoped to sender and recipient. An identical logical send reuses
its existing ID and receipt; different content under that key fails. Keys should
identify one operation, not a recurring thread. A new operation needs a new key.

Within one flush, up to four recipients deliver concurrently. Each recipient has
one in-flight message, preserving its order. Clients reuse connections, and each
peer's Agent Card is fetched once per command and reused for that command's batch;
the next command rereads the registry and capabilities. Sender locks still prevent
concurrent flushes from overtaking one another.

`flush` attempts at most `--max-messages` messages (default 100, maximum 1000), skips
the rest of an unreachable recipient's backlog for that pass, and returns exit 3
while matching pending/queued messages remain. Exit 2 signals an invalid invocation
or execution failure. A delivered or rejected attempt updates the outbound fact and
appends its delivery event in one SQLite transaction. `history` reads those events
without exposing bodies, marking inbox messages read, or creating/migrating stores.

## Bounded retry supervisor

```sh
python -m conductor.agent_a2a supervise --as-name sender \
  --duration 300 --interval 5 --max-messages 100
```

This explicit foreground command retries for a finite wall-clock budget. Duration
is 1–3600 seconds; each flush child has a timeout of at most 15 seconds and at most
the remaining budget. Queue contents survive timeouts and restarts. It does not run
automatically when hooks are installed or a session starts.

Idle supervisors reconcile at most once per 60 seconds and wake immediately on
mailbox database/WAL changes on Linux. Lifecycle state is written on a changed
flush result and at most one idle heartbeat per minute, plus startup and final
state. Active retry queues continue using `--interval` and durable due times.

## Notify consumers without idle scans

```sh
python -m conductor.agent_a2a watch --as-name recipient --json --interval 120
python -m conductor.agent_a2a watch --as-name recipient --once --json --replay-unread
```

`watch` uses Linux inotify on the mailbox directory, including WAL file replacement,
and polls at the requested interval as recovery for missed notifications. Platforms
without inotify and identities without a database use bounded polling. Inotify
resource failures are reported before falling back. No additional daemon starts.

By default watch emits each unpresented message once, then marks it presented;
presentation is distinct from read acknowledgment. `--replay-unread` recovers
previously presented messages that were never read. Consumers must explicitly
acknowledge durable processing with `read`, and can use replay after a crash.
Explicitly superseded informational messages with `requires_response=false` are
coalesced out of the unpresented watch stream. Actionable requests remain visible;
full inbox inspection and historical message facts are preserved.

## Supervisor endpoint ownership

Add `--serve` to maintain this identity's endpoint for the supervisor lifetime.
An already healthy endpoint is reused and never stopped. An endpoint the supervisor
starts is stopped when the budget ends, a failure occurs, or SIGINT/SIGTERM arrives;
unexpected exits have a bounded restart budget (`--max-restarts`, default 3).
An invalid process occupying the port is reported, never displaced. Only one
supervisor can hold an identity at a time. Forced termination such as SIGKILL cannot
run cleanup; the next invocation inspects the endpoint instead of assuming ownership.

`<identity>/supervisor.json` records identity, PID, start/end/update timestamps,
duration, cycle count, endpoint PID, status, and the last flush summary. A
`completed` supervisor means its requested time elapsed; it does **not** mean every
peer recovered or every message was delivered. Inspect pending/queued counts and
delivery history separately. Terminal rejections remain failed and are not retried.

The `watch` CLI emits bounded inbox summaries for presentation; it does not deliver
outbound queues. Session startup performs one sender-scoped flush and bounded preview.
Fleet last-heard consumes the same compact JSON schema, never parses display text.
