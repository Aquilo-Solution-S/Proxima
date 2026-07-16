# 14 — Protocol Surface

> **Status:** current + deferred sections. Deferred rows are design intent, not implementation claims.

Transport-agnostic engine contract. 14 owns verb semantics,
change-log semantics, consistency, auth, and error shape. Transport
framing and UI are consumer concerns (see [§Out of scope](#out-of-scope)).

## Scope

A deployment is one composite binary: core plus linked flavor crates
(see [08](08-core-and-flavors.md)). Frontends, wire clients, and
trusted sources talk to the binary through graph verbs and operational RPCs.

| Surface | Status | Contract |
|---|---|---|
| graph verbs | current | cognitive graph reads/writes/events |
| operational/config RPCs | current | deployment profiles and auth surfaces |
| compliance admin operations | current Host API; transport RPCs deferred | abandonment-only erase primitives in [13](13-compliance.md); non-erase admin primitives remain design intent |
| operators / wake / tools / LLM calls | internal | clients read committed graph effects from the `change_event` pull log |

No runtime schema/source/tool/flavor registration surface exists.

Rust embedding has two public tiers:

| Tier | Import | Contract |
|---|---|---|
| Host API | `use proxima::{Proxima, RuntimeBuilder, Engine};` | compose/run a binary, call graph verbs, hold server-resolved `AuthzContext` |
| Flavor SDK | `use proxima::flavor::{FlavorBundle, FlavorRegistry, FactPayload, pg_sidecar};` | register schemas/relations/tools/sidecars at build time; no raw `PgPool`, no `proxima_core.*` SQL |

Transport adapters (MCP/HTTP) project the Host API. Flavor crates target
the Flavor SDK and typed services only.

## Core Memory MCP Surface

Agent long-term memory is core substrate. MCP tools are thin callers of
Engine verbs; MCP resources expose read-only graph and registry views.
Storage stays behind the Engine. The substrate surface is 10 tools + 10
resources; `proxima://tools` returns the live tool catalog only, and
resources are discovered through MCP `resources/list` and
`resources/templates/list`.

Owner remains the storage and graph isolation primitive. Access is server-resolved `OwnerRoles` over concrete `OwnerRef`s; Core enforces those roles at verb/tool entry and never adds org/share-set semantics. Edge rows are source-owned; registered relation descriptors decide whether a foreign target is admitted and whether target read/write is required. Read projection is source-local for the edge row with independent target `Visible` / `Redacted` / `Unavailable` rendering.

Canonical substrate tools:

| Tool | Contract |
|---|---|
| `core_remember` | write agent-authored Fact |
| `core_record_utterance` | write utterance Fact |
| `core_derive` | write agent-authored Abstraction |
| `core_link` | write registered relation edge |
| `core_search_memories` | search memories; may include neighbor edges, per-result tags, lexical-degradation status, and selected memory-space labels. Optional `min_score` relevance floor and hybrid `semantic_weight` (default 0.6 semantic / 0.4 lexical). Pages of at most 50: `has_more` plus an opaque `next_cursor` that is passed back as `cursor` with the identical query shape (the cursor is fingerprint-bound and fails closed on any other query, order, filter, or space set) |
| `core_memory_spaces` | list server-issued memory-space keys with labels and coarse unrestricted-access flags |
| `core_membership` | group roster dispatcher: `add_member`, `remove_member`, `list_members`; host/controller scoped |
| `core_publish` | owner-transfer dispatcher: `publish_to_world`; irreversible transfer to `OwnerRef::World`, not membership or ACL |
| `core_goal` | goal action dispatcher: `set`, `transition`, `modify`, `mark_achieved`, `decompose` |
| `core_fact` | Fact action dispatcher: `citation_of_fact`, `citation_of_entity_head`, `facts_citing_object` |

Compatibility: `core_membership:publish_to_world` is removed. Clients and
tool-scope palettes must use `core_publish:publish_to_world`; no compatibility
alias is retained.

Graph search is unified into `core_search_memories`; there is no
separate graph-search tool.

Canonical substrate resources:

| Resource | Contract |
|---|---|
| `proxima://schemas{?kind}` | registered payload schemas |
| `proxima://edge-types` | registered relation descriptors |
| `proxima://tools` | live tool catalog |
| `proxima://graph` | graph snapshot and status fields, including `fact_retention_seconds` |
| `proxima://memory/{id}{?expand_neighbors}` | hydrate memory by id; optional neighbor edges |
| `proxima://memory/{id}/lineage{?direction,depth,limit}` | traverse provenance / supersession lineage |
| `proxima://change-events{?since,limit}` | forward `change_event` poll, ascending, with `next_since` and `has_more` |
| `proxima://goals{?state,limit,cursor}` | owner-scoped goal listing: optional state filter (Active/Paused/Achieved/Abandoned), keyset `cursor`/`next_cursor` + `has_more`, wake-config read-back per goal |
| `proxima://goal/{id}` | single-goal read by `G:<uuid>` reference, including stored wake configuration |
| `proxima://wake-candidates{?fact,limit}` | armed Active Goal heads admitted for wake planning by one readable trigger Fact; read model only, no executor |

`proxima://how-to` is an instructional MCP resource outside the 10-resource
protocol count.

## The verbs

Semantic graph/client contract. Five current verbs. The live-push
`Subscribe` stream has been **retired and removed from code** — there is
no outbox, `LISTEN`/`NOTIFY`, or server-push transport. `change_event`
is now a durable, owner-scoped, seq-ordered **pull log** (see
[§Change Log](#change-log--pull-only)).

> **Forward poll.** `change_event` is read in both directions. Backward,
> bounded reads use the `ChangeHistory` engine verb. The forward seq-cursor
> poll a harness wake loop needs ships as the
> **`proxima://change-events{?since,limit}`** MCP resource — change records with
> `seq > since`, ascending, plus a `next_since` cursor and a `has_more`
> hint — a thin owner-scoped wrapper over `Engine::list_change_events`, which
> routes to `ChangeEventPort::list_change_events_after`. The admission half of
> the loop ships as **`proxima://wake-candidates{?fact,limit}`** over
> `Engine::list_goal_wake_candidates` /
> `GoalWakeCandidatePort::list_goal_wake_candidates`: given one appended Fact,
> the armed Active Goal heads it wakes, narrowed to the caller's read/write
> owner sets and effective tool scope. Neither is one of the five
> transport-level graph verbs; storage still stays behind the Engine/port
> boundary.

| Verb | Direction | Idempotency | Scope | Current status |
|---|---|---|---|---|
| `Query` | client -> engine, sync | yes | Owner | current |
| `ChangeHistory` | client -> engine, sync | yes | Owner | current |
| `GoalWrite` | client -> engine, sync | `request_id` | Owner | current |
| `FactIngest` | source/app -> engine, sync | optional public `receipt_id` / storage `receipt_id` | Owner | current |
| `Schema` | client -> engine, sync | yes | binary | current |
| `Subscribe` | (removed) | n/a | Owner | **retired** — `change_event` is a pull log |

These five current verbs are the cognitive graph surface. Operational/config
RPCs below are not graph verbs. Receipt-backed FactIngest idempotency uses
`fact_receipts.receipt_id` / `memories.receipt_id`; receiptless Fact writes
have no receipt id and admit a fresh Fact per successful call.

## Owner-scoping — the primary axis

Every graph write/ingest names one concrete `Owner` (see [01](01-event-source.md#owner--scoping-primitive)); graph reads and event polls are scoped to the server-resolved authorized Owner set. Dispatch resolves caller access before storage and never accepts caller-supplied roles.

| Shape | Contract |
|---|---|
| single-tenant | one accessible Owner |
| multi-tenant reads/events | one call may read the authorized Owner set, with redaction/projection applied per row |
| owner-selected writes | write/ingest tools resolve one selected Owner before storage |
| multi-space memory search | MCP fanout over selected authorized memory spaces, merged with per-result space labels |
| cross-owner graph data | exposed only through rows readable by source Owner; target rendering is independent |

Public MCP inputs may name server-issued space keys from `core_memory_spaces`; the keys are selectors only, not authority. Each use is resolved through `AuthzContext` into `OwnerRoles` and Owner visibility before calling storage/graph paths.

## Graph Verbs

### Query

Snapshot read of memories, goals, and edges scoped to the server-resolved authorized Owner set (`S_read`).

| Axis | Current contract |
|---|---|
| owner set | `AuthzContext` resolves `S_read`; request principal is not an authority vector |
| entity kind | optional |
| schema id | optional |
| supersession | heads-only or include superseded |
| tombstones | present-only or include tombstoned |
| Goal/Perspective selectors | explicit ids; selectors never authorize by themselves |
| pagination | single-kind keyset: `limit` + `page.after` over `(created_at, id) DESC` |
| payloads | optional typed payload projections; identity hydration by memory/goal/edge ids |
| stateful Facts | heads by registered natural key; tombstone heads suppress prior present rows |
| flavor-typed filters | design intent; advertised/validated only when implemented by a linked flavor |
| edge traversal / time range | deferred |

Cursor streams:

| `entity_kind` | cursor |
|---|---|
| `Fact` / `Abstraction` / `Perspective` | `QueryCursor::Memory { created_at, memory_id }` |
| `Goal` | `QueryCursor::Goal { created_at, goal_id }` |
| absent | rejected when `page.after` is present; `next_cursor = None` |

Cursor mismatch returns `InvalidArgument`. Storage fetches `limit + 1`,
returns at most `limit`, and emits `next_cursor` from the last returned
row only when another row exists. Edge hydration is bounded to the
returned node window.

Returns rows plus `seq_high_water`. Clients persist the watermark as the
seq cursor for a subsequent forward poll of `change_event`.

### ChangeHistory

Bounded read of `change_event`, newest-first, scoped to the server-resolved authorized Owner set (`S_read`).

| Field | Contract |
|---|---|
| owner set | `AuthzContext` resolves `S_read`; request principal is not an authority vector |
| `limit` | required; `1..=1000`, server-clamped |
| `before` | optional UUIDv7 cursor; returns `seq < before` |
| filters | authorized-owner set only in current implementation |
| return order | newest-first |
| `seq_high_water` | latest visible event seq at read time |

No `after` cursor — this verb is backward-only. Forward replay (events
with `seq > cursor`) is served by the `proxima://change-events{?since,limit}`
MCP resource over `Engine::list_change_events` and
`ChangeEventPort::list_change_events_after`; it is intentionally neither
added as an `after` cursor here nor promoted into the five graph verbs.

### GoalWrite

Owner-scoped append or supersession of a Goal row (see
[06 §Goal-Write API](06-goals-and-self.md#goal-write-api)).

| Rule | Contract |
|---|---|
| schema | validates registered `GoalPayload` schema |
| request id | `(Owner, request_id)` idempotency key |
| replay | same body and side-effect inputs return prior `GoalId`; drift returns conflict |
| supersession | prior goal must be same Owner and current head |
| log | success commits the Goal row and its `change_event` row |

Embedded Rust hosts use the typed helper instead of table SQL:

```rust
let outcome = engine
    .create_goal(
        &authz,
        GoalCreateRequest::product(
            owner,
            target_perspective_id,
            IdempotencyKey::new("product:onboarding:initial-goal:1")?,
            "Practice goal",
            "Practice every weekday.",
            ProductGoalPayload { external_goal_id },
        ),
    )
    .await?;
```

Current create semantics assign every new Active Goal to an explicit
Perspective by writing `Goal --core/inspires--> Perspective`.
Unassigned owner-only Goal rows are not part of the public helper.

### FactIngest

Fact write path for external sources and in-app sources. Receipt-backed writes
carry source receipt metadata; receiptless writes carry no source-batch
witness.

| Rule | Contract |
|---|---|
| receipt id | optional server-computed content hash of source, Owner, payload; public response field is `receipt_id` and omitted/null when receiptless |
| replay | duplicate receipt id returns prior outcome / no new Fact; receiptless writes do not replay |
| commit | returns after Fact, optional receipt row, optional sidecar/citation, and structural edges are committed |
| log | success commits the corresponding `change_event` rows |
| auth | user or source credential, resolved through server-built `AuthzContext` |

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
    read events where seq > last_seq, ascending   # ChangeEventPort::list_change_events_after
    process in order; persist the new high-water seq
```

This is the harness wake path. It is exposed as the
`proxima://change-events{?since,limit}` MCP resource — a thin owner-scoped
wrapper over `Engine::list_change_events`, backed by
`ChangeEventPort::list_change_events_after`, that returns events
ascending plus a `next_since` cursor and a `has_more` hint.
`ChangeHistory` is the backward-only, engine-exposed counterpart.

Cold-start stitching — seed from a snapshot, then poll forward:

```
1. snapshot, hwm = Query(owner, filters)
2. apply snapshot
3. read change_event where seq > hwm            # forward poll
4. hydrate identities with Query
```

Events committed after `hwm` are read by the poll; events at or before
`hwm` are already represented in the snapshot. A history-rail variant
seeds recent context with `ChangeHistory(owner, limit = N)` before the
first forward poll.

## Consistency — Strong Write -> Log

Graph writes commit entity/edge rows and corresponding
`change_event` rows in one storage transaction.

| Property | Contract |
|---|---|
| atomic write/event | no committed graph row without its `change_event` row |
| write return | `GoalWrite` / `FactIngest` success means the graph change is committed and durably readable |
| read | a committed event is visible to any subsequent forward poll / `ChangeHistory` read |
| replay | `ChangeHistory` and the forward poll read the same `change_event` log |
| broker | none; `change_event` is a pull log — no tailing broker or push delivery |

`change_event` is the durable pull log (see
[07](07-storage.md#core-tables--abstract)). Compliance audit is
separate (see [13](13-compliance.md#audit-log)).

## Operational / Config RPCs

Current RPCs outside the five graph verbs:

| Family | RPCs | Contract |
|---|---|---|
| deployment profiles | environment-derived `ToolScope` | narrows advertised/callable MCP tools and resources; never grants graph authority |

Goal wake configuration is part of `GoalWrite`/Goal storage, not a separate
runtime config entity. Self is queried from authorized active Goal heads and
Perspective rows; no materialized Self row authorizes access.

## Compliance Admin Surface

Compliance primitives are defined in [13](13-compliance.md). Current
Rust Host API exposes abandonment-only erase entry points; transport/admin
RPCs beyond those concrete methods stay deferred.

| Primitive | Protocol status |
|---|---|
| `delete_owner` | current Host API: `erase_abandoned_group_owner`, `erase_dropped_personal_owner`, `erase_world_owner` refusal |
| `delete_source_scope` | current Host API: `erase_abandoned_group_source_scope`, `erase_dropped_personal_source_scope` |
| `pause_owner` / `resume_owner` | design intent |
| `export_owner` | design intent |
| tool-recipient export | deferred |
| legal-consequence blocking | deferred |

Compliance operations are admin/controller actions, not cognitive
graph writes or MCP memory-profile actions.

## Auth Model

Auth is pluggable per binary, not per flavor. Transport extracts
credentials; engine dispatch receives resolved principal and Owner
access.

| Credential class | Use |
|---|---|
| `None` | local embedded / NoAuth deployments |
| bearer token | user principal |
| source credential | external source identity |

| Credential subject | Access |
|---|---|
| user (`OwnerRef::Personal`) | `Query`, `ChangeHistory`, `GoalWrite`; in-app `FactIngest` when acting as a source |
| source | `FactIngest` for the registered source |
| admin/controller | operational/config RPCs and future compliance admin operations |

Per-call dispatch enforces `call.owner` inside the resolved Owner set.
Signup, MFA, billing, tenancy lifecycle, group naming, invites, archive/delete,
and product audit timelines live in front of the engine. `core_membership`
mutates only the explicit group roster when the host exposes that
controller-scoped tool. `core_publish` transfers a memory or goal's owner to
`OwnerRef::World` — an irreversible owner transfer, not an ACL flag or share
row; World is universally readable and never a write owner again afterward.
It requires write/manage authority (`Relation::Admin`) on the entity's
current owner.

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

## Source Registration

Source identities register at startup from linked flavor crates, same
build-time posture as schemas, relations, prompts, and tools. Runtime
source registration is deferred and requires a new ADR.

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

- Owner and source-ingest invariants: [01](01-event-source.md).
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
- `changehistory`
- `goalwrite`
- `factingest`
- `schema`
- `change-log--pull-only`
- `consistency--strong-write---log`
- `operational--config-rpcs`
- `compliance-admin-surface`
- `auth-model`
- `error-envelope`
- `eventsource-registration`
- `backpressure`
- `out-of-scope`
- `cross-references`
