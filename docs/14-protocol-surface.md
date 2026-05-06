# 14 — Protocol Surface

The engine's contract to clients. Transport-agnostic — *what* the
verbs are, *what* the change-stream emits, *how* consistency, auth,
and errors behave. Wire format (gRPC / HTTP / framing / serialization)
is downstream of this and lives in 09.

## Scope

A Proxima deployment is one composite binary (core + linked flavor
crates, per 08). Clients — frontend apps (Memophant mobile, Code
web, etc.) and trusted EventSources (chat webhooks, system processes)
— talk to the binary through the surfaces below. The decider, the
operators (04), the tool registry (05), and any flavor-registered
behavior all run **inside** the binary; clients never see them on the
wire.

## The six verbs

| Verb | Direction | Idempotent | Scope |
|---|---|---|---|
| `Query` | client → engine, sync | yes | Owner |
| `Subscribe` | engine → client, stream | n/a | Owner |
| `EventHistory` | client → engine, sync | yes | Owner |
| `GoalWrite` | client → engine, sync | by `request_id` | Owner |
| `EventIngest` | source → engine, sync | by `event_id` | Owner |
| `Schema` | client → engine, sync | yes | global (binary-scoped) |

That's the entire client-facing surface. Operators, tools, the
decider, LLM calls, external-tool dispatches are internal — clients
observe their effects only as `ChangeEvent`s on `Subscribe`.

## Owner-scoping — the primary axis

Every read, write, and subscribe carries one `Owner` (01). A
principal resolves to a set of accessible Owners (membership lives
in the org model, see auth below); each call names exactly one.

Multi-tenant and single-tenant deployments use the same protocol —
single-tenant is just `|Owners| = 1` for one principal. No surface
flag, no two modes.

## Verbs

### Query

Owner-scoped snapshot read of memories, goals, and edges.

- **Filters** split into two layers:
  - **Core-generic** — entity_kind (per 02 `EntityKind`), schema_id,
    owner, time range, supersession status (head only / include
    superseded), edge traversal (follow N relations from a seed),
    pagination cursor + limit.
  - **Flavor-typed** — registered per sidecar by flavors ([08](docs/08-core-and-flavors.md)). A Code
    flavor that wants `severity >= P1` filtering on a `BugReportV1`
    sidecar registers it the same way it registers the schema. The
    `Schema` verb advertises which filter keys are available per
    schema_id so clients can render typed filter UI generically.
- **Returns** data + a `seq_high_water` watermark. The client uses
  this watermark to start a `Subscribe` without missing or
  duplicating events (see *Cold-start stitching* below).

### Subscribe

Owner-scoped server-push stream of `ChangeEvent`s. One stream per
Owner (a principal with N Owners opens N streams).

```rust
struct ChangeEvent {
    seq:    Uuid7,         // monotonic cursor; embeds timestamp
    owner:  Owner,
    kind:   ChangeKind,
}

enum ChangeKind {
    EntityAppend {
        entity_kind:    EntityKind,        // Fact | Abstraction | Perspective | Goal
        entity_id:      EntityId,          // 02 §Edges
        schema_id:      SchemaId,
        schema_version: SchemaVersion,
        supersedes:     Option<EntityId>,  // present iff this row supersedes a prior head; A/P/Goal only — never set for Facts (02 §Re-derivation and supersession)
    },
    EdgeAppend {
        edge_id:  EdgeId,
        relation: RelationId,
        source:   EntityId,
        target:   EntityId,
    },
}
```

**Append-only world, two event types.** No `EntityRemoved`, no
`EntityMutated`. Supersession is itself an `EntityAppend` carrying
`supersedes`, so the client learns "this is the new head" through
the same event shape as a fresh write. **Supersession is A/P/Goal
only**: `EventIngest` never produces a `supersedes`-carrying
`EntityAppend` (Facts are immutable per [02 §Re-derivation and
supersession](02-memory.md#re-derivation-and-supersession)). The
supersedes-carrying paths are `GoalWrite::supersede_goal` (client →
engine) and the internal A/P-emit pipeline (operator → engine, not a
public verb). Stateful Fact projections ("current revision of file X")
arrive on the stream as repeated `EntityAppend`s under the same
schema and natural key; clients fold to heads on the read side
(`Query` heads-only filter).

**Filters** mirror `Query`'s core-generic + flavor-typed split.
Filtering happens in the engine; the client only sees events
matching its subscription.

**`ChangeEvent` is identity + reference, not payload.** Payload
fetch is a follow-up `Query` keyed on the `entity_id` /
`edge_id`. This keeps the stream cheap on bandwidth and lets clients
render typed payloads through the registered schema.

### EventHistory

Owner-scoped bounded read of the change-event log, newest-first.

- **Filters** — owner only in v1. The same core-generic + flavor-typed
  axes that `Subscribe` and `Query` carry land here together when
  any of them gain new axes.
- **Pagination** — `limit` is required (1..=1000, server-clamped).
  `before: Option<Uuid7>` returns rows with `seq < before` for
  older pages. No `after` cursor; resume into newer events is
  `Subscribe`'s job.
- **Returns** events newest-first plus a `seq_high_water` watermark
  with the same semantics as `Query` — the latest seq in the
  owner's `change_event` log at read time. Clients seed local
  caches from `events` and start `Subscribe(since = seq_high_water)`
  to pick up live appends.
- **Idempotent.** `EventHistory` reads only; no commit, no side
  effect.

### GoalWrite

```rust
fn write_goal(draft: GoalDraft, owner: Owner, author: GoalAuthorship,
              request_id: String) -> Result<GoalId, ProtocolError>;

fn supersede_goal(prior: GoalId, draft: GoalDraft, author: GoalAuthorship,
                  request_id: String) -> Result<GoalId, ProtocolError>;
```

- Validates the typed payload against the registered `GoalPayload`
  schema ([06](docs/06-goals-and-self.md)). Unknown `schema_id` → `UnknownSchema`.
- `request_id` is the client-supplied idempotency key. Replay with
  the same `request_id` and identical body returns the same
  `GoalId`; replay with a different body returns
  `IdempotencyConflict`.
- Strong write→stream consistency: when the call returns success,
  the corresponding `EntityAppend{ entity_kind: Goal }` is
  guaranteed to have been emitted to the outbox (see *Consistency*
  below).

### EventIngest

The 01 `EventSource` path, exposed as a verb so external sources
can push.

- Auth is per-source. A client EventSource (e.g. Memophant in-app
  chat) authenticates as the user. A webhook EventSource (Git,
  Slack, Linear) authenticates with a per-source shared secret
  registered alongside the source.
- Synchronous through to Fact materialization (per [05 §Validation](docs/05-actions.md#validation-at-ingest)
  at ingest): the call returns only after the resulting Fact and
  any structural edges from its payload are committed and emitted
  on the change-stream.
- `event_id` is the idempotency key; replay is silently a no-op.

### Schema

Registry introspection. Lists registered schemas, versions, and
declared filter keys for the running binary.

- Binary-scoped: tells the client what *this* deployment exposes,
  regardless of which flavors are linked.
- Lets clients render typed payloads and filters generically rather
  than hard-coding flavor knowledge.
- Returned shape covers all six payload traits (FactPayload,
  AbstractionPayload, PerspectivePayload, GoalPayload,
  CitedObjectPayload, CitationMappingPayload — see [03](docs/03-schema-registry.md), [06](docs/06-goals-and-self.md), [11](docs/11-citations.md)).

## Cursor & resume

`seq` is a `Uuid7` — monotonic by time, server-generated at write.
Resume on disconnect:

```
client persists last_seq it has acknowledged
on reconnect:
    Subscribe(owner, since = last_seq - Δ)   // small overlap window
    server returns events with seq > since
    client dedupes by seq (set membership of what it already has)
```

The overlap covers clock skew and any in-flight events the client
hadn't received when it dropped. No ack protocol; no per-client
server-side state. At-least-once with client-side dedup.

## Cold-start stitching — `Query` → `Subscribe`

Fresh client without local state must seed itself without missing or
duplicating events:

```
1. snapshot, hwm = Query(owner, filters)
2. apply snapshot to local cache
3. Subscribe(owner, since = hwm, filters = same)
4. apply each ChangeEvent to local cache
```

`Query`'s `seq_high_water` is the watermark of the engine's outbox at
read time. Any event committed after that watermark is delivered via
`Subscribe`; any event before is in the snapshot. No race window.

### With history seed

A client that wants to render the recent change-log (e.g. an "Event
stream" UI rail) seeds it with `EventHistory` in parallel with the
snapshot:

```
1. (snapshot, hwm_q), (events, hwm_e) = parallel(
       Query(owner, filters),
       EventHistory(owner, limit = N, before = None))
2. apply snapshot to local cache
3. seed local event log with events
4. Subscribe(owner, since = max(hwm_q, hwm_e), filters = same)
```

`max(hwm_q, hwm_e)` is defensive — the two reads observe the same
log; in steady state they agree. Clients dedup live events by
`seq` membership against the seeded log (`Subscribe` is at-least-once
per the cursor section).

## Multi-Owner stance

One stream per Owner. Rationale:

- One stream = one auth scope; principal access changes don't create
  mid-stream consistency questions.
- Backpressure isolation: a slow Owner can't starve other Owners.
- v1 deployments are single-Owner-active (Memophant per user, Code
  per dev); paying complexity now for benefit later isn't worth it.

A principal with N Owners opens N streams. A multiplexed-stream
variant can be added later as a wrapper without breaking the
per-Owner shape.

## Consistency — strong write→stream, via outbox

Every write commits the entity row and a corresponding
`change_event` row in the same DB transaction. A publisher process
tails `change_event` by `seq` and fans out to subscribed clients.

```
single DB transaction:
    INSERT entity row (memory | goal | edge)
    INSERT change_event row (seq = Uuid7, owner, ChangeKind)
COMMIT

publisher process:
    SELECT * FROM change_event WHERE seq > last_published ORDER BY seq
    fan out to per-Owner Subscribe streams
    advance last_published
```

Properties:

- **Atomic write→event.** No torn states visible to subscribers.
- **`GoalWrite` / `EventIngest` return = event committed.** UI can
  rely on "I wrote it, the next stream tick contains it."
- **At-least-once with client dedup by `seq`.** Publisher can
  replay safely.
- **The `change_event` table is the audit log.** Replay engine
  history by reading it.
- **No external broker required for v1.** Postgres + a tail process
  is sufficient. A broker (NATS, Redis Streams) is a later
  optimization, not an architectural commitment.

The `change_event` storage shape lives in [07](07-storage.md) — this
doc only fixes the *behavior* (atomic write→event, monotonic `seq`,
at-least-once with client dedup).

## Auth model

### Resolution surface

Auth is pluggable per binary, not per flavor. The engine exposes one
trait; transport (09) extracts credentials from the wire and hands
them in:

```rust
trait AuthResolver: Send + Sync {
    fn resolve(&self, creds: &Credentials) -> Result<Resolved, AuthError>;
}

struct Resolved {
    principal:         Principal,     // 01 — User(uid) | Group(gid)
    accessible_owners: Set<Owner>,    // gates the Owner each call names
}

enum Credentials {
    None,                             // NoAuth deployment
    Bearer(BearerToken),              // user principals
    Source(SourceCredential),         // shape per-source (01)
}
```

Per-verb dispatch enforces `call.owner ∈ resolved.accessible_owners`,
returning `Forbidden` otherwise. `Subscribe` opens one stream per
Owner the principal asks for; each is gated independently.

Reference impls live in the workspace:

- **`NoAuth`** — fixed principal + fixed accessible Owner. Local
  desktop, embedded-engine mode (09), single-tenant CLI use.
- **`OIDC`** — validates a JWT against a configured issuer + JWKS,
  maps `sub` to `Principal::User`, reads a configured claim for the
  accessible Owner set. Default for hosted deployments.

A binary picks one resolver at startup. Switching deployment posture
is a config change, not a code change.

### Principal classes

- **User principals** access `Query`, `Subscribe`, `GoalWrite`.
  Multi-Owner per principal works without protocol changes (Justitia
  lawyer with N client matters; family Memophant account with multiple
  personal Owners).
- **Source principals** access `EventIngest`. An in-app EventSource on
  a client may piggyback on the user principal; a webhook source
  authenticates with a shared secret registered to that source. New
  source types introduce their own credential shape as needed; the
  engine doesn't prescribe one.

### Deployment concerns live outside the binary

The resolver only answers *who is this caller and which Owners can
they touch*. Everything that shapes that answer — signup UX, password
reset, MFA, billing, plan enforcement, the Group-membership store
(01 §Owner already names `usermanager` as its home) — runs in front
of the engine and is not Proxima's concern. A typical hosted
deployment puts an IdP (e.g. Zitadel) and a tenancy service
(e.g. `usermanager`) ahead of the engine; the `OIDC` resolver
consumes whatever token they hand off. Local and embedded deployments
need none of it and pick `NoAuth`. Same binary serves both.

`Schema` is unauthenticated by default (the registry is structural
metadata, not user data). Deployments may gate it behind a principal
if they want to keep the schema set private.

## Error envelope — one shape across all verbs

```rust
struct ProtocolError {
    code:        ErrorCode,
    message:     String,
    details:     Option<ErrorDetails>,  // structured per code
    request_id:  Option<String>,        // echo on writes
}

enum ErrorCode {
    // input / schema (the engine is strict at the boundary)
    UnknownSchema,
    SchemaVersionMismatch,
    InvalidPayload,
    InvalidRelation,            // mask violation, unknown relation

    // auth / scope
    AuthRequired,
    Forbidden,                  // principal lacks access to this Owner
    OwnerNotFound,

    // idempotency / state
    IdempotencyConflict,        // request_id reused with different body
    Superseded,                 // target of supersede_* already superseded
    NotFound,

    // engine
    EngineUnavailable,
    TransactionConflict,        // retry-safe
    Internal,                   // catch-all, no detail leak
}
```

Same envelope on every verb. Codes are typed so clients can branch;
`details` carries the structured payload (e.g.
`{ schema_id, expected_version, got_version }` for
`SchemaVersionMismatch`). On `Subscribe`, errors arrive as a
stream-terminating frame.

## EventSource registration — build-time in v1

EventSources register at startup from the linked flavor crates,
same posture as T2 tools (05) and schemas (03). v1 ships
build-time only. Runtime registration of EventSource manifests is
deferred — same trajectory as T1 tools.

## Backpressure — client-side concern in v1

Slow clients on flaky networks request smaller pages on `Query` and
narrower filters on `Subscribe`. Engine-side per-stream buffer
policy is deferred; the cursor + dedup model means any drop is
recoverable on reconnect. Revisit when a real deployment hurts.

## What's out of scope here

- **Transport** (HTTP/2 + SSE? WebSocket? gRPC? long-poll?) — 09.
- **Serialization** (protobuf? JSON? CBOR?) — 09.
- **Stream framing** for `Subscribe` — 09.
- **Schema-driven UI codegen** (proto → typed components) — 09.
- **Local-first replica + offline queue** — 09.

This doc fixes the semantic contract; 09 picks the bytes.

## Cross-references

- `EntityId` / `EntityKind` / `Edge`: 02 §Edges.
- Six payload traits: 03, 06, 11.
- Operators (F→A, A→P, A→Goal): 04.
- Tools (T1/T2), `EventSource` invariants: [01](docs/01-event-source.md), [05](docs/05-actions.md), [12](docs/12-tool-manifest.md).
- Goal entity, `GoalAuthorship`, `GoalPayload`: [06](docs/06-goals-and-self.md).
- Storage shape, `change_event` table (TBD here): [07](docs/07-storage.md).
- Flavor registration (`proxima_flavor!`), build-time posture: [08](docs/08-core-and-flavors.md).
- Frontend client model + transport choices: [09](docs/09-frontend.md).

## Anchors

- `scope`
- `the-five-verbs`
- `owner-scoping-the-primary-axis`
- `verbs`
- `query`
- `subscribe`
- `goalwrite`
- `eventingest`
- `schema`
- `cursor-resume`
- `cold-start-stitching-query-to-subscribe`
- `multi-owner-stance`
- `consistency-strong-write-to-stream-via-outbox`
- `auth-model`
- `resolution-surface`
- `principal-classes`
- `deployment-concerns-live-outside-the-binary`
- `error-envelope-one-shape-across-all-verbs`
- `eventsource-registration-build-time-in-v1`
- `backpressure-client-side-concern-in-v1`
- `whats-out-of-scope-here`
- `cross-references`
