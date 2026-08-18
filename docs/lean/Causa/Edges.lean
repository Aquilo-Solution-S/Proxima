/-
Causa — Pins (no Edge table)

v0.0.8: there is no index table. `origins[]` and `refs[]` live on the
Memory row (UML §5). The two closed kinds remain:

  origin    = made-from   (origins)
  reference = points-at   (refs)

Rebuildability is identity: the pin set IS node content. Goal pins live on
the Goal row (`assignment_t`, `dependency_t`, `evidence_t`, `close_fact_t`,
`write_act_t`) and never appear in `memory.refs`.

E1–E7 rebase:
  E1 existence   — every pinned t is a hot Memory.t or a cooled stub
  E2 ownership   — the pin is on the declaring row; no separate owner column
  E3 layering    — OriginKindValid (UML CHECKs)
  E4 kind follows — origins vs refs; no verb writes a pin
  E5 / E6        — there is no pin row, so no pin id and no pin payload
  E7 rebuild     — derivePins m = (m.origins, m.refs)
-/

import Causa.Prelude
import Causa.Owner
import Causa.Identity
import Causa.Memory
import Causa.Goals

namespace Causa

inductive EdgeKind where
  | origin
  | reference
  deriving DecidableEq, Repr

/-- Pins extracted from a node — THE derivation, and it is identity. -/
def derivePins (m : Memory) : List MemoryId × List MemoryId :=
  (memory_origins m, memory_refs m)

theorem pins_are_node_content (m : Memory) :
    derivePins m = (m.origins, m.refs) := rfl

theorem derived_table_rebuildable (m : Memory) :
    derivePins m = (memory_origins m, memory_refs m) :=
  pins_are_node_content m

/-- E4z — a write with empty origins is legal (interpretation Perspective). -/
theorem declaration_without_origins_writes_no_origin_pins (m : Memory)
    (h : memory_origins m = []) :
    ∀ id : MemoryId, id ∈ memory_origins m → False := by
  intro id hin
  rw [h] at hin
  cases hin

/-- Fact sources declare no origins, so they never originate from A/P. -/
theorem fact_source_reaches_only_facts (m : Memory)
    (hk : memory_kind m = .Fact) :
    memory_origins m = [] :=
  m.fact_origins_empty hk

/-- Layer rule on origins: A origins are Facts or Abstractions; P origins
    are empty or A's. Targets may be hot or cooled. -/
theorem origin_layer_rule
    (memories : Set Memory) (cooled : Set Cooled) (m : Memory)
    (hv : OriginKindValid memories cooled m) :
    (memory_kind m = .Abstraction →
      ∀ id : MemoryId, id ∈ memory_origins m →
        pinKindFactOrAbstraction memories cooled id) ∧
    (memory_kind m = .Perspective →
      memory_origins m = [] ∨
      ∀ id : MemoryId, id ∈ memory_origins m →
        pinKindIs memories cooled id .Abstraction ∧
          MemoryKind.layer .Abstraction ≤ (memory_kind m).layer) := by
  constructor
  · intro habs id hid
    exact hv.absFactOrAbs habs id hid
  · intro hp
    cases hv.perspAbsOrEmpty hp with
    | inl hempty => exact Or.inl hempty
    | inr hall =>
      refine Or.inr ?_
      intro id hid
      refine ⟨hall id hid, ?_⟩
      rw [hp]
      exact Nat.le_succ 1

theorem abstraction_origin_fact_or_abstraction
    (memories : Set Memory) (cooled : Set Cooled) (m : Memory) (id : MemoryId)
    (hv : OriginKindValid memories cooled m)
    (habs : memory_kind m = .Abstraction)
    (hid : id ∈ memory_origins m) :
    pinKindFactOrAbstraction memories cooled id :=
  hv.absFactOrAbs habs id hid

/-- Interpretation is a Perspective with empty origins that refs a subject. -/
def interpretationOf (p : Memory) (subject : Memory) : Prop :=
  memory_kind p = .Perspective ∧
  memory_origins p = [] ∧
  memory_t subject ∈ memory_refs p

theorem interpretation_is_never_a_fact
    (p subject : Memory) (h : interpretationOf p subject) :
    memory_kind p ≠ .Fact := by
  intro hf
  rw [h.1] at hf
  exact (nomatch hf)

theorem interpretation_rows_are_references
    (p subject : Memory) (h : interpretationOf p subject) :
    memory_t subject ∈ memory_refs p :=
  h.2.2

/-- Goal pins are the Goal-row columns, never `Memory.refs`. -/
theorem goal_declared_rows_are_references (g : Goal) :
    goalDeclaredTargetIds g =
      (goal_assignment g).toList ++ goal_dependencies g ++ goal_evidence g ++
        (goal_close_fact_t g).toList ++ (goal_write_act_t g).toList :=
  rfl

theorem goal_declared_row_count (g : Goal) :
    (goalDeclaredTargetIds g).length =
      ((goal_assignment g).toList ++ goal_dependencies g ++ goal_evidence g ++
        (goal_close_fact_t g).toList ++ (goal_write_act_t g).toList).length := rfl

/-- Closed vocabulary: two kinds. -/
theorem principle_epistemic_edge_kinds_are_exactly_two_aux (k : EdgeKind) :
    k = .origin ∨ k = .reference := by
  cases k with
  | origin => exact Or.inl rfl
  | reference => exact Or.inr rfl

/-- info: 'Causa.pins_are_node_content' does not depend on any axioms -/
#guard_msgs in
#print axioms pins_are_node_content

/-- info: 'Causa.fact_source_reaches_only_facts' does not depend on any axioms -/
#guard_msgs in
#print axioms fact_source_reaches_only_facts

/-- info: 'Causa.interpretation_is_never_a_fact' does not depend on any axioms -/
#guard_msgs in
#print axioms interpretation_is_never_a_fact

/-- info: 'Causa.declaration_without_origins_writes_no_origin_pins' does not depend on any axioms -/
#guard_msgs in
#print axioms declaration_without_origins_writes_no_origin_pins

/-- info: 'Causa.origin_layer_rule' does not depend on any axioms -/
#guard_msgs in
#print axioms origin_layer_rule

/-- info: 'Causa.abstraction_origin_fact_or_abstraction' does not depend on any axioms -/
#guard_msgs in
#print axioms abstraction_origin_fact_or_abstraction

end Causa
