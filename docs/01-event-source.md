# 01 — Event Source

The Event Source is the **membrane between Reality and the agent**. Reality is
unknowable in itself; the agent only ever sees what an Event Source surfaces.
This is the *only* path by which Reality enters the wheel.

Every Fact in the system traces back to an Event Source. No exceptions.

## Owner — scoping primitive

Every Event, Memory, and Goal carries an `Owner`. Two distinct
concerns: **principal** = access scope; **org_id** = billing unit
(measured for data usage / quota).

```rust
struct Owner {
    principal: Principal,       // access scope
    org_id:    OrgId,           // billing unit
}

enum Principal {
    User(UserId),               // personal — only that user sees it
    Group(GroupId),             // group-shared — group members see it
}
```

Used identically across components 01 / 02 / 05 / 06. Storage: three
columns (`owner_principal_kind`, `owner_principal_id`, `owner_org_id`)
plus a check constraint. Schemas ([03](docs/03-schema-registry.md)) are binary-scoped (per [03 §Scoping](docs/03-schema-registry.md#scoping-one-namespace-per-binary)).

Access rule (`org_id` never enters):

```
visible(m, requester) iff
    m.principal == User(requester.user_id)
  ∨ ( m.principal == Group(g) ∧ requester ∈ members(g) )
```

Group membership lives in usermanager as a new membership type
alongside org membership.

v1 constraints:

- Group lives in one org: `group.org_id` set at creation; a memory's
  `owner.org_id` is denormalised from `group.org_id` when principal is
  `Group`. Cross-org groups deferred (v2+).
- "Org-wide visible" expressed as a default `<org>-everyone` group
  whose membership auto-syncs with org membership. No `Principal::Org`
  variant.

Per-memory ACL (`AccessGrant` table) is a v2+ extension layered above
`Owner`; not in v1.

## The contract

An Event Source produces **typed, cited, deduplicable events** that the engine
stores as Facts (memories of `kind = fact`). The typing comes from a
**registered schema** in the schema registry (component 03) — the source
declares which schema its events conform to, and the engine stores the typed
payload in the schema's sidecar table.

The source carries no interpretation; performs no abstraction; does not look
across other sources.

```
trait EventSource {
    type Event;                                  // typed payload, source-specific

    fn source_id(&self) -> SourceId;             // stable identifier
    fn schema_version(&self) -> Version;         // payload schema version

    // Pull mode: engine asks for events since cursor.
    // Push mode: source emits to a queue the engine subscribes to.
    // A given source implements one or the other, not both.
}
```

Each event the source produces becomes one Fact. The 1:1 mapping is mandatory
*at the engine boundary*: no Event Source filters, deduplicates, or
re-aggregates events into "richer" Facts. That work belongs to consolidation.

A single Reality observation may produce **many events**, however. A PDF
upload produces one event per chunk after OCR; a repo crawl produces one
event per file; a Telegram conversation produces one event per message. The
1:1 rule applies to events crossing into the engine, not to the upstream
fan-out. All events from a single observation share a **source batch id**:
a UUIDv7 the source declares opaquely at emit time. The engine validates
uniqueness within `(source_id, owner)` and rejects collisions; sources
already control observation grouping, so they own this id (Q6). F→A
consolidation operates on a source batch (component 02): the chunks of one
PDF, the files of one repo crawl, the messages of one chat session.

Batch lifecycle (open / closed / consolidated) is persisted in the core
`source_batches` table — see [04 §Source-batch lifecycle](docs/04-consolidation.md#source-batch-lifecycle). The source
signals batch-complete via `engine.close_batch(source_batch_id)`; the
engine gates F→A on `closed_at IS NOT NULL`.

`source_batch_id` is the F→A consolidation episode, distinct from the
artefact a Fact cites (`citation_mapping_id` → `cited_object_id`,
see [11](docs/11-citations.md)). They often coincide — one PDF ingestion → one batch → one
Document — but for streams (one ChatSession lasting months → many
batches over time) they don't. Coincidence isn't identity.

## Properties of an Event

Every event carries:

| Field | Meaning |
|---|---|
| `event_id` | Deterministic hash of `(source_id, owner, payload)`. Re-receipt produces the same id. |
| `source_id` | Which source emitted this. |
| `owner` | `Owner` — scope of this event (whose Reality slice). Source sets at emit time from its config or per-event observation context. |
| `source_batch_id` | UUIDv7 declared by the source at emit time; engine validates uniqueness within `(source_id, owner)` and rejects collisions. Groups events from the same Reality observation. |
| `schema_id` | Which registered schema this event conforms to (component 03). |
| `schema_version` | Version of that schema. |
| `observed_at` | When the agent observed the event. |
| `occurred_at` | When the underlying Reality change happened (may differ — a webhook arrives after the commit). |
| `payload` | Typed, source-specific data conforming to `schema_id @ schema_version`. This includes source-specific fields like `source_uri` (e.g., `forgejo://AQS/aquilo/commit/<sha>`, `telegram://chat/<id>/<msg>`) and `source_locus` (e.g., line number, message index, file path, query offset). |

`event_id` determinism is what makes the system idempotent against re-receipt.
A webhook re-fired, a poll loop overlapping its own previous cursor, a manual
replay during debugging — all produce the same `event_id`, and the engine
silently drops the duplicate.

## What the Event Source must not do

These are violations of the contract, not stylistic preferences. The wheel
fails if any of these leak into a source:

1. **No abstraction.** A source observing 100 webhooks does not synthesize
   "this repo had a busy day" into a single event. Each webhook is its own
   Fact. Synthesis is consolidation's job.
2. **No interpretation.** A source observing a Telegram message does not
   classify it as "user is frustrated". The text is the Fact; the
   classification is an Abstraction produced upstream.
3. **No cross-source joining.** A source does not look at what another source
   produced. Each source is local to its own Reality slice. Cross-source
   patterns are Perspectives.
4. **No filtering by relevance.** A source may filter by *correctness* (drop
   malformed payloads, validate schema). It may not filter by "is this
   interesting" — that is Goal-driven, not source-driven.
5. **No persistence beyond cursor state.** A source remembers where it is
   (`since_cursor` for pulls, last-seen offset for streams) and nothing else.

These constraints exist because the membrane has a single job: faithfully
expose Reality. Anything else is a failure mode.

## Domain examples

Different Realities, identical contract:

| Reality | Source | What it produces |
|---|---|---|
| Code | Forgejo webhook | Commit, PR, issue, comment events |
| Code | Repo crawler (pull) | File-snapshot events for each file at HEAD |
| Code | gRPC handler tracer | Service-call events from runtime |
| Learning | Document ingester | Page / section / passage events from a PDF or markdown |
| Learning | Conversation logger | Message events from a chat session |
| Learning | Exam harness | Question + answer + grade events |
| Jurisdiction | Mandate ingester | Document, hearing, ruling events |
| Jurisdiction | Email watcher | Inbound mail events from `mandant@` |
| Jurisdiction | Court calendar | Hearing-scheduled events |

The list is open-ended. Each new domain (or each new source within a domain)
adds a new `EventSource` impl. Nothing in the engine changes.

## Push vs pull

A source picks one based on the shape of its Reality:

- **Push** — Reality emits events on its own timeline (webhooks, MQTT,
  Telegram, Forgejo, NATS). The source registers as a subscriber and forwards
  into the engine's input queue.
- **Pull** — Reality is queryable, and the agent advances through it on its
  own cadence (filesystem walk, repo snapshot, ERPNext API, court calendar).
  The source maintains a cursor and replays new content when polled.

The engine treats both uniformly: typed events arrive, become Facts, get
stored. The source's mode is its own implementation detail.

## Schema evolution

Schemas evolve. A new version of `forgejo-commit` adds fields or changes
semantics. The source declares which schema and version it emits to. Events
are stored against that version's sidecar table. Component 03 covers the
registry mechanics — registration, versioning, and how older Facts remain
queryable when a schema rev lands.

The engine does not migrate Facts across schema versions. Facts are
immutable; their typing is frozen at insert time.

## Bootstrap

The engine itself has no founding goal. Per-flavor onboarding is the
bootstrap mechanism (see [06](docs/06-goals-and-self.md)): the flavor's signup flow asks the user
flavor-specific founding-letter questions and writes Goals + Events
under that user's `Owner`.

Engine-level config registers source *instances* with their default
owner. Source-instance shape lives here; the broader runtime config
surface (LLM endpoint, embedding model, credential resolution)
extends the same `proxima.config.yaml` and is specified in
[10](docs/10-configuration.md).

For example, a shared Forgejo crawler for org-AQS emits with
`principal = Group(org_AQS_everyone)`, `org_id = org_AQS`; a personal
Telegram source emits with `principal = User(u)`, `org_id =
u.personal_org`. Sources may also override owner per-event when the
observation context demands it.

```yaml
proxima.config.yaml
  sources:
    - id: forgejo-aquilo
      type: forgejo-webhook
      uri: https://git.aquilo-cloud.com/AQS/aquilo
      auth_secret: ...
      default_owner:
        principal: { group: org_AQS_everyone }
        org_id: org_AQS
```

## What this gives us

A clean separation: anyone can write an Event Source for any Reality without
touching the engine. The engine is fully domain-agnostic at the input
boundary, and the cost of supporting a new Reality is exactly one
`EventSource` implementation plus a `Citation` URI scheme.

## Anchors

- `owner-scoping-primitive`
- `the-contract`
- `properties-of-an-event`
- `what-the-event-source-must-not-do`
- `domain-examples`
- `push-vs-pull`
- `schema-evolution`
- `bootstrap`
- `what-this-gives-us`
