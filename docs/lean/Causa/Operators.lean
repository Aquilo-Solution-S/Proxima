/-
Causa — Operators

The production rules (doc 02 §The Layering Principle, doc 04):

  F→A   : 2^F × wake context → A      facts become one typed Abstraction
  A→P   : 2^A × wake context → P      abstractions become one typed Perspective
  A→Goal: 2^A × wake context → Goal   goals derived from Abstraction evidence
  frame : P × A_cross → Edge

Wake context is not a materialized PersonalityInstance. In spec-mode
Lean, operators are not modeled as functions — entities plus
write-shape obligations carry the same content: what each authorship
class may produce, and what every derived memory must possess.

Minimized trusted core (D14): operator edge shape is row validity, not
a global axiom over all raw `Edge` values. Target-kind conjuncts of
CN-1/CN-2 and of the provenance obligation are PROVED from valid edge
rows plus the edge matrix (Provenance pins the target kind uniquely
given the source kind).

CN-5 — no downward writes (A→F, P→A, P→F): the kernel face is the
operator edge shape plus the class-legality matrix. Source/flavor
ingest may materialize typed Facts, but the core kernel does not model
a separate Event entity or materialized PersonalityInstance. "Dreaming" needs no axioms: dream outputs are ordinary
typed writes under these same rules (doc 02 §Wake/Dream/Write — no
Dream entity, no Dream relation class, no Core dream pipeline; the
ABSENCE of dream primitives here is deliberate).

CN-9 (atomic invocation) and the wake-dispatcher loop are
storage/runtime contracts, not kernel axioms — same stance WH takes
on event/projection atomicity. Recorded as exclusions in COVERAGE.md.
-/

import Causa.Prelude
import Causa.Owner
import Causa.Identity
import Causa.Memory
import Causa.Goals
import Causa.Edges

namespace Causa

-- ============================================================
-- Operator edge shapes (doc 02 §Provenance, §Edge Scope authorship;
-- doc 02 §Relation Registry for motivated-by's Structural class)
-- ============================================================

/-- What each authorship class is permitted to write — CN-1..CN-4 as
    one def over the closed `EdgeAuthorship` vocabulary (the same
    move `legalClasses` makes for kinds). Non-operator authorships
    (source-ingest, Engine, User, ExternalAgent) carry no extra shape
    here: their legality is the matrix + masks. Target kind for A→P
    remains matrix-forced; F→A states its Fact target directly because
    A→A provenance is legal. -/
def operatorEdgeShape (registry : RelationRegistry) : EdgeAuthorship → Edge → Prop
  | .OperatorFtoA, e =>
      EdgeHasClass registry e .Provenance ∧
      (∃ ms : Memory, edge_source e = .memory ms ∧ memory_kind ms = .Abstraction) ∧
      (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt = .Fact)
  | .OperatorAtoA, e =>
      EdgeHasClass registry e .Provenance ∧
      (∃ ms : Memory, edge_source e = .memory ms ∧ memory_kind ms = .Abstraction) ∧
      (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt = .Abstraction)
  | .OperatorAtoP, e =>
      EdgeHasClass registry e .Provenance ∧
      (∃ ms : Memory, edge_source e = .memory ms ∧ memory_kind ms = .Perspective) ∧
      (∃ mt : Memory, edge_target e = .memory mt)
  | .OperatorAtoGoal, e =>
      EdgeHasClass registry e .Structural ∧
      (∃ g : Goal, edge_source e = .goal g) ∧
      (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt ≠ .Perspective)
  | .PerspectiveLink, e =>
      (EdgeHasClass registry e .Causal ∨ EdgeHasClass registry e .Interpretive) ∧
      (∃ ms : Memory, edge_source e = .memory ms ∧ memory_kind ms = .Perspective)
  | .PerspectiveGoalLink, e =>
      EdgeHasClass registry e .Causal ∧
      (∃ g : Goal, edge_source e = .goal g) ∧
      ((∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt = .Fact) ∨
       (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt = .Perspective))
  | _, _ => True

/-- CN-1..CN-4 — authorship-shape validity for one persisted Edge row.
    This is row validity, not a global property of raw `Edge` values. -/
def EdgeOperatorShapeValid (registry : RelationRegistry) (e : Edge) : Prop :=
  operatorEdgeShape registry (edge_authorship e) e

/-- Former `operator_edges_shaped` axiom, now projected from row validity. -/
theorem operator_edges_shaped :
    ∀ registry e, EdgeOperatorShapeValid registry e →
      operatorEdgeShape registry (edge_authorship e) e := by
  intro _ _ h
  exact h

/-- CN-5 — operator memory outputs are never Facts. The output side of
    F→A/A→A/A→P operator edges is the source endpoint by Proxima's
    provenance direction convention (`new -> inputs`). -/
theorem operator_memory_output_not_fact :
    ∀ registry (e : Edge) (m : Memory),
      EdgeOperatorShapeValid registry e →
      (edge_authorship e = .OperatorFtoA ∨
       edge_authorship e = .OperatorAtoA ∨
       edge_authorship e = .OperatorAtoP) →
      edge_source e = .memory m →
      memory_kind m ≠ .Fact := by
  intro registry e m hshape ha hsout
  rcases ha with hfa | ha
  · have h := operator_edges_shaped registry e hshape
    rw [hfa] at h
    rcases h with ⟨_, ⟨ms, hsrc, hkind⟩, _⟩
    rw [hsout] at hsrc
    injection hsrc with heq
    rw [heq, hkind]
    intro hfalse
    exact (nomatch hfalse)
  · rcases ha with haa | hap
    · have h := operator_edges_shaped registry e hshape
      rw [haa] at h
      rcases h with ⟨_, ⟨ms, hsrc, hkind⟩, _⟩
      rw [hsout] at hsrc
      injection hsrc with heq
      rw [heq, hkind]
      intro hfalse
      exact (nomatch hfalse)
    · have h := operator_edges_shaped registry e hshape
      rw [hap] at h
      rcases h with ⟨_, ⟨ms, hsrc, hkind⟩, _⟩
      rw [hsout] at hsrc
      injection hsrc with heq
      rw [heq, hkind]
      intro hfalse
      exact (nomatch hfalse)

/-- Helper: a valid Provenance-class memory→memory edge with a known source
    kind has its target kind pinned by the matrix. -/
theorem provenance_pins_target :
    ∀ registry (e : Edge), EdgeHasClass registry e .Provenance → ∀ (ms mt : Memory),
      edge_source e = .memory ms → edge_target e = .memory mt →
      (memory_kind ms = .Abstraction →
        memory_kind mt = .Fact ∨ memory_kind mt = .Abstraction) ∧
      (memory_kind ms = .Perspective → memory_kind mt = .Abstraction) := by
  intro registry e hc ms mt hs ht
  have hleg := edge_class_legal registry e .Provenance hc ms mt hs ht
  constructor
  · intro hk
    rw [hk] at hleg
    revert hleg
    cases memory_kind mt <;> intro hleg <;>
      first
        | exact Or.inl rfl
        | exact Or.inr rfl
        | exact hleg.elim
        | (rcases hleg with h' | h' <;> first | exact (nomatch h') | exact (nomatch h'))
  · intro hk
    rw [hk] at hleg
    revert hleg
    cases memory_kind mt <;> intro hleg <;>
      first
        | rfl
        | exact hleg.elim
        | (rcases hleg with h' | h'
           · exact (nomatch h')
           · rcases h' with h'' | h''
             · exact (nomatch h'')
             · rcases h'' with h3 | h3 <;> exact (nomatch h3))

/-- CN-1 in full — F→A writes `Abstraction → Fact` provenance edges. -/
theorem ftoa_edge_shape :
    ∀ registry e, EdgeOperatorShapeValid registry e → edge_authorship e = .OperatorFtoA →
      EdgeHasClass registry e .Provenance ∧
      (∃ ms : Memory, edge_source e = .memory ms ∧ memory_kind ms = .Abstraction) ∧
      (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt = .Fact) := by
  intro registry e hshape ha
  have h := operator_edges_shaped registry e hshape
  rw [ha] at h
  exact h

/-- CN-2 in full — A→P writes `Perspective → Abstraction` provenance
    edges. THEOREM: the target kind is matrix-forced. -/
theorem atop_edge_shape :
    ∀ registry e, EdgeOperatorShapeValid registry e →
      edge_authorship e = .OperatorAtoP →
      EdgeHasClass registry e .Provenance ∧
      (∃ ms : Memory, edge_source e = .memory ms ∧ memory_kind ms = .Perspective) ∧
      (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt = .Abstraction) := by
  intro registry e hshape ha
  have h := operator_edges_shaped registry e hshape
  rw [ha] at h
  obtain ⟨hc, ⟨ms, hs, hk⟩, ⟨mt, ht⟩⟩ := h
  exact ⟨hc, ⟨ms, hs, hk⟩,
    ⟨mt, ht, (provenance_pins_target registry e hc ms mt hs ht).2 hk⟩⟩

-- ============================================================
-- CN-6 — derived memories have provenance (doc 02 §Provenance)
-- ============================================================

/- CN-6 is table-scoped in `Causa.Provenance`: persisted derived rows
   require persisted registered Provenance edges to persisted input rows.
   This file keeps only the edge-shape machinery used by that table bundle. -/

-- ============================================================
-- CN-8 — F→A source-batch gate (doc 04 §Source-batch lifecycle,
-- §Phase 2 F→A exclusivity)
-- ============================================================

/-- Which operator produced a derived memory, and from which source
    batch. Opaque operator identity; reproducibility metadata
    (model id, prompt version, wake depth) stays engine-level row
    metadata (doc 04 §Idempotence — recorded, not axiomatized). -/
axiom OperatorId : Type
axiom memory_operator : Memory → Option OperatorId
axiom memory_source_batch : Memory → Option SourceBatchId

/-- The F→A input contract: the Fact-schema set the gate row keys on
    (doc 04 §Source-batch lifecycle: "Fact schema set | input
    contract"). Opaque — its content is a set of SchemaRefs
    engine-side; the kernel needs only its identity as a gate
    dimension. -/
axiom InputContract : Type
axiom memory_input_contract : Memory → Option InputContract

-- ============================================================
-- CN-8b — operator invocation ledger / input completeness
-- ============================================================

/-- The coarse operator phase whose input/output kinds are checked by the
    kernel. This is a ledger classification, not executable operator code. -/
inductive OperatorPhase where
  | ftoa
  | atoa
  | atop
  | atogoal
  deriving DecidableEq, Repr

/-- Phase input-kind contract. It says which declared input rows an invocation
    ledger may claim, not that retrieval found every relevant row in reality. -/
def OperatorPhase.inputKind : OperatorPhase → MemoryKind → Prop
  | .ftoa, k => k = .Fact
  | .atoa, k => k = .Abstraction
  | .atop, k => k = .Abstraction
  | .atogoal, k => k = .Abstraction

/-- Phase memory-output contract. A→Goal produces Goal rows, not Memory rows. -/
def OperatorPhase.outputMemoryKind : OperatorPhase → MemoryKind → Prop
  | .ftoa, k => k = .Abstraction
  | .atoa, k => k = .Abstraction
  | .atop, k => k = .Perspective
  | .atogoal, _ => False

/-- Phase goal-output contract. -/
def OperatorPhase.outputGoalAllowed : OperatorPhase → Prop
  | .atogoal => True
  | _ => False

/-- The edge authorship expected for edges emitted as part of one invocation's
    input/output ledger. -/
def OperatorPhase.edgeAuthorship : OperatorPhase → EdgeAuthorship → Prop
  | .ftoa, a => a = .OperatorFtoA
  | .atoa, a => a = .OperatorAtoA
  | .atop, a => a = .OperatorAtoP
  | .atogoal, a => a = .OperatorAtoGoal

/-- A ledger/manifest for one operator run: declared inputs, outputs, and edges.
    The kernel treats this as a consistency witness. It does not prove the input
    set was semantically complete, the prompt was good, or the model was right. -/
structure OperatorInvocation where
  phase : OperatorPhase
  operatorId : OperatorId
  inputContractId : InputContract
  inputs : Set Memory
  outputMemories : Set Memory
  outputGoals : Set Goal
  outputEdges : Set Edge

/-- Invocation rows reference admitted graph rows only. -/
structure InvocationInGraph
    (memories : Set Memory) (goals : Set Goal) (edges : Set Edge)
    (inv : OperatorInvocation) : Prop where
  inputsPresent : ∀ m : Memory, m ∈ inv.inputs → m ∈ memories
  outputMemoriesPresent : ∀ m : Memory, m ∈ inv.outputMemories → m ∈ memories
  outputGoalsPresent : ∀ g : Goal, g ∈ inv.outputGoals → g ∈ goals
  outputEdgesPresent : ∀ e : Edge, e ∈ inv.outputEdges → e ∈ edges

/-- Invocation phase/kind consistency. -/
structure InvocationShapeValid (inv : OperatorInvocation) : Prop where
  inputsShape : ∀ m : Memory, m ∈ inv.inputs →
    inv.phase.inputKind (memory_kind m)
  outputMemoriesShape : ∀ m : Memory, m ∈ inv.outputMemories →
    inv.phase.outputMemoryKind (memory_kind m)
  outputGoalsShape : ∀ g : Goal, g ∈ inv.outputGoals →
    inv.phase.outputGoalAllowed

/-- Invocation edge consistency under the active relation registry. -/
structure InvocationEdgeShapeValid (registry : RelationRegistry) (inv : OperatorInvocation) : Prop where
  outputEdgesShape : ∀ e : Edge, e ∈ inv.outputEdges →
    EdgeOperatorShapeValid registry e ∧ inv.phase.edgeAuthorship (edge_authorship e)

/-- Input completeness relative to the invocation ledger: every declared input is
    represented by a declared persisted edge from each relevant output. Memory
    outputs use Provenance; Goal outputs use Structural evidence. -/
structure InvocationProvenanceComplete
    (registry : RelationRegistry) (inv : OperatorInvocation) : Prop where
  memoryInputs : ∀ out : Memory, out ∈ inv.outputMemories →
    ∀ inp : Memory, inp ∈ inv.inputs →
      ∃ e : Edge, e ∈ inv.outputEdges ∧ EdgeHasClass registry e .Provenance ∧
        edge_source e = .memory out ∧ edge_target e = .memory inp
  goalInputs : ∀ g : Goal, g ∈ inv.outputGoals →
    ∀ inp : Memory, inp ∈ inv.inputs →
      ∃ e : Edge, e ∈ inv.outputEdges ∧ EdgeHasClass registry e .Structural ∧
        edge_source e = .goal g ∧ edge_target e = .memory inp

/-- Projection: memory-output provenance edges declared by the invocation ledger
    are present in the admitted Edge table. -/
theorem invocation_memory_input_provenance_persisted :
    ∀ registry memories goals edges inv,
      InvocationInGraph memories goals edges inv →
      InvocationProvenanceComplete registry inv →
      ∀ out : Memory, out ∈ inv.outputMemories →
      ∀ inp : Memory, inp ∈ inv.inputs →
        ∃ e : Edge, e ∈ edges ∧ EdgeHasClass registry e .Provenance ∧
          edge_source e = .memory out ∧ edge_target e = .memory inp := by
  intro registry memories goals edges inv hgraph hcomplete out hout inp hin
  obtain ⟨e, heInv, hc, hs, ht⟩ := hcomplete.memoryInputs out hout inp hin
  exact ⟨e, hgraph.outputEdgesPresent e heInv, hc, hs, ht⟩

/-- Projection: goal-output evidence edges declared by the invocation ledger are
    present in the admitted Edge table. -/
theorem invocation_goal_input_evidence_persisted :
    ∀ registry memories goals edges inv,
      InvocationInGraph memories goals edges inv →
      InvocationProvenanceComplete registry inv →
      ∀ g : Goal, g ∈ inv.outputGoals →
      ∀ inp : Memory, inp ∈ inv.inputs →
        ∃ e : Edge, e ∈ edges ∧ EdgeHasClass registry e .Structural ∧
          edge_source e = .goal g ∧ edge_target e = .memory inp := by
  intro registry memories goals edges inv hgraph hcomplete g hg inp hin
  obtain ⟨e, heInv, hc, hs, ht⟩ := hcomplete.goalInputs g hg inp hin
  exact ⟨e, hgraph.outputEdgesPresent e heInv, hc, hs, ht⟩

/-- Projection: input kind checks are phase-local ledger checks. -/
theorem invocation_input_kind_valid :
    ∀ inv, InvocationShapeValid inv → ∀ m : Memory, m ∈ inv.inputs →
      inv.phase.inputKind (memory_kind m) := by
  intro inv hshape m hm
  exact hshape.inputsShape m hm

/-- Projection: memory-output kind checks are phase-local ledger checks. -/
theorem invocation_output_memory_kind_valid :
    ∀ inv, InvocationShapeValid inv → ∀ m : Memory, m ∈ inv.outputMemories →
      inv.phase.outputMemoryKind (memory_kind m) := by
  intro inv hshape m hm
  exact hshape.outputMemoriesShape m hm

/-- F→A exclusivity, per doc 04 §Phase 2: "Exclusive per (input
    contract, operator id, output Abstraction schema)" within one
    source batch. OWNER-CONDITIONED (minimization pass): batch ids
    are unique only within `(source_id, owner)` (doc 01 Q6), so the
    gate's scope carries the owner dimension explicitly — without it
    the axiom would identify Abstractions across Owners whose sources
    coincidentally declared the same batch UUID. Decision:
    `docs/domain/decisions/2026-06-11-batch-id-scope.md`. -/
axiom ftoa_batch_exclusive :
  ∀ m1 m2 : Memory,
    memory_kind m1 = .Abstraction → memory_kind m2 = .Abstraction →
    memory_owner m1 = memory_owner m2 →
    memory_source_batch m1 ≠ none →
    memory_source_batch m1 = memory_source_batch m2 →
    memory_input_contract m1 = memory_input_contract m2 →
    memory_operator m1 = memory_operator m2 →
    memory_schema m1 = memory_schema m2 →
    m1 = m2

end Causa
