/-
Causa — Identity

ID types and the append-only storage discipline (doc 07). The storage rule in one
line (doc 07 §Append-Only): INSERT is the normal write path; UPDATE
is not part of the cognitive entity lifecycle; DELETE exists only as
compliance erasure (ST-11, ST-13 — see Causa.Compliance).

Identity rules (doc 07 §Identity Rules):
  - Fact / Abstraction / Perspective: fresh UUIDv7 MemoryId.
  - Goal: fresh GoalId; supersession writes a new row.
  - Edge: insert-only.
  - Source/flavor ingest deduplication is metadata around typed Facts,
    not a separate core Event entity.
  - Embeddings are engine-side: the kernel models no `Embedding` entity,
    and the deliberate absence of a `Memory → Embedding` accessor IS the
    vector-store-independence invariant (ST-15..17).
-/

import Causa.Prelude
import Causa.Owner

namespace Causa

-- ============================================================
-- Identity — ONE opaque token (doc 07 §ID Types)
-- ============================================================

/-- The one identity primitive: a fresh, unique, engine-minted token (UUIDv7).
    EQUALITY is all the kernel asks of it — for row uniqueness and for id-pointers
    (e.g. Goal supersession). Its 128-bit layout and v7 time-sortability are
    engine commitments, deliberately NOT modeled: the kernel's time axis is
    `Memory.created_at : Instant`, never the id (two clocks would be a hazard),
    and a concrete UUIDv7 would expose ordering/arithmetic the kernel never uses.
    Opaque, not concrete, is the faithful boundary.

    The kernel needs NO cross-type id distinctness (no theorem reads it), so the
    per-entity id names are documentation abbrevs over this single type rather
    than separate opaque types. -/
axiom Id : Type

abbrev MemoryId          : Type := Id
abbrev GoalId            : Type := Id
/-- Stable handle for a stateful Fact aggregate. It is a fresh engine-minted
    token, NOT a content hash and NOT Fact identity; Fact identity remains the
    current version's `MemoryId`. -/
abbrev FactEntityId      : Type := Id
abbrev CitedObjectId     : Type := Id
abbrev CitationMappingId : Type := Id

/-- Stable persisted owner-table reference. This is not the resolved membership
    map (`Owner := User → Option Role`). Entity rows should store this reference;
    the host resolves it through `OwnerState` before authorization. No new axiom:
    group refs reuse the existing engine-minted `Id`. -/
inductive OwnerRef where
  | world
  | personal (u : User)
  | group (id : Id)

/-- The resolved Leopard-style owner map used by pure authorization rules. -/
abbrev ResolvedOwner : Type := Owner

/-- Server/host owner state: the trusted boundary that resolves stable owner refs
    into current role maps. The kernel does not model the Leopard expansion
    algorithm, SQL tables, or caches; it consumes only this resolved function. -/
structure OwnerState where
  resolve : OwnerRef → ResolvedOwner
  world_resolves : resolve .world = world
  personal_resolves : ∀ u : User, resolve (.personal u) = Owner.ofUser u

/-- The fresh-token half of `EdgeId` (operator / user / engine edges). -/
abbrev EdgeUuid          : Type := Id
/-- A source/flavor ingest batch grouping token. -/
abbrev SourceBatchId     : Type := Id

/-- The content-addressable arm's id. The kernel cannot observe "hash-ness" — it
    sees no payload — so a content hash is, to the kernel, the same opaque
    equality-token as any other `Id`. That source-ingest edges are
    content-addressable (so re-ingest dedups, AGENTS.md invariant 17) is an engine
    commitment, carried by the `sourceAuthored` CONSTRUCTOR, not by a distinct id
    type. The name is kept only to document which `EdgeId` arm it is; the split
    read by `edge_id_authorship_split` (Causa.Edges) is the constructor. -/
abbrev ContentHash : Type := Id

/-- EdgeId is a SUM: source-ingest-authored edges carry a content-addressable id
    (deduplicable); operator / user / engine edges carry a fresh `Id`. The kernel
    distinguishes the two by CONSTRUCTOR alone — both arms are `Id` underneath. -/
inductive EdgeId where
  | sourceAuthored (h : ContentHash)
  | authored       (u : EdgeUuid)

/-- The opaque per-row schema tag every Memory/Goal/Edge/CitedObject carries —
    NOT an identity. THE domainless boundary, payload opacity in its strongest
    form: the kernel sees a row is schema-typed but has NO accessor on it at all
    (no resolution, no payload, no capabilities). A flavor's sidecar conforms to
    this schema; the kernel never inspects it. Namespacing, versioning, the flavor
    registry, and admission of registered schemas are engine/build-time concerns,
    not kernel ontology — the universal rules bind every row regardless of which
    flavor produced it. -/
axiom SchemaRef : Type

-- ============================================================
-- Lifecycle capability classes (doc 07 §Append-Only)
-- ============================================================

/-- ST-11 — no UPDATE in the cognitive lifecycle. Marker class; an
    instance asserts rows of α are never mutated in place. -/
class AppendOnly (α : Type) : Prop

/-- Never superseded, never updated. Facts, Edges, cited objects,
    citation mappings (ST-2, ST-5..8). -/
class Immutable (α : Type) : Prop

-- ============================================================
-- Source/batch identifiers
-- ============================================================

/- ES-5 — batch-id uniqueness is scoped: "unique within (source_id,
   owner)" (doc 01 §The contract Q6, doc 07 §ID Types, doc 04). An
   earlier global-injectivity axiom here asserted MORE than the docs
   (a shared batch id across different owners is doc-admitted: each
   scope is collision-free, the per-scope engine validation accepts
   both). Removed by the minimization pass — the scoped validation is
   an engine check with no kernel-observable face; the F→A gate now
   carries its own owner dimension (`ftoa_batch_exclusive`,
   Causa.Operators). Decision:
   `docs/domain/decisions/2026-06-11-batch-id-scope.md`. -/

/- ST-15..17 — vector-store independence is carried by ABSENCE: the kernel
   declares no `Embedding` entity and no `Memory → Embedding` accessor. Entity
   writes never block on embedding; embeddings can be rebuilt or dropped without
   mutating entities; multiple models coexist; re-embedding is a new engine row.
   Embeddings (over F/A/P and Goals) and the fact that edges are never embedded
   as relations are engine concerns — similarity is query-time evidence and never
   authors graph edges (doc 07, grounding U-2). Nothing kernel-side to model. -/

end Causa
