# 18 — Fact Outbox

> **Status:** current.

Host-side at-least-once publication of **declared listenable Facts**. 18 owns
the listen declaration, the capture record, the delivery state machine, and
the publisher/consumer adapters. It does not own NATS stream topology: the
deployment provisions streams, transforms, partitions, consumers, permissions,
storage, retention and capacity. It owns no verb (see
[14](14-protocol-surface.md#the-verbs)), no authorization of its own (see
[01](01-event-source.md)), and no orchestration.

## Purpose

A host reacts to Fact creation without every flavor carrying a message client
and its delivery state. A committed Fact survives publisher crash and broker
outage: the event stays available for later delivery with its original identity
and bytes.

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
        │  host publisher: claim(lease) → send the captured bytes, unchanged
        ▼
   NATS JetStream, deployment-provisioned topology
        │  PubAck → mark_published    (no PubAck → release/expire → `pending`)
        ▼
   durable pull consumer
        ACK only after durable intake OR durable retention of a rejection
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
| `PayloadTooLarge` | serialized export exceeds `PROXIMA_OUTBOX_MAX_PAYLOAD_BYTES` | `InvalidArgument` | `-32602` | 400 `invalid-argument` |
| `CapacityExhausted` | unpublished record count is at `PROXIMA_OUTBOX_MAX_PENDING` — explicit backpressure, never silent eviction | `CapacityExhausted` | `-32000`, `data.code = "capacity_exhausted"` | **503** `capacity-exhausted` |
| `UntypedListenableWrite` | a listenable series reached the chokepoint without its registered typed export | `InvalidArgument` | `-32602` | 400 `invalid-argument` |
| `SourceUnbound` | a listenable schema reached the chokepoint with no configured publication source | `Internal` | `-32603` | 500 `internal` |
| `ExportFailed` | the envelope would not serialize, or `t` carries no UUIDv7 timestamp | `Internal` | `-32603` | 500 `internal` |

`CapacityExhausted` is its own class on every surface, and deliberately not the
class of a permanent precondition failure. It says *retry later*: the write was
refused because a **transient** backlog is at its ceiling, and a caller that
sees 503 knows to back off where a 400 would have told it to change its
request and a 500 would have told it to open an incident. See
[17 §Error surface](17-rest-surface.md).

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
verbatim as bytes; the publisher sends those bytes unchanged — it never reloads
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

Producer authority is the configured credential and the authorized owner
binding, never a self-asserted header.

## Outbox Row and State Machine

Table `proxima_core.publication_outbox` (migration `0011`), one row per
captured Fact `t`.

| Column | Note |
|---|---|
| `t` | `uuid` **PRIMARY KEY** — the Fact's memory `t`. There is **no FK to `memory`** (below) |
| `owner_id` | `uuid NOT NULL REFERENCES owners(owner_id)`. A single id column, not the `(owner_kind, owner_id)` pair `memory` carries: the kind is read back through the `owners` join at claim time, so the record cannot disagree with the owner table |
| `schema_id`, `schema_version` | the registered typed export's identity |
| `event_type` | the `CloudEvents` `type`, duplicated out of the envelope so a routing decision needs no JSON parse |
| `event_id` | the `CloudEvents` `id` (`F:<uuid>`), `UNIQUE` — also the broker dedup key |
| `envelope` | `bytea`, the verbatim `CloudEvent` |
| `envelope_digest` | `bytea`, blake3 over those exact bytes; `CHECK (octet_length = 32)` |
| `state` | SQL enum `publication_state` (`pending`/`claimed`/`published`) |
| `claim_token`, `claimed_by`, `lease_expires_at`, `attempts` | claim bookkeeping; `claimed_by` is the operator-facing `PublisherId`, `claim_token` is the fencing token |
| `published_at`, `published_stream`, `published_seq` | the PubAck: when, which stream, which sequence |
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
| `publication_outbox_published_chk` | `state = 'published'` **iff** `published_at`, `published_stream` and `published_seq` are all present |

`publication_outbox_capture_immutable` (a `BEFORE UPDATE` trigger) rejects any
UPDATE that changes `t`, `event_id`, `event_type`, `envelope`,
`envelope_digest`, `owner_id`, `schema_id`, `schema_version` or `captured_at`,
with `SQLSTATE 25006`. The lifecycle columns are the only writable ones: the
captured event is immutable **by constraint**, so no later re-rendering,
re-owning or re-keying can rewrite what a consumer will be told happened.

**Why no FK to `memory`.** `forget` DELETEs the hot `memory` row — it moves the
payload to cold storage — so a cascading reference would destroy a captured
event that no publisher had delivered yet: a committed event lost to an
unrelated lifecycle operation. Completeness rests on `owner_id -> owners`
instead, exactly as `mcp_call_logged_v1` does for the same reason.

| From | Event | To |
|---|---|---|
| `pending` | claim (lease) | `claimed` |
| `claimed` | PubAck recorded | `published` |
| `claimed` | release | `pending` |
| `claimed` | lease expiry | `pending` |
| `published` | retention horizon (below) | row deleted |
| `published` | otherwise | terminal |

**No delivery transition deletes a row.** Expiry permits retry, never loss.
Discovery is `state = 'pending' OR (state = 'claimed' AND lease_expires_at <
now())` under `FOR UPDATE SKIP LOCKED` — a set scan, not a high-water cursor,
so a delayed commit cannot disappear behind an advanced position.

**Claim order is `ORDER BY attempts ASC, t ASC`,** not `t` alone. A record the
broker keeps refusing would otherwise hold the front of every batch window and
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

## Host-Only Ports

```rust
trait PublicationOutboxPort {
    async fn claim(&self, publisher: &PublisherId, limit: NonZeroU32, lease: Duration)
        -> Result<Vec<ClaimedPublication>>;
    async fn mark_published(&self, id: Uuid, claim: ClaimToken, receipt: &BrokerReceipt)
        -> Result<AckOutcome>;               // StaleClaim | Published | AlreadyPublished
    async fn release(&self, id: Uuid, claim: ClaimToken) -> Result<ReleaseOutcome>;
    async fn pending_count(&self) -> Result<u64>;
}

trait PublicationRetentionPort {
    async fn prune_published(&self, older_than: Duration, limit: NonZeroU32) -> Result<u64>;
}
```

Broker-neutral, and **host-only**: neither is reachable from a flavor, a
`ToolCtx`, or a `WriteSession`, and neither is part of `StoragePorts`. A stale
claim token cannot acknowledge or discard another publisher's record.

They are **two traits on purpose**. `PublicationOutboxPort` has no delete
method and never should — an expiring lease can only make a record deliverable
again, never destroy it — so the handle a drain loop carries cannot remove a
record at all. The only DELETE lives on the second trait, which a publisher
does not need to hold. "A publisher may drain" and "an operator may reclaim
delivered storage" stay two capabilities rather than one.

`claim` refuses a lease shorter than **one second**. A sub-second lease expires
before the first publish can finish, so every record claimed under it would be
re-claimed while it was still in flight; the floor is a refusal rather than a
silent clamp, because a caller that asked for 0 s asked for something that
cannot work.

## Two Acknowledgements

| Boundary | Completion condition | What it does NOT mean |
|---|---|---|
| outbox → JetStream | PubAck under the deployment's configured broker semantics permits `mark_published` | that a consumer saw it |
| JetStream → consumer | ACK follows committed durable intake **or** durable retention of a rejection | that a business operation started or finished |

Lost PubAck, timeout, or a crash before the published marker commits ⇒
republication of the **same bytes under the same `id`**. Duplicate delivery is
permitted; consumers deduplicate on the durable event identity beyond the
broker's dedup window. The producer never waits for a subscriber's validation
or response.

## Deployment topology

NATS topology is not an application profile. DevOps provisions the stream and
durable consumer, including subject transforms and partitioning, before the
publisher is enabled. The publisher emits the canonical source subject:

```text
proxima.fact.<owner_kind>.<owner_uuid>.<type_token>
        │
        └── deployment-owned stream transform / partition mapping
                         │
                         └── deployment-owned durable consumer filter
```

The publisher needs only broker connectivity, publish permission on the source
subject, and permission to receive its publish reply inbox. It does not create,
update, inspect or validate streams or consumers. A successful `PubAck` records
broker acceptance; the deployment and consumer own the source → transform →
partition → durable-intake path.

The local fixture provisions a reproducible stream separately so tests can
exercise the adapter without making the application a topology controller.
Production deployments must provide equivalent provisioning, permissions,
retention and capacity through their own infrastructure system.

The adapter acceptance boundary is explicit: a publisher credential with no
stream-management rights must still connect, publish and record a `PubAck`.
The deployment acceptance boundary is separate: publish a sentinel through
the provisioned source → transform → partition → durable-consumer route and
prove the consumer durably accepts or retains its outcome before ACK.

### The `type_token` rule

The last subject element encodes the event type — the registered schema id —
and the encoding is **injective**, because subject permissions are how doc 18
tells an operator to scope a NATS account to a set of event types. Two schema
ids sharing a token would silently widen such a grant.

The rule, over the schema id's BYTES:

| Input byte | Output |
|---|---|
| `A-Z`, `a-z`, `0-9`, `-` | itself |
| **everything else, including `_`** | `_` followed by two lowercase hex digits of the byte (`_%02x`) |

Escaping `_` itself is what makes the map injective; without it `a/b` and `a_b`
would collide. Non-ASCII is escaped byte by byte, so one multi-byte character
becomes several `_xx` groups. The empty type maps to a bare `_`, because `""`
is not a legal subject token — and a bare `_` is never the image of a non-empty
input, since every escape carries its two hex digits.

| Schema id | Token |
|---|---|
| `probe/listenable-v1` | `probe_2flistenable-v1` |
| `probe_listenable-v1` | `probe_5flistenable-v1` |
| `acme/build.finished-v1` | `acme_2fbuild_2efinished-v1` |

Owner routing lives in the subject, so NATS account permissions restrict a
consumer by subject prefix. File storage plus PubAck is durability against
process death, **not** a claim of surviving arbitrary disk loss.

The reference consumer binds an existing durable by the deployment-provided
stream and durable names. It does not create, update or validate the consumer's
filter, ACK policy, redelivery policy or flow-control settings. Those settings
are part of the deployment topology and are tested there.

### Stream space never frees itself

With `retention: Limits` and `discard: New`, **an ACK does not free space** —
by design. `Limits` retains a message until an age, size or count bound
evicts it, and consumer acknowledgement is not one of those bounds. So a full
stream stays full until an operator acts:

| Signal | What it means | Operator action |
|---|---|---|
| PubAck refused, `BrokerCapacity` in the publisher log | the deployment's stream is at capacity | raise the deployment's stream capacity, or purge already-consumed sequences |
| records stay `pending`, `attempts` climbing | the same, seen from the database | as above; **nothing is lost** — the records are still there with their original bytes |
| `CapacityExhausted` on writes | the DATABASE backlog hit `PROXIMA_OUTBOX_MAX_PENDING` | the broker side is the cause; fix that first |

Purging is `nats stream purge PROXIMA_FACTS --seq <n>`, and `<n>` is safe only
when **every** consumer's `ack_floor` is at or beyond it — that is the operator
judgement the substrate refuses to make for you.

The local fixture uses `Limits` and `discard: New` because that combination
makes broker capacity visible as backpressure. Production may choose another
retention policy, but DevOps must test its exact semantics: a deployment whose
consumer has not been created, or was deleted during an incident, must not
silently discard committed events while returning a `PubAck`. A full stream
that refuses new work is a failure an operator can see and undo; a stream that
accepts work and discards it is not.

The local fixture pins nats-server **2.14.6** and `async-nats` **0.50.0**. The
optional adapter is `crates/outbox-nats/`; the NATS dependency never enters
`proxima-core`.

## Rollback

| Action | Effect |
|---|---|
| unset `PROXIMA_NATS_URL` | publisher off; **capture continues**; pending records are retained for repair and replay under their original identities |
| broker outage | capture continues while local capacity lasts; delivery resumes from `pending` |
| disable capture | not available while a listenable type is registered — refuse the write path instead |
| unset `PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS` | retention off; delivered records are kept forever. Never affects an undelivered one either way |

No destructive schema rollback, no hosted reroute.

## Non-Goals

Exactly-once delivery. A global commit order across owners. Host orchestration,
Goal matching, claims or retries of application work. Typed invocation
dispatch and automatic invocation-context propagation. A queue HTTP service or
a generic broker-plugin framework.

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
