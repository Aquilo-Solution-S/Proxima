/-
Causa — Operators

The production rules (doc 02 §The Layering Principle, doc 04):

  F→A   : 2^F × wake context → A      facts become one typed Abstraction
  A→P   : 2^A × wake context → P      abstractions become one typed Perspective
  A→Goal: 2^A × wake context → Goal   goals derived from Abstraction evidence
  frame : P × A_cross → P             a Perspective whose payload references
                                       the cross-domain Abstraction — never a
                                       standalone edge

Wake context is not a materialized PersonalityInstance. In spec-mode
Lean, operators are not modeled as functions — entities plus
write-shape obligations carry the same content: what each phase may
produce, and what every derived memory must possess.

The v0.0.8 edge model removed the layer this file used to check against.
There is no `EdgeAuthorship` column to match a phase against and no relation
class to require, because the kind of a row follows the operation that wrote
it (E4). What a derived write declares it was made from — its ORIGINS — is
the whole claim, so the obligations that remain are exactly the ones the write
path checks: the declared inputs exist, they are of the phase's input kind,
and they are older than the row they ground.

CN-5 — no downward writes (A→F, P→A, P→F): the kernel face is now the phase's
own output contract (`operator_memory_output_not_fact`) rather than an edge
shape. "Dreaming" needs no axioms: dream outputs are ordinary typed writes
under these same rules (doc 02 §Wake/Dream/Write — no Dream entity, no Dream
kind, no Core dream pipeline; the ABSENCE of dream primitives here is
deliberate).

CN-9 (atomic invocation) and the wake-dispatcher loop are storage/runtime
contracts, not kernel axioms. Recorded as exclusions in COVERAGE.md.
-/

import Causa.Prelude
import Causa.Owner
import Causa.Identity
import Causa.Memory
import Causa.Goals
import Causa.Edges

namespace Causa

-- ============================================================
-- CN-8 — F→A source-batch gate (doc 04 §Source-batch lifecycle,
-- §Phase 2 F→A exclusivity)
-- ============================================================

/- Derived-memory operator metadata is structural on `Memory`:
   `memory_operator`, `memory_source_batch`, and `memory_input_contract` are
   ordinary field projections. Their values stay opaque equality tokens; the
   trusted CN-8 rule is a table-validity witness below, not a global axiom over
   all raw `Memory` values. -/

/-- F→A exclusivity, per doc 04 §Phase 2: "Exclusive per (input
    contract, operator id, output Abstraction schema)" within one
    source batch. OWNER-CONDITIONED (minimization pass): batch ids
    are unique only within `(source_id, owner)` (doc 01 Q6), so the
    gate's scope carries the owner dimension explicitly. This is admitted-table
    validity, not a global property of raw values: invalid duplicate rows can be
    constructed but not admitted into a valid table. -/
structure FtoaBatchExclusive (memories : Set Memory) : Prop where
  exclusive : ∀ m1 m2 : Memory,
    m1 ∈ memories → m2 ∈ memories →
    memory_kind m1 = .Abstraction → memory_kind m2 = .Abstraction →
    memory_owner m1 = memory_owner m2 →
    memory_source_batch m1 ≠ none →
    memory_source_batch m1 = memory_source_batch m2 →
    memory_input_contract m1 = memory_input_contract m2 →
    memory_operator m1 = memory_operator m2 →
    memory_schema m1 = memory_schema m2 →
    m1 = m2

/-- CN-8 projection theorem: a valid admitted table has at most one F→A
    Abstraction for each `(owner, source batch, input contract, operator,
    output schema)` gate. -/
theorem ftoa_batch_exclusive :
  ∀ memories : Set Memory,
    FtoaBatchExclusive memories →
    ∀ m1 m2 : Memory,
      m1 ∈ memories → m2 ∈ memories →
      memory_kind m1 = .Abstraction → memory_kind m2 = .Abstraction →
      memory_owner m1 = memory_owner m2 →
      memory_source_batch m1 ≠ none →
      memory_source_batch m1 = memory_source_batch m2 →
      memory_input_contract m1 = memory_input_contract m2 →
      memory_operator m1 = memory_operator m2 →
      memory_schema m1 = memory_schema m2 →
      m1 = m2 := by
  intro memories h m1 m2 hm1 hm2
  exact h.exclusive m1 m2 hm1 hm2

-- ============================================================
-- CN-8b — the operator invocation ledger
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

/-- The index kind an output declares its inputs with. A memory output DERIVED
    from its inputs declares them as origins; a Goal output RESTS on them, and
    a Goal declares only references (its evidence column). Consequent, never
    chosen — this def only records which consequence each phase has. -/
def OperatorPhase.inputEdgeKind : OperatorPhase → EdgeKind
  | .ftoa | .atoa | .atop => .origin
  | .atogoal => .reference

/-- A ledger/manifest for one operator run: declared inputs, outputs, and the
    index rows they imply. The kernel treats this as a consistency witness. It
    does not prove the input set was semantically complete, the prompt was
    good, or the model was right.

    A write with NO derivation declaration carries no manifest at all, because
    it has no derivation to prove (doc 16 §Kernel Invariants, E4). In this
    file that case is `inputs = ∅`, which every obligation below discharges
    vacuously (`invocation_without_inputs_is_complete`). -/
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

/-- Every index row the invocation declares carries the phase's consequent
    kind and is a valid row. There is no authorship column to cross-check and
    no relation to register: the kind IS the consequence. -/
structure InvocationEdgeShapeValid (inv : OperatorInvocation) : Prop where
  outputEdgesShape : ∀ e : Edge, e ∈ inv.outputEdges →
    EdgeValid e ∧ edge_kind e = inv.phase.inputEdgeKind

/-- Input completeness relative to the ledger: every declared input is
    represented by a declared index row from each relevant output. Memory
    outputs declare `origin` rows (what they were made from); Goal outputs
    declare `reference` rows (what they rest on). -/
structure InvocationProvenanceComplete (inv : OperatorInvocation) : Prop where
  memoryInputs : ∀ out : Memory, out ∈ inv.outputMemories →
    ∀ inp : Memory, inp ∈ inv.inputs →
      ∃ e : Edge, e ∈ inv.outputEdges ∧ edge_kind e = .origin ∧
        edge_source e = .memory out ∧ edge_target e = .memory inp
  goalInputs : ∀ g : Goal, g ∈ inv.outputGoals →
    ∀ inp : Memory, inp ∈ inv.inputs →
      ∃ e : Edge, e ∈ inv.outputEdges ∧ edge_kind e = .reference ∧
        edge_source e = .goal g ∧ edge_target e = .memory inp

/-- E4 — a write that declares nothing it was made from has no manifest to
    satisfy. The interpretation Perspective is exactly this case: it grounds
    through its references and consumes nothing, so the operator-invocation
    manifest is skipped rather than failed. -/
theorem invocation_without_inputs_is_complete :
    ∀ inv : OperatorInvocation, (∀ m : Memory, m ∉ inv.inputs) →
      InvocationProvenanceComplete inv := by
  intro inv hempty
  exact ⟨fun _ _ inp hin => absurd hin (hempty inp),
         fun _ _ inp hin => absurd hin (hempty inp)⟩

/-- Projection: memory-output origin rows declared by the ledger are present in
    the admitted index table. -/
theorem invocation_memory_input_provenance_persisted :
    ∀ memories goals edges inv,
      InvocationInGraph memories goals edges inv →
      InvocationProvenanceComplete inv →
      ∀ out : Memory, out ∈ inv.outputMemories →
      ∀ inp : Memory, inp ∈ inv.inputs →
        ∃ e : Edge, e ∈ edges ∧ edge_kind e = .origin ∧
          edge_source e = .memory out ∧ edge_target e = .memory inp := by
  intro memories goals edges inv hgraph hcomplete out hout inp hin
  obtain ⟨e, heInv, hc, hs, ht⟩ := hcomplete.memoryInputs out hout inp hin
  exact ⟨e, hgraph.outputEdgesPresent e heInv, hc, hs, ht⟩

/-- Projection: goal-output evidence rows declared by the ledger are present in
    the admitted index table. -/
theorem invocation_goal_input_evidence_persisted :
    ∀ memories goals edges inv,
      InvocationInGraph memories goals edges inv →
      InvocationProvenanceComplete inv →
      ∀ g : Goal, g ∈ inv.outputGoals →
      ∀ inp : Memory, inp ∈ inv.inputs →
        ∃ e : Edge, e ∈ edges ∧ edge_kind e = .reference ∧
          edge_source e = .goal g ∧ edge_target e = .memory inp := by
  intro memories goals edges inv hgraph hcomplete g hg inp hin
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

/-- CN-5 / P3 — operator memory outputs are NEVER Facts. THEOREM from the
    phase output contract alone: F→A and A→A produce Abstractions, A→P
    produces a Perspective, and A→Goal produces no memory row at all. Reality
    enters as typed Facts; an operator's generalization cannot re-enter as
    one. -/
theorem operator_memory_output_not_fact :
    ∀ (inv : OperatorInvocation) (m : Memory),
      InvocationShapeValid inv → m ∈ inv.outputMemories → memory_kind m ≠ .Fact := by
  intro inv m hshape hm
  have h := hshape.outputMemoriesShape m hm
  cases hphase : inv.phase with
  | ftoa => rw [hphase] at h; rw [h]; intro hf; exact (nomatch hf)
  | atoa => rw [hphase] at h; rw [h]; intro hf; exact (nomatch hf)
  | atop => rw [hphase] at h; rw [h]; intro hf; exact (nomatch hf)
  | atogoal => rw [hphase] at h; exact absurd h (by simp [OperatorPhase.outputMemoryKind])

/-- CN-1/CN-2/CN-3 in one — the input side of every phase is pinned by the
    ledger: an F→A run may only claim Facts, an A→A / A→P / A→Goal run only
    Abstractions. The old per-authorship edge-shape table said the same thing
    through a relation class it no longer needs. -/
theorem operator_inputs_match_phase :
    ∀ (inv : OperatorInvocation) (m : Memory),
      InvocationShapeValid inv → m ∈ inv.inputs →
        (inv.phase = .ftoa → memory_kind m = .Fact) ∧
        (inv.phase ≠ .ftoa → memory_kind m = .Abstraction) := by
  intro inv m hshape hm
  have h := hshape.inputsShape m hm
  constructor
  · intro hphase
    rw [hphase] at h
    exact h
  · intro hphase
    cases hp : inv.phase with
    | ftoa => exact absurd hp hphase
    | atoa => rw [hp] at h; exact h
    | atop => rw [hp] at h; exact h
    | atogoal => rw [hp] at h; exact h

/-- CN-5 — a derived row's origins point at rows at or below its own layer:
    the declaration is layer-checked, so provenance never runs upward. THEOREM
    from E3 applied to the ledger's own index rows. -/
theorem operator_origin_row_not_upward :
    ∀ (inv : OperatorInvocation) (e : Edge) (out inp : Memory),
      InvocationEdgeShapeValid inv → e ∈ inv.outputEdges →
      edge_source e = .memory out → edge_target e = .memory inp →
      (memory_kind inp).layer ≤ (memory_kind out).layer := by
  intro inv e out inp hshape he hs ht
  exact edge_layer_rule e (hshape.outputEdgesShape e he).1 out inp hs ht

end Causa
