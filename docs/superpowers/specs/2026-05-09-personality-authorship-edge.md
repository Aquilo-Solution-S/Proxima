# Personality authorship edge: `core/authored`

**Status:** design
**Date:** 2026-05-09
**Owner:** Heinrich
**Related:**
- `docs/02-memory.md` (directionality rule, Authorship enum, Relation registry)
- `docs/06-goals-and-self.md` (`core/inspires` precedent)
- `docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md` (PersonalityToolContext, emit tools)

## Problem

When a Personality emits a memory through a substrate tool
(`core/emit_abstraction`, `core/emit_perspective`), the resulting row
carries `personality_instance_id` so authorship is recoverable via SQL,
and the substrate auto-wires `core/derived-from` provenance edges from
the new memory back to its sources (`{triggering_event} ∪ read_log`).

What is missing is a graph-level link between the **authoring
Personality's current Root Perspective** and the **emitted memory**. As
a result:

- Graph traversal cannot answer "what has this Personality produced?"
  without falling back to a SQL filter on `personality_instance_id`.
- The Root Perspective node — the semantic identity of the Personality
  in the graph — has no outgoing edges to any of its own outputs.
- The "causal chain" from a Personality to its work product is implicit
  in the row attribution, not visible to the substrate's edge layer.

**Concrete observation that surfaced this gap.** Personality
`Senior Software Engineer` (instance `019e07de-124d-7812-b32e-baa66b164142`)
emitted its first `proxima-code/commit-summary-v1` Abstraction. The
provenance edges back to the triggering `code/commit-fact-v1` were
written, but no edge connected the new Abstraction to the Engineer's
Root Perspective.

## Why row attribution isn't sufficient

The `personality_instance_id` column on `proxima_core.memories` records
*who* authored the row, but the engine's substrate vocabulary is the
edge graph. The four-pillar ontology, the A/P traversal contract, and
the lineage walker all operate over edges. Authorship that lives only
as a row attribute is invisible to:

- Lineage walks (`walk_lineage`) — they follow edges, not column
  filters.
- The wake-filter dispatcher — it matches `(trigger_kind, trigger_id)`
  pairs, where edges are first-class triggers (`on_edge`).
- Frontend graph visualization — Root Perspective renders as a node
  with no outgoing edges to its work.

Goals already use the edge layer for the same conceptual link via
`core/inspires: Goal → Root Perspective`. That establishes the
precedent: Root Perspective is a graph anchor, not an opaque pointer
on the runtime row.

## The directionality constraint

`docs/02-memory.md` §"The directionality rule" is hardcoded:

> Within F/A/P, an edge from layer m to layer n is permitted iff
> m ≥ n.

With layers F < A < P, this rules out two of the three plausible edge
shapes:

| Edge | Direction | Allowed? |
|---|---|---|
| Abstraction → Root Perspective | A → P | **forbidden** |
| Root Perspective → Abstraction | P → A | **allowed** |
| Root Perspective → Perspective | P → P | allowed (within-set) |

The rule eliminates any "memory → root" direction. The only viable
shape is **Root → memory**.

## Decision

Add a new substrate relation `core/authored` with class `Causal`. The
substrate auto-wires one edge per Personality-emitted memory:

```
Root Perspective --core/authored--> emitted Memory (Abstraction or Perspective)
```

This mirrors the existing `core/inspires` pattern in shape and
purpose:

| Relation | Direction | Class | Meaning |
|---|---|---|---|
| `core/inspires` | Goal → Root Perspective | Causal | "this Goal directs this Personality" |
| `core/authored` | Root Perspective → Memory | Causal | "this Personality produced this memory" |

Together they form the two complementary anchors on the Root
Perspective node: *intent* (Goals) and *output* (authored memories).

## Mechanics

### Relation registration

`crates/core/src/relation.rs`:

```rust
pub const CORE_AUTHORED_RELATION: &str = "core/authored";

pub fn core_relation_descriptors() -> Vec<RelationDescriptor> {
    vec![
        RelationDescriptor::substrate(CORE_DERIVED_FROM_RELATION, RelationClass::Provenance),
        RelationDescriptor::substrate(CORE_SUPERSEDES_RELATION, RelationClass::Supersession),
        RelationDescriptor::substrate(CORE_INSPIRES_RELATION, RelationClass::Causal),
        RelationDescriptor::substrate(CORE_AUTHORED_RELATION, RelationClass::Causal),
    ]
}
```

Substrate-only (no `EdgePayload` sidecar). All needed state lives on
the `proxima_core.edges` row.

### Edge wiring

`PersonalityToolContext` already carries
`current_root_perspective_memory_id` (snapshotted by the dispatcher at
wake-context assembly time). This is threaded through the existing
write path:

1. `PersonalityWriteRequest` (`crates/core/src/storage.rs`) gains two
   new fields, mirroring the existing `provenance_relation` /
   `supersedes_relation` resolution pattern:
   ```rust
   pub authored_relation: RegisteredRelation<'a>,
   pub current_root_perspective_memory_id: MemoryId,
   ```

2. Substrate tool path (`crates/core/src/personality/tools/shared.rs`,
   `emit_personality_memory`) resolves
   `CORE_AUTHORED_RELATION` from `ctx.engine.registry()` and populates
   both new fields. The relation resolution mirrors the existing
   `resolve_relation(CORE_DERIVED_FROM_RELATION)` call in the same
   function; the snapshot value comes from
   `ctx.current_root_perspective_memory_id`.

3. Storage's `append_personality_memories`
   (`crates/storage-pg/src/verbs/consolidate.rs`) writes one
   additional `core/authored` edge per memory in the same transaction
   as the memory + provenance edges:

   ```rust
   let draft = EdgeDraft {
       edge_id: uuid::Uuid::now_v7(),
       relation: req.authored_relation,
       source_kind: "Perspective",
       source_memory_id: Some(req.current_root_perspective_memory_id.into_inner()),
       source_goal_id: None,
       target_kind: memory.kind.as_str(),  // "Abstraction" or "Perspective"
       target_memory_id: Some(memory_id),
       target_goal_id: None,
       authorship_kind: "Engine",
       authorship_owner_memory_id: None,
       owner: &req.owner,
   };
   append_edge_in_tx(&mut tx, &draft, None).await?;
   ```

   The edge is written *after* the memory row (to satisfy FK on
   `target_memory_id`) and *before* the transaction commits (so the
   memory and its authorship edge land atomically).

### Snapshot semantics

The `current_root_perspective_memory_id` written into the edge is the
**Root Perspective that was active when the wake fired**, not whatever
the runtime row points to at edge-write time. This matters because
Root Perspective evolution (supersession) can happen concurrently with
wake execution. The dispatcher already snapshots this value into
`PersonalityToolContext` at wake-context assembly; we just thread the
same snapshot through to the storage write.

If the runtime row advances mid-wake, the historical record still
reads correctly: the new memory is attributed to the Root Perspective
that was speaking during that wake, and lineage walks across
`core/supersedes` can reconstruct the chain.

## Authorship class

`Core(Engine)`. The substrate writes this edge automatically on the
Personality's behalf in the same transaction as the memory append —
just like `core/supersedes`. Per `docs/02-memory.md` line 282,
`Engine` authorship is permitted for any edge whose
`RelationDescriptor` masks accept it; `core/authored` is registered as
substrate-only with no descriptor restriction beyond the layer rule
(P → A and P → P, both allowed).

The Personality is *not* the authorship principal of the edge — the
substrate is. The Personality is the authorship principal of the
*memory* (recorded via the `personality_instance_id` row column). The
edge merely records the structural fact that the substrate observed
this Personality producing that memory.

## ChangeEvent and wake-filter implications

Every edge insert in `append_edge_in_tx` emits an `EdgeAppend`
`change_event` row (`crates/storage-pg/src/verbs/edge_append.rs:139`).
The new `core/authored` edges will too.

This is **opt-in for wake subscribers**: the wake dispatcher matches
`(trigger_kind, trigger_id)` pairs against `WakeEntry` rows. No
default `WakeEntry` ships with `(on_edge, core/authored)`, so no
spurious wakes fire. A flavor that wants reactive wakes on
"Personality X just produced something" can opt in by registering a
`WakeEntry { trigger_kind: on_edge, trigger_id: "core/authored", ... }`.

This matches the existing pattern for `core/inspires` (Goal proposals
emit edge change_events; only personalities that subscribe wake on
them).

## Self-wake exclusion

The dispatcher already excludes wakes where
`event.author == personality.instance_id` (no self-wake). The new
edges are authored by `Core(Engine)`, not by the Personality, so the
self-wake guard does not need to be touched. A Personality with
`(on_edge, core/authored)` and `authored_by: Any` would wake on edges
from *other* personalities' authoring activity, which is the intended
"watch what other personalities are producing" semantic.

## Scope

**In scope:**
- New `core/authored` substrate relation registered in
  `core_relation_descriptors()`.
- `PersonalityWriteRequest` gains
  `current_root_perspective_memory_id`.
- `emit_personality_memory` (the shared substrate-tool helper) passes
  the snapshot through.
- `append_personality_memories` writes one `core/authored` edge per
  memory in the same transaction.
- Tests that assert the edge exists after `emit_abstraction` and
  `emit_perspective` calls.

**Out of scope:**
- `emit_goal` keeps its existing `core/inspires` wiring. Goals are
  already anchored to the Root Perspective by a different relation
  with different semantics; we do not double-link.
- `create_edge` (the user-asked edge tool) is unchanged. Edges
  authored explicitly by personality reasoning are not auto-linked to
  the Root Perspective; they have their own author and direction
  declared by the personality. (Self-authorship of those edges is
  already recorded via `authorship_kind` on the edge row.)
- **No backfill for existing memories.** Memories written before this
  change retain `personality_instance_id` row attribution but no
  `core/authored` edge. Backfill would have to use the *current* Root
  Perspective as a best-effort approximation of the *root-at-write-time*
  (which we did not record), so the historical edges would be
  inconsistent with going-forward edges. Better to keep the cutover
  clean.
- `core/authored` does not get a typed sidecar. If a future need
  arises (e.g., recording the wake_invocation_id that produced the
  memory) it can be added as a non-breaking change.

## Migration

A single migration registers nothing new in SQL — `core/authored` is a
runtime registry entry, and `proxima_core.edges` already accepts
arbitrary relation strings within the registered set. The
`relation_class = 'Causal'` value is already present in the SQL CHECK
constraint (used by `core/inspires`).

If frontend graph queries need an index on `(relation, source_memory_id)`
or `(relation, target_memory_id)` to make "show me everything this
Root authored" cheap, that index is added in a follow-up migration
after a real query lands. For v1, the existing edge indexes suffice.

## Tests

New / updated assertions:

1. `crates/core/tests/wake_dispatch_e2e_pg.rs`: after a wake produces
   an Abstraction via `emit_abstraction`, assert one
   `core/authored` edge exists from the runtime row's
   `current_root_perspective_memory_id` to the new memory_id.

2. `flavors/code/tests/commit_summary_e2e.rs`: same assertion for the
   `proxima-code/commit-summary-v1` path that surfaced the gap.

3. `flavors/code/tests/engineer_e2e.rs`: assert the edge for the
   Engineer's `emit_perspective` calls (target_kind = Perspective,
   P → P).

4. `crates/storage-pg/tests/personality_wake_pg.rs`: storage-level
   assertion that `append_personality_memories` writes one
   `core/authored` edge per memory in the same transaction (rollback
   on either insert fails the whole tx).

5. New unit test in `crates/core/src/relation.rs`: assert
   `core/authored` is in `core_relation_descriptors()` with class
   `Causal`.

## Why not the other shapes

For the record (so future-us doesn't relitigate this):

- **Include Root in the auto-provenance set, reuse `core/derived-from`.**
  Would write `Abstraction → Root Perspective` for `emit_abstraction`.
  **Forbidden** by the directionality rule (A → P). Even for
  `emit_perspective` (P → P, allowed), reusing `derived-from` blurs
  "what informed this memory" with "who produced this memory" and
  pollutes provenance walks with the Root Perspective on every step.

- **`core/authored-by` in Memory → Root direction (mirrors
  `core/inspires` direction).** Forbidden by the directionality rule
  for Abstractions (A → P). Would only work for Perspectives
  (P → P), which would mean the relation behaves differently
  depending on what's emitted. Bad shape.

- **Row column for snapshot Root Perspective on `proxima_core.memories`.**
  Adds attribution as a column instead of an edge. Cheaper to query
  for "all memories of this Root", but invisible to the edge layer
  (graph viz, wake filters, lineage walks). The edge solution
  subsumes the row-column solution for graph queries; the row
  attribution stays as `personality_instance_id` for SQL queries. No
  reason to add a third encoding.

## Addendum (2026-05-09): Fact emits during a wake

Workspace mode (see
`docs/superpowers/specs/2026-05-09-workspace-mode-design.md`) requires
the same `core/authored` auto-wire when an **event source** ingests a
**Fact** within an active wake context. The original spec scoped the
auto-wire to the substrate tools `core/emit_abstraction` /
`core/emit_perspective`; this addendum extends it to `EventIngest`.

### Why it stays inside the original contract

The contract becomes:

> Any Memory emitted while a wake_token is in scope receives one
> `core/authored` edge from that wake's snapshotted
> `current_root_perspective_memory_id`, regardless of whether the
> emit path is a substrate tool or `EventIngest`.

The directionality rule allows P → F (m=2, n=0, m ≥ n), so the edge
shape is unchanged. The Personality is still not the authorship
principal of the edge (Core(Engine) writes it on the substrate's
behalf); it is the authorship principal of the row only via the same
`personality_instance_id`-equivalent attribution that the workspace
runner records in the Fact's payload.

### Mechanics

`EventIngest`'s storage path
(`crates/storage-pg/src/verbs/event_ingest.rs` — alongside
`append_personality_memories` / `append_edge_in_tx`) gains the same
two parameters the substrate tools already thread:

```rust
pub authored_relation: Option<RegisteredRelation<'a>>,
pub current_root_perspective_memory_id: Option<MemoryId>,
```

Both `Option`-wrapped because non-wake EventIngest calls (e.g., a
LocalGitSource poll outside any wake) leave them `None` and emit no
authorship edge. The wake-context check is structural: the engine's
`EventIngest` request handler reads the active `WakeTokenContext` from
the same `wake_token_store` the substrate tools use, and threads
`(CORE_AUTHORED_RELATION, ctx.current_root_perspective_memory_id)`
through to storage when present.

When both are `Some`, storage writes one extra `core/authored` edge
per Fact in the same transaction as the Fact insert and its
provenance edges, identical in shape to the existing
`append_personality_memories` path.

### Authorship class and self-wake

Unchanged: edge authorship is `Core(Engine)`. The dispatcher's
self-wake guard (`event.author == personality.instance_id`) does not
fire because the edge is engine-authored, not
personality-authored. A WakeEntry registered with `(on_edge,
core/authored)` and `authored_by: Any` wakes on edges from *other*
personalities' Fact emits — this is the intended cross-personality
"watch what other personalities are producing" semantic, now extended
to Facts.

### Tests

Added to the existing test list:

- `flavors/code/tests/workspace_run_pg.rs` (lives in workspace-mode
  spec): firing a workspace wake that emits a `workspace-run-v1`
  Fact via `EventIngest` writes one `core/authored` edge from the
  Engineer's Root Perspective to the Fact, atomic with the Fact
  insert.
- `crates/storage-pg/tests/event_ingest_pg.rs`: Fact emit *outside*
  any wake context writes no `core/authored` edge (regression guard
  for the `Option` semantics).
