# 14 — Protocol Surface

Transport-agnostic engine contract. 14 owns verb semantics,
change-log semantics, consistency, auth, and error shape. Transport
framing and UI are consumer concerns (see [§Out of scope](#out-of-scope)).

## Scope

A deployment is one composite binary: core plus linked flavor crates
(see [08](08-core-and-flavors.md)). Frontends, wire clients, and
trusted EventSources talk to the binary through graph verbs and
operational RPCs.

| Surface | Status | Contract |
|---|---|---|
| graph verbs | current | cognitive graph reads/writes/events |
| operational/config RPCs | current | runtime personality config |
| compliance admin operations | design intent | compliance primitives in [13](13-compliance.md), admin surface deferred |
| operators / wake / tools / LLM calls | internal | clients read committed graph effects from the `change_event` pull log |

No runtime schema/source/tool/flavor registration surface exists.

## Core Memory MCP Surface

Agent long-term memory is core substrate. MCP tools are thin callers of
Engine verbs; MCP resources expose read-only graph and registry views.
Storage stays behind the Engine. The substrate surface is 9 tools + 7
resources; `proxima://tools` returns the live tool catalog only, and
resources are discovered through MCP `resources/list` and
`resources/templates/list`.

Canonical substrate tools:

| Tool | Contract |
|---|---|
| `core_remember` | write agent-authored Fact |
| `core_record_utterance` | write utterance Fact |
| `core_derive` | write agent-authored Abstraction |
| `core_link` | write registered relation edge |
| `core_search_memories` | search memories; may include neighbor edges, per-result tags, and lexical-degradation status |
| `core_goal` | goal action dispatcher: `set`, `transition`, `modify`, `mark_achieved`, `decompose` |
| `core_wake` | wake-config action dispatcher: `add`, `update`, `remove`, `set`, `list` |
| `core_personality` | personality action dispatcher: `instantiate`, `tombstone`, `set_read_scope`, `list`, `get`, `list_read_scope` |
| `core_fact` | Fact action dispatcher: `citation_of_fact`, `citation_of_entity_head`, `facts_citing_object`, `tombstone` |

Graph search is unified into `core_search_memories`; there is no
separate graph-search tool.

Canonical substrate resources:

| Resource | Contract |
|---|---|
| `proxima://schemas{?kind}` | registered payload schemas |
| `proxima://edge-types` | registered relation descriptors |
| `proxima://tools` | live tool catalog |
| `proxima://graph{?include_tombstoned}` | graph snapshot and status fields, including `fact_retention_seconds` |
| `proxima://memory/{id}{?expand_neighbors}` | hydrate memory by id; optional neighbor edges |
| `proxima://memory/{id}/lineage{?direction,depth,limit}` | traverse provenance / supersession lineage |
| `proxima://events{?since,limit}` | forward `change_event` poll, ascending, with `next_since` and `has_more` |

`proxima://how-to` is an instructional MCP resource outside the 7-resource
protocol count.

## The verbs

Semantic graph/client contract. Five current verbs. The live-push
`Subscribe` stream has been **retired and removed from code** — there is
no outbox, `LISTEN`/`NOTIFY`, or server-push transport. `change_event`
is now a durable, owner-scoped, seq-ordered **pull log** (see
[§Change Log](#change-log--pull-only)).

> **Forward poll.** `change_event` is read in both directions. Backward,
> bounded reads use the `EventHistory` engine verb. The forward seq-cursor
> poll a harness wake loop needs ships as the
> **`proxima://events{?since,limit}`** MCP resource — events with
> `seq > since`, ascending, plus a `next_since` cursor and a `has_more`
> hint — a thin owner-scoped wrapper over
> `Storage::list_change_events_after`. There is intentionally no gRPC/engine
> forward verb: the resource reads storage directly, the same posture as
> `core_search_memories` and the
> `proxima://memory/{id}/lineage{?direction,depth,limit}` resource.

| Verb | Direction | Idempotency | Scope | Current status |
|---|---|---|---|---|
| `Query` | client -> engine, sync | yes | Owner | current |
| `EventHistory` | client -> engine, sync | yes | Owner | current |
| `GoalWrite` | client -> engine, sync | `request_id` | Owner | current |
| `EventIngest` | source -> engine, sync | `event_id` | Owner | current |
| `Schema` | client -> engine, sync | yes | binary | current |
| `Subscribe` | (removed) | n/a | Owner | **retired** — `change_event` is a pull log |

These five current verbs are the cognitive graph surface. Operational/config
RPCs below are not graph verbs.

## Owner-scoping — the primary axis

Every graph read, write, ingest, and event poll names exactly one
`Owner` (see [01](01-event-source.md#owner--scoping-primitive)).
Dispatch verifies caller access to that Owner.

| Shape | Contract |
|---|---|
| single-tenant | one accessible Owner |
| multi-tenant | same calls, different Owner per call |
| multi-owner event reads | one `EventHistory` / poll call per Owner |
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
| payloads | optional typed payload projections; identity hydration by memory/goal/edge ids |
| stateful Facts | heads by registered natural key; tombstone heads suppress prior present rows |
| flavor-typed filters | design intent; advertised/validated only when implemented by a linked flavor |
| edge traversal / time range | deferred |

Returns rows plus `seq_high_water`. Clients persist the watermark as
the seq cursor for a subsequent forward poll of `change_event`.

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

No `after` cursor — this verb is backward-only. Forward replay (events
with `seq > cursor`) is served by the `proxima://events{?since,limit}`
MCP resource over `Storage::list_change_events_after`; it is intentionally
neither added as an `after` cursor here nor wrapped by a gRPC/engine verb.

### GoalWrite

Owner-scoped append or supersession of a Goal row (see
[06 §Goal-Write API](06-goals-and-self.md#goal-write-api)).

| Rule | Contract |
|---|---|
| schema | validates registered `GoalPayload` schema |
| request id | `(Owner, request_id)` idempotency key |
| replay | same body returns prior `GoalId`; different body returns conflict |
| supersession | prior goal must be same Owner and current head |
| log | success commits the Goal row and its `change_event` row |

### EventIngest

EventSource path from [01](01-event-source.md), exposed for external
sources and in-app sources.

| Rule | Contract |
|---|---|
| event id | server-computed content hash of source, Owner, payload |
| replay | duplicate event id returns prior outcome / no new Fact |
| commit | returns after Fact and structural edges are committed |
| log | success commits the corresponding `change_event` rows |
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

## Change Log — Pull-Only

`change_event` is a durable, append-only, owner-scoped log. Each row
carries a server-generated UUIDv7 `seq` that doubles as the cursor.
There is no push transport, no ack protocol, and no per-client server
cursor state — clients poll.

Forward poll (events after a cursor):

```
client persists last_seq it processed
on wake / reconnect:
    read events where seq > last_seq, ascending   # Storage::list_change_events_after
    process in order; persist the new high-water seq
```

This is the harness wake path. It is exposed as the
`proxima://events{?since,limit}` MCP resource — a thin owner-scoped
wrapper over the `Storage::list_change_events_after` trait method that
returns events ascending plus a `next_since` cursor and a `has_more`
hint.
`EventHistory` is the backward-only, engine-exposed counterpart.

Cold-start stitching — seed from a snapshot, then poll forward:

```
1. snapshot, hwm = Query(owner, filters)
2. apply snapshot
3. read change_event where seq > hwm            # forward poll
4. hydrate identities with Query
```

Events committed after `hwm` are read by the poll; events at or before
`hwm` are already represented in the snapshot. A history-rail variant
seeds recent context with `EventHistory(owner, limit = N)` before the
first forward poll.

## Consistency — Strong Write -> Log

Graph writes commit entity/edge rows and corresponding
`change_event` rows in one storage transaction.

| Property | Contract |
|---|---|
| atomic write/event | no committed graph row without its `change_event` row |
| write return | `GoalWrite` / `EventIngest` success means the event is committed and durably readable |
| read | a committed event is visible to any subsequent forward poll / `EventHistory` read |
| replay | `EventHistory` and the forward poll read the same `change_event` log |
| broker | none; `change_event` is a pull log — no tailing broker or push delivery |

`change_event` is the durable pull log (see
[07](07-storage.md#core-tables--abstract)). Compliance audit is
separate (see [13](13-compliance.md#audit-log)).

## Operational / Config RPCs

Current RPCs outside the five graph verbs:

| Family | RPCs | Contract |
|---|---|---|
| personality lifecycle | `core_personality` actions: `instantiate`, `tombstone`, `set_read_scope`, `list`, `get`, `list_read_scope`; `core_wake` actions: `add`, `update`, `remove`, `set`, `list` | mutate runtime personality config and wake entries; not graph verbs |

`core_personality` action `instantiate` writes the root self-Perspective and commits
one Perspective `change_event` row. Other personality config mutations
do not emit cognitive `change_event`s; personality list UIs refresh
with `core_personality` action `list`, not by polling the event log.

### Personality Lifecycle

| Operation | Contract |
|---|---|
| instantiate | creates a runtime personality instance and root self-Perspective |
| set wake entries | replaces wake-entry config for one personality instance |
| list | returns active instances by default; tombstoned rows opt-in |
| tombstone | removes instance from default listings and dispatcher selection |

`core_personality` action `tombstone` is idempotent for an existing instance.
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
| user | `Query`, `EventHistory`, `GoalWrite`; in-app `EventIngest` when acting as a source |
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

## EventSource Registration

EventSources register at startup from linked flavor crates, same
build-time posture as schemas, relations, prompts, and tools. Runtime
EventSource registration is deferred and requires a new ADR.

## Backpressure

Current contract:

| Concern | Rule |
|---|---|
| sync reads | clients request smaller `limit`s |
| event polls | read after the last `seq`; bounded `limit` per call |
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
- `the-verbs`
- `owner-scoping--the-primary-axis`
- `graph-verbs`
- `query`
- `eventhistory`
- `goalwrite`
- `eventingest`
- `schema`
- `change-log--pull-only`
- `consistency--strong-write---log`
- `operational--config-rpcs`
- `personality-lifecycle`
- `compliance-admin-surface`
- `auth-model`
- `error-envelope`
- `eventsource-registration`
- `backpressure`
- `out-of-scope`
- `cross-references`
