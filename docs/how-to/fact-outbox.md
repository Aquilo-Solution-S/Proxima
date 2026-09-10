# Publish Facts over the outbox

Recipe for turning on the Fact outbox and its NATS JetStream publisher.
Contract: [18-fact-outbox.md](../18-fact-outbox.md). Env reference:
[10-configuration.md](../10-configuration.md), [reference/env-vars.md](../reference/env-vars.md).

## 1. Declare a listenable Fact type

```rust
impl FactPayload for BuildFinished {
    const SCHEMA_ID: &'static str = "acme.build_finished";
    const SCHEMA_VERSION: u32 = 1;
    const LISTENABLE: bool = true;   // opt in; default false
    // json_schema() is REQUIRED once LISTENABLE is true — boot refuses otherwise
}
```

Write it through the ordinary authorized typed Fact write. There is no publish
call: capture happens inside the Fact's transaction (see
[09 §Typed Fact and Goal Writes](../09-developing-flavors.md#typed-fact-and-goal-writes)).

## 2. Run the fixture

```sh
docker compose -f docker-compose.dev.yml up postgres nats
```

Postgres on `127.0.0.1:5434`, NATS with JetStream on `127.0.0.1:4222`. The
JetStream store directory is a named volume — see the persistence note in
[15 §Fact outbox](../15-deployment.md#fact-outbox). No hosted endpoint is used.

## 3. Configure the host

```sh
export PROXIMA_PUBLICATION_SOURCE="urn:proxima:dev-host"   # required once a type is listenable
export PROXIMA_NATS_URL="nats://127.0.0.1:4222"            # unset ⇒ publisher off, capture still runs
export PROXIMA_NATS_PROFILE="local-file"                   # the only shipped profile
cargo run -p proxima-mcp --features nats
```

The stream `PROXIMA_FACTS` is created on first start if absent. Omitting
`PROXIMA_NATS_URL` is the rollback path: writes still capture, records stay
`pending`.

## 4. Run the reference consumer

```sh
cargo run -p proxima-outbox-nats --example durable_intake
```

It binds the durable pull consumer, implements `DurableIntake`, and ACKs only
after a durable accept or a durably retained rejection — the handoff described
in [18 §Two Acknowledgements](../18-fact-outbox.md#two-acknowledgements). It is
not an application orchestrator.

## 5. Verify

```sh
nats stream info PROXIMA_FACTS
nats sub 'proxima.fact.>'                      # structured CloudEvents JSON
psql "$DATABASE_URL" -c \
  "select state, count(*) from proxima_core.publication_outbox group by 1"
```

Without the `nats` CLI, run the broker-backed suite instead:

```sh
PROXIMA_TEST_NATS_URL=nats://127.0.0.1:4222 cargo nextest run -p proxima-outbox-nats
```

Unset `PROXIMA_TEST_NATS_URL` and the broker tests skip with a message rather
than passing vacuously.

## Signal → action

| Signal | Meaning | Action |
|---|---|---|
| `pending` count climbing | publisher off, broker unreachable, or stream full | check `PROXIMA_NATS_URL`, then `nats stream info` for `discard: New` rejections |
| `CapacityExhausted` on write | unpublished backlog hit `PROXIMA_OUTBOX_MAX_PENDING` | drain the backlog; raising the cap hides real backpressure |
| `PayloadTooLarge` on write | export exceeds `PROXIMA_OUTBOX_MAX_PAYLOAD_BYTES` | shrink the payload, or raise the cap **and** the stream's `max_message_size` |
| boot fails `SourceUnbound` | a listenable type is registered with no `PROXIMA_PUBLICATION_SOURCE` | set the producer identity URI |
| duplicate deliveries | expected: at-least-once | deduplicate on the CloudEvent `id` (`F:<uuid>`) |
