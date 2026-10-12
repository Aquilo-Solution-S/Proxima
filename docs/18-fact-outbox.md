# 18 — Fact Outbox

> **Status:** current.

Host-side at-least-once delivery of **declared listenable Facts**, in process.
18 owns the listen declaration, the capture record, the delivery state machine,
and the drain that hands captured events to a host-provided intake. It owns no
broker: a host that wants one forwards to it from its own intake (§Forwarding
to a broker). It owns no verb (see [14](14-protocol-surface.md#the-verbs)), no
authorization of its own (see [01](01-event-source.md)), and no orchestration.

## Purpose

A host reacts to Fact creation without every flavor carrying a message client
and its delivery state. A committed Fact survives a crash and an intake outage:
the event stays available for later delivery with its original identity and
bytes.

```
flavor write (typed, authorized)
        │
        ▼
┌─────────────────────── ONE Postgres transaction ───────────────────────┐
│  memory row (Fact)  +  registered sidecars  +  publication_outbox row  │
└────────────────────────────────────────────────────────────────────────┘
        │  commit — any capture failure rolls the whole write back
        ▼
   publication_outbox row, state `pending`
        │  runtime drain: claim(lease) → accept(the captured bytes, unchanged)
        ▼
   the host's DurableIntake, in this process
        │  Ok → mark_published    (Err or timeout → release → `pending`)
        ▼
   state `published`
        accept returns only after the intake made its outcome durable
```

## Flavor Author Contract

| Step | Contract |
|---|---|
| declare | `FactPayload::LISTENABLE = true` on the registered type (see [03 §`FactPayload`](03-schema-registry.md#factpayload), [09 §Payload Traits](09-developing-flavors.md#payload-traits)) |
| write | the ordinary authorized typed Fact write (see [09 §Typed Fact and Goal Writes](09-developing-flavors.md#typed-fact-and-goal-writes)). **No explicit publish call**, no second enablement list, no hand-maintained duplicate schema |
| export | the registered payload's own serde is the wire payload; the registered `json_schema()` is the advertised contract |
| default | unlisted types stay internal. The Code flavor declares none, so an existing deployment sees no change |

**Freeze rule — `listenable ⇒ json_schema`.** A type with `LISTENABLE = true`
and no registered JSON schema is a freeze refusal at boot, not a runtime
surprise: a consumer cannot bind to a contract that is not published. Same
placement as the other freeze checks (see [09 §Registry](09-developing-flavors.md#registry)).

## Capture

Capture runs inside the Fact write chokepoint, in the Fact's own transaction,
beside the registered sidecar writes.

| Case | Behaviour |
|---|---|
| listenable Fact | build envelope → validate → capacity check → insert `publication_outbox` row in the SAME tx |
| non-listenable Fact | untouched path; no row |
| replay of an existing admission | no Fact write ⇒ **no second record**; the identity is the Fact's `t`, and replay reuses it |
| a genuinely new Fact | its own `t`, its own record |
| any capture failure | the whole write rolls back — the Fact does not commit |

Capture is **mandatory** while a listenable type is registered. There is no
"publish best-effort" mode: a write path that cannot capture is refused.

| Error | Raised when | MCP tool class | JSON-RPC | REST |
|---|---|---|---|---|
| `PayloadTooLarge` | serialized export exceeds `PROXIMA_OUTBOX_MAX_PAYLOAD_BYTES` | `InvalidInput` | `-32602` | 400 `invalid-argument` |
| `CapacityExhausted` | unpublished record count is at `PROXIMA_OUTBOX_MAX_PENDING` — explicit backpressure, never silent eviction | `CapacityExhausted` | `-32000`, `data.code = "capacity_exhausted"` | **503** `capacity-exhausted` |
| `UntypedListenableWrite` | a listenable series reached the chokepoint without its registered typed export | `InvalidInput` | `-32602` | 400 `invalid-argument` |
| `SourceUnbound` | a listenable schema reached the chokepoint with no configured publication source | `Internal` | `-32603` | 500 `internal` |
| `ExportFailed` | the envelope would not serialize, or `t` carries no UUIDv7 timestamp | `InvalidInput` | `-32602` | 400 `invalid-argument` |

The REST slugs are those of a write through `Engine` or `UnitOfWork`, which
every shipped tool uses (`ProtocolError`). A tool that returns the raw
`StorageError::PublicationRefused` gets the same statuses with
`invalid-input` in place of `invalid-argument`.

`CapacityExhausted` is its own class on every surface, and deliberately not the
class of a permanent precondition failure. It says *retry later*: the write was
refused because a **transient** backlog is at its ceiling, and a caller that
sees 503 knows to back off where a 400 would have told it to change its
request and a 500 would have told it to open an incident. See
[17 §Errors](17-rest-surface.md#errors).

**The pending ceiling is a SOFT bound.** The capacity check counts unpublished
records inside the write's own transaction under `READ COMMITTED`, so N
concurrent writers can each observe `pending_count < max_pending` and each
commit: the backlog can overshoot the ceiling by at most the number of
concurrent listenable writers. That is deliberate. Making it hard would need a
table lock on the hot path of every listenable write, which costs more than the
overshoot is worth — the ceiling exists to bound unbounded growth, not to be a
quota. The count is also bounded work: it stops at `max_pending` rows rather
than counting the whole backlog.

## Publication Contract

CloudEvents 1.0.2, **structured JSON**, `application/cloudevents+json`. Stored
verbatim as bytes; the drain hands those bytes over unchanged — it never reloads
today's Fact state, regenerates a payload with an upgraded flavor, or
substitutes today's installation identity.

| Attribute | Value | Captured from |
|---|---|---|
| `specversion` | `"1.0"` | const |
| `id` | the Fact's `t` in canonical wire form, `F:<uuid>` — not the series handle, receipt, or content hash | write session |
| `source` | `PROXIMA_PUBLICATION_SOURCE`, the configured producer identity URI | engine config; never caller-supplied |
| `type` | the registered schema id, **verbatim** — the version travels in `dataschema` only, so a consumer subscription is not version-pinned | typed registration |
| `dataschema` | `proxima://schema/{schema_id}/{schema_version}` — resolved against the build-time catalog and served by the MCP resource; **no HTTP fetch** at publish or consume time | typed registration |
| `datacontenttype` | `application/json` | const |
| `time` | RFC 3339 (ms precision), decoded from the UUIDv7 timestamp inside `t`. `memory` carries no `recorded_at` column, so the recording time is read out of the id rather than stamped a second time — stable across republication | the id itself |
| `data` | the typed export (serde of the registered payload) | payload |
| `proximaowner` (ext) | `OwnerRef` wire form of the Fact's owner | authorized write context |
| `proximamodel` (ext) | model/runner identity **certified by the authenticating token**, when the deployment binds one; never the caller-supplied `model_id` label (see [15 §Trusted model provenance](15-deployment.md#trusted-model-provenance)). Omitted when absent | authenticated edge |
| host-bound extensions (ext) | any extension attribute the HOST bound to the authorization context — the orchestration context a run happens under (`workflowid`, `runid`, `stepid`, …). Name-ordered, after `proximamodel` and before `data`; nothing at all when unbound (§Host-bound extension attributes) | host authorization context |

Producer authority is the configured credential and the authorized owner
binding, never a self-asserted header.

### Host-bound extension attributes

A host running Flavors under an orchestration context binds that context ONCE,
on the authorization context; every Fact that context goes on to capture
carries it. Flavor code assembles nothing and can neither set nor read it.

| Property | Value |
|---|---|
| Carrier | `AuthzContext::with_publication_extensions(PublicationExtensions)`, on the same footing as `with_trusted_model_id` (see [15 §Trusted model provenance](15-deployment.md#trusted-model-provenance)) — host-only, never a payload, tool argument, request header, or MCP `clientInfo` |
| Value types | `String` / `Integer` (int32) / `Boolean` — the `CloudEvents` 1.0.2 JSON-format types with no canonicalization question. `Timestamp`/`URI`/`Binary` are absent: each has a string rendering the host produces itself |
| Name rules | non-empty, `[a-z0-9]` only, ≤ 20 characters (`CloudEvents` 1.0.2 §3.1); never a core context attribute name; never the `proxima` prefix, which this substrate reserves |
| String values | non-empty, no control characters, ≤ 256 bytes |
| Bound | ≤ 8 attributes per event |
| Additivity | a second bind MERGES; a repeated name is an error. Nothing overwrites or clears a bound attribute, and there is no unbind call — a host that should stop emitting one stops binding it |
| Validation point | at binding, not at capture: an attribute a consumer would refuse is a host misconfiguration, and the refusal belongs at the boundary that produced it |
| Order | NAME order, not binding order. The digest, the intake's dedup key and every consumer signature check are over the envelope bytes, so the order a host happened to bind in must not reach the wire |
| Unbound | emits nothing — not an empty object, not a null. An event from a deployment that binds none is byte-identical to one from a release without this surface |
| Storage | no column and no migration: the captured record IS the envelope bytes |
| Identity | NOT identity material. `identity_for_revalidation()` does not carry them, and a redeemed delegated phase rebuilds without them, exactly as it does for `trusted_model_id` |
| Consumers | parsed into `CloudEventEnvelope::extensions` (name-ordered); a signature check still reads `ReceivedEvent::raw` |

## Goal changes on the feed

A goal write is an action, and an action is an ordinary Fact
([05](05-actions.md)). With an intake configured, every goal write that inserts
a Goal row also writes one core Fact of schema `core/goal-change-v1` in the
same transaction, and the capture above delivers it. That covers set, modify,
each decomposed child, pause, resume, abandon and achieve, on every path
including the unit-of-work path. The outbox still holds only Facts: a goal
change is one, with its own `t`, its own sidecar row and its own record.

| Property | Value |
|---|---|
| Schema | `core/goal-change-v1` version 1, sidecar `proxima_core.goal_change_v1`; declares no payload references |
| Listenable | only when the runtime has an `outbox_intake`: `compose_registry` calls `FlavorRegistry::publish_goal_changes` before freeze. The schema is always registered, so a stored change Fact stays readable. A goal write writes the Fact exactly when the schema is listenable in the frozen registry, so `capture_iff_listenable` holds unchanged |
| No intake | a goal write is unchanged: no change Fact, no record. Omitting the intake later stops goal changes; it does not keep capturing them (§Rollback) |
| Boot | the schema being listenable, an intake without `PROXIMA_PUBLICATION_SOURCE` fails boot with a `PublicationConfig::validate_against` error that names `core/goal-change-v1` |
| Replay | a replayed goal write (same idempotency key) writes no second Fact and captures no second record |
| Writer | only a goal write. Engine Fact ingest refuses the schema (`InvalidArgument`), so Fact-write authority cannot stand in for goal-write authority on an event a scheduler acts on |
| Event `id` | the change Fact's `F:<uuid>`; the goal version is in `data.goal` |

`data`:

```json
{
  "series": "<uuid>",
  "goal":   "G:<uuid>",
  "prior":  "G:<uuid>" | null,
  "revision": <n>,
  "state":  "Active" | "Paused" | "Achieved" | "Abandoned",
  "change": {"kind": "set"} | {"kind": "modify"}
          | {"kind": "decompose_child", "parent": "G:<uuid>"}
          | {"kind": "pause", "pending": "keep" | "discard"}
          | {"kind": "resume"} | {"kind": "abandon"} | {"kind": "achieve"}
}
```

| Field | Meaning |
|---|---|
| `series` | the goal's series handle, the same on every version. Opaque, not a `G:` handle |
| `goal` | the version this write created |
| `prior` | the version it replaced; `null` when the write starts a series (set, decomposed child) |
| `revision` | the version's position in its series: 1 for a set and for a decomposed child, one more for each later version. Storage counts the series' goal rows in the write's transaction, under the series lock |
| `state` | the state after the write |
| `change.pending` | on `pause` only, and always there: `keep` means the scheduler holds the work it queued for the goal and runs it after a resume; `discard` means it drops that work (ready or waiting to retry). Work already running finishes either way. Proxima records the decision and does not interpret it. Every other change says nothing about pending work; after `abandon` or `achieve` the goal is terminal and the scheduler drops what it queued |

**Ordering.** A failed delivery is retried after later records (claim order
`attempts ASC, t ASC`), so a consumer can see a `resume` before the `pause` it
follows. Key state by `series` and apply an event only if its `revision` is
greater than the last `revision` applied for that series. `goal` versions are
identifiers, not an order: a version's `t` comes from the database clock and
can sort below its predecessor after a clock step. Proxima has no code for
this.

## Who wrote what

Proxima knows what was written; the host knows who called. Proxima keeps no
per-write author: the opt-in MCP call ledger (`record_mcp_calls`) records who
called a tool, not what that call wrote. A host that needs an audit (who set,
paused or modified a goal in a shared owner) records it from this feed, in its
own table and under its own retention
([13 §The position](13-compliance.md#the-position)). A goal read
returns no `set_by` for the same reason.

The host binds the actor as a host-bound extension attribute (§Host-bound
extension attributes), in its `Authenticator`. Every Fact that context goes on
to capture carries it, `core/goal-change-v1` included, because a goal write
stamps its change Fact from the same `AuthzContext` as any other Fact write.
Proxima names no actor attribute and reads none: the name and the value are the
host's. The examples use the name `actor` and the host's subject id as a
string. `proximaowner` is the owner (for a group, the group), never the actor.
`proximamodel` is the token-certified model, not the person.

| Property | Value |
|---|---|
| Exists iff committed | the record is written in the write's transaction, so a rolled-back write leaves no event |
| At least once | a redelivery is normal (§Delivery guarantee), so the host's ledger inserts-or-ignores on the event `id` |
| Replay | a replayed write (same idempotency key) writes no second Fact and captures no second record, so the first writer keeps the write |
| Order | a ledger appends, so reordering does not matter. For the latest version per goal, use `revision` (§Goal changes on the feed) |
| Coverage | listenable Facts, and goal writes while an intake is configured. Not on the feed: derivations (Abstractions, Perspectives), interpretations, links, forget, transfer and membership changes. The host attributes those from its own request records |
| No actor | a context the host bound nothing to (a `for_system` context, unless the host binds one on it) and a redeemed delegated phase capture no extension attributes. The host attributes such a write from its own records: which job, which delegation |

The actor attribute lies in the envelope bytes of the OWNER's record. Erasing
that owner destroys its records. Erasing the actor's own personal owner does
not reach the records of a group the actor wrote into. Published records stay
until `PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS` removes them (unset keeps them),
so set it to bound how long they hold the attribute. The host's ledger, its
retention and who may read it, is the host's
([13](13-compliance.md#the-position)).

Recipe: [how-to/fact-outbox.md §7](how-to/fact-outbox.md#7-record-who-set-a-goal).

## Outbox Row and State Machine

Table `proxima_core.publication_outbox` (migration `0011`; `0025` simplified its
published check), one row per captured Fact `t`.

| Column | Note |
|---|---|
| `t` | `uuid` **PRIMARY KEY** — the Fact's memory `t`. There is **no FK to `memory`** (below) |
| `owner_id` | `uuid NOT NULL REFERENCES owners(owner_id)`. A single id column, not the `(owner_kind, owner_id)` pair `memory` carries: the kind is read back through the `owners` join at claim time, so the record cannot disagree with the owner table |
| `schema_id`, `schema_version` | the registered typed export's identity |
| `event_type` | the `CloudEvents` `type`, duplicated out of the envelope so a routing decision needs no JSON parse |
| `event_id` | the `CloudEvents` `id` (`F:<uuid>`), `UNIQUE` — also the intake's dedup key |
| `envelope` | `bytea`, the verbatim `CloudEvent` |
| `envelope_digest` | `bytea`, blake3 over those exact bytes; `CHECK (octet_length = 32)` |
| `state` | SQL enum `publication_state` (`pending`/`claimed`/`published`) |
| `claim_token`, `claimed_by`, `lease_expires_at`, `attempts` | claim bookkeeping; `claimed_by` is the operator-facing `PublisherId`, `claim_token` is the fencing token |
| `published_at` | when the intake's outcome was recorded. Set exactly when `state = 'published'` |
| `published_stream`, `published_seq` | the JetStream PubAck of the removed NATS publisher. Nothing writes them any more: a record published in process leaves both `NULL`, and one published before v0.0.27 keeps its receipt |
| `captured_at` | insert time, `DEFAULT now()` |

There is **no `source` column and no `recorded_at` column**. Both live in the
envelope and nowhere else: `source` is one configured value per installation
(a column would be the same string on every row and could drift from the
envelope's), and the recording time is decoded from the UUIDv7 inside `t`
rather than stamped a second time.

Two CHECK constraints make the state machine's shape unfakeable rather than
merely conventional:

| Constraint | Says |
|---|---|
| `publication_outbox_claim_chk` | `state = 'claimed'` **iff** `claim_token`, `lease_expires_at` and `claimed_by` are all present |
| `publication_outbox_published_chk` | `state = 'published'` **iff** `published_at` is present |

`publication_outbox_capture_immutable` (a `BEFORE UPDATE` trigger) rejects any
UPDATE that changes `t`, `event_id`, `event_type`, `envelope`,
`envelope_digest`, `owner_id`, `schema_id`, `schema_version` or `captured_at`,
with `SQLSTATE 25006`. The lifecycle columns are the only writable ones: the
captured event is immutable **by constraint**, so no later re-rendering,
re-owning or re-keying can rewrite what a consumer will be told happened.

**Why no FK to `memory`.** `forget` DELETEs the hot `memory` row — it moves the
payload to cold storage — so a cascading reference would destroy a captured
event that no drain had delivered yet: a committed event lost to an
unrelated lifecycle operation. Completeness rests on `owner_id -> owners`
instead, exactly as `mcp_call_logged_v1` does for the same reason.

| From | Event | To |
|---|---|---|
| `pending` | claim (lease) | `claimed` |
| `claimed` | intake outcome recorded (`mark_published`) | `published` |
| `claimed` | release | `pending` |
| `claimed` | lease expiry | `pending` |
| `published` | retention horizon (below) | row deleted |
| `published` | otherwise | terminal |

**No delivery transition deletes a row.** Expiry permits retry, never loss.
Discovery is `state = 'pending' OR (state = 'claimed' AND lease_expires_at <
now())` under `FOR UPDATE SKIP LOCKED` — a set scan, not a high-water cursor,
so a delayed commit cannot disappear behind an advanced position.

**Claim order is `ORDER BY attempts ASC, t ASC`,** not `t` alone. A record the
intake keeps refusing would otherwise hold the front of every batch window and
starve every Fact behind it; ordering on the attempt count first demotes it
after its first failure, so a poison record costs one slot per pass instead of
the whole pass. The partial index `publication_outbox_pending_idx`
(`(t) WHERE state <> 'published'`) still serves the predicate; it no longer
serves the sort, which is bounded by the backlog rather than by history.

**Retention.** `PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS` (unset or `0` = keep
forever; floor 60 s) lets a host reclaim storage from records that are already
delivered. It deletes only rows that are `state = 'published'` **and** whose
`published_at` is older than the horizon, in bounded batches, and it can never
reach a `pending` or `claimed` row whatever its age — an undelivered record is
a promise this deployment has not kept, and no retention policy may quietly
cancel it. Kernel carrier: `prune_never_removes_undelivered` (OB-9).

Surface declaration (see [13 §What an erase destroys](13-compliance.md#what-an-erase-destroys)):

| Field | Value | Why |
|---|---|---|
| `key` | `KeyShape::MemoryT { column: "t" }` | the record IS the Fact |
| `transfer` | `TransferRule::RetainAtSource` | a captured event is a delivery obligation of the owner it was captured for; the Fact moves, the event that already described it does not |
| `erase` | `EraseRule::ByOwner` | owner-pinned, **not** keyed: `forget` deletes the hot `memory` row while the record stays, so an erase that selected on the memory set would walk past exactly the records a forgotten Fact left behind |
| `export` | `ExportRule::Excluded` | a derived delivery copy of a typed Fact the export already carries; exporting it would hand the subject the same content twice, once inside a transport envelope naming this installation |
| `forget` | `ForgetRule::Keep` | cooling a Fact does not un-commit the event captured with it |
| `counter` | `CounterRule::Counted("publications")` | destroyed records appear on the erase receipt |
| `completeness` | `publication_outbox_owner_id_fkey` | the FK that stands in for the absent one to `memory` |

Single-memory erase does not go through the owner sweep, so it deletes the
record **explicitly**, on `t` alone. The owner predicate an earlier version
carried was a leak: `TransferRule::RetainAtSource` means a transferred Fact's
record keeps the ORIGINAL owner's `owner_id`, so `WHERE t = $1 AND owner_id =
$2` would have walked past exactly the records a transferred-then-erased Fact
left behind.

## Original publication attribution and erase

Migration `0012` adds `proxima_core.publication_origin`: one payload-free row
per published Fact `t`, with its immutable original typed owner and optional
native `SourceId`. Capture inserts it in the same transaction as a genuinely
new Fact and its outbox row. Non-listenable writes and receipt replays create
no origin; cooling, owner transfer, and pruning a delivered outbox body keep
the origin. The outbox still has only `owner_id`; its owner kind is recovered
through the retained `owners.kind` row, rather than added as a duplicate
column. This native Proxima `SourceId` is the Fact's source-scope label; it is
distinct from the configured CloudEvents producer `source` URI above.

The origin is the selector for later revocation after the delivery body is
gone or the Fact has moved. Whole-owner erase revokes every origin captured by
that owner, including rows with no source. Source-scope erase selects both
physical Fact ids still in the requested owner/source and matching immutable
original owner/source locators. A destination-owner erase also selects every
physical Fact `t`, even when its origin belongs to a former owner. Each SQL
predicate is a union over unique `t` keys, so a row matching both legs is
deleted and counted once. A source erase can revoke an original copy while a
transferred live Fact stays with its current owner. An authorized physical
hard erase removes origin and outbox rows by exact Fact `t`, regardless of the
writer that captured it. The same source label may be used by a fresh Fact
`t` after revocation.

Before host code accepts a delayed payload, `PublicationOriginEligibilityPort`
checks the typed original owner and Fact `MemoryId` for a surviving origin and
the absence of a hard-delete witness. It returns only eligible/ineligible; it
does not expose Fact payload. The caller runs this check inside its existing
unit-of-work transaction, under the shared lifecycle fence held from entry,
and keeps that transaction through the payload write. Owner/source and custom
physical erases take the exclusive fence before their owner, source, handle,
or target locks, so revocation cannot pass between the check and the write.

The migration backfills only a surviving outbox row plus a retained hot or
cooled Fact that proves source identity. Both source columns NULL mean known
source absence; a missing retained Fact, a malformed one-NULL cooled pair, a
hard-delete witness, or an outbox row already pruned before this migration
does not produce an origin. History already removed by retention cannot be
reconstructed. Direct SQL or separately composed engines remain trusted-host
residuals; lifecycle registration is not a SQL sandbox.

## Host-Only Ports

```rust
trait PublicationOutboxPort {
    async fn claim(&self, publisher: &PublisherId, limit: NonZeroU32, lease: Duration)
        -> Result<Vec<ClaimedPublication>>;
    async fn mark_published(&self, id: Uuid, claim: ClaimToken)
        -> Result<AckOutcome>;               // StaleClaim | Published | AlreadyPublished
    async fn release(&self, id: Uuid, claim: ClaimToken) -> Result<ReleaseOutcome>;
    async fn pending_count(&self) -> Result<u64>;
}

trait PublicationRetentionPort {
    async fn prune_published(&self, older_than: Duration, limit: NonZeroU32) -> Result<u64>;
}
```

**Host-only**: neither is reachable from a flavor, a `ToolCtx`, or a
`WriteSession`, and neither is part of `StoragePorts`. `PgStorage` implements
both. A stale claim token cannot acknowledge or discard another drain's record.
The runtime's own drain is built on `PublicationOutboxPort`, and the port is
exported so a host that needs a different loop (one that holds claims across
its own round trip, say) can write one.

They are **two traits on purpose**. `PublicationOutboxPort` has no delete
method and never should — an expiring lease can only make a record deliverable
again, never destroy it — so the handle a drain loop carries cannot remove a
record at all. The only DELETE lives on the second trait, which a drain does
not need to hold. "A drain may deliver" and "an operator may reclaim delivered
storage" stay two capabilities rather than one.

`claim` refuses a lease shorter than **one second**. A sub-second lease expires
before the first delivery can finish, so every record claimed under it would be
re-claimed while it was still in flight; the floor is a refusal rather than a
silent clamp, because a caller that asked for 0 s asked for something that
cannot work.

## Delivery

The host hands the runtime one intake, on the runtime builder
(`RuntimeBuilder::outbox_intake(Arc<dyn DurableIntake>)`, or the same method on
`Proxima::<A>::app()`; [10 §Runtime features](10-configuration.md#runtime-features)).
Set, the outbox publisher starts in `build()` and `run()` alike, owns its
cancellation, and is joined on `shutdown()`. Unset, nothing drains: capture
still runs and records stay `pending` (§Rollback). There is no environment
variable and no cargo feature.

```rust
#[async_trait::async_trait]
trait DurableIntake: Send + Sync + Debug {
    async fn accept(&self, event: &ReceivedEvent) -> Result<Intake, IntakeError>;
}
enum Intake { Accepted, Rejected { reason: String } }
struct IntakeError { pub message: String }     // IntakeError::new(message)
```

All six names (`DurableIntake`, `Intake`, `IntakeError`, `ReceivedEvent`,
`EventIdentity`, `CloudEventEnvelope`) live in `proxima_core::publication::intake`
and are re-exported by `proxima`.

| `ReceivedEvent` field | Value |
|---|---|
| `id` | the `CloudEvents` `id` (`F:<uuid>`): the dedup key, a plain string |
| `owner` | the Fact's `OwnerRef`, typed from the outbox row (`owner_kind` joined at claim time) |
| `delivered_count` | the record's `attempts`: how many times it has been claimed, this claim included. `> 1` means an earlier claim did not end in the mark: its delivery failed or timed out, the process stopped before the mark, or shutdown or a quiesce handed the claim back before `accept` was called. It counts claims, not intake calls |
| `raw` | the captured envelope bytes, verbatim. A signature check reads these |
| `envelope` | `raw`, parsed: `CloudEventEnvelope` |

**The contract.** `accept` MUST have made its outcome durable before it
returns, because `mark_published` follows it: the outbox forgets the record
once it is marked. Both outcomes count — a rejection nobody recorded is
indistinguishable from an event that never arrived. Deduplicate on `id`;
redelivery is not an error case, it is the normal cost of at-least-once. `accept`
should return within about five seconds (the timeout below): an intake that
needs longer accepts into its own durable queue and returns.

| What happens | The drain | The record |
|---|---|---|
| `accept` returns `Ok(Accepted)` | `mark_published` | `published` |
| `accept` returns `Ok(Rejected { .. })` | the same: a rejection is a decision the intake made durable | `published` |
| `accept` returns `Err(IntakeError)` | releases the claim | `pending`, tried again after the backoff |
| `accept` does not return within 5 s | drops the call and releases the claim | `pending`, tried again |
| the captured bytes are not a `CloudEvents` envelope | logs the event id and the class of the parse error at error level, releases the claim; the intake is not called | `pending` (re-claimed each pass, `attempts` climbing) |
| the process stops between `accept` and `mark_published` | nothing: the lease (30 s) expires | `pending`, delivered again with `delivered_count + 1` |

A record leaves `pending`/`claimed` through exactly one path:
`mark_published`, fenced on the claim token. The only other thing that removes
a row is the published-record retention below, and compliance erasure (§Outbox
Row and State Machine).

**The drain.** One loop in the `proxima` crate, with fixed settings: a batch of
64, a 30 s lease, a 500 ms poll, a 5 s `accept` timeout, claims labelled
`proxima:<pid>`. A pass claims a batch and delivers its records one at a time.

- **Per-record isolation.** A record that fails is released at once and the
  pass carries on. A pass reports a failure only when it delivered nothing at
  all. A record the intake will never take is demoted by the claim order
  (`attempts ASC`), so it costs one slot per pass instead of stalling every
  Fact behind it until the backlog fills.
- **Backoff.** A pass that delivered nothing doubles the wait before the next
  claim, from the poll interval up to 30 s; any delivery resets it. A pass that
  claimed anything does not sleep before the next one.
- **Logs.** A failed pass logs at warn level with a failure category (storage,
  malformed, intake, timed out). The intake's own message and the parse error's
  text are never logged by the runtime: either can quote a payload.
- **Shutdown** abandons the `accept` in flight and hands back every claim the
  pass still holds, so the next process does not wait out a lease for work
  nobody is doing. The same bytes are delivered again under the same `id`.

There is no health reader for the drain. The boot report says whether it
started; `pending_count`, the `attempts` column and the warn-level log say
whether it is making progress.

<a id="publisher-quiesce"></a>
### Publisher quiesce

`feature_control(Feature::OutboxPublisher)` pauses claiming without stopping
the task or the server ([10 §Quiesce and resume](10-configuration.md#quiesce-and-resume)).

- `quiesce()` lets the record in flight finish: a record whose `accept` has
  returned is recorded with `mark_published`, not dropped. The unattempted
  rest of the claimed batch is released to `pending`, as on shutdown, and
  the task parks before its next claim. Settling costs at most one `accept`
  timeout, not a batch of them. Shutdown differs: it abandons the `accept` in
  flight.
- The task stays alive and `Running`; `FeatureControl::status()` is what
  reports the pause. `resume()` claims again.
- Capture continues. A Fact accepted while the publisher is quiescent still
  commits its outbox record in its own transaction, and the record stays
  `pending` until `resume()`: quiescent is not drained. The published-record
  prune keeps running; it deletes delivered records only.
- Lease caveat: a release that failed leaves its lease (30 s) to expire, so
  quiescent does not mean no row is `claimed`. A late `mark_published` from a
  quiesced process is refused as stale.
- Kernel: `Causa/Publication.lean` excludes publication liveness and proves
  only the safety half of at-least-once. A pause changes when records are
  delivered, not how a duplicate is identified.

## Delivery guarantee

Delivery is **at-least-once**. A crash between `accept` and the published
marker, a lost `mark_published`, or an `accept` that outlived its timeout
delivers the **same bytes under the same `id`** again. Duplicate delivery is
permitted; the intake deduplicates on the durable event identity. The drain
never waits for a business operation the event may start, and nothing about
`Accepted` says one began or finished.

`ReceivedEvent::id` stays the dedup key, a plain string.
`ReceivedEvent::identity()` reads it as `EventIdentity::Fact(MemoryId)` when it
is `F:` and a lowercase hyphenated UUID, the exact spelling the producer writes,
and as `EventIdentity::Opaque(String)`, byte for byte, otherwise: upper-case,
simple, braced or `urn:` UUIDs, other prefixes, no prefix, padding, anything
else. Two ids are equal exactly when their identities are equal. A parsed
identity proves syntax, not admission: it does not say a Fact with that id was
admitted, that its schema is listenable, that the owner owns it, or that
Proxima sent the event. Signature and owner checks stay with the intake.

## Forwarding to a broker

Proxima ships no broker client. A host that needs NATS, Kafka or anything else
forwards from inside its own `accept`: publish `raw` with `id` as the broker's
dedup key, and return `Ok` only after the broker has acknowledged under the
host's durability semantics. A broker that is down or full is then an
`IntakeError`: the record stays `pending` with its original bytes and is
delivered once the broker recovers. The visible symptom of a broker that stays
down is `CapacityExhausted` on writes, once the DATABASE backlog reaches
`PROXIMA_OUTBOX_MAX_PENDING`. Topology, partitioning, retention, capacity and
permissions belong to the host's deployment.

Copies in a host's broker outlive the Fact's authorization, and a broker copy
is not removed by any database erase. Two host accessors exist for that:

- `ProximaHost::origin_scope_for_host()` returns the installation's
  `OriginScope`, minted once by migration `0012` in
  `proxima_core.installation`. A host stamps it on each forwarded copy (as a
  header beside the bytes, not inside them: the envelope bytes are the sealed
  artifact the outbox digest covers).
- `ProximaHost::publication_origin_eligibility_for_host()` returns the
  `PublicationOriginEligibilityPort` (§Original publication attribution and
  erase). A cleaner deletes a copy only when it carries THIS installation's
  scope **and** the port answers `Ineligible`.

The stamp exists because the eligibility answer comes from the ABSENCE of an
origin row, and an absence carries no scope: a Fact this database never held
and a Fact whose origin erasure revoked look identical. A cleaner must treat a
copy stamped by another installation as a reason to halt, and an unstamped copy
as unattributable (retain it). The stamp identifies an installation *lineage*:
a restore or a clone carries the same identity, so never point a restored or
cloned database's cleaner at the original's broker. Proxima does not do this
cleanup for a host.

## Rollback

| Action | Effect |
|---|---|
| omit `outbox_intake` | drain off; **capture continues**; pending records are retained and delivered, under their original identities, by the next boot that supplies an intake |
| intake outage | capture continues while local capacity lasts; delivery resumes from `pending` |
| disable capture | not available while a listenable type is registered — refuse the write path instead |
| omit `outbox_intake` after running with one | goal writes stop writing `core/goal-change-v1` Facts, because the schema is no longer listenable; the change Facts already written stay readable and their pending records stay `pending` |
| unset `PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS` | retention off; delivered records are kept forever. Never affects an undelivered one either way |

No destructive schema rollback, no hosted reroute.

## Non-Goals

Exactly-once delivery. A global commit order across owners. Host orchestration,
Goal matching, claims or retries of application work. Typed invocation
dispatch and automatic invocation-context propagation. A queue HTTP service or
a generic broker-plugin framework. An audit of who wrote what
(§Who wrote what).

## Kernel Carrier

`docs/lean/Causa/Publication.lean` (`Causa.Publication`) carries the
domainless half: `capture_iff_listenable`, `replay_no_second_record`,
`step_preserves_capture`, `no_step_deletes`, `expiry_preserves_record`,
`erasure_is_the_only_removal`, `owner_follows_fact`,
`duplicates_share_identity`, and — for retention —
`prune_never_removes_undelivered` and `prune_keeps_recent_deliveries`.
All-or-none transactionality and delivery liveness are **explicitly excluded**
there, with tests as their carrier — rows OB-1..OB-9 in
[lean/COVERAGE.md](lean/COVERAGE.md).

Operating recipe: [how-to/fact-outbox.md](how-to/fact-outbox.md). Env rows:
[10 §Framework facade (host-app boot)](10-configuration.md#framework-facade-host-app-boot).
