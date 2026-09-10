# 18 — Fact Outbox

> **Status:** current.

Host-side at-least-once publication of **declared listenable Facts**. 18 owns
the listen declaration, the capture record, the delivery state machine, and the
one shipped JetStream profile. It owns no verb (see
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
   NATS JetStream, stream PROXIMA_FACTS
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

| Error | Raised when |
|---|---|
| `PayloadTooLarge` | serialized export exceeds `PROXIMA_OUTBOX_MAX_PAYLOAD_BYTES` |
| `CapacityExhausted` | unpublished record count is at `PROXIMA_OUTBOX_MAX_PENDING` — explicit backpressure, never silent eviction |
| `UntypedListenableWrite` | a listenable series reached the chokepoint without its registered typed export |
| `SourceUnbound` | a listenable schema reached the chokepoint with no configured publication source |
| `ExportFailed` | the envelope would not serialize, or `t` carries no UUIDv7 timestamp |

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

Table `proxima_core.publication_outbox`, one row per published Fact `t`.

| Column | Note |
|---|---|
| `id` | uuid PK = the Fact's memory `t`; FK to `memory` `ON DELETE CASCADE` |
| `owner_ref` | the Fact's owner, same column shape as `memory` (see [07 §Owner Columns](07-storage.md#owner-columns)) |
| `event_type`, `source`, `recorded_at` | routing keys duplicated out of the envelope; `recorded_at` is the UUIDv7 time of `id` |
| `envelope` | `bytea`, the verbatim CloudEvent |
| `envelope_digest` | blake3 integrity witness |
| `state` | SQL enum `publication_state` |
| `claim_token`, `lease_expires_at`, `attempts` | claim bookkeeping |
| `published_at`, `published_seq` | JetStream stream sequence from the PubAck |
| `captured_at` | insert time |

| From | Event | To |
|---|---|---|
| `pending` | claim (lease) | `claimed` |
| `claimed` | PubAck recorded | `published` |
| `claimed` | release | `pending` |
| `claimed` | lease expiry | `pending` |
| `published` | — | terminal |

**No transition deletes a row.** Expiry permits retry, never loss. Discovery is
`state <> 'published'` ordered by `(recorded_at, id)` under
`FOR UPDATE SKIP LOCKED` — a set scan, not a high-water cursor, so a delayed
commit cannot disappear behind an advanced position.

Surface declaration (see [13 §What an erase destroys](13-compliance.md#what-an-erase-destroys)):

| Field | Value | Why |
|---|---|---|
| `key` | `KeyShape::MemoryT { column: "id" }` | the record IS the Fact |
| `transfer` | `TransferRule::RetainAtSource` | a captured event records what an owner published, not what the memory currently is |
| `erase` | `EraseRule::ByKey` | reached through the Fact's selection set; the FK cascade is the second proof |
| `export` | `ExportRule::Excluded` | delivery state is publisher state, not owner content |
| `forget` | `ForgetRule::Keep` | forget cools a memory; it does not un-publish |
| `counter` | `CounterRule::Counted` | destroyed records appear on the erase receipt |

## Host-Only Port

```rust
trait PublicationOutbox {
    async fn claim(&self, publisher: PublisherId, limit: NonZeroU32, lease: Duration)
        -> Result<Vec<ClaimedPublication>>;
    async fn mark_published(&self, id: MemoryId, token: ClaimToken, receipt: BrokerReceipt)
        -> Result<AckOutcome>;               // StaleClaim | Published | AlreadyPublished
    async fn release(&self, id: MemoryId, token: ClaimToken) -> Result<ReleaseOutcome>;
    async fn pending_count(&self) -> Result<u64>;
}
```

Broker-neutral, and **host-only**: not reachable from a flavor, a `ToolCtx`, or
a `WriteSession`. There is no delete method. A stale claim token cannot
acknowledge or discard another publisher's record.

## Two Acknowledgements

| Boundary | Completion condition | What it does NOT mean |
|---|---|---|
| outbox → JetStream | PubAck under the configured durability profile permits `mark_published` | that a consumer saw it |
| JetStream → consumer | ACK follows committed durable intake **or** durable retention of a rejection | that a business operation started or finished |

Lost PubAck, timeout, or a crash before the published marker commits ⇒
republication of the **same bytes under the same `id`**. Duplicate delivery is
permitted; consumers deduplicate on the durable event identity beyond the
broker's dedup window. The producer never waits for a subscriber's validation
or response.

## Delivery Profile `local-file`

The only shipped profile; any other `PROXIMA_NATS_PROFILE` value is an explicit
boot error.

| Setting | Value |
|---|---|
| stream | `PROXIMA_FACTS`, subjects `proxima.fact.>` |
| subject | `proxima.fact.<owner_kind>.<owner_uuid>.<type_token>` (`type_token` = event type, chars outside `[A-Za-z0-9_-]` → `_`) |
| storage | File, `replicas: 1` |
| retention | Limits, `discard: New` — a full stream returns a PubAck error, so the record stays `pending` (backpressure, not eviction) |
| `max_age` | none: unacknowledged work never silently expires |
| `max_bytes` / `max_msgs` | configurable; defaults 1 GiB / unlimited |
| duplicate window | 2 min, keyed on header `Nats-Msg-Id` = the CloudEvent `id` |
| `max_message_size` | `PROXIMA_OUTBOX_MAX_PAYLOAD_BYTES` + envelope headroom |
| consumer | durable **pull**, `ack_policy: Explicit`, `ack_wait: 30s`, `max_deliver: unlimited`, `deliver_policy: All`, `max_ack_pending: 1000` |

Owner routing lives in the subject, so NATS account permissions restrict a
consumer by subject prefix. File storage plus PubAck is durability against
process death, **not** a claim of surviving arbitrary disk loss.

Pins: nats-server **2.14.6**, `async-nats` **0.50.0**. The optional adapter is
`crates/outbox-nats/`; the NATS dependency never enters `proxima-core`.

## Rollback

| Action | Effect |
|---|---|
| unset `PROXIMA_NATS_URL` | publisher off; **capture continues**; pending records are retained for repair and replay under their original identities |
| broker outage | capture continues while local capacity lasts; delivery resumes from `pending` |
| disable capture | not available while a listenable type is registered — refuse the write path instead |

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
`duplicates_share_identity`. All-or-none transactionality and delivery
liveness are **explicitly excluded** there, with tests as their carrier — rows
OB-1..OB-8 in [lean/COVERAGE.md](lean/COVERAGE.md).

Operating recipe: [how-to/fact-outbox.md](how-to/fact-outbox.md). Env rows:
[10 §Framework facade (host-app boot)](10-configuration.md#framework-facade-host-app-boot).
