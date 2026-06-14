# 14 — Protocol Surface

Transport-agnostic engine contract. 14 owns verb semantics,
change-stream semantics, consistency, auth, and error shape. 09 owns
client transport, Shell bindings, stream framing, and UI state.

## Scope

A deployment is one composite binary: core plus linked flavor crates
(see [08](08-core-and-flavors.md)). Frontends, wire clients, and
trusted EventSources talk to the binary through graph verbs and
operational RPCs.

| Surface | Status | Contract |
|---|---|---|
| graph verbs | current | cognitive graph reads/writes/events |
| operational/config RPCs | current | runtime personality and inference config |
| compliance admin operations | design intent | compliance primitives in [13](13-compliance.md), admin surface deferred |
| operators / wake / tools / LLM calls | internal | clients observe committed graph effects as `ChangeEvent`s |

No runtime schema/source/tool/flavor registration surface exists.

## The six verbs

Semantic graph/client contract:

| Verb | Direction | Idempotency | Scope | Current status |
|---|---|---|---|---|
| `Query` | client -> engine, sync | yes | Owner | current |
| `Subscribe` | engine -> client, stream | n/a | Owner | current |
| `EventHistory` | client -> engine, sync | yes | Owner | current |
| `GoalWrite` | client -> engine, sync | `request_id` | Owner | current |
| `EventIngest` | source -> engine, sync | `event_id` | Owner | current |
| `Schema` | client -> engine, sync | yes | binary | current |

These six verbs are the cognitive graph surface. Operational/config
RPCs below are not graph verbs.

## Owner-scoping — the primary axis

Every graph read, write, ingest, and subscribe names exactly one
`Owner` (see [01](01-event-source.md#owner--scoping-primitive)).
Dispatch verifies caller access to that Owner.

| Shape | Contract |
|---|---|
| single-tenant | one accessible Owner |
| multi-tenant | same calls, different Owner per call |
| multi-owner streams | one `Subscribe` stream per Owner |
| cross-owner graph data | not exposed by protocol |

## Graph Verbs

### Query

Owner-scoped snapshot read of memories, goals, and edges.

| Axis | Current contract |
|---|---|
| owner | required |
| entity kind | optional |
| schema id | optional |
| supersession | heads-only or include superseded |
| tombstones | present-only or include tombstoned |
| personality roots | include/exclude inactive root Perspectives |
| pagination | `limit`; cursor pagination deferred |
| payloads | optional payload bytes; identity hydration by memory/goal/edge ids |
| stateful Facts | heads by registered natural key; tombstone heads suppress prior present rows |
| flavor-typed filters | design intent; advertised/validated only when implemented by a linked flavor |
| edge traversal / time range | deferred |

Returns rows plus `seq_high_water`. Clients use the watermark as the
`Subscribe(since)` cursor for cold-start stitching.

### Subscribe

Owner-scoped server-push stream of identity-only `ChangeEvent`s.
Current core request shape is `owner + since`; gRPC may carry a
`ReadFilter`, but current conversion does not enforce it.

| Field | Contract |
|---|---|
| `seq` | server-generated UUIDv7 cursor |
| `owner` | event Owner |
| `EntityAppend` | Fact / Abstraction / Perspective / Goal identity, schema, optional supersedes |
| `EdgeAppend` | edge identity, relation, source, target |
| authoring metadata | optional personality instance and wake-chain depth |

Rules:

- No payload bytes on the stream. Hydration is a follow-up `Query`.
- No `EntityRemoved` or `EntityMutated`; append-only deltas only.
- `supersedes` is valid for A/P/Goal, never for Facts.
- Stateful Fact projections stream as repeated `EntityAppend`s under
  the same schema/natural key; readers fold with `Query`.
- Subscribe-side filtering beyond owner/since is deferred until the
  engine enforces the same axes advertised to clients.

### EventHistory

Owner-scoped bounded read of `change_event`, newest-first.

| Field | Contract |
|---|---|
| `owner` | required |
| `limit` | required; `1..=1000`, server-clamped |
| `before` | optional UUIDv7 cursor; returns `seq < before` |
| filters | owner-only in current implementation |
| return order | newest-first |
| `seq_high_water` | latest owner event seq at read time |

No `after` cursor. Live resume is `Subscribe`.

### GoalWrite

Owner-scoped append or supersession of a Goal row (see
[06 §Goal-Write API](06-goals-and-self.md#goal-write-api)).

| Rule | Contract |
|---|---|
| schema | validates registered `GoalPayload` schema |
| request id | `(Owner, request_id)` idempotency key |
| replay | same body returns prior `GoalId`; different body returns conflict |
| supersession | prior goal must be same Owner and current head |
| stream | success commits a Goal `EntityAppend` in the outbox |

### EventIngest

EventSource path from [01](01-event-source.md), exposed for external
sources and in-app sources.

| Rule | Contract |
|---|---|
| event id | server-computed content hash of source, Owner, payload |
| replay | duplicate event id returns prior outcome / no new Fact |
| commit | returns after Fact and structural edges are committed |
| stream | success commits corresponding `ChangeEvent`s |
| auth | user or source credential, depending on source type |

### Schema

Binary-scoped registry introspection.

| Includes | Contract |
|---|---|
| payload schemas | registered Fact / Abstraction / Perspective / Goal / cited-object / citation-mapping schemas |
| relations | registered `RelationDescriptor`s |
| filters | only keys actually registered by the running binary |

Schema is structural metadata. Deployments may expose it without
auth or gate it like any other call.

## Cursor & Resume

`seq` is a server-generated UUIDv7 cursor. Servers return events with
`seq > since`.

```
client persists last_seq it processed
on reconnect:
    Subscribe(owner, since = last_seq)
    client dedupes by seq
```

No ack protocol. No per-client server cursor state. Delivery is
at-least-once with client dedup.

## Cold-Start Stitching — Query -> Subscribe

Snapshot-only seed:

```
1. snapshot, hwm = Query(owner, filters)
2. apply snapshot
3. Subscribe(owner, since = hwm)
4. hydrate streamed identities with Query
```

Any event committed after `hwm` arrives via `Subscribe`; events at or
before `hwm` are represented in the snapshot.

History rail seed:

```
1. (snapshot, hwm_q), (events, hwm_e) = parallel(
       Query(owner, filters),
       EventHistory(owner, limit = N, before = None))
2. apply snapshot
3. seed event log with events
4. Subscribe(owner, since = max(hwm_q, hwm_e))
5. dedupe live events by seq
```

## Consistency — Strong Write -> Stream

Graph writes commit entity/edge rows and corresponding
`change_event` rows in one storage transaction.

| Property | Contract |
|---|---|
| atomic write/event | no committed graph row without outbox row |
| write return | `GoalWrite` / `EventIngest` success means event is committed |
| delivery | at-least-once; clients dedupe by `seq` |
| replay | `EventHistory` reads the same protocol outbox |
| broker | none required for v1; Postgres tailing is sufficient |

`change_event` is the protocol outbox and replay log (see
[07](07-storage.md#core-tables--abstract)). Compliance audit is
separate (see [13](13-compliance.md#audit-log)).

## Operational / Config RPCs

Current RPCs outside the six graph verbs:

| Family | RPCs | Contract |
|---|---|---|
| personality lifecycle | `InstantiatePersonality`, `SetWakeEntries`, `ListPersonalityInstances`, `TombstonePersonality` | mutate runtime personality config and wake entries; not graph verbs |

`InstantiatePersonality` writes the root self-Perspective and emits
one Perspective `EntityAppend`. Other personality config mutations do
not emit cognitive `ChangeEvent`s; personality list UIs refresh with
the list RPC, not by folding `Subscribe`.

### Personality Lifecycle

| Operation | Contract |
|---|---|
| instantiate | creates a runtime personality instance and root self-Perspective |
| set wake entries | replaces wake-entry config for one personality instance |
| list | returns active instances by default; tombstoned rows opt-in |
| tombstone | removes instance from default listings and dispatcher selection |

`TombstonePersonality` is idempotent for an existing instance.
Operations against missing or tombstoned rows return `NotFound` where
the UI must distinguish stale state from backend failure.

## Compliance Admin Surface

Compliance primitives are defined in [13](13-compliance.md). Their
admin protocol surface is deferred unless a concrete RPC exists.

| Primitive | Protocol status |
|---|---|
| `delete_owner` | design intent |
| `delete_source_scope` | design intent |
| `pause_owner` / `resume_owner` | design intent |
| `export_owner` | design intent |
| tool-recipient export | deferred |
| legal-consequence blocking | deferred |

Compliance operations are admin/controller actions, not cognitive
graph writes.

## Auth Model

Auth is pluggable per binary, not per flavor. Transport extracts
credentials; engine dispatch receives resolved principal and Owner
access.

| Credential class | Use |
|---|---|
| `None` | local embedded / NoAuth deployments |
| bearer token | user principal |
| source credential | external EventSource |

| Principal | Access |
|---|---|
| user | `Query`, `Subscribe`, `EventHistory`, `GoalWrite`; in-app `EventIngest` when acting as a source |
| source | `EventIngest` for the registered source |
| admin/controller | operational/config RPCs and future compliance admin operations |

Per-call dispatch enforces `call.owner` inside the resolved Owner set.
Signup, MFA, billing, group membership, and tenancy services live in
front of the engine.

## Error Envelope

Current core envelope:

| Field | Contract |
|---|---|
| `code` | typed `ErrorCode` |
| `message` | safe client-facing text |
| `request_id` | optional echo for write/idempotency paths |

Transport-specific extensions may carry structured `details`; gRPC
uses typed trailer details in the proto surface.

Current code families include auth/scope, schema/input, idempotency,
not-found/state, tool/inference config, and internal errors. Docs must
not require a code variant until it exists in `crates/core/src/error.rs`
or the owning wire surface.

On `Subscribe`, errors terminate the stream through the transport
error path.

## EventSource Registration

EventSources register at startup from linked flavor crates, same
build-time posture as schemas, relations, prompts, and tools. Runtime
EventSource registration is deferred and requires a new ADR.

## Backpressure

Current contract:

| Concern | Rule |
|---|---|
| sync reads | clients request smaller `limit`s |
| streams | reconnect with `since`; dedupe by `seq` |
| per-stream server buffer policy | deferred |
| broker/offline queue | deferred |

## Out of Scope

- Client transport/framing/serialization and any UI graph store: the
  consumer's concern, built over this surface — Proxima ships no frontend.
- Runtime registration of schemas/tools/sources/flavors.
- Local-first durable replica and offline write queue.
- Compliance operation implementation details: [13](13-compliance.md).

## Cross-References

- Owner and EventSource invariants: [01](01-event-source.md).
- Entity / edge model: [02](02-memory.md).
- Payload/schema registry: [03](03-schema-registry.md).
- Operators: [04](04-consolidation.md).
- Actions and human approval: [05](05-actions.md).
- Goal entity and GoalWrite: [06](06-goals-and-self.md).
- Storage and `change_event`: [07](07-storage.md).
- Flavor composition: [08](08-core-and-flavors.md).
- Runtime configuration: [10](10-configuration.md).
- Compliance primitives: [13](13-compliance.md).

## Anchors

- `scope`
- `the-six-verbs`
- `owner-scoping--the-primary-axis`
- `graph-verbs`
- `query`
- `subscribe`
- `eventhistory`
- `goalwrite`
- `eventingest`
- `schema`
- `cursor--resume`
- `cold-start-stitching--query---subscribe`
- `consistency--strong-write---stream`
- `operational--config-rpcs`
- `personality-lifecycle`
- `compliance-admin-surface`
- `auth-model`
- `error-envelope`
- `eventsource-registration`
- `backpressure`
- `out-of-scope`
- `cross-references`
