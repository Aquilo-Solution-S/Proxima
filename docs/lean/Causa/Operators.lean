/-
Causa — Operators

Production shapes. Provenance is the output row's `origins[]` (or a Goal's
`evidence_t`). No edge ledger. No source-batch gate on Memory (CN-8 retired:
visit is a Fact + refs). Recipe metadata is sidecar, not a Memory column.
-/

import Causa.Prelude
import Causa.Owner
import Causa.Identity
import Causa.Memory
import Causa.Goals
import Causa.Edges

namespace Causa

inductive OperatorPhase where
  | ftoa
  | atoa
  | atop
  | atogoal
  deriving DecidableEq, Repr

def OperatorPhase.inputKind : OperatorPhase → MemoryKind → Prop
  | .ftoa, k => k = .Fact
  | .atoa, k => k = .Abstraction
  | .atop, k => k = .Abstraction
  | .atogoal, k => k = .Abstraction

def OperatorPhase.outputMemoryKind : OperatorPhase → MemoryKind → Prop
  | .ftoa, k => k = .Abstraction
  | .atoa, k => k = .Abstraction
  | .atop, k => k = .Perspective
  | .atogoal, _ => False

def OperatorPhase.outputGoalAllowed : OperatorPhase → Prop
  | .atogoal => True
  | _ => False

def OperatorPhase.inputEdgeKind : OperatorPhase → EdgeKind
  | .ftoa | .atoa | .atop => .origin
  | .atogoal => .reference

structure OperatorInvocation where
  phase : OperatorPhase
  operatorId : OperatorId
  inputContractId : InputContract
  inputs : Set Memory
  outputMemories : Set Memory
  outputGoals : Set Goal

structure InvocationInGraph
    (memories : Set Memory) (goals : Set Goal)
    (inv : OperatorInvocation) : Prop where
  inputsPresent : ∀ m : Memory, m ∈ inv.inputs → m ∈ memories
  outputMemoriesPresent : ∀ m : Memory, m ∈ inv.outputMemories → m ∈ memories
  outputGoalsPresent : ∀ g : Goal, g ∈ inv.outputGoals → g ∈ goals

structure InvocationShapeValid (inv : OperatorInvocation) : Prop where
  inputsShape : ∀ m : Memory, m ∈ inv.inputs →
    inv.phase.inputKind (memory_kind m)
  outputMemoriesShape : ∀ m : Memory, m ∈ inv.outputMemories →
    inv.phase.outputMemoryKind (memory_kind m)
  outputGoalsShape : ∀ g : Goal, g ∈ inv.outputGoals →
    inv.phase.outputGoalAllowed

/-- Completeness: each memory output names every input in `origins`;
    each Goal output names every input in `evidence_t`. -/
structure InvocationProvenanceComplete (inv : OperatorInvocation) : Prop where
  memoryInputs : ∀ out : Memory, out ∈ inv.outputMemories →
    ∀ inp : Memory, inp ∈ inv.inputs →
      memory_t inp ∈ memory_origins out
  goalInputs : ∀ g : Goal, g ∈ inv.outputGoals →
    ∀ inp : Memory, inp ∈ inv.inputs →
      memory_t inp ∈ goal_evidence g

/-- E4z — no declared inputs ⇒ nothing to prove. -/
theorem invocation_without_inputs_is_complete :
    ∀ inv : OperatorInvocation, (∀ m : Memory, m ∉ inv.inputs) →
      InvocationProvenanceComplete inv := by
  intro inv hempty
  exact ⟨fun _ _ inp hin => absurd hin (hempty inp),
         fun _ _ inp hin => absurd hin (hempty inp)⟩

theorem invocation_memory_input_provenance_persisted :
    ∀ memories goals inv,
      InvocationInGraph memories goals inv →
      InvocationProvenanceComplete inv →
      ∀ out : Memory, out ∈ inv.outputMemories →
      ∀ inp : Memory, inp ∈ inv.inputs →
        memory_t inp ∈ memory_origins out := by
  intro _ _ inv _ hcomplete out hout inp hin
  exact hcomplete.memoryInputs out hout inp hin

theorem invocation_goal_input_evidence_persisted :
    ∀ memories goals inv,
      InvocationInGraph memories goals inv →
      InvocationProvenanceComplete inv →
      ∀ g : Goal, g ∈ inv.outputGoals →
      ∀ inp : Memory, inp ∈ inv.inputs →
        memory_t inp ∈ goal_evidence g := by
  intro _ _ inv _ hcomplete g hg inp hin
  exact hcomplete.goalInputs g hg inp hin

theorem invocation_input_kind_valid :
    ∀ inv, InvocationShapeValid inv → ∀ m : Memory, m ∈ inv.inputs →
      inv.phase.inputKind (memory_kind m) := by
  intro inv hshape m hm
  exact hshape.inputsShape m hm

theorem invocation_output_memory_kind_valid :
    ∀ inv, InvocationShapeValid inv → ∀ m : Memory, m ∈ inv.outputMemories →
      inv.phase.outputMemoryKind (memory_kind m) := by
  intro inv hshape m hm
  exact hshape.outputMemoriesShape m hm

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

/-- Origins of a derived output sit at or below its layer (UML CHECKs). -/
theorem operator_origin_row_not_upward :
    ∀ (memories : Set Memory) (out inp : Memory),
      MemoryIdUnique memories →
      OriginKindValid memories out →
      out ∈ memories → inp ∈ memories →
      memory_t inp ∈ memory_origins out →
      (memory_kind inp).layer ≤ (memory_kind out).layer := by
  intro memories out inp huniq hv _hout hinp hin
  cases hk : memory_kind out with
  | Fact =>
    have hempty := hv.factEmpty hk
    rw [hempty] at hin
    cases hin
  | Abstraction =>
    obtain ⟨tgt, hmem, ht, hkind⟩ := hv.absFacts hk (memory_t inp) hin
    have heq : tgt = inp := huniq tgt inp hmem hinp ht
    have hkl : memory_kind inp = .Fact := by rw [← heq]; exact hkind
    have houtk : memory_kind out = .Abstraction := hk
    simp [MemoryKind.layer, hkl, houtk]
  | Perspective =>
    cases hv.perspAbsOrEmpty hk with
    | inl hempty =>
      rw [hempty] at hin
      cases hin
    | inr hall =>
      obtain ⟨tgt, hmem, ht, hkind⟩ := hall (memory_t inp) hin
      have heq : tgt = inp := huniq tgt inp hmem hinp ht
      have hkl : memory_kind inp = .Abstraction := by rw [← heq]; exact hkind
      have houtk : memory_kind out = .Perspective := hk
      simp [MemoryKind.layer, hkl, houtk]

end Causa
