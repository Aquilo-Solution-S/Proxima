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

Postgres on `127.0.0.1:5434`, NATS with JetStream on `127.0.0.1:4224`
(`PROXIMA_DEV_NATS_PORT`; `4224` rather than `4222` so the fixture cannot
collide with a system NATS, monitoring on `8224`). The JetStream store
directory is a named volume — see the persistence note in
[15 §Fact outbox](../15-deployment.md#fact-outbox). No hosted endpoint is used.

## 3. Configure the host

```sh
export PROXIMA_PUBLICATION_SOURCE="urn:proxima:dev-host"   # required once a type is listenable
export PROXIMA_NATS_URL="nats://127.0.0.1:4224"            # unset ⇒ publisher off, capture still runs
export PROXIMA_NATS_PROFILE="local-file"                   # the only shipped profile
cargo run -p proxima-mcp --features nats
```

The stream `PROXIMA_FACTS` is created on first start if absent. If a stream of
that name already exists it is **verified, not adopted**: storage, retention,
`discard`, subjects, `max_age`, replicas, `max_message_size` and
`duplicate_window` must all match the profile, and boot fails naming every
mismatch. Omitting `PROXIMA_NATS_URL` is the rollback path: writes still
capture, records stay `pending`.

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
PROXIMA_TEST_NATS_URL=nats://127.0.0.1:4224 cargo nextest run -p proxima-outbox-nats
```

Unset `PROXIMA_TEST_NATS_URL` and the broker tests skip with a message rather
than passing vacuously.

## Signal → action

| Signal | Meaning | Action |
|---|---|---|
| `pending` count climbing | publisher off, broker unreachable, or stream full | check `PROXIMA_NATS_URL`, then `nats stream info` for `discard: New` rejections |
| `CapacityExhausted` on write (HTTP 503) | unpublished backlog hit `PROXIMA_OUTBOX_MAX_PENDING` | find out why delivery stopped and fix THAT; raising the cap hides real backpressure |
| stream at `max_bytes`, `BrokerCapacity` in the publisher log | the stream is full — and an ACK does **not** free space (below) | raise `PROXIMA_NATS_MAX_STREAM_BYTES`, or purge already-consumed sequences |
| `PayloadTooLarge` on write | export exceeds `PROXIMA_OUTBOX_MAX_PAYLOAD_BYTES` | shrink the payload, or raise the cap **and** `PROXIMA_NATS_MAX_MESSAGE_BYTES` — raising only the first is a boot error, by design |
| one record's `attempts` climbing while others publish | a single record the broker keeps refusing; the claim order demoted it so it no longer blocks the queue | read the `error`-level line naming its event id, then fix the record or the stream limit it violates |
| boot fails `SourceUnbound` | a listenable type is registered with no `PROXIMA_PUBLICATION_SOURCE` | set the producer identity URI |
| boot fails naming stream mismatches | an existing `PROXIMA_FACTS` disagrees with the profile | fix the stream, or point at a different name; do NOT ignore a `max_age` mismatch — it silently expires captured events |
| duplicate deliveries | expected: at-least-once | deduplicate on the CloudEvent `id` (`F:<uuid>`) |

### A full stream does not drain itself

With `retention: Limits` and `discard: New`, **acknowledgement frees no
space**: `Limits` evicts on age, size or count, and a consumer's ACK is none of
those. Nothing is lost when the stream fills — the PubAck is refused, the
record stays `pending` with its original bytes, and delivery resumes when space
exists. But space only exists when an operator makes it:

```sh
nats stream info PROXIMA_FACTS                 # bytes vs max_bytes
nats consumer info PROXIMA_FACTS proxima-reference   # ack_floor.stream_seq
nats stream purge PROXIMA_FACTS --seq <n>      # n <= EVERY consumer's ack_floor
```

`<n>` is safe only when every consumer bound to the stream has an `ack_floor`
at or beyond it. That judgement is yours: the substrate will not guess how many
consumers you run.

`WorkQueue` and `Interest` retention would free space on ACK, and are **not
offered**. Both delete a message once its bound consumers acknowledge it, so a
stream with zero bound consumers drops what it accepts — a deployment whose
consumer has not been created yet, or was deleted during an incident, would
lose committed events and get a PubAck for each one. A stream that refuses new
work is a failure you can see and undo; a stream that accepts work and discards
it is not.

### Reclaiming database storage

Delivered records stay in `publication_outbox` forever unless you say
otherwise:

```sh
export PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS=604800   # a week; minimum 60
```

It deletes only records that are already `published` and older than the
horizon, in bounded batches beside the drain loop. A `pending` or `claimed`
record is never touched, at any horizon, at any age.
