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
  - Embedding: re-embed writes a new row; the entity row is untouched.
-/

import Causa.Prelude
import Causa.Owner

namespace Causa

-- ============================================================
-- ID slots
-- ============================================================

/-- UUIDv7 — sortable-by-time entity identity (doc 07 §ID Types).
    The kernel keeps UUID structure opaque; "v7" is an engine
    commitment recorded here as commentary. -/
axiom MemoryId : Type
axiom GoalId   : Type
axiom CitedObjectId      : Type
axiom CitationMappingId  : Type

/-- The two id representations of doc 07 §ID Types / AGENTS.md
    invariant 17: deterministic content hashes vs fresh UUIDv7s.
    Both opaque — the SPLIT is the invariant, not the encoding. -/
axiom ContentHash : Type
axiom EdgeUuid    : Type

/-- EdgeId is a SUM (AGENTS.md invariant 17, doc 07 §ID Types):
    source-ingest-authored edges carry a deterministic content hash
    (payload-derived structural edges are deduplicable); operator /
    user / engine edges carry a fresh UUIDv7. The authorship coupling
    is pinned in Causa.Edges (`edge_id_authorship_split`). -/
inductive EdgeId where
  | sourceAuthored (h : ContentHash)
  | authored       (u : EdgeUuid)

axiom SourceId      : Type
axiom SourceBatchId : Type

/-- Opaque (schema_id, schema_version) reference. THE domainless
    boundary: the kernel sees that every Memory/Goal is schema-typed,
    never what the schema contains. Resolution to a namespaced
    SchemaId happens in Causa.Composition. -/
axiom SchemaRef : Type

-- ============================================================
-- Lifecycle capability classes (doc 07 §Append-Only)
-- ============================================================

/-- ST-11 — no UPDATE in the cognitive lifecycle. Marker class; an
    instance asserts rows of α are never mutated in place. -/
class AppendOnly (α : Type) : Prop

/-- Never superseded, never updated. Facts, Edges, cited objects,
    citation mappings, embeddings (ST-2, ST-5..8, ST-15). -/
class Immutable (α : Type) : Prop

/-- Append-only with supersession: a new row may name the prior head
    it supersedes (ST-3, ST-4; doc 02 §Re-derivation). Supersession
    is logical — current state is a query over append-only rows
    (ST-26). -/
class Supersedable (α : Type) where
  supersedes : α → Option α

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

-- ============================================================
-- Embeddings — vector-store independence (doc 07 §Vector Store)
-- ============================================================

/-- ST-15..17 — embeddings reference entity ids but the entity has no
    accessor to its embeddings: there is NO kernel function
    `Memory → Embedding`. That deliberate absence IS the independence
    invariant — entity writes never block on embedding; embeddings can
    be rebuilt or dropped without mutating entities; multiple models
    coexist. Re-embedding writes a new row (`Immutable Embedding`);
    the entity row does not change.

    Embeddings may point at Facts, Abstractions, Perspectives, AND
    Goals (doc 07 §Vector Store) — hence the id-sum target. Edges are
    never embedded as relations: similarity is query-time evidence and
    never authors graph edges (doc 07, grounding U-2). -/
inductive EmbeddingTarget where
  | memory (id : MemoryId)
  | goal   (id : GoalId)

axiom Embedding : Type
axiom embedding_target : Embedding → EmbeddingTarget

instance : Immutable Embedding := ⟨⟩

end Causa
