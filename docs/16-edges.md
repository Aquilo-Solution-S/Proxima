# 16. Edges

The reference for the edge layer, shipped in v0.0.8. It supersedes what
[02-memory.md](02-memory.md) used to say under §Edges, §Relation Registry
and §The Directionality Rule — 02 now restates the parts a reader of 02
needs and points here — and it amends [11-citations.md](11-citations.md)
§Multiplicity, which is why an Abstraction may cite. Proxima is pre-1.0:
the changes below broke the MCP wire surface and the database schema
without a compatibility shim, deliberately.

## The Thesis

> Edges are fundamental, non-extensible connection patterns. An edge
> carries no information beyond its existence: its endpoints, its
> direction, its creation time, and its kind. All content lives in
> nodes; meaning arises from the synthesis of the connected nodes.

Two corollaries make the thesis checkable:

1. **Kind follows operation.** The kind of an edge is a *consequence*
   of the write that produced it, never a choice made by the writer. A
   chosen kind is information and belongs in a node; a consequent kind
   is metadata. This is the line that separates what felt natural in
   the old model (`authorship_kind` — a consequence of the code path)
   from what felt artificial (`relation` — a vocabulary the writer
   picked from).

2. **The node-home test.** Every edge must be re-derivable from a
   statement owned by some node. If no node owns the statement, the
   model is not missing an edge kind — it is missing a node.

The philosophical footnote is older than the repository name: KrV
B129–130 — combination (*conjunctio*) is the one representation that
is never given through objects; it is performed by the understanding.
The content lies in what is combined. The combination itself is an
act with a form, and the forms are closed a priori.

## Motivation

The old model was two-layered: a closed `RelationClass` (five
variants) under an open, flavor-extensible relation vocabulary
(`RelationDescriptor`, namespaced ids, endpoint/authorship masks,
owner and target-access policies, typed edge sidecars). The empirical
case against it:

- In-tree flavors registered exactly three relations across the whole
  tree, none of which needed to be a relation (see the mapping table
  below).
- Recording "this OCR reading came from that PDF upload" — the
  simplest conceivable provenance statement — took two PRs (#155,
  #156), a content-derived edge-id scheme, and a `NULLS NOT DISTINCT`
  partial unique index to make idempotent, because the edge write was
  a free-standing verb instead of a property of the node write.
- Two defects were found in the machinery along the way: a
  non-idempotent edge-append verb (#157) and a typed-sidecar write
  silently discarded on `edge_id` conflict. Both dissolve below —
  not fixed, but made inexpressible.
- `core_link` stored `reason` and `confidence` on the edge. A claim
  with a reason and a confidence is a judgment, and judgments are
  Perspectives; the edge was a Perspective hiding in a cheaper
  container.

## The Model

Nodes carry everything. There are exactly three ways a connection
comes to exist, and none of them is "an agent writes an edge":

| Way | Statement lives in | Index entry written by |
|---|---|---|
| **Origin** — a memory declares what it was made from | the derived node's write (`derived_from`) | the node write's own transaction |
| **Reference** — a payload field points at another node | the payload, as schema-declared reference fields | ingest / derivation, from payload content |
| **Interpretation** — a claim about existing nodes | an interpretation node (Perspective; Abstraction for computed results) whose payload references its subjects | ingest of that node, as ordinary references |

Supersession is not a connection between two things; it is the same
thing persisting through revision. It becomes a lineage pointer
(`superseded_by`) on the memory/goal row, not an edge. Authorship of a
memory ("emitted by Perspective P") is likewise node metadata — a
column on the row, known at write time — not an edge.

### The edge table is an index

```
proxima_core.edges (
    source_kind, source_id,        -- memory | goal | fact-entity head
    target_kind, target_id,
    kind,                          -- 'origin' | 'reference'
    owner_kind, owner_id,          -- always the source owner
    created_at,
    PRIMARY KEY (source_kind, source_id, target_kind, target_id, kind)
)
```

- **No `edge_id`.** Rows have no identity beyond their content, so
  idempotency is structural: replaying any write re-asserts the same
  primary key. The v0.0.7 identity-hash scheme (BLAKE3-derived v8
  ids, the partial unique index) exists to approximate what this
  table has by construction.
- **No payload, no sidecar, no citation, no status.** A connection
  that needs to say more than "these two, this way, since then" is a
  node (see the `proxima-code/calls` mapping below).
- **No relation, no namespace, no authorship column.** The kind is
  binary and follows the operation. Who authored the connection is
  answered by the node that owns the statement.
- **Multiplicity collapses.** Ten call sites from chunk A to chunk B
  are one index row and ten entries in A's payload. The index answers
  "is there a connection"; the node answers "what is it".
- **Rebuildable.** Dropping the table and re-deriving it from node
  content yields the same set. This is the master invariant; every
  other guarantee is a corollary.

### Kinds are closed

Two kinds, and the enum is not extensible — not by flavors, not by
core features. A feature that seems to need a third kind fails the
node-home test and is missing a node.

| Old (`RelationClass`) | Becomes |
|---|---|
| `Provenance` | `origin` |
| `Structural` | `reference` |
| `Causal`, `Interpretive` | `reference` from an interpretation node; the causal/interpretive content is that node's payload |
| `Supersession` | `superseded_by` pointer on the row |

### Direction and layering

The source of an edge is the node that owns the statement: the derived
node for `origin`, the referrer for `reference`. The F/A/P layering
rule survives unchanged — `ℓ(source) ≥ ℓ(target)` for memory
endpoints; Goal endpoints sit outside the layer comparison as today.
Facts still cannot be interpretation *sources* (a Fact asserts no
judgment), which the layering rule already enforces.

### Ownership and visibility

One uniform rule replaces the per-descriptor `ownerPolicy` /
`targetAccessPolicy` matrix: the row is owned by the source owner, and
the write is admitted iff the writer holds write authority on the
source and read authority on the target at write time. Supersession
pointers are same-owner by construction (they live on the row).
Neighbor redaction at read time works as today: a readable edge may
still redact an unreadable endpoint.

## Computed Scores Are Abstractions

A computed score — similarity, ranking, quality, any algorithmic
verdict about other nodes — is not an edge property and not a cache
row. It is an **Abstraction**: payload holds the value and the method,
references point at the inputs, and the proof is an **optional
citation** of the computation record (parameters, model id, receipt)
as a content-addressed CitedObject.

This forces the amendment to [11-citations.md](11-citations.md):
`Memory.citation_mapping_id` becomes optional for **Fact and
Abstraction** (previously Fact-only). Multiplicity stays 0..1 per
memory. Perspectives still never cite directly — an interpretation
grounds through its references. Bibliographic closure for A/P now
terminates at Fact citations *and* direct Abstraction citations.

A score that is merely recomputed on demand (query-time similarity)
stays where it is — computed at read, persisted nowhere. The rule
decides the boundary case that used to be undecidable: persist it and
it is a claim, so it is an Abstraction with its proof attached; don't
persist it and it is nothing.

## What This Removes

| Machinery | Fate |
|---|---|
| `RelationDescriptor`, relation registry, freeze path | deleted |
| Relation ids and namespaces (`proxima-code/calls`, …) | deleted; semantics move to payloads |
| `RelationClass` as public vocabulary | replaced by the two-kind enum |
| `EdgeAuthorshipKind` (10 variants), both bit masks | deleted; authorship lives on nodes |
| Endpoint bindings as API (`FollowHead`/`Pin` on descriptors) | a property of the schema-declared reference field |
| Typed edge sidecars (`EdgePayload`, `edge_schemas = []`) | deleted; content moves into node payloads |
| `relations = []` in `proxima_flavor!` | deleted |
| `core_list_edge_types` | deleted; the vocabulary is this document |
| `append_memory_edge_authorized` and edge handles | deleted; no verb writes an edge directly |
| Per-relation policy cells (`ownerPolicy`, `targetAccessPolicy`) | one uniform admission rule |
| Edge-id derivation (BLAKE3 identity hash, partial unique index) | structural PK |

**Deliberately lost, not overlooked:** third-party flavors can no
longer define novel *traversable* link vocabularies without touching
core. The in-tree evidence (three relations, all expressible as node
content) says that flexibility was speculative. The escape valve is
total: any relationship whatsoever can be asserted as an
interpretation node — the question is never whether something can be
expressed, only where it lives.

## MCP Surface (Breaking)

| Tool | Change |
|---|---|
| `core_link` | **removed.** Its use case — an agent connecting two existing memories with a reason and a confidence — became `core_interpret`: authors an interpretation Perspective (`claim`, `confidence` 0..=100 defaulting to 80, subject memory handles) under `core/interpretation-v1`, and returns a `P:` memory handle. |
| `core_list_edge_types` | removed; the vocabulary is this document |
| `proxima://edge-types`, `proxima://edge/{id}` | removed. `proxima://edges{?kind,source,target,limit,cursor}` remains, filtered by `kind` rather than relation |
| edge reads (`proxima://edges`, `proxima://graph`, `?expand_neighbors`) | return `(source, target, kind, created_at)`; there is no edge handle to dereference and no payload to hydrate |
| `core_derive`, `core_goal`, `core_interpret` | report an `edge_count` where they used to hand back edge handles |
| memory lineage | traverses `origin`; dependency reads read `reference` |
| the `E:` handle prefix | removed from the wire grammar — an edge has no id to name |
| `core_derive` | unchanged argument shape; provenance lands as `origin` entries |
| Fact ingest `derived_from` (#156) | unchanged shape; the API introduced there is the origin path |

## Flavor Migration (In-Tree)

| Old relation | Becomes |
|---|---|
| `proxima-code/calls` + `EdgeCallsV1` sidecar | call sites moved into the caller chunk's payload (`CodeChunkV1.calls`: one `CodeCallV1` per callee, its `sites` carrying the multiplicity); one `reference` entry per callee, derived at ingest |
| `code/targets-execution-request` | a **new node**: the `proxima-code/work-assignment-v1` Perspective, whose payload references the worker Perspective and the request Fact. The node-home test at work — the in-tree "execution plan" is an Abstraction and the assignment target is a worker Perspective, so neither existing endpoint owned the targeting claim, and a Fact asserts no judgment so the request could not be the source. Missing a node, not a kind. |
| `code/has-acceptance-criteria` | the acceptance-criteria Fact's payload references the request Fact |
| `commit→parent`, `chunk→file_revision` (SourceIngest) | already payload-borne; ingest writes `reference` entries instead of relation rows |
| `core/derived-from` | `origin` |
| `core/supersedes` | `supersedes` / `superseded_by` pointers on the row |
| `core/authored` | `memories.authoring_perspective_id` |
| `core/inspires`, `core/wake-motivated-by` | `goals.assignment_perspective_id` and `goals.evidence_memory_ids` — the Goal row knows the Perspective it inspires and the evidence it rests on |
| `core/depends-on` | `goals.dependency_goal_ids`; on the memory side, `depends_on_memory_ids` on the depending request payload |
| `core/agent-link-refers-to` + `AgentLinkV1` | the `core/interpretation-v1` Perspective (`proxima_core.interpretation_v1`): the reason became the claim, the confidence stayed, and the two endpoints became subject references |

## Storage Migration

Two lanes, both resets, in the spirit of the v0.0.4 reset: the edges
table is replaced, not evolved, and nothing carries over. The choice
between a transform and a reset was made in favour of the reset —
mechanically transforming rows would have been the more elaborate way to
arrive at data the substrate can regenerate from what the nodes already
say, which is exactly what rebuildability means.

**Core lane — `0015_v008.sql`.** Drops `proxima_core.edges`, its
`agent_link_v1` sidecar and the `relation_class` / `edge_authorship_kind`
enums; creates the two-kind `edge_kind` enum, the five-label endpoint
enum (including `FactEntityHead`), the new `edges` table with its
structural primary key, the layering / owner / self-loop CHECKs and the
existence trigger; adds `superseded_by` to memories and goals,
`authoring_perspective_id` to memories, and
`assignment_perspective_id` / `dependency_goal_ids` / `evidence_memory_ids`
to goals; widens the citation constraint to Fact ∪ Abstraction; creates
`proxima_core.interpretation_v1`; and reshapes `change_event` to carry
the whole edge rather than a handle to it.
`MIN_CORE_MIGRATION_VERSION` bumps to **15**, so a database one lane
behind the binary fails at boot rather than at first query.

**Flavor lane — `20260801000020_v008_baseline.sql`.** `DROP SCHEMA
proxima_code CASCADE` plus a folded schema; the five superseded lanes are
deleted from the tree. This one is not a preference: the old baseline
created `proxima_code.code_calls_v1` with a foreign key to
`proxima_core.edges(edge_id)`, and 0015 removed that column along with the
identity it stood for, so the old lane can no longer run at all — not on a
fresh database and not on an old one.

The way back is **re-register and re-index**, which the code flavor
already ships a runbook for (`proxima-code_erase_repo`, then
`proxima-code_register_repo` and `proxima-code_ingest_head_snapshot`).
Origin rows come back the moment a node write declares what it was made
from; reference rows come back with re-ingest. Operational steps are in
`MIGRATING.md`.

## Kernel Invariants

The runtime enforces all of these today (CHECKs, a trigger, and the
write paths that are the only producers of rows), and the Lean kernel
restates them in `docs/lean/Causa/Edges.lean` (coverage rows E1–E7 in
`docs/lean/COVERAGE.md`). The edge obligations are:

- **E1 Existence** — both endpoints exist.
- **E2 Ownership** — `edge.owner = source.owner`.
- **E3 Layering** — `ℓ(source) ≥ ℓ(target)` for memory endpoints.
- **E4 Kind-follows-operation** — `origin` rows are written only by
  node writes carrying a derivation declaration; `reference` rows only
  from schema-declared reference fields. No code path writes an edge
  as a free-standing act. A write with **zero** origins is legal: an
  interpretation Perspective grounds through its references and
  consumes nothing, so the operator-invocation manifest is skipped
  rather than failed — a manifest proves a derivation, and a write
  with no derivation has none to prove.
- **E5 Structural idempotency** — the primary key is the row.
- **E6 No content** — edges carry no payload, citation, or status.
- **E7 Rebuildability** — the edge set is a function of node content.
  This is the master invariant; E4–E6 are its preconditions.

`EdgeIdAuthorshipValid` and `EdgeOperatorShapeValid` retired with the
columns they constrained, along with `RelationClass`,
`RelationDescriptor`, `RelationRegistry`, `EdgeAuthorship`, the class
matrix and `EdgeId` itself. Supersession, authorship and Goal topology
became row fields in the kernel exactly as they did in the schema.

## Relationship to Open Work

- **#156** is the proof of concept of this direction: it moved
  provenance from a caller-chosen edge to a node-write property. Its
  `derived_from` API survives verbatim as the origin path; its id and
  index machinery is superseded by the structural PK.
- **#157** (non-idempotent edge verb) dissolves — the verb is removed.
- The sidecar-discard defect noted in #156 dissolves — there are no
  sidecars.
