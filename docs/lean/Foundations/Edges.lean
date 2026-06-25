/-
Proxima Foundations — Edges

Edges connect Memories and Goals (doc 02 §Edges). Every relation
resolves to a build-time RelationDescriptor (Foundations.Composition);
unregistered relations are invalid.

The philosophical load (universe §Perspectivist constructivism):
causal claims are perspective-relative, so **Perspective is the locus
of causal claims, never Facts**. Direct semantic or causal Fact→Fact
edges are forbidden — "cosine similarity is observer-independent and
so cannot encode an observer-relative relation". The class-legality
matrix below is where that commitment becomes structural.

Minimized trusted core (2026-06-11): the PRIMITIVE axioms are the
descriptor-mask pair (`edge_respects_mask`,
`descriptor_masks_tighten_only`) and the scope/shape axioms; the
class-legality matrix (ME-11), the layer rule (ME-10), and the
memory-supersession laws (ME-4/5a/5b, via the pointer↔edge bridge)
are PROVED. A failing proof is drift.

ME-15 — causal chains are queries, not entities: chain(f, P_active)
= structural Fact backbone + Causal/Interpretive edges authored by
P_active + provenance closure. Different active Perspectives yield
different valid chains; a materialized chain view is a cache only,
never authoritative (doc 02 §Causal Chain Query). No Chain primitive
exists here by design.

CI-12 — edges do not cite: there is no citation accessor on Edge.
An edge's reasoning provenance is its authorship; anything
citation-worthy is already on the authoring memory's citation chain.
-/

import Foundations.Prelude
import Foundations.Owner
import Foundations.Identity
import Foundations.Memory
import Foundations.Goals

namespace Proxima

-- ============================================================
-- Relation classes (doc 02 §Relation Registry)
-- ============================================================

/-- The CLOSED substrate vocabulary — five classes, doc 02 verbatim.
    Flavors add relation *ids*, never classes (doc 03: "relation_class
    is not extensible by flavors"). Closedness is carried by the
    inductive itself (CF-F). -/
inductive RelationClass where
  | Structural    -- payload / system structure
  | Provenance    -- derived-from lineage
  | Supersession  -- new entity supersedes prior entity
  | Causal        -- perspective-relative cause / motivation
  | Interpretive  -- perspective-relative non-causal interpretation
  deriving DecidableEq, Repr

/-- Edge authorship vocabulary (doc 02 §Edge Scope Invariant). -/
inductive EdgeAuthorship where
  | EventSource     -- payload-derived structural edges
  | OperatorFtoA    -- F→A provenance
  | OperatorAtoA    -- A→A provenance
  | OperatorAtoP    -- A→P provenance
  | OperatorAtoGoal -- A→Goal provenance
  | PerspectiveLink -- P-authored causal / interpretive framing
  | Engine          -- substrate-authored (supersession / authored)
  | User            -- explicit user/API graph edits
  | ExternalAgent   -- agent-authored MCP / imported edges
  deriving DecidableEq, Repr

-- ============================================================
-- Endpoints
-- ============================================================

/-- An edge endpoint: Memory or Goal (doc 02 §Edges; doc 03 EdgePayload:
    "source endpoint | Memory or Goal"). Goals sit outside the F/A/P
    layer comparison (ME-14); descriptor masks govern their shapes. -/
inductive NodeRef where
  | memory (m : Memory)
  | goal   (g : Goal)

noncomputable def NodeRef.owner : NodeRef → Owner
  | .memory m => memory_owner m
  | .goal g   => goal_owner g

noncomputable def NodeRef.schema : NodeRef → SchemaRef
  | .memory m => memory_schema m
  | .goal g   => goal_schema g

-- ============================================================
-- Relations and edges
-- ============================================================

/-- Flavor-qualified relation identity (e.g. `core/derived-from`,
    `core/motivated-by`). Namespacing is pinned in
    Foundations.Composition. -/
axiom RelationId : Type
axiom relation_class : RelationId → RelationClass

axiom Edge : Type
axiom edge_id         : Edge → EdgeId
axiom edge_source     : Edge → NodeRef
axiom edge_target     : Edge → NodeRef
axiom edge_relation   : Edge → RelationId
axiom edge_owner      : Edge → Owner
axiom edge_authorship : Edge → EdgeAuthorship

/-- AGENTS.md invariant 17 / doc 07 §ID Types — the id-representation
    split is coupled to authorship: EventSource-authored edges carry
    the deterministic content hash (deduplicable payload-derived
    structure); every other authorship carries a fresh UUIDv7. -/
axiom edge_id_authorship_split :
  ∀ e : Edge,
    (∃ h : ContentHash, edge_id e = .sourceAuthored h) ↔
    edge_authorship e = .EventSource

/-- SR-25 / ST-5 — edges are immutable and insert-only in v1; rewrites
    produce new memories and new edges, old edges remain attached. -/
instance : Immutable Edge := ⟨⟩
instance : AppendOnly Edge := ⟨⟩

-- ============================================================
-- Edge scope (doc 02 §Edge Scope Invariant)
-- ============================================================

/-- ME-9 — all edges are single-Owner:
    `source.owner == target.owner == edge.owner` (verbatim).
    Cross-owner sharing is a query/access concern, never an edge
    write (also ST-10, doc 06 §Scoping). -/
axiom edge_scope_single_owner :
  ∀ e : Edge,
    (edge_source e).owner = edge_owner e ∧
    (edge_target e).owner = edge_owner e

-- ============================================================
-- The class-legality matrix (doc 02 §The Directionality Rule)
-- ============================================================

/-- The matrix, doc 02 §The Directionality Rule, transcribed cell by
    cell. Upward rows are `False` (no legal class).

    Carries in one place:
      - no upward F/A/P edges (ME-10 follows — proved below);
      - no semantic/causal Fact→Fact edges (U-2: Fact→Fact admits
        only Structural, Provenance — never Causal, Interpretive,
        Supersession);
      - Supersession never touches Facts;
      - Supersession same-kind between memories (only the A→A and
        P→P cells admit it). -/
def legalClasses : MemoryKind → MemoryKind → Set RelationClass
  | .Fact, .Fact =>
      fun c => c = .Structural ∨ c = .Provenance
  | .Abstraction, .Fact =>
      fun c => c = .Provenance ∨ c = .Structural
  | .Abstraction, .Abstraction =>
      fun c => c = .Structural ∨ c = .Supersession ∨ c = .Provenance
  | .Perspective, .Fact =>
      fun c => c = .Causal ∨ c = .Interpretive ∨ c = .Structural
  | .Perspective, .Abstraction =>
      fun c => c = .Provenance ∨ c = .Causal ∨ c = .Interpretive ∨ c = .Structural
  | .Perspective, .Perspective =>
      fun c => c = .Structural ∨ c = .Supersession ∨ c = .Causal ∨ c = .Interpretive
  | _, _ => fun _ => False

-- ============================================================
-- Descriptor masks — the PRIMITIVE write-legality layer
-- (doc 02: "Descriptor masks may tighten legal shapes, never relax
-- F/A/P layering.")
-- ============================================================

/-- ME-14 — per-relation endpoint admission (the descriptor mask),
    kept opaque. Goal-endpoint shapes are governed by masks alone,
    outside layer comparison. -/
axiom relation_endpoint_admitted : RelationId → NodeRef → NodeRef → Prop

/-- The tighten-only law: whatever a mask admits between memories
    already satisfies the class matrix. -/
axiom descriptor_masks_tighten_only :
  ∀ (r : RelationId) (ms mt : Memory),
    relation_endpoint_admitted r (.memory ms) (.memory mt) →
    relation_class r ∈ legalClasses (memory_kind ms) (memory_kind mt)

/-- Every edge satisfies its relation's mask. -/
axiom edge_respects_mask :
  ∀ e : Edge,
    relation_endpoint_admitted (edge_relation e) (edge_source e) (edge_target e)

-- ============================================================
-- ME-11 and ME-10 — PROVED from the mask layer
-- ============================================================

/-- ME-11 — every memory→memory edge's relation class is legal for
    its endpoint kinds. THEOREM: an edge satisfies its mask, and
    masks only tighten the matrix. -/
theorem edge_class_legal :
    ∀ (e : Edge) (ms mt : Memory),
      edge_source e = .memory ms → edge_target e = .memory mt →
      relation_class (edge_relation e) ∈
        legalClasses (memory_kind ms) (memory_kind mt) := by
  intro e ms mt hs ht
  have h := edge_respects_mask e
  rw [hs, ht] at h
  exact descriptor_masks_tighten_only (edge_relation e) ms mt h

/-- ME-10 — ℓ(source) ≥ ℓ(target) for memory→memory edges. THEOREM:
    the matrix's upward cells admit no class at all. -/
theorem edge_layer_rule :
    ∀ (e : Edge) (ms mt : Memory),
      edge_source e = .memory ms → edge_target e = .memory mt →
      (memory_kind mt).layer ≤ (memory_kind ms).layer := by
  intro e ms mt hs ht
  have h := edge_class_legal e ms mt hs ht
  revert h
  cases memory_kind ms <;> cases memory_kind mt <;> intro h <;>
    first
      | exact h.elim
      | simp [MemoryKind.layer]

-- ============================================================
-- Memory supersession — bridge + PROVED laws (doc 02
-- §Re-derivation and Supersession)
-- ============================================================

/-- The pointer↔edge bridge: a supersession pointer IS a
    Supersession-class edge (doc 02, verbatim: "new_entity
    --core/supersedes--> old_entity"). The pointer accessor lives in
    Foundations.Memory; this axiom identifies it with its edge. -/
axiom supersession_pointer_is_edge :
  ∀ m m' : Memory, memory_supersedes m = some m' →
    ∃ e : Edge,
      edge_source e = .memory m ∧ edge_target e = .memory m' ∧
      relation_class (edge_relation e) = .Supersession

/-- ME-5a — supersession endpoint kind must match (doc 02). THEOREM:
    only the A→A and P→P matrix cells admit Supersession. -/
theorem supersession_same_kind :
    ∀ m m' : Memory, memory_supersedes m = some m' →
      memory_kind m = memory_kind m' := by
  intro m m' h
  obtain ⟨e, hs, ht, hc⟩ := supersession_pointer_is_edge m m' h
  have hleg := edge_class_legal e m m' hs ht
  rw [hc] at hleg
  revert hleg
  cases memory_kind m <;> cases memory_kind m' <;> intro hleg <;>
    first
      | rfl
      | exact hleg.elim
      | (rcases hleg with h' | h' <;> first | exact (nomatch h') | rcases h' with h'' | h'' <;> first | exact (nomatch h'') | rcases h'' with h3 | h3 <;> exact (nomatch h3))

/-- ME-4 — "Facts never supersede and are never superseded"
    (doc 02, verbatim). THEOREM: no Fact cell admits Supersession. -/
theorem facts_never_supersede :
    ∀ m m' : Memory, memory_supersedes m = some m' →
      memory_kind m ≠ .Fact ∧ memory_kind m' ≠ .Fact := by
  intro m m' h
  obtain ⟨e, hs, ht, hc⟩ := supersession_pointer_is_edge m m' h
  have hleg := edge_class_legal e m m' hs ht
  rw [hc] at hleg
  have hk := supersession_same_kind m m' h
  constructor
  · intro hf
    rw [hf, ← hk, hf] at hleg
    rcases hleg with h' | h' <;> exact (nomatch h')
  · intro hf
    rw [hf] at hk
    rw [hk, hf] at hleg
    rcases hleg with h' | h' <;> exact (nomatch h')

/-- ME-5b — supersession stays within one Owner. THEOREM: from the
    bridge edge and single-owner edge scope. -/
theorem supersession_same_owner :
    ∀ m m' : Memory, memory_supersedes m = some m' →
      memory_owner m = memory_owner m' := by
  intro m m' h
  obtain ⟨e, hs, ht, _⟩ := supersession_pointer_is_edge m m' h
  have hscope := edge_scope_single_owner e
  have h1 : NodeRef.owner (edge_source e) = edge_owner e := hscope.1
  have h2 : NodeRef.owner (edge_target e) = edge_owner e := hscope.2
  rw [hs] at h1
  rw [ht] at h2
  show memory_owner m = memory_owner m'
  calc memory_owner m = edge_owner e := h1
    _ = memory_owner m' := h2.symm

/-- ME-12 — Supersession-class edges connect same-shaped endpoints:
    memory→memory (same kind — that part PROVABLE from the matrix) or
    Goal→Goal. The residue the matrix cannot supply — that a
    Supersession edge never mixes a Memory endpoint with a Goal
    endpoint — stays axiomatic (doc 02 §Relation Registry:
    `core/supersedes` is A→A, P→P, Goal→Goal). -/
axiom supersession_same_endpoint_shape :
  ∀ e : Edge, relation_class (edge_relation e) = .Supersession →
    ((∃ ms mt : Memory, edge_source e = .memory ms ∧ edge_target e = .memory mt ∧
        memory_kind ms = memory_kind mt) ∨
     (∃ gs gt : Goal, edge_source e = .goal gs ∧ edge_target e = .goal gt))

end Proxima
