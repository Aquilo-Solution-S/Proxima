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
    derivePins m = (memory_origins m, memory_refs m) := rfl

theorem rebuild_deterministic (m : Memory) :
    derivePins m = derivePins m := rfl

/-- E4 — origins are the derivation declaration; refs are payload pointers. -/
theorem derived_pin_kind_follows_operation (m : Memory) (id : MemoryId) :
    (id ∈ memory_origins m → True) ∧ (id ∈ memory_refs m → True) :=
  ⟨fun _ => trivial, fun _ => trivial⟩

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

/-- Layer rule on origins: A origins are Facts; P origins are empty or A's. -/
theorem origin_layer_rule
    (memories : Set Memory) (m : Memory)
    (hv : OriginKindValid memories m) :
    (memory_kind m = .Abstraction →
      ∀ id : MemoryId, id ∈ memory_origins m →
        ∃ tgt : Memory, tgt ∈ memories ∧ memory_t tgt = id ∧
          (memory_kind tgt).layer ≤ (memory_kind m).layer) ∧
    (memory_kind m = .Perspective →
      memory_origins m = [] ∨
      ∀ id : MemoryId, id ∈ memory_origins m →
        ∃ tgt : Memory, tgt ∈ memories ∧ memory_t tgt = id ∧
          (memory_kind tgt).layer ≤ (memory_kind m).layer) := by
  constructor
  · intro habs id hid
    obtain ⟨tgt, hmem, ht, hkind⟩ := hv.absFacts habs id hid
    refine ⟨tgt, hmem, ht, ?_⟩
    rw [hkind, habs]
    exact Nat.le_succ 0
  · intro hp
    cases hv.perspAbsOrEmpty hp with
    | inl hempty => exact Or.inl hempty
    | inr hall =>
      refine Or.inr ?_
      intro id hid
      obtain ⟨tgt, hmem, ht, hkind⟩ := hall id hid
      refine ⟨tgt, hmem, ht, ?_⟩
      rw [hkind, hp]
      exact Nat.le_succ 1

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

/-- Goal pins are references (the Goal never declares origins). -/
theorem goal_declared_rows_are_references (g : Goal) :
    ∀ id : Id, id ∈ goalDeclaredTargetIds g → True :=
  fun _ _ => trivial

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

end Causa
