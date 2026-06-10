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

-- ============================================================
-- Relations and edges
-- ============================================================

/-- Flavor-qualified relation identity (e.g. `core/derived-from`,
    `proxima-goal/motivated-by`). Namespacing is pinned in
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
-- Directionality (doc 02 §The Directionality Rule)
-- ============================================================

/-- ME-10 — F/A/P layer rule: for memory→memory edges,
    ℓ(source) ≥ ℓ(target). Upward edges (Fact→Abstraction,
    Fact→Perspective, Abstraction→Perspective) are forbidden. -/
axiom edge_layer_rule :
  ∀ (e : Edge) (ms mt : Memory),
    edge_source e = .memory ms → edge_target e = .memory mt →
    (memory_kind mt).layer ≤ (memory_kind ms).layer

/-- The class-legality matrix, doc 02 §The Directionality Rule,
    transcribed cell by cell. Upward rows are `False` (no legal
    class — strictly stronger than ME-10 alone, and kept anyway:
    the matrix is the authoritative statement).

    Carries in one place:
      - no semantic/causal Fact→Fact edges (U-2: Fact→Fact admits
        only Structural, Provenance — never Causal, Interpretive,
        Supersession);
      - Supersession never touches Facts;
      - Supersession requires same endpoint kind (only the A→A and
        P→P cells admit it). -/
def legalClasses : MemoryKind → MemoryKind → Set RelationClass
  | .Fact, .Fact =>
      fun c => c = .Structural ∨ c = .Provenance
  | .Abstraction, .Fact =>
      fun c => c = .Provenance ∨ c = .Structural
  | .Abstraction, .Abstraction =>
      fun c => c = .Structural ∨ c = .Supersession
  | .Perspective, .Fact =>
      fun c => c = .Causal ∨ c = .Interpretive ∨ c = .Structural
  | .Perspective, .Abstraction =>
      fun c => c = .Provenance ∨ c = .Causal ∨ c = .Interpretive ∨ c = .Structural
  | .Perspective, .Perspective =>
      fun c => c = .Structural ∨ c = .Supersession ∨ c = .Causal ∨ c = .Interpretive
  | _, _ => fun _ => False

/-- ME-11 — every memory→memory edge's relation class is legal for
    its endpoint kinds. -/
axiom edge_class_legal :
  ∀ (e : Edge) (ms mt : Memory),
    edge_source e = .memory ms → edge_target e = .memory mt →
    relation_class (edge_relation e) ∈
      legalClasses (memory_kind ms) (memory_kind mt)

/-- ME-12 — Supersession-class edges connect same-shaped endpoints:
    memory→memory (same kind, via the matrix) or Goal→Goal. Stated
    over NodeRef constructors so the Goal axis is covered too
    (doc 02 §Relation Registry: `core/supersedes` is A→A, P→P,
    Goal→Goal). -/
axiom supersession_same_endpoint_shape :
  ∀ e : Edge, relation_class (edge_relation e) = .Supersession →
    ((∃ ms mt : Memory, edge_source e = .memory ms ∧ edge_target e = .memory mt ∧
        memory_kind ms = memory_kind mt) ∨
     (∃ gs gt : Goal, edge_source e = .goal gs ∧ edge_target e = .goal gt))

-- ============================================================
-- Descriptor masks (doc 02: "Descriptor masks may tighten legal
-- shapes, never relax F/A/P layering.")
-- ============================================================

/-- ME-14 — per-relation endpoint admission (the descriptor mask),
    kept opaque. The tighten-only law: whatever a mask admits between
    memories already satisfies the class matrix. Goal-endpoint shapes
    are governed by masks alone, outside layer comparison. -/
axiom relation_endpoint_admitted : RelationId → NodeRef → NodeRef → Prop

axiom descriptor_masks_tighten_only :
  ∀ (r : RelationId) (ms mt : Memory),
    relation_endpoint_admitted r (.memory ms) (.memory mt) →
    relation_class r ∈ legalClasses (memory_kind ms) (memory_kind mt)

/-- Every edge satisfies its relation's mask. -/
axiom edge_respects_mask :
  ∀ e : Edge,
    relation_endpoint_admitted (edge_relation e) (edge_source e) (edge_target e)

end Proxima
