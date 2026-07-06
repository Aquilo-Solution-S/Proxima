/-
Causa — Edges

Edges connect pinned Memories, Goals, and FollowHead FactEntity handles
(doc 02 §Edges plus FactEntity endpoint design). Every relation resolves to a
build-time RelationDescriptor; unregistered relations are invalid.

The philosophical load (universe §Perspectivist constructivism):
causal claims are perspective-relative, so **Perspective is the locus
of causal claims, never Facts**. Direct semantic or causal Fact→Fact
edges are forbidden — "cosine similarity is observer-independent and
so cannot encode an observer-relative relation". The class-legality
matrix below is where that commitment becomes structural.

Minimized trusted core (D14/D16): `Edge` is a raw row shape;
row-admission laws are predicates over valid rows, not global axioms
over every constructible Lean value. Relation-specific policy lives on
`RelationDescriptor` rows: endpoint binding, endpoint masks, ownership
policy, target access policy, and mask-tightening proofs travel together.

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

/-- Durable endpoint binding for one relation side. `Pin` means the edge names
    an exact Memory/Goal row. `FollowHead` means the edge names a FactEntity
    aggregate and resolves through its current Fact head. -/
inductive EndpointBinding where
  | Pin
  | FollowHead
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

/-- An edge endpoint: a pinned Memory/Goal row or a FollowHead stateful-Fact
    aggregate. `FactEntity` is not a fifth semantic node kind: it resolves to
    its current Fact memory and is treated as Fact for layer/class checks. -/
inductive NodeRef where
  | memory     (m : Memory)
  | goal       (g : Goal)
  | factEntity (e : FactEntity)

def NodeRef.owner : NodeRef → Owner
  | .memory m     => memory_owner m
  | .goal g       => goal_owner g
  | .factEntity e => fact_entity_owner e

def NodeRef.schema : NodeRef → SchemaRef
  | .memory m     => memory_schema m
  | .goal g       => goal_schema g
  | .factEntity e => fact_entity_schema e

/-- Memory-kind view for endpoints that participate in F/A/P layer rules.
    FactEntity endpoints are Fact-like; Goal endpoints sit outside the F/A/P
    comparison. -/
def NodeRef.memoryKind? : NodeRef → Option MemoryKind
  | .memory m     => some (memory_kind m)
  | .goal _       => none
  | .factEntity _ => some .Fact

/-- FollowHead endpoints resolve to the current Fact version. -/
def NodeRef.resolvedFact? : NodeRef → Option Fact
  | .factEntity e => some (fact_entity_current e)
  | .memory m =>
      if h : memory_kind m = .Fact then some ⟨m, h⟩ else none
  | .goal _ => none

/-- Binding alignment between a relation side and the concrete endpoint ref. -/
def endpointBindingAligned : EndpointBinding → NodeRef → Prop
  | .Pin, .memory _ => True
  | .Pin, .goal _ => True
  | .Pin, .factEntity _ => False
  | .FollowHead, .factEntity _ => True
  | .FollowHead, .memory _ => False
  | .FollowHead, .goal _ => False

/-- A FollowHead endpoint is always Fact-like. -/
theorem followHeadEndpointIsFact :
    ∀ ref : NodeRef,
      endpointBindingAligned .FollowHead ref → ref.memoryKind? = some .Fact := by
  intro ref h
  cases ref <;> simp [endpointBindingAligned, NodeRef.memoryKind?] at h ⊢

/-- Pin endpoints never use the FactEntity aggregate handle. -/
theorem pinEndpointIsPinnedRow :
    ∀ ref : NodeRef,
      endpointBindingAligned .Pin ref →
        (∃ m : Memory, ref = .memory m) ∨ (∃ g : Goal, ref = .goal g) := by
  intro ref h
  cases ref with
  | memory m => exact Or.inl ⟨m, rfl⟩
  | goal g => exact Or.inr ⟨g, rfl⟩
  | factEntity _ => cases h

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
    the matrix row: class, endpoint binding mode, endpoint mask, source/target
    ownership policy, write-admission target policy, and the proof that every
    memory-like endpoint pair (including FollowHead FactEntity refs) only
    tightens the closed F/A/P class matrix. -/
structure RelationDescriptor where
  id : RelationId
  relClass : RelationClass
  sourceBinding : EndpointBinding
  targetBinding : EndpointBinding
  ownerPolicy : RelationOwnerPolicy
  targetAccessPolicy : RelationTargetAccessPolicy
  endpointAdmitted : NodeRef → NodeRef → Prop
  masksTightenOnly :
    ∀ (s t : NodeRef) (ks kt : MemoryKind),
      endpointAdmitted s t →
      s.memoryKind? = some ks →
      t.memoryKind? = some kt →
      relClass ∈ legalClasses ks kt
  supersessionSameOwner : relClass = .Supersession → ownerPolicy = .SameOwner

/-- Minimal build-time relation registry. This is deliberately narrower than
    the deleted Composition module: no flavor ontology, runtime registration,
    tool registry, or schema namespace model. It only says valid edge rows are
    checked against a frozen set of relation descriptors, with unique ids. -/
structure RelationRegistry where
  descriptors : Set RelationDescriptor
  relationIdUnique :
    ∀ d₁ d₂ : RelationDescriptor,
      d₁ ∈ descriptors →
      d₂ ∈ descriptors →
      d₁.id = d₂.id →
      d₁ = d₂

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

/-- A valid edge's concrete endpoint refs match the descriptor's durable binding
    mode: Pin uses Memory/Goal rows; FollowHead uses FactEntity refs. -/
def EdgeEndpointBindingValidWith (d : RelationDescriptor) (e : Edge) : Prop :=
  endpointBindingAligned d.sourceBinding (edge_source e) ∧
  endpointBindingAligned d.targetBinding (edge_target e)

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
  endpointBinding : EdgeEndpointBindingValidWith d e
  ownerPolicy : EdgeOwnerPolicyValidWith d e
  mask : EdgeMaskValidWith d e
  supersessionEndpointShape : EdgeSupersessionEndpointShapeValidWith d.relClass e

/-- Core validity for one persisted Edge row under the active build-time
    relation registry. The descriptor witness must be registered; ad-hoc
    descriptors cannot validate rows. -/
def EdgeCoreValid (registry : RelationRegistry) (e : Edge) : Prop :=
  ∃ d : RelationDescriptor, d ∈ registry.descriptors ∧ EdgeValidWith d e

/-- A valid edge classified by its registered validating descriptor. This
    replaces the former global `relation_class : RelationId → RelationClass`
    accessor. -/
def EdgeHasClass (registry : RelationRegistry) (e : Edge) (c : RelationClass) : Prop :=
  ∃ d : RelationDescriptor, d ∈ registry.descriptors ∧ EdgeValidWith d e ∧ d.relClass = c

/-- Class evidence already includes ordinary core validity under the same
    registry. -/
theorem edge_has_class_core_valid :
    ∀ registry e c, EdgeHasClass registry e c → EdgeCoreValid registry e := by
  intro registry e c h
  rcases h with ⟨d, hregistered, hvalid, _⟩
  exact ⟨d, hregistered, hvalid⟩

/-- Registered relation ids resolve to a unique descriptor for a given edge. -/
theorem registered_edge_descriptor_unique :
    ∀ (registry : RelationRegistry) (e : Edge) (d₁ d₂ : RelationDescriptor),
      d₁ ∈ registry.descriptors →
      d₂ ∈ registry.descriptors →
      EdgeValidWith d₁ e →
      EdgeValidWith d₂ e →
      d₁ = d₂ := by
  intro registry e d₁ d₂ h₁ h₂ hv₁ hv₂
  exact registry.relationIdUnique d₁ d₂ h₁ h₂ (hv₁.relationMatches.symm.trans hv₂.relationMatches)

/-- Table-scoped Edge validity under one frozen registry. -/
def EdgeTableValid (registry : RelationRegistry) (edges : Set Edge) : Prop :=
  ∀ e : Edge, e ∈ edges → EdgeCoreValid registry e

-- ============================================================
-- Goal assignment and evidence queries (doc 06 §Goal Assignment)
-- ============================================================

/-- GO-12 — Goal assignment to a Self-Perspective, without introducing a Self
    entity or pinning a named `core/inspires` relation id in Lean. The kernel
    face is the registered Causal Goal→Perspective edge shape; the concrete
    relation id remains build-time vocabulary. -/
def goalAssignedToPerspective
    (registry : RelationRegistry) (edges : Set Edge) (goal : Goal) (self : Memory) : Prop :=
  memory_kind self = .Perspective ∧
  ∃ e : Edge,
    e ∈ edges ∧
    EdgeHasClass registry e .Causal ∧
    edge_source e = .goal goal ∧
    edge_target e = .memory self

/-- Projection: an assignment target is a Perspective row. -/
theorem goal_assignment_target_perspective :
    ∀ registry edges goal self,
      goalAssignedToPerspective registry edges goal self → memory_kind self = .Perspective := by
  intro _ _ _ _ h
  exact h.1

/-- GO-12 — active goals for a queried Self-Perspective: begin at assigned
    Goal sources, follow Goal supersession inside the Goal table, and return
    only current Active heads. This is a query over Goals+Edges, not a Self row. -/
def activeGoalsForSelf
    (registry : RelationRegistry) (goals : Set Goal) (edges : Set Edge)
    (self : Memory) : Set Goal :=
  fun head =>
    memory_kind self = .Perspective ∧
    ∃ source : Goal,
      source ∈ goals ∧
      goalAssignedToPerspective registry edges source self ∧
      activeGoalHeadFrom goals source head

/-- Projection: every Self-assigned active Goal is Active. -/
theorem active_goal_for_self_active :
    ∀ registry goals edges self head,
      head ∈ activeGoalsForSelf registry goals edges self → goal_state head = .Active := by
  intro registry goals edges self head h
  rcases h with ⟨_, source, _, _, hhead⟩
  exact active_goal_head_from_active goals source head hhead

/-- Projection: every Self-assigned active Goal is a lifecycle head. -/
theorem active_goal_for_self_head :
    ∀ registry goals edges self head,
      head ∈ activeGoalsForSelf registry goals edges self → goalIsHead goals head := by
  intro registry goals edges self head h
  rcases h with ⟨_, source, _, _, hhead⟩
  exact active_goal_head_from_head goals source head hhead

/-- Projection: Self-assigned active Goals come from Perspective-targeted
    assignment, not from an owner-only active-goal scan. -/
theorem active_goal_for_self_has_assignment :
    ∀ registry goals edges self head,
      head ∈ activeGoalsForSelf registry goals edges self →
        ∃ source : Goal,
          source ∈ goals ∧
          goalAssignedToPerspective registry edges source self ∧
          activeGoalHeadFrom goals source head := by
  intro registry goals edges self head h
  exact h.2

/-- Goal evidence edge shape: Goal → Fact/Abstraction evidence. In the Rust
    vocabulary this is `core/motivated-by`; Lean keeps the relation id opaque
    and records the kernel-visible Structural shape. -/
def goalEvidenceEdge
    (registry : RelationRegistry) (edges : Set Edge) (goal : Goal) (memory : Memory) : Prop :=
  ∃ e : Edge,
    e ∈ edges ∧
    EdgeHasClass registry e .Structural ∧
    edge_source e = .goal goal ∧
    edge_target e = .memory memory ∧
    (memory_kind memory = .Fact ∨ memory_kind memory = .Abstraction)

/-- GO-14/GO-16 — table-scoped evidence requirement for operator-authored
    Goals. User/External Goals may be intent without evidence here; A→Goal
    operator output must carry a Goal→Fact/Abstraction evidence edge. -/
def GoalEvidenceValid
    (registry : RelationRegistry) (goals : Set Goal) (memories : Set Memory)
    (edges : Set Edge) : Prop :=
  ∀ g : Goal,
    g ∈ goals →
    goal_authorship g = .SystemOperator →
      ∃ m : Memory, m ∈ memories ∧ goalEvidenceEdge registry edges g m

/-- Projection: every SystemOperator Goal has table-resolved evidence. -/
theorem system_operator_goal_has_evidence :
    ∀ registry goals memories edges,
      GoalEvidenceValid registry goals memories edges →
      ∀ g : Goal,
        g ∈ goals →
        goal_authorship g = .SystemOperator →
          ∃ m : Memory, m ∈ memories ∧ goalEvidenceEdge registry edges g m := by
  intro registry goals memories edges hvalid g hg hauth
  exact hvalid g hg hauth

/-- Projection: Goal evidence never points at a Perspective. -/
theorem goal_evidence_not_perspective :
    ∀ registry edges g m,
      goalEvidenceEdge registry edges g m → memory_kind m ≠ .Perspective := by
  intro registry edges g m h hperspective
  rcases h with ⟨_, _, _, _, _, hkind⟩
  rcases hkind with hfact | habstraction
  · rw [hfact] at hperspective
    exact (nomatch hperspective)
  · rw [habstraction] at hperspective
    exact (nomatch hperspective)

-- ============================================================
-- Validity projection theorems
-- ============================================================

/-- Former `edge_id_authorship_split` axiom, now projected from registered row validity. -/
theorem edge_id_authorship_split :
    ∀ registry e, EdgeCoreValid registry e →
      ((∃ h : ContentHash, edge_id e = .sourceAuthored h) ↔
        edge_authorship e = .SourceIngest) := by
  intro registry e hvalid
  rcases hvalid with ⟨d, _, h⟩
  exact h.idAuthorship

/-- Former `causal_goal_edge_perspectival` axiom, now projected from registered row validity. -/
theorem causal_goal_edge_perspectival :
    ∀ registry e, EdgeHasClass registry e .Causal →
      ((∃ g : Goal, edge_source e = .goal g) ∨
        (∃ g : Goal, edge_target e = .goal g)) →
      edge_authorship e = .PerspectiveGoalLink := by
  intro registry e hclass hgoal
  rcases hclass with ⟨d, _, h, hd⟩
  exact h.goalCausal hd hgoal

/-- Former `edge_source_owned` axiom, now projected from registered row validity. -/
theorem edge_source_owned :
    ∀ registry e, EdgeCoreValid registry e → (edge_source e).owner = edge_owner e := by
  intro registry e hvalid
  rcases hvalid with ⟨d, _, h⟩
  exact h.sourceOwned

/-- Former `supersession_intra_owner` axiom, now projected from descriptor
    owner policy: Supersession-class descriptors are `.SameOwner`, and a
    valid edge satisfies that descriptor policy. -/
theorem supersession_intra_owner :
    ∀ registry e, EdgeHasClass registry e .Supersession →
      (edge_target e).owner = (edge_source e).owner := by
  intro registry e hclass
  rcases hclass with ⟨d, _, h, hd⟩
  have hpolicy : d.ownerPolicy = .SameOwner := d.supersessionSameOwner hd
  have howner := h.ownerPolicy
  unfold EdgeOwnerPolicyValidWith ownerPolicySatisfied at howner
  rw [hpolicy] at howner
  exact howner

/-- Former `edge_respects_mask` axiom, now projected from the registered
    descriptor witness used to validate the row. -/
theorem edge_respects_mask :
    ∀ registry e, EdgeCoreValid registry e →
      ∃ d : RelationDescriptor,
        d ∈ registry.descriptors ∧
        EdgeValidWith d e ∧ d.endpointAdmitted (edge_source e) (edge_target e) := by
  intro registry e hvalid
  rcases hvalid with ⟨d, hregistered, h⟩
  exact ⟨d, hregistered, h, h.mask⟩

/-- Projection: a FollowHead source endpoint is Fact-like. -/
theorem source_follow_head_endpoint_is_fact :
    ∀ (d : RelationDescriptor) (e : Edge),
      EdgeValidWith d e →
      d.sourceBinding = .FollowHead →
      (edge_source e).memoryKind? = some .Fact := by
  intro d e hvalid hbinding
  have h := hvalid.endpointBinding.1
  rw [hbinding] at h
  exact followHeadEndpointIsFact (edge_source e) h

/-- Projection: a FollowHead target endpoint is Fact-like. -/
theorem target_follow_head_endpoint_is_fact :
    ∀ (d : RelationDescriptor) (e : Edge),
      EdgeValidWith d e →
      d.targetBinding = .FollowHead →
      (edge_target e).memoryKind? = some .Fact := by
  intro d e hvalid hbinding
  have h := hvalid.endpointBinding.2
  rw [hbinding] at h
  exact followHeadEndpointIsFact (edge_target e) h

-- ============================================================
-- ME-11 and ME-10 — PROVED from valid rows + mask layer
-- ============================================================

/-- ME-11 — every valid edge whose endpoints are memory-like (Memory rows or
    FollowHead FactEntity refs) has a relation class legal for the resolved
    endpoint kinds. THEOREM: valid edges satisfy their mask, and masks only
    tighten the matrix. -/
theorem edge_class_legal_for_node :
    ∀ registry (e : Edge) (c : RelationClass), EdgeHasClass registry e c →
      ∀ (ks kt : MemoryKind),
        (edge_source e).memoryKind? = some ks →
        (edge_target e).memoryKind? = some kt →
        c ∈ legalClasses ks kt := by
  intro registry e c hclass ks kt hs ht
  rcases hclass with ⟨d, _, h, hd⟩
  have hmask := h.mask
  unfold EdgeMaskValidWith at hmask
  have hlegal := d.masksTightenOnly (edge_source e) (edge_target e) ks kt hmask hs ht
  rw [hd] at hlegal
  exact hlegal

/-- ME-11 — memory→memory specialization of `edge_class_legal_for_node`. -/
theorem edge_class_legal :
    ∀ registry (e : Edge) (c : RelationClass), EdgeHasClass registry e c → ∀ (ms mt : Memory),
      edge_source e = .memory ms → edge_target e = .memory mt →
      c ∈ legalClasses (memory_kind ms) (memory_kind mt) := by
  intro registry e c hclass ms mt hs ht
  apply edge_class_legal_for_node registry e c hclass
  · rw [hs]
    rfl
  · rw [ht]
    rfl

/-- ME-10 — ℓ(source) ≥ ℓ(target) for valid memory→memory edges.
    THEOREM: the matrix's upward cells admit no class at all. -/
theorem edge_layer_rule :
    ∀ registry (e : Edge), EdgeCoreValid registry e → ∀ (ms mt : Memory),
      edge_source e = .memory ms → edge_target e = .memory mt →
      (memory_kind mt).layer ≤ (memory_kind ms).layer := by
  intro registry e hvalid ms mt hs ht
  rcases hvalid with ⟨d, hregistered, h⟩
  have h := edge_class_legal registry e d.relClass ⟨d, hregistered, h, rfl⟩ ms mt hs ht
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
def memorySupersedes (registry : RelationRegistry) (new old : Memory) : Prop :=
  ∃ e : Edge,
    EdgeHasClass registry e .Supersession ∧
    edge_source e = .memory new ∧
    edge_target e = .memory old

/-- Table-scoped memory supersession: the superseding edge must be present in the
    admitted Edge table. This is the query shape consumers need for current-head
    projections; raw `memorySupersedes` remains only the relation predicate. -/
def memorySupersedesInTable
    (registry : RelationRegistry) (edges : Set Edge) (new old : Memory) : Prop :=
  ∃ e : Edge,
    e ∈ edges ∧
    EdgeHasClass registry e .Supersession ∧
    edge_source e = .memory new ∧
    edge_target e = .memory old

/-- Projection: table-scoped supersession is ordinary memory supersession. -/
theorem memory_supersedes_in_table :
    ∀ registry edges new old,
      memorySupersedesInTable registry edges new old →
      memorySupersedes registry new old := by
  intro registry edges new old h
  rcases h with ⟨e, _he, hclass, hsource, htarget⟩
  exact ⟨e, hclass, hsource, htarget⟩

/-- A Memory lifecycle head in the actual admitted tables: the row is present and
    no later admitted Memory row supersedes it through an admitted Supersession
    edge. -/
def memoryIsHead
    (registry : RelationRegistry) (memories : Set Memory) (edges : Set Edge)
    (m : Memory) : Prop :=
  m ∈ memories ∧
  ¬ ∃ m' : Memory, m' ∈ memories ∧ memorySupersedesInTable registry edges m' m

/-- Generic current Memory-head query. -/
def memoryHeads
    (registry : RelationRegistry) (memories : Set Memory) (edges : Set Edge) :
    Set Memory :=
  fun m => memoryIsHead registry memories edges m

/-- Generic current Perspective-head query. Downstream apps can add their own
    schema/payload filters; the kernel only supplies the F/A/P head shape. -/
def perspectiveHeads
    (registry : RelationRegistry) (memories : Set Memory) (edges : Set Edge) :
    Set Memory :=
  fun m => memory_kind m = .Perspective ∧ memoryIsHead registry memories edges m

/-- Superseded rows are not Memory lifecycle heads. -/
theorem memory_superseded_not_head :
    ∀ registry memories edges old new,
      new ∈ memories →
      memorySupersedesInTable registry edges new old →
      ¬ memoryIsHead registry memories edges old := by
  intro registry memories edges old new hnew hsup hhead
  exact hhead.2 ⟨new, hnew, hsup⟩

/-- Projection: a Perspective head is a Perspective. -/
theorem perspective_head_is_perspective :
    ∀ registry memories edges m,
      m ∈ perspectiveHeads registry memories edges →
      memory_kind m = .Perspective := by
  intro registry memories edges m h
  exact h.1

/-- Projection: a Perspective head is also a Memory head. -/
theorem perspective_head_is_memory_head :
    ∀ registry memories edges m,
      m ∈ perspectiveHeads registry memories edges →
      memoryIsHead registry memories edges m := by
  intro registry memories edges m h
  exact h.2

/-- ME-5a — supersession endpoint kind must match (doc 02). THEOREM:
    only the A→A and P→P matrix cells admit Supersession. -/
theorem supersession_same_kind :
    ∀ registry (e : Edge), ∀ (m m' : Memory),
      EdgeHasClass registry e .Supersession →
      edge_source e = .memory m →
      edge_target e = .memory m' →
      memory_kind m = memory_kind m' := by
  intro registry e m m' hsup hs ht
  have hleg := edge_class_legal registry e .Supersession hsup m m' hs ht
  revert hleg
  cases memory_kind m <;> cases memory_kind m' <;> intro hleg <;>
    first
      | rfl
      | exact hleg.elim
      | (rcases hleg with h' | h' <;> first | exact (nomatch h') | rcases h' with h'' | h'' <;> first | exact (nomatch h'') | rcases h'' with h3 | h3 <;> exact (nomatch h3))

/-- ME-4 — "Facts never supersede and are never superseded"
    (doc 02, verbatim). THEOREM: no Fact cell admits Supersession. -/
theorem facts_never_supersede :
    ∀ registry (e : Edge), ∀ (m m' : Memory),
      EdgeHasClass registry e .Supersession →
      edge_source e = .memory m →
      edge_target e = .memory m' →
      memory_kind m ≠ .Fact ∧ memory_kind m' ≠ .Fact := by
  intro registry e m m' hsup hs ht
  have hleg := edge_class_legal registry e .Supersession hsup m m' hs ht
  have hk := supersession_same_kind registry e m m' hsup hs ht
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
    ∀ registry (e : Edge), ∀ (m m' : Memory),
      EdgeHasClass registry e .Supersession →
      edge_source e = .memory m →
      edge_target e = .memory m' →
      memory_owner m = memory_owner m' := by
  intro registry e m m' hsup hs ht
  have htgt := supersession_intra_owner registry e hsup
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
    ∀ registry e,
      EdgeHasClass registry e .Supersession →
      ((∃ ms mt : Memory, edge_source e = .memory ms ∧ edge_target e = .memory mt ∧
          memory_kind ms = memory_kind mt) ∨
       (∃ gs gt : Goal, edge_source e = .goal gs ∧ edge_target e = .goal gt)) := by
  intro registry e hsup
  rcases hsup with ⟨d, hregistered, h, hd⟩
  have hshape := h.supersessionEndpointShape hd
  rcases hshape with hmem | hgoal
  · rcases hmem with ⟨ms, mt, hs, ht⟩
    exact Or.inl ⟨ms, mt, hs, ht, supersession_same_kind registry e ms mt ⟨d, hregistered, h, hd⟩ hs ht⟩
  · exact Or.inr hgoal

end Causa
