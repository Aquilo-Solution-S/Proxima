# Deliver Facts over the outbox

Recipe for capturing declared Facts and delivering them, in process, to an
intake your host provides. No broker, cargo feature or environment variable is
involved. Contract: [18-fact-outbox.md](../18-fact-outbox.md). Env reference:
[10-configuration.md](../10-configuration.md), [reference/env-vars.md](../reference/env-vars.md).

## 1. Declare a listenable Fact type

```rust
impl FactPayload for BuildFinished {
    const SCHEMA_ID: &'static str = "acme/build-finished-v1";
    const SCHEMA_VERSION: u32 = 1;
    const LISTENABLE: bool = true;   // opt in; default false
    // json_schema() is REQUIRED once LISTENABLE is true — boot refuses otherwise
}
```

Write it through the ordinary authorized typed Fact write. There is no publish
call: capture happens inside the Fact's transaction (see
[09 §Typed Fact and Goal Writes](../09-developing-flavors.md#typed-fact-and-goal-writes)).

## 2. Implement the intake

```rust
use std::sync::Arc;
use proxima::{DurableIntake, Intake, IntakeError, ReceivedEvent};

#[derive(Debug)]
struct Inbox { /* your durable store */ }

#[async_trait::async_trait]
impl DurableIntake for Inbox {
    async fn accept(&self, event: &ReceivedEvent) -> Result<Intake, IntakeError> {
        // Insert-or-ignore on `event.id`: a redelivery is normal, and
        // `event.delivered_count > 1` says one happened. Commit before returning.
        self.record(&event.id, event.owner, &event.raw)
            .await
            .map_err(|error| IntakeError::new(error.to_string()))?;
        Ok(Intake::Accepted)
    }
}
```

`accept` must have made its outcome durable when it returns: the outbox marks
the record published right after. Return `Ok(Intake::Rejected { reason })` for
an event you recorded and refuse; return `Err(IntakeError)` only when you could
not record anything, so the record is tried again. Return within about five
seconds; an intake that needs longer accepts into its own queue and returns
([18 §Delivery](../18-fact-outbox.md#delivery)).

## 3. Configure the host

```rust
let app = Proxima::<HostBundle>::app()
    .publication(PublicationConfig::new(PublicationSource::new("urn:proxima:dev-host")?))
    .outbox_intake(Arc::new(inbox));   // unset ⇒ nothing drains, capture still runs
let running = app.run().await?;
```

`publication` names the producer identity URI (required once a type is
listenable; the env spelling is `PROXIMA_PUBLICATION_SOURCE`). `outbox_intake`
starts the drain; omitting it is the rollback path: writes still capture and
records stay `pending` until a boot that supplies an intake. The shipped
`proxima-mcp` binary supplies none. Setting it also makes goal writes emit
`core/goal-change-v1` events (§6), so the publication source above is required.

### Several consumers

`outbox_intake` is the consumer named `default`. To deliver every Fact to more
than one intake, name each one:

```rust
let app = Proxima::<HostBundle>::app()
    .publication(PublicationConfig::new(PublicationSource::new("urn:proxima:dev-host")?))
    .outbox_intake(Arc::new(inbox))                                   // `default`
    .outbox_consumer(ConsumerName::new("scheduler")?, Arc::new(scheduler));
```

Each consumer gets every record captured while it is registered, on a lane of
its own: one that fails or lags is retried on its own and holds up nobody else.
A record is `published`, and so prunable, once every consumer has it.

Boot registers each configured consumer; removing one from the builder does
**not** unregister it, so a rolling deploy cannot drop its backlog. Its
undelivered lanes keep counting toward `PROXIMA_OUTBOX_MAX_PENDING`, and boot
logs a warning naming it. When you mean to stop it for good, retire it:

```rust
match built.outbox_consumers().retire(&ConsumerName::new("scheduler")?).await? {
    RetireOutcome::Retired { undelivered } => tracing::info!(undelivered, "retired"),
    RetireOutcome::NotRegistered => {}
}
```

([18 §Consumers](../18-fact-outbox.md#consumers))

## 4. Forward to a broker (optional)

Proxima has no broker client. If your deployment needs NATS or Kafka, publish
from inside `accept`: send `event.raw` with `event.id` as the broker's dedup
key and return `Ok` only after the broker acknowledged. A broker that is down is
an `IntakeError`, and the Fact is delivered when it recovers. To stamp copies
and clean them after an erase, use `origin_scope_for_host()` and
`publication_origin_eligibility_for_host()`
([18 §Forwarding to a broker](../18-fact-outbox.md#forwarding-to-a-broker)).

## 5. Verify

```sh
psql "$DATABASE_URL" -c \
  "select consumer, state, count(*) from proxima_core.publication_delivery group by 1, 2"
```

Each consumer's lanes move `pending` → `published`; a record in
`publication_outbox` turns `published` once all its lanes have. For the same behaviour under test, run the
in-process suite against a Postgres:

```sh
PROXIMA_TEST_PG_URL=postgres://proxima:proxima@127.0.0.1:5434/proxima \
  cargo nextest run -p proxima -E 'test(in_process_outbox)'
```

Unset `PROXIMA_TEST_PG_URL` and those tests skip with a message rather than
passing vacuously.

## 6. Follow goal changes (a scheduler)

A host that runs queued work for Goals (a wake scheduler) learns of goal writes
from the same intake. With an outbox consumer configured, every goal write
delivers one `core/goal-change-v1` event: set, modify, each decomposed child,
pause, resume, abandon and achieve. Without one no change Fact is written.

```json
{ "series": "<uuid>", "goal": "G:<uuid>", "prior": "G:<uuid>", "revision": 2,
  "state": "Paused", "change": { "kind": "pause", "pending": "discard" } }
```

- A pause carries `pending`, required on the `core_goal` call. `keep`: hold the
  work queued for the goal and run it after a resume. `discard`: drop that work
  (ready or waiting to retry). Running work finishes either way.
- After `abandon` or `achieve` the goal is terminal: drop what you queued.
- Deliveries can be reordered (a retried record arrives after later ones), so
  key by `event.position`'s `subject` (the `series`) and apply an event only if
  its `sequence` (the `revision`) is greater than the last one you applied for
  that subject. `goal` versions are identifiers, not an order
  ([18 §Ordering](../18-fact-outbox.md#ordering)).
- Proxima already admits no new run for a goal that is not Active; `pending`
  covers the work you had queued before the pause.

Contract and payload: [18 §Goal changes on the feed](../18-fact-outbox.md#goal-changes-on-the-feed).

## 7. Record who set a goal

Proxima keeps no per-write author. The host's ledger records it, from the same intake:
bind the actor in the `Authenticator`, then read it back in `accept`.

```rust
use proxima::{AuthError, Authenticator, AuthzContext, Credentials, PublicationExtensions};

#[async_trait::async_trait]
impl Authenticator for HostAuth {
    async fn authenticate(&self, creds: &Credentials) -> Result<AuthzContext, AuthError> {
        let (ctx, subject) = self.verify(creds).await?;   // your verifier: the context and the caller's id (String)
        let bound = PublicationExtensions::new()
            .with("actor", subject)                       // name: [a-z0-9], at most 20 characters
            .map_err(|_| AuthError::InvalidCredentials)?; // a value the feed cannot carry: log it, refuse the caller
        ctx.with_publication_extensions(bound)
            .map_err(|_| AuthError::InvalidCredentials)
    }
}
```

In `accept`, record the goal changes beside whatever else the intake keeps:

```rust
let envelope = &event.envelope;
if envelope.event_type == "core/goal-change-v1" {
    let actor = envelope.extensions.get("actor").and_then(|value| value.as_str());
    let data = &envelope.data;
    // Insert-or-ignore on `event.id`; commit before returning.
    self.record(&event.id, event.owner, actor,
                &data["series"], &data["goal"], &data["revision"], &data["change"]["kind"])
        .await
        .map_err(|error| IntakeError::new(error.to_string()))?;
}
```

- `actor` is `None` for a write with no bound actor: a `for_system` context
  the host bound nothing to, and a redeemed delegated phase, which rebuilds
  without host-bound attributes. Attribute those from your own records.
- The attribute rides on every Fact that context captures, not only goal
  changes. The row holds personal data, so your erase must reach it.
- Replay and reordering: a replayed write adds no row, so the first writer keeps
  the write; order rows by `sequence` per `subject`, not by arrival.

Contract: [18 §Who wrote what](../18-fact-outbox.md#who-wrote-what).

## Signal → action

| Signal | Meaning | Action |
|---|---|---|
| `pending` count climbing | no intake configured, an intake that keeps failing, or a consumer that is registered but no longer configured | check the boot report (`Feature::OutboxPublisher`, which names the consumers) and the boot warning for unconfigured ones, then the intake's own errors; the drain logs a warn-level line per failed pass, with `consumer=`. Retire a consumer you no longer run |
| `CapacityExhausted` on write (HTTP 503) | unpublished backlog hit `PROXIMA_OUTBOX_MAX_PENDING` | find out why delivery stopped and fix THAT; raising the cap hides real backpressure |
| `PayloadTooLarge` on write | export exceeds `PROXIMA_OUTBOX_MAX_PAYLOAD_BYTES` | shrink the payload or raise the Proxima capture cap |
| one record's `attempts` climbing while others deliver | a single record the intake keeps refusing, or captured bytes that are not an envelope; the claim order demoted it so it no longer blocks the queue | for the latter, read the `error`-level line naming its event id; for the former, your intake's own log |
| boot fails `SourceUnbound` | a listenable type is registered with no `PROXIMA_PUBLICATION_SOURCE` | set the producer identity URI |
| boot fails naming `core/goal-change-v1` | an outbox consumer is configured, which makes goal changes listenable, with no `PROXIMA_PUBLICATION_SOURCE` | set the producer identity URI |
| duplicate deliveries | expected: at-least-once | deduplicate on the CloudEvent `id` (`F:<uuid>`) |

### Reclaiming database storage

Delivered records stay in `publication_outbox` forever unless you say
otherwise:

```sh
export PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS=604800   # a week; minimum 60
```

It deletes only records that are already `published` (delivered to every
consumer) and older than the horizon, in bounded batches beside the drain loop.
A record some consumer is still owed is never touched, at any horizon, at any
age.
