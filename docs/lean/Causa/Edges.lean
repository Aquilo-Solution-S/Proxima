/-
Causa — Edges

Edges connect Memories and Goals (doc 02 §Edges). Every relation
resolves to a build-time RelationDescriptor; unregistered relations are
invalid.

The philosophical load (universe §Perspectivist constructivism):
causal claims are perspective-relative, so **Perspective is the locus
of causal claims, never Facts**. Direct semantic or causal Fact→Fact
edges are forbidden — "cosine similarity is observer-independent and
so cannot encode an observer-relative relation". The class-legality
matrix below is where that commitment becomes structural.

Minimized trusted core (D14/D16): `Edge` is a raw row shape;
row-admission laws are predicates over valid rows, not global axioms
over every constructible Lean value. Relation-specific policy lives on
`RelationDescriptor` rows: endpoint masks, ownership policy, target
access policy, and mask-tightening proofs travel together.

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

import Causa.Prelude
import Causa.Owner
import Causa.Identity
import Causa.Memory
import Causa.Goals

namespace Causa

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

/-- Relation-local owner policy. Every Edge row is still source-owned
    (`edge.owner = source.owner`); this policy says whether the target
    may cross Owner boundaries for this relation. -/
inductive RelationOwnerPolicy where
  | SourceOwned -- target may be cross-owner
  | SameOwner   -- target.owner = source.owner
  deriving DecidableEq, Repr

/-- Relation-local write-admission policy for the target endpoint.
    Source write authority is universal for edge writes; this field says
    whether the relation additionally requires target read/write authority.
    The requester-sensitive gate lives in EdgeAuthorization, not row shape. -/
inductive RelationTargetAccessPolicy where
  | None
  | Read
  | Write
  deriving DecidableEq, Repr

/-- Edge authorship vocabulary (doc 02 §Edge Scope Invariant). -/
inductive EdgeAuthorship where
  | SourceIngest    -- payload-derived structural edges from typed Fact ingest
  | OperatorFtoA    -- F→A provenance
  | OperatorAtoA    -- A→A provenance
  | OperatorAtoP    -- A→P provenance
  | OperatorAtoGoal -- A→Goal provenance
  | PerspectiveLink -- P-authored causal / interpretive framing
  /-- Goal-analogue of `PerspectiveLink`: carries perspectival causal
      claims involving Goals, including `core/inspires` Goal→Perspective
      inspiration and Goal→Fact outcome attribution. -/
  | PerspectiveGoalLink
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
    `core/motivated-by`). Runtime/storage encoding and namespace
    validation are registry concerns; the kernel needs only the id value. -/
abbrev RelationId : Type := String

structure Edge where
  id         : EdgeId
  source     : NodeRef
  target     : NodeRef
  relation   : RelationId
  owner      : Owner
  authorship : EdgeAuthorship

/-- Compatibility accessor for prose/Rust vocabulary. -/
def edge_id : Edge → EdgeId := Edge.id

/-- Compatibility accessor for prose/Rust vocabulary. -/
def edge_source : Edge → NodeRef := Edge.source

/-- Compatibility accessor for prose/Rust vocabulary. -/
def edge_target : Edge → NodeRef := Edge.target

/-- Compatibility accessor for prose/Rust vocabulary. -/
def edge_relation : Edge → RelationId := Edge.relation

/-- Compatibility accessor for prose/Rust vocabulary. -/
def edge_owner : Edge → Owner := Edge.owner

/-- Compatibility accessor for prose/Rust vocabulary. -/
def edge_authorship : Edge → EdgeAuthorship := Edge.authorship

-- ============================================================
-- Row-validity predicates
-- ============================================================

/-- AGENTS.md invariant 17 / doc 07 §ID Types — the id-representation
    split is coupled to authorship: source-ingest-authored edges carry
    the deterministic content hash (deduplicable payload-derived
    structure); every other authorship carries a fresh UUIDv7.
    Table/row validity, not a global property of raw `Edge` values. -/
def EdgeIdAuthorshipValid (e : Edge) : Prop :=
  (∃ h : ContentHash, edge_id e = .sourceAuthored h) ↔
    edge_authorship e = .SourceIngest

/-- N4 — any Causal edge touching a Goal endpoint is perspectival.
    `core/inspires` is intentionally Causal: Goal→Perspective inspiration
    is a perspectival causal claim, alongside Goal→Fact outcome attribution. -/
def EdgeGoalCausalValidWith (c : RelationClass) (e : Edge) : Prop :=
  c = .Causal →
    ((∃ g : Goal, edge_source e = .goal g) ∨
      (∃ g : Goal, edge_target e = .goal g)) →
    edge_authorship e = .PerspectiveGoalLink

/-- ME-9 (group-ownership realign) — edges are SOURCE-owned: an edge's
    Owner is its source endpoint's Owner. Query surfaces may still redact
    or suppress unreadable targets; ownership is source-local. -/
def EdgeSourceOwned (e : Edge) : Prop :=
  (edge_source e).owner = edge_owner e

/-- SR-25 / ST-5 — edges are immutable and insert-only in v1; rewrites
    produce new memories and new edges, old edges remain attached. -/
instance : Immutable Edge := ⟨⟩
instance : AppendOnly Edge := ⟨⟩

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
-- Relation descriptors — the PRIMITIVE write-legality layer
-- (doc 02: "Descriptor masks may tighten legal shapes, never relax
-- F/A/P layering.")
-- ============================================================

/-- ME-14/D16 — one build-time relation descriptor row. The descriptor is
    the matrix row: class, endpoint mask, source/target ownership policy,
    write-admission target policy, and the proof that memory→memory masks
    only tighten the closed F/A/P class matrix. -/
structure RelationDescriptor where
  id : RelationId
  relClass : RelationClass
  ownerPolicy : RelationOwnerPolicy
  targetAccessPolicy : RelationTargetAccessPolicy
  endpointAdmitted : NodeRef → NodeRef → Prop
  masksTightenOnly :
    ∀ (ms mt : Memory),
      endpointAdmitted (.memory ms) (.memory mt) →
      relClass ∈ legalClasses (memory_kind ms) (memory_kind mt)
  supersessionSameOwner : relClass = .Supersession → ownerPolicy = .SameOwner

/-- Relation owner-policy satisfaction for a concrete edge. Source-owned
    edge rows are universal (`EdgeSourceOwned`); `.SameOwner` is a stricter
    relation-local target policy. -/
def ownerPolicySatisfied (p : RelationOwnerPolicy) (e : Edge) : Prop :=
  match p with
  | .SourceOwned => True
  | .SameOwner => (edge_target e).owner = (edge_source e).owner

/-- A valid edge satisfies its descriptor's endpoint mask. -/
def EdgeMaskValidWith (d : RelationDescriptor) (e : Edge) : Prop :=
  d.endpointAdmitted (edge_source e) (edge_target e)

/-- A valid edge satisfies its descriptor's owner policy. -/
def EdgeOwnerPolicyValidWith (d : RelationDescriptor) (e : Edge) : Prop :=
  ownerPolicySatisfied d.ownerPolicy e

/-- ME-12 residue — a Supersession-class edge never mixes a Memory endpoint
    with a Goal endpoint. The same-kind memory half remains proved from the
    matrix; Goal→Goal carries no F/A/P kind. -/
def EdgeSupersessionEndpointShapeValidWith (c : RelationClass) (e : Edge) : Prop :=
  c = .Supersession →
    ((∃ ms mt : Memory, edge_source e = .memory ms ∧ edge_target e = .memory mt) ∨
     (∃ gs gt : Goal, edge_source e = .goal gs ∧ edge_target e = .goal gt))

/-- A persisted Edge row validated against one descriptor row. -/
structure EdgeValidWith (d : RelationDescriptor) (e : Edge) : Prop where
  relationMatches : edge_relation e = d.id
  idAuthorship : EdgeIdAuthorshipValid e
  goalCausal : EdgeGoalCausalValidWith d.relClass e
  sourceOwned : EdgeSourceOwned e
  ownerPolicy : EdgeOwnerPolicyValidWith d e
  mask : EdgeMaskValidWith d e
  supersessionEndpointShape : EdgeSupersessionEndpointShapeValidWith d.relClass e

/-- Core validity for one persisted Edge row. This is the replacement for
    the former global axioms over all raw `Edge` values. The descriptor
    witness is the build-time relation row used to validate this edge. -/
def EdgeCoreValid (e : Edge) : Prop :=
  ∃ d : RelationDescriptor, EdgeValidWith d e

/-- A valid edge classified by its validating descriptor. This replaces the
    former global `relation_class : RelationId → RelationClass` accessor. -/
def EdgeHasClass (e : Edge) (c : RelationClass) : Prop :=
  ∃ d : RelationDescriptor, EdgeValidWith d e ∧ d.relClass = c

/-- Class evidence already includes ordinary core validity. -/
theorem edge_has_class_core_valid :
    ∀ e c, EdgeHasClass e c → EdgeCoreValid e := by
  intro e c h
  rcases h with ⟨d, hvalid, _⟩
  exact ⟨d, hvalid⟩

/-- Table-scoped Edge validity. -/
def EdgeTableValid (edges : Set Edge) : Prop :=
  ∀ e : Edge, e ∈ edges → EdgeCoreValid e

-- ============================================================
-- Validity projection theorems
-- ============================================================

/-- Former `edge_id_authorship_split` axiom, now projected from row validity. -/
theorem edge_id_authorship_split :
    ∀ e : Edge, EdgeCoreValid e →
      ((∃ h : ContentHash, edge_id e = .sourceAuthored h) ↔
        edge_authorship e = .SourceIngest) := by
  intro e hvalid
  rcases hvalid with ⟨d, h⟩
  exact h.idAuthorship

/-- Former `causal_goal_edge_perspectival` axiom, now projected from row validity. -/
theorem causal_goal_edge_perspectival :
    ∀ e : Edge, EdgeHasClass e .Causal →
      ((∃ g : Goal, edge_source e = .goal g) ∨
        (∃ g : Goal, edge_target e = .goal g)) →
      edge_authorship e = .PerspectiveGoalLink := by
  intro e hclass hgoal
  rcases hclass with ⟨d, h, hd⟩
  exact h.goalCausal hd hgoal

/-- Former `edge_source_owned` axiom, now projected from row validity. -/
theorem edge_source_owned :
    ∀ e : Edge, EdgeCoreValid e → (edge_source e).owner = edge_owner e := by
  intro e hvalid
  rcases hvalid with ⟨d, h⟩
  exact h.sourceOwned

/-- Former `supersession_intra_owner` axiom, now projected from descriptor
    owner policy: Supersession-class descriptors are `.SameOwner`, and a
    valid edge satisfies that descriptor policy. -/
theorem supersession_intra_owner :
    ∀ e : Edge, EdgeHasClass e .Supersession →
      (edge_target e).owner = (edge_source e).owner := by
  intro e hclass
  rcases hclass with ⟨d, h, hd⟩
  have hpolicy : d.ownerPolicy = .SameOwner := d.supersessionSameOwner hd
  have howner := h.ownerPolicy
  unfold EdgeOwnerPolicyValidWith ownerPolicySatisfied at howner
  rw [hpolicy] at howner
  exact howner

/-- Former `edge_respects_mask` axiom, now projected from the descriptor
    witness used to validate the row. -/
theorem edge_respects_mask :
    ∀ e : Edge, EdgeCoreValid e →
      ∃ d : RelationDescriptor,
        EdgeValidWith d e ∧ d.endpointAdmitted (edge_source e) (edge_target e) := by
  intro e hvalid
  rcases hvalid with ⟨d, h⟩
  exact ⟨d, h, h.mask⟩

-- ============================================================
-- ME-11 and ME-10 — PROVED from valid rows + mask layer
-- ============================================================

/-- ME-11 — every valid memory→memory edge's relation class is legal for
    its endpoint kinds. THEOREM: a valid edge satisfies its mask, and
    masks only tighten the matrix. -/
theorem edge_class_legal :
    ∀ (e : Edge) (c : RelationClass), EdgeHasClass e c → ∀ (ms mt : Memory),
      edge_source e = .memory ms → edge_target e = .memory mt →
      c ∈ legalClasses (memory_kind ms) (memory_kind mt) := by
  intro e c hclass ms mt hs ht
  rcases hclass with ⟨d, h, hd⟩
  have hmask := h.mask
  unfold EdgeMaskValidWith at hmask
  rw [hs, ht] at hmask
  have hlegal := d.masksTightenOnly ms mt hmask
  rw [hd] at hlegal
  exact hlegal

/-- ME-10 — ℓ(source) ≥ ℓ(target) for valid memory→memory edges.
    THEOREM: the matrix's upward cells admit no class at all. -/
theorem edge_layer_rule :
    ∀ (e : Edge), EdgeCoreValid e → ∀ (ms mt : Memory),
      edge_source e = .memory ms → edge_target e = .memory mt →
      (memory_kind mt).layer ≤ (memory_kind ms).layer := by
  intro e hvalid ms mt hs ht
  rcases hvalid with ⟨d, h⟩
  have h := edge_class_legal e d.relClass ⟨d, h, rfl⟩ ms mt hs ht
  revert h
  cases memory_kind ms <;> cases memory_kind mt <;> intro h <;>
    first
      | exact h.elim
      | simp [MemoryKind.layer]

-- ============================================================
-- Memory supersession — PROVED from valid Supersession-class edges
-- (doc 02 §Re-derivation and Supersession)
-- ============================================================

/-- Memory supersession is not a Memory row field. It is the existence
    of a valid Supersession-class edge from the new row to the old row. -/
def memorySupersedes (new old : Memory) : Prop :=
  ∃ e : Edge,
    EdgeHasClass e .Supersession ∧
    edge_source e = .memory new ∧
    edge_target e = .memory old

/-- ME-5a — supersession endpoint kind must match (doc 02). THEOREM:
    only the A→A and P→P matrix cells admit Supersession. -/
theorem supersession_same_kind :
    ∀ (e : Edge), ∀ (m m' : Memory),
      EdgeHasClass e .Supersession →
      edge_source e = .memory m →
      edge_target e = .memory m' →
      memory_kind m = memory_kind m' := by
  intro e m m' hsup hs ht
  have hleg := edge_class_legal e .Supersession hsup m m' hs ht
  revert hleg
  cases memory_kind m <;> cases memory_kind m' <;> intro hleg <;>
    first
      | rfl
      | exact hleg.elim
      | (rcases hleg with h' | h' <;> first | exact (nomatch h') | rcases h' with h'' | h'' <;> first | exact (nomatch h'') | rcases h'' with h3 | h3 <;> exact (nomatch h3))

/-- ME-4 — "Facts never supersede and are never superseded"
    (doc 02, verbatim). THEOREM: no Fact cell admits Supersession. -/
theorem facts_never_supersede :
    ∀ (e : Edge), ∀ (m m' : Memory),
      EdgeHasClass e .Supersession →
      edge_source e = .memory m →
      edge_target e = .memory m' →
      memory_kind m ≠ .Fact ∧ memory_kind m' ≠ .Fact := by
  intro e m m' hsup hs ht
  have hleg := edge_class_legal e .Supersession hsup m m' hs ht
  have hk := supersession_same_kind e m m' hsup hs ht
  constructor
  · intro hf
    rw [hf, ← hk, hf] at hleg
    rcases hleg with h' | h' <;> exact (nomatch h')
  · intro hf
    rw [hf] at hk
    rw [hk, hf] at hleg
    rcases hleg with h' | h' <;> exact (nomatch h')

/-- ME-5b — supersession stays within one Owner. THEOREM: from the
    valid Supersession-class edge and Supersession intra-Owner validity. -/
theorem supersession_same_owner :
    ∀ (e : Edge), ∀ (m m' : Memory),
      EdgeHasClass e .Supersession →
      edge_source e = .memory m →
      edge_target e = .memory m' →
      memory_owner m = memory_owner m' := by
  intro e m m' hsup hs ht
  have htgt := supersession_intra_owner e hsup
  rw [hs, ht] at htgt
  -- htgt : NodeRef.owner (.memory m') = NodeRef.owner (.memory m)
  --      ≡ memory_owner m' = memory_owner m
  exact htgt.symm

/-- ME-12 — Supersession-class edges connect same-shaped endpoints:
    memory→memory (same kind — that part PROVABLE from the matrix) or
    Goal→Goal. The residue the matrix cannot supply — that a
    Supersession edge never mixes a Memory endpoint with a Goal
    endpoint — is row validity (doc 02 §Relation Registry:
    `core/supersedes` is A→A, P→P, Goal→Goal). -/
theorem supersession_same_endpoint_shape :
    ∀ e : Edge,
      EdgeHasClass e .Supersession →
      ((∃ ms mt : Memory, edge_source e = .memory ms ∧ edge_target e = .memory mt ∧
          memory_kind ms = memory_kind mt) ∨
       (∃ gs gt : Goal, edge_source e = .goal gs ∧ edge_target e = .goal gt)) := by
  intro e hsup
  rcases hsup with ⟨d, h, hd⟩
  have hshape := h.supersessionEndpointShape hd
  rcases hshape with hmem | hgoal
  · rcases hmem with ⟨ms, mt, hs, ht⟩
    exact Or.inl ⟨ms, mt, hs, ht, supersession_same_kind e ms mt ⟨d, h, hd⟩ hs ht⟩
  · exact Or.inr hgoal

end Causa
