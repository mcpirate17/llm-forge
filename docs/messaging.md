# Local agent messaging

`forge mailbox` is a durable local mailbox transport. It coordinates agents; receiving
a message never authorizes an action or supplies a governance approval. Endpoints
bind to loopback, registry tokens protect recipient writes, and sender names remain
cooperative identity metadata within the local fleet trust domain.

The registry lives at `<host>/.agents/a2a/agents.json`; each registered identity has
`<identity>/store.sqlite`. `--state-dir` selects another existing registry. Keep
registry tokens private. Provision identities explicitly with `init --name NAME
--port PORT`; serving one identity does not launch an AI agent.

## Inspect and acknowledge with Rust

The inspection commands open the existing SQLite stores directly. They need no Python
interpreter and perform no network operations:

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

## Resolve messages and retain evidence

```sh
forge mailbox read --as-name recipient MESSAGE_ID
forge mailbox resolve --as-name recipient MESSAGE_ID
forge mailbox hold --as-name recipient MESSAGE_ID --reason 'Awaiting review'
forge mailbox hold --as-name recipient MESSAGE_ID --clear
```

`resolve` requires an existing read acknowledgment and atomically records the
resolved state. Repeating it returns the original resolution timestamp. Its
receipt includes the message ID, sender and timestamp; normal lifecycle rows
do not load or print message bodies. A legacy row without lifecycle metadata is
reconstructed with pinned retention and content hashes, subject to a 1 MiB
materialization limit. Larger legacy rows fail without changes.

`hold` normalizes a 1–240 character reason or clears it explicitly. It requires
existing lifecycle metadata; legacy messages remain pinned until migrated.
Both commands affect only the inbound fact, including when a self-send also has
an outbound row with the same ID. They preserve message content and do not
create missing stores. Retention eligibility still requires the separate,
explicit retention policy and operation.

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
forge mailbox send --from-name sender --to recipient --body-file message.txt
forge mailbox flush --as-name sender --max-messages 100
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

Native `send`/`enqueue` accept `--idempotency-key KEY` (1–256
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
forge mailbox supervise --as-name sender \
  --duration 300 --interval 5 --max-messages 100
```

This native foreground command retries for a finite wall-clock budget. Duration
is 1–3600 seconds; each flush child has a timeout of at most 15 seconds and at most
the remaining budget. Queue contents survive timeouts and restarts. It does not run
automatically when hooks are installed or a session starts.

Idle supervisors reconcile once per 60 seconds and wake on
mailbox database/WAL changes on Linux. Lifecycle state is written on a changed
flush result and at most one idle heartbeat per minute, plus startup and final
state. Active retry queues continue using `--interval` and durable due times.
Idle cycles launch no flush process. Pending cycles use the same native executable
with bounded stdout/stderr and a process group owned by the supervisor. A persistent
read-only SQLite connection avoids self-generated open/close wakeups. The supervisor
drains its own writes before checking pending work; later enqueues wake the directory
watch. A lost event is reconciled by the finite timer. Inotify setup or directory
invalidation errors fail visibly. This supervisor requires Linux.

Supervised flushes explicitly use `--defer-busy`: a sender lock held by another
enqueue or flush leaves its queue untouched and returns exit 3 for a later retry.
Ordinary `flush` retains its error on concurrent delivery unless this flag is set.
SIGINT/SIGTERM cancels readiness waits and flushes through signalfd; pidfds report
owned receiver exits without PID-reuse races. The bounded Agent Card preflight
can take up to one second. A termination returns exit 130 with durable
`interrupted` state. A timed-out delivery might already have reached its peer;
the retained message ID makes the later retry deduplicable.

## Notify consumers without idle scans

```sh
forge mailbox watch --as-name recipient --json --interval 120 --duration 300
forge mailbox watch --as-name recipient --once --json --replay-unread
```

Native `watch` uses Linux inotify on the mailbox directory, including WAL file
replacement, and a finite reconciliation timer for missed notifications. It runs
in the foreground for 1–3600 seconds (default 300), or emits at most one batch with
`--once`. It requires an existing initialized store. Inotify failures, invalidated
directories and unsafe lease paths are errors. One private identity lease prevents
concurrent presentation consumers. No receiver, daemon or model starts.

By default watch drains bounded unpresented batches, then marks emitted IDs presented;
presentation is distinct from read acknowledgment. `--replay-unread` recovers
previously presented messages that were never read. Consumers must explicitly
acknowledge durable processing with `read`, and can use replay after a crash.
`--replay-unread` re-presents a bounded unread batch on reconciliation; use `--once`
for one recovery batch. Presentation writes do not trigger a tight replay loop.
Explicitly superseded informational messages with `requires_response=false` are
coalesced out of the unpresented watch stream. Actionable requests remain visible;
full inbox inspection and historical message facts are preserved.

Each JSON batch is bounded by `--max-messages` (1–8), `--preview-chars` (32–320)
and `--max-chars` (256–10000); the newline is separate. A batch that cannot fit
fails without consuming messages. Output is nonblocking with a three-second
backpressure deadline, capped by the remaining lifetime; SIGINT/SIGTERM interrupts
the stream with exit 130. Broken or stalled output leaves the batch unpresented.
Emission and SQLite acknowledgment cannot be one transaction: a crash after output
but before acknowledgment can repeat a batch. Consumers can deduplicate by message ID.

Watch emits mailbox previews. Request source context explicitly with native tools,
for example `forge graph --host /path/to/project refs --text 'src/example.rs::worker'
--max-bytes 600`, or inspect the named source through `forge code inspect`.
The Python watch and its automatic graph-context attachment have been retired;
source inspection remains an explicit native operation.

## Supervisor endpoint ownership

Add `--serve` to maintain this identity's endpoint for the supervisor lifetime.
An already healthy endpoint is reused and never stopped. An endpoint the supervisor
starts is stopped when the budget ends, a failure occurs, or SIGINT/SIGTERM arrives;
unexpected exits have a bounded restart budget (`--max-restarts`, default 3).
An invalid process occupying the port is reported, never displaced. Only one
supervisor can hold an identity at a time. Permanent private lock files coordinate
startup and supervision; they are never unlinked while another caller could use them.
Forced termination such as SIGKILL cannot run cleanup. An owned receiver also has a
finite lifetime rounded up from the remaining supervisor budget; the next invocation
inspects any live endpoint instead of assuming ownership.

`<identity>/supervisor.json` records identity, PID, start/end/update timestamps,
duration, cycle count, endpoint PID/start count, status, and the last flush summary. A
`completed` supervisor means its requested time elapsed; it does **not** mean every
peer recovered or every message was delivered. Inspect pending/queued counts and
delivery history separately. Terminal rejections remain failed and are not retried.

The `watch` CLI emits bounded inbox summaries for presentation; it does not deliver
outbound queues. Session startup performs one sender-scoped flush and bounded preview.
Fleet last-heard consumes the same compact JSON schema, never parses display text.
