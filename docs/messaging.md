# Local agent messaging

`agent_a2a` is a durable local mailbox transport. It coordinates agents; receiving
a message never authorizes an action or supplies a governance approval. Endpoints
bind to loopback, registry tokens protect recipient writes, and sender names remain
cooperative identity metadata within the local fleet trust domain.

The registry lives at `<host>/.agents/a2a/agents.json`; each registered identity has
`<identity>/store.sqlite`. `--state-dir` selects another existing registry. Keep
registry tokens private. Provision identities explicitly with `init --name NAME
--port PORT`; serving one identity does not launch an AI agent.

## Deliver and inspect

```sh
python -m conductor.agent_a2a send --from-name sender --to recipient --body-file message.txt
python -m conductor.agent_a2a flush --as-name sender --max-messages 100
python -m conductor.agent_a2a history --as-name sender --limit 20
python -m conductor.agent_a2a inbox --as-name recipient --compact --json
```

Structured `coordination-v2` sends can queue while the peer is offline. A reachable
peer that does not advertise the protocol is refused before a new send is recorded.
Before queued delivery, the recipient's current Agent Card must advertise the
protocol; incompatible peers and malformed replies produce terminal failure events.
No protocol downgrade happens. Transport failures stay queued; `--no-queue` makes an
unavailable send fail instead.

Each sender has one delivery lock. Older messages to a recipient are retried first;
new sends cannot overtake an undelivered backlog. A process interrupted between
recording and delivery leaves `pending` evidence that the next flush retries with
the same message ID. Receiver deduplication makes a lost acknowledgement safe to
retry. Delivery is at least once over the wire and stored once by message ID.

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

The `watch` CLI polls bounded inbox summaries for presentation; it does not deliver
outbound queues. Session startup performs one sender-scoped flush and bounded preview.
Fleet last-heard consumes the same compact JSON schema, never parses display text.
